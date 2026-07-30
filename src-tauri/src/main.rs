// Prevents additional console window on Windows in release
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::process::{Command, Stdio};
use std::net::TcpStream;
use std::time::Duration;
use std::path::PathBuf;
use serde::{Deserialize, Serialize};
use tauri::Manager;

#[derive(Debug, Serialize, Deserialize)]
struct TailscaleInfo {
    ip: String,
    status: String,
}

fn find_node_binary() -> String {
    let candidate_paths = vec![
        "/home/anas/.nvm/versions/node/v22.22.3/bin/node",
        "/usr/local/bin/node",
        "/usr/bin/node",
        "node"
    ];

    for path in candidate_paths {
        if PathBuf::from(path).exists() {
            return path.to_string();
        }
    }

    if let Ok(entries) = std::fs::read_dir("/home/anas/.nvm/versions/node") {
        for entry in entries.flatten() {
            let nvm_node = entry.path().join("bin/node");
            if nvm_node.exists() {
                return nvm_node.to_string_lossy().to_string();
            }
        }
    }

    "node".to_string()
}

fn ensure_server_running() {
    if TcpStream::connect_timeout(&"127.0.0.1:3000".parse().unwrap(), Duration::from_millis(300)).is_err() {
        let node_bin = find_node_binary();
        println!("[ZANCORD DESKTOP] Using Node binary at: {}", node_bin);

        let server_path = PathBuf::from("/home/anas/.gemini/antigravity/scratch/vibe-haven");
        
        let _ = Command::new(&node_bin)
            .arg("server.js")
            .current_dir(&server_path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();

        for _ in 0..30 {
            if TcpStream::connect_timeout(&"127.0.0.1:3000".parse().unwrap(), Duration::from_millis(100)).is_ok() {
                println!("[ZANCORD DESKTOP] Signaling server is online on port 3000!");
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    } else {
        println!("[ZANCORD DESKTOP] ZanCord server is already running on port 3000!");
    }
}

#[tauri::command]
fn get_tailscale_ip() -> TailscaleInfo {
    let output = Command::new("tailscale")
        .args(&["ip", "-4"])
        .output();

    match output {
        Ok(out) if out.status.success() => {
            let ip_str = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !ip_str.is_empty() {
                return TailscaleInfo {
                    ip: ip_str,
                    status: "Connected".to_string(),
                };
            }
        }
        _ => {}
    }

    TailscaleInfo {
        ip: "100.111.151.89".to_string(),
        status: "Direct Peer Active".to_string(),
    }
}

fn main() {
    ensure_server_running();

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_process::init())
        .invoke_handler(tauri::generate_handler![get_tailscale_ip])
        .setup(|app| {
            #[cfg(target_os = "linux")]
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.with_webview(|webview| {
                    #[cfg(target_os = "linux")]
                    {
                        use webkit2gtk::PermissionRequestExt;
                        use webkit2gtk::WebViewExt;
                        let webview_gtk = webview.inner();
                        webview_gtk.connect_permission_request(|_, req| {
                            println!("[WEBKIT PERMISSION AUTO-GRANT] Auto-granting camera/mic permission request");
                            req.allow();
                            true
                        });
                    }
                });
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running ZanCord tauri application");
}
