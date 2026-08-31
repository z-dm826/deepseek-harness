use std::env;
use std::fs;
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use serde::{Deserialize, Serialize};
use tauri::{Manager, State, Url};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ProxyConfig {
    pub enabled: bool,
    pub http_proxy: String,
    pub https_proxy: String,
    pub no_proxy: String,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            http_proxy: "http://127.0.0.1:7890".to_string(),
            https_proxy: "http://127.0.0.1:7890".to_string(),
            no_proxy: "localhost,127.0.0.1".to_string(),
        }
    }
}

pub struct AppState {
    pub proxy: Arc<Mutex<ProxyConfig>>,
    pub child: Arc<Mutex<Option<Child>>>,
    pub port: Arc<Mutex<u16>>,
    pub project_dir: PathBuf,
}

fn get_config_path() -> PathBuf {
    let mut path = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    path.push(".dsh");
    let _ = fs::create_dir_all(&path);
    path.push("desktop_proxy.json");
    path
}

fn load_proxy_config() -> ProxyConfig {
    let path = get_config_path();
    if path.exists() {
        if let Ok(content) = fs::read_to_string(&path) {
            if let Ok(config) = serde_json::from_str::<ProxyConfig>(&content) {
                return config;
            }
        }
    }
    ProxyConfig::default()
}

fn save_proxy_config(config: &ProxyConfig) -> Result<(), String> {
    let path = get_config_path();
    let content = serde_json::to_string_pretty(config).map_err(|e| e.to_string())?;
    fs::write(path, content).map_err(|e| e.to_string())?;
    Ok(())
}

fn find_node_binary() -> String {
    if let Ok(node_path) = env::var("DSH_NODE_PATH") {
        if PathBuf::from(&node_path).exists() {
            println!("[dsh-desktop] Using Node from DSH_NODE_PATH: {}", node_path);
            return node_path;
        }
    }

    let mut search_path = env::var("PATH").unwrap_or_default();
    let extra_paths = "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin";
    if search_path.is_empty() {
        search_path = extra_paths.to_string();
    } else {
        search_path = format!("{}:{}", extra_paths, search_path);
    }

    if let Ok(output) = Command::new("node").env("PATH", &search_path).arg("--version").output() {
        if output.status.success() {
            let ver = String::from_utf8_lossy(&output.stdout);
            println!("[dsh-desktop] Found Node.js in PATH: {}", ver.trim());
            return "node".to_string();
        }
    }

    let home = dirs::home_dir();
    let mut candidates = vec![
        PathBuf::from("/opt/homebrew/bin/node"),
        PathBuf::from("/usr/local/bin/node"),
        PathBuf::from("/usr/bin/node"),
    ];

    if let Some(home_dir) = &home {
        candidates.push(home_dir.join(".fnm/current/bin/node"));
        candidates.push(home_dir.join(".n/bin/node"));
        candidates.push(home_dir.join(".volta/bin/node"));

        let nvm_dir = home_dir.join(".nvm/versions/node");
        if let Ok(entries) = fs::read_dir(&nvm_dir) {
            for entry in entries.flatten() {
                let nvm_node = entry.path().join("bin/node");
                if nvm_node.exists() {
                    candidates.push(nvm_node);
                }
            }
        }

        let fnm_state_dir = home_dir.join(".local/state/fnm_multishells");
        if let Ok(entries) = fs::read_dir(&fnm_state_dir) {
            for entry in entries.flatten() {
                let fnm_node = entry.path().join("bin/node");
                if fnm_node.exists() {
                    candidates.push(fnm_node);
                }
            }
        }
    }

    for path in candidates {
        if path.exists() {
            let s = path.to_string_lossy().to_string();
            println!("[dsh-desktop] Found node executable at {}", s);
            return s;
        }
    }

    "node".to_string()
}

