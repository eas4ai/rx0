//! rx0 desktop shell: spawns the rx0 server as a sidecar and shows
//! its UI in a system webview.
//!
//! U2: sidecar lifecycle against the launch directory (U3 adds folder
//! selection). The sidecar starts with `--port 0 --no-open`; its
//! `serving ... at {url}` stdout line tells the window where to go.
//! Only loopback URLs are ever loaded.

use tauri::{AppHandle, Manager, Url, WindowEvent};
use tauri_plugin_shell::process::{CommandChild, CommandEvent};
use tauri_plugin_shell::ShellExt;

/// `externalBin` entry in tauri.conf.json (triple-suffixed on disk).
const SIDECAR: &str = "rx0-sidecar";

struct SidecarState(tauri::async_runtime::Mutex<Option<CommandChild>>);

/// `rx0 {VERSION} serving {root} at {url}` -> url, loopback only.
fn serving_url(line: &str) -> Option<Url> {
    let url = line.rsplit(" at ").next()?;
    let url: Url = url.trim().parse().ok()?;
    match url.host_str() {
        Some("127.0.0.1") | Some("::1") | Some("localhost") => Some(url),
        _ => None,
    }
}

async fn boot_sidecar(app: AppHandle, target: String) {
    let cmd = match app.shell().sidecar(SIDECAR) {
        Ok(cmd) => cmd,
        Err(e) => {
            eprintln!("rx0 desktop: cannot build sidecar command: {e}");
            return;
        }
    };
    let (mut rx, child) = match cmd.args(["--port", "0", "--no-open", &target]).spawn() {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("rx0 desktop: cannot spawn sidecar: {e}");
            return;
        }
    };
    *app.state::<SidecarState>().0.lock().await = Some(child);
    while let Some(event) = rx.recv().await {
        match event {
            CommandEvent::Stdout(bytes) => {
                let line = String::from_utf8_lossy(&bytes);
                if let Some(url) = serving_url(&line) {
                    if let Some(window) = app.get_webview_window("main") {
                        if let Err(e) = window.navigate(url) {
                            eprintln!("rx0 desktop: cannot navigate to sidecar: {e}");
                        }
                    }
                }
            }
            CommandEvent::Stderr(bytes) => {
                eprintln!(
                    "rx0-sidecar: {}",
                    String::from_utf8_lossy(&bytes).trim_end()
                );
            }
            _ => {}
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // U2 serves the launch directory; U3 replaces this with the picked,
    // remembered, or CLI-passed folder.
    let target = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| ".".to_string());
    tauri::Builder::default()
        .manage(SidecarState(tauri::async_runtime::Mutex::new(None)))
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_single_instance::init(|_, _, _| {}))
        .plugin(tauri_plugin_store::Builder::default().build())
        .plugin(tauri_plugin_window_state::Builder::default().build())
        .setup(|app| {
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move { boot_sidecar(handle, target).await });
            Ok(())
        })
        .on_window_event(|window, event| {
            // CloseRequested first, Destroyed as the backstop: either way
            // the private server must not outlive the window. (A SIGKILLed
            // shell still orphans its sidecar, like any desktop app.)
            if matches!(
                event,
                WindowEvent::CloseRequested { .. } | WindowEvent::Destroyed
            ) {
                if let Some(child) = window.state::<SidecarState>().0.blocking_lock().take() {
                    let _ = child.kill();
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("rx0 desktop failed to start");
}
