//! rx0 desktop shell: spawns the rx0 server as a sidecar and shows
//! its UI in a system webview.
//!
//! U3: the served folder resolves as CLI path arg, then the remembered
//! folder, then a folder picker (cancel quits: the installed app has no
//! meaningful working directory). A second launch with a folder arg
//! focuses the window and restarts the sidecar there. Only loopback
//! sidecar URLs are ever loaded.

use std::path::PathBuf;
use tauri::{AppHandle, Manager, Url, WindowEvent};
use tauri_plugin_dialog::{DialogExt, FilePath};
use tauri_plugin_shell::process::{CommandChild, CommandEvent};
use tauri_plugin_shell::ShellExt;
use tauri_plugin_store::StoreExt;

/// `externalBin` entry in tauri.conf.json (triple-suffixed on disk).
const SIDECAR: &str = "rx0-sidecar";
/// Remembered-folder key in the store.
const WORKSPACE_KEY: &str = "workspace";

struct SidecarState {
    child: tauri::async_runtime::Mutex<Option<CommandChild>>,
    target: tauri::async_runtime::Mutex<String>,
}

/// `rx0 {VERSION} serving {root} at {url}` -> url, loopback only.
fn serving_url(line: &str) -> Option<Url> {
    let url = line.rsplit(" at ").next()?;
    let url: Url = url.trim().parse().ok()?;
    match url.host_str() {
        // host_str keeps IPv6 brackets: "[::1]".
        Some("127.0.0.1") | Some("[::1]") | Some("localhost") => Some(url),
        _ => None,
    }
}

/// A CLI token to a workspace dir: dirs as-is, files via their parent.
fn cli_target(args: &[String]) -> Option<String> {
    let raw = args.iter().skip(1).find(|a| !a.starts_with('-'))?;
    let path = PathBuf::from(raw);
    if path.is_dir() {
        return Some(path.to_string_lossy().into_owned());
    }
    if path.is_file() {
        return path
            .parent()
            .map(|p| p.to_string_lossy().into_owned())
            .filter(|s| !s.is_empty());
    }
    None
}

async fn remember(app: &AppHandle, target: &str) {
    if let Ok(store) = app.store("rx0-desktop.json") {
        store.set(WORKSPACE_KEY, serde_json::Value::String(target.to_string()));
        let _ = store.save();
    }
}

async fn remembered(app: &AppHandle) -> Option<String> {
    let store = app.store("rx0-desktop.json").ok()?;
    let value = store.get(WORKSPACE_KEY)?;
    let dir = value.as_str()?.to_string();
    if PathBuf::from(&dir).is_dir() {
        Some(dir)
    } else {
        None
    }
}

async fn pick_folder(app: &AppHandle) -> Option<String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog().file().pick_folder(move |picked| {
        let _ = tx.send(picked);
    });
    let picked = rx.await.ok()??;
    match picked {
        FilePath::Path(path) => Some(path.to_string_lossy().into_owned()),
        FilePath::Url(_) => None,
    }
}

/// CLI arg, remembered folder, folder picker; None when the user
/// cancels the picker (the app then quits: no sane default exists).
async fn resolve_target(app: &AppHandle) -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    if let Some(dir) = cli_target(&args) {
        return Some(dir);
    }
    if let Some(dir) = remembered(app).await {
        return Some(dir);
    }
    pick_folder(app).await
}

/// Kill the running sidecar (if any) and start one serving `target`,
/// navigating the main window once its URL arrives.
async fn start_sidecar(app: &AppHandle, target: &str) {
    if let Some(child) = app.state::<SidecarState>().child.lock().await.take() {
        let _ = child.kill();
    }
    let cmd = match app.shell().sidecar(SIDECAR) {
        Ok(cmd) => cmd,
        Err(e) => {
            eprintln!("rx0 desktop: cannot build sidecar command: {e}");
            return;
        }
    };
    let (mut rx, child) = match cmd.args(["--port", "0", "--no-open", target]).spawn() {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("rx0 desktop: cannot spawn sidecar: {e}");
            return;
        }
    };
    *app.state::<SidecarState>().child.lock().await = Some(child);
    *app.state::<SidecarState>().target.lock().await = target.to_string();
    remember(app, target).await;
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

/// Second launch: a folder arg restarts the sidecar there, otherwise
/// the running window just focuses.
fn handle_second_launch(app: &AppHandle, args: Vec<String>) {
    if let Some(dir) = cli_target(&args) {
        let handle = app.clone();
        tauri::async_runtime::spawn(async move {
            let same = handle.state::<SidecarState>().target.lock().await.clone();
            if same != dir {
                start_sidecar(&handle, &dir).await;
            }
        });
    }
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.set_focus();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serving_url_accepts_loopback_only() {
        let good = serving_url("rx0 0.1.5 serving /r at http://127.0.0.1:41569");
        assert_eq!(good.unwrap().port(), Some(41569));
        assert!(serving_url("rx0 0.1.5 serving /r at http://[::1]:1234/").is_some());
        // Anything else — remote hosts, missing URLs, noise — is refused.
        for bad in [
            "rx0 0.1.5 serving /r at http://example.com:80/",
            "rx0 0.1.5 serving /r at http://192.168.1.2:80/",
            "rx0 0.1.5 serving /r at http://127.0.0.1.evil.com/",
            "rx0: cannot listen on 127.0.0.1:7777: address in use",
            "",
        ] {
            assert!(serving_url(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn cli_target_skips_flags_and_missing_paths() {
        // One positional TARGET only; flags and their values never parse
        // as paths (a bare value falls through to remembered/picker).
        let argv = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(cli_target(&argv(&["rx0", "--verbose"])), None);
        assert_eq!(cli_target(&argv(&["rx0", "0"])), None);
        assert_eq!(cli_target(&argv(&["rx0", "/no/such/dir"])), None);
        assert_eq!(
            cli_target(&argv(&["rx0", "/tmp"])),
            Some("/tmp".to_string())
        );
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(SidecarState {
            child: tauri::async_runtime::Mutex::new(None),
            target: tauri::async_runtime::Mutex::new(String::new()),
        })
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            handle_second_launch(app, args);
        }))
        .plugin(tauri_plugin_store::Builder::default().build())
        .plugin(tauri_plugin_window_state::Builder::default().build())
        .setup(|app| {
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                match resolve_target(&handle).await {
                    Some(target) => start_sidecar(&handle, &target).await,
                    None => handle.exit(0),
                }
            });
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
                if let Some(child) = window.state::<SidecarState>().child.blocking_lock().take() {
                    let _ = child.kill();
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("rx0 desktop failed to start");
}