fn find_project_dir() -> PathBuf {
    if let Ok(env_dir) = env::var("DSH_PROJECT_DIR") {
        let p = PathBuf::from(env_dir);
        if p.exists() {
            println!("[dsh-desktop] Using project dir from DSH_PROJECT_DIR: {:?}", p);
            return p;
        }
    }

    let check_dir = |dir: &PathBuf| -> bool {
        dir.join("apps/cli/src/bin.ts").exists() || dir.join("apps/cli/lib/bin.js").exists()
    };

    if let Ok(curr) = env::current_dir() {
        let mut p = curr;
        for _ in 0..10 {
            if check_dir(&p) {
                println!("[dsh-desktop] Found project dir via current_dir traversal: {:?}", p);
                return p;
            }
            if !p.pop() {
                break;
            }
        }
    }

    if let Ok(exe) = env::current_exe() {
        let mut p = exe;
        for _ in 0..10 {
            if check_dir(&p) {
                println!("[dsh-desktop] Found project dir via current_exe traversal: {:?}", p);
                return p;
            }
            if !p.pop() {
                break;
            }
        }
    }

    let fallback = PathBuf::from("/Users/mm/00zzz/05app/deepseek-harness");
    if check_dir(&fallback) {
        println!("[dsh-desktop] Found project dir via hardcoded fallback: {:?}", fallback);
        return fallback;
    }

    eprintln!("[dsh-desktop] Could not resolve project dir containing apps/cli!");
    env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn start_dsh_process(project_dir: &PathBuf, proxy: &ProxyConfig, port: u16) -> Option<Child> {
    let node_bin = find_node_binary();
    let bin_js = project_dir.join("apps/cli/lib/bin.js");
    let bin_ts = project_dir.join("apps/cli/src/bin.ts");

    let mut cmd = Command::new(&node_bin);

    if bin_js.exists() {
        println!("[dsh-desktop] Launching compiled backend: {:?}", bin_js);
        cmd.arg(&bin_js)
           .arg("web")
           .arg("--port")
           .arg(port.to_string());
    } else if bin_ts.exists() {
        println!("[dsh-desktop] Launching TypeScript source backend: {:?}", bin_ts);
        cmd.arg("--import")
           .arg("tsx/esm")
           .arg(&bin_ts)
           .arg("web")
           .arg("--port")
           .arg(port.to_string());
    } else {
        eprintln!("[dsh-desktop] Cannot find backend entry point! Checked: {:?} and {:?}", bin_js, bin_ts);
        return None;
    }

    cmd.current_dir(project_dir);

    let current_path = env::var("PATH").unwrap_or_default();
    let extra_paths = "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin";
    let enriched_path = if !current_path.is_empty() {
        format!("{}:{}", extra_paths, current_path)
    } else {
        extra_paths.to_string()
    };
    cmd.env("PATH", enriched_path);

    if proxy.enabled {
        println!("[dsh-desktop] Starting backend WITH proxy settings:");
        println!("  HTTP_PROXY  = {}", proxy.http_proxy);
        println!("  HTTPS_PROXY = {}", proxy.https_proxy);
        println!("  NO_PROXY    = {}", proxy.no_proxy);

        cmd.env("HTTP_PROXY", &proxy.http_proxy)
           .env("HTTPS_PROXY", &proxy.https_proxy)
           .env("ALL_PROXY", &proxy.http_proxy)
           .env("NO_PROXY", &proxy.no_proxy)
           .env("http_proxy", &proxy.http_proxy)
           .env("https_proxy", &proxy.https_proxy)
           .env("all_proxy", &proxy.http_proxy)
           .env("no_proxy", &proxy.no_proxy);
    } else {
        println!("[dsh-desktop] Starting backend WITHOUT proxy settings");
    }

    match cmd.spawn() {
        Ok(child) => {
            println!("[dsh-desktop] Spawned dsh backend process (PID: {})", child.id());
            Some(child)
        }
        Err(err) => {
            eprintln!("[dsh-desktop] Failed to spawn dsh backend with '{}': {}", node_bin, err);
            None
        }
    }
}

fn wait_for_server(port: u16, max_retries: u32) -> bool {
    let addr = format!("127.0.0.1:{}", port);
    for i in 0..max_retries {
        if TcpStream::connect(&addr).is_ok() {
            return true;
        }
        if i > 0 && i % 10 == 0 {
            println!("[dsh-desktop] Still waiting for server at {}... ({}/{})", addr, i, max_retries);
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    false
}

#[tauri::command]
fn get_proxy_config(state: State<'_, AppState>) -> ProxyConfig {
    let proxy = state.proxy.lock().unwrap();
    proxy.clone()
}

#[tauri::command]
fn save_and_restart(state: State<'_, AppState>, config: ProxyConfig) -> Result<String, String> {
    save_proxy_config(&config)?;
    
    {
        let mut p = state.proxy.lock().unwrap();
        *p = config.clone();
    }

    {
        let mut child_guard = state.child.lock().unwrap();
        if let Some(mut child) = child_guard.take() {
            let _ = child.kill();
            let _ = child.wait();
            println!("[dsh-desktop] Killed previous dsh backend process");
        }

        let port = *state.port.lock().unwrap();
        let new_child = start_dsh_process(&state.project_dir, &config, port);
        *child_guard = new_child;
    }

    Ok("Proxy configuration updated and backend restarted successfully".to_string())
}

#[tauri::command]
fn get_backend_status(state: State<'_, AppState>) -> serde_json::Value {
    let child_guard = state.child.lock().unwrap();
    let is_running = child_guard.is_some();
    let port = *state.port.lock().unwrap();
    let proxy = state.proxy.lock().unwrap().clone();

    serde_json::json!({
        "running": is_running,
        "port": port,
        "url": format!("http://127.0.0.1:{}", port),
        "proxy": proxy,
    })
}

fn main() {
    let initial_proxy = load_proxy_config();
    let project_dir = find_project_dir();
    
    let default_port = 51730;
    
    let initial_child = start_dsh_process(&project_dir, &initial_proxy, default_port);

    let state = AppState {
        proxy: Arc::new(Mutex::new(initial_proxy)),
        child: Arc::new(Mutex::new(initial_child)),
        port: Arc::new(Mutex::new(default_port)),
        project_dir,
    };

    tauri::Builder::default()
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            get_proxy_config,
            save_and_restart,
            get_backend_status
        ])
        .setup(move |app| {
            let port = default_port;
            let app_handle = app.handle().clone();
            
            std::thread::spawn(move || {
                println!("[dsh-desktop] Waiting for backend server on port {}...", port);
                let ready = wait_for_server(port, 100);
                if ready {
                    println!("[dsh-desktop] Backend is ready. Navigating window...");
                } else {
                    eprintln!("[dsh-desktop] Backend server timed out; opening window anyway.");
                }

                let target_url = format!("http://127.0.0.1:{}", port);
                if let Some(window) = app_handle.get_webview_window("main") {
                    if let Ok(url) = Url::parse(&target_url) {
                        let _ = window.navigate(url);
                    }
                }
            });
            
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
