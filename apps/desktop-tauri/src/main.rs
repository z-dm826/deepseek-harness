use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::{Arc, Mutex};
use serde::{Deserialize, Serialize};
use tauri::State;

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

fn start_dsh_process(project_dir: &PathBuf, proxy: &ProxyConfig, port: u16) -> Option<Child> {
    let bin_ts = project_dir.join("apps/cli/src/bin.ts");
    if !bin_ts.exists() {
        eprintln!("[dsh-desktop] Cannot find bin.ts at {:?}", bin_ts);
        return None;
    }

    let mut cmd = Command::new("node");
    cmd.arg("--import")
        .arg("tsx/esm")
        .arg(&bin_ts)
        .arg("web")
        .arg("--port")
        .arg(port.to_string())
        .current_dir(project_dir);

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
            eprintln!("[dsh-desktop] Failed to spawn dsh backend: {}", err);
            None
        }
    }
}

#[tauri::command]
fn get_proxy_config(state: State<'_, AppState>) -> ProxyConfig {
    let proxy = state.proxy.lock().unwrap();
    proxy.clone()
}

#[tauri::command]
fn save_and_restart(state: State<'_, AppState>, config: ProxyConfig) -> Result<String, String> {
    save_proxy_config(&config)?;

    // Update state
    {
        let mut p = state.proxy.lock().unwrap();
        *p = config.clone();
    }

    // Kill old process if running
    {
        let mut child_guard = state.child.lock().unwrap();
        if let Some(mut child) = child_guard.take() {
            let _ = child.kill();
            let _ = child.wait();
            println!("[dsh-desktop] Killed previous dsh backend process");
        }

        // Restart with new proxy config
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
    let current_dir = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    // Resolve deepseek-harness root directory
    let mut project_dir = current_dir.clone();
    if project_dir.ends_with("apps/desktop-tauri") {
        project_dir.pop();
        project_dir.pop();
    }

    let default_port = 51730;

    // Start backend process
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
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
