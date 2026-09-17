//! Bottom-drawer terminal: one interactive shell per server, rooted at
//! the workspace, driven over a websocket by xterm.js.
//!
//! The manager owns a single [`Session`]. `start` spawns the shell under
//! a native PTY ([`portable_pty`]); a reader thread forwards output chunks
//! into a channel the websocket task drains. `stop` (and `Drop`) kills
//! the child and closes the PTY, so no orphaned shell survives the page
//! or the server going away.

use std::io::Read;
use std::path::PathBuf;
use std::sync::{mpsc, Mutex};
use std::time::Duration;

/// Default interactive shell: `$SHELL`, else `sh` (`powershell.exe` on
/// Windows). The server passes this to [`TerminalManager::start`] unless
/// the user configured another shell.
pub fn default_shell() -> String {
    if cfg!(windows) {
        return "powershell.exe".to_string();
    }
    let sh = std::env::var("SHELL").unwrap_or_default();
    if sh.trim().is_empty() {
        "sh".to_string()
    } else {
        sh
    }
}

struct Session {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    writer: Mutex<Box<dyn std::io::Write + Send>>,
    master: Mutex<Box<dyn portable_pty::MasterPty + Send>>,
    rx: mpsc::Receiver<Vec<u8>>,
}

/// One shell session. Empty when the drawer has never been opened or the
/// last session was stopped.
pub struct TerminalManager {
    root: PathBuf,
    session: Mutex<Option<Session>>,
}

impl TerminalManager {
    pub fn new(root: PathBuf) -> Self {
        TerminalManager {
            root,
            session: Mutex::new(None),
        }
    }

    /// True while a shell child is alive.
    pub fn running(&self) -> bool {
        let mut guard = self.session.lock().unwrap();
        if let Some(s) = guard.as_mut() {
            match s.child.try_wait() {
                Ok(None) => true,
                _ => {
                    *guard = None;
                    false
                }
            }
        } else {
            false
        }
    }

    /// Spawn `shell` with `args` under a fresh PTY of `rows`x`cols`.
    /// Refuses when a session is already running: single session only.
    pub fn start(&self, shell: &str, args: &[String], rows: u16, cols: u16) -> Result<(), String> {
        let mut guard = self.session.lock().unwrap();
        if guard.is_some() {
            return Err("terminal already running".to_string());
        }
        let pty_system = portable_pty::native_pty_system();
        let pair = pty_system
            .openpty(portable_pty::PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| format!("open pty: {e}"))?;
        let mut cmd = portable_pty::CommandBuilder::new(shell);
        cmd.cwd(&self.root);
        cmd.args(args.iter().map(|s| s.as_str()));
        cmd.env("TERM", "xterm-256color");
        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| format!("spawn {shell}: {e}"))?;
        drop(pair.slave);
        // No reaper thread: a shell that exits on its own is reaped by
        // the next `running`/`stop` probe (`try_wait` reaps), and `stop`
        // plus `Drop` always reap explicitly.
        let writer = pair.master.take_writer().map_err(|e| e.to_string())?;
        let mut reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut buf = vec![0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        *guard = Some(Session {
            child,
            writer: Mutex::new(writer),
            master: Mutex::new(pair.master),
            rx,
        });
        Ok(())
    }

    /// Feed keystrokes to the shell. Errors when no session runs.
    pub fn write(&self, data: &[u8]) -> Result<(), String> {
        use std::io::Write;
        let guard = self.session.lock().unwrap();
        match guard.as_ref() {
            None => Err("no terminal running".to_string()),
            Some(s) => s
                .writer
                .lock()
                .unwrap()
                .write_all(data)
                .map_err(|e| e.to_string()),
        }
    }

    /// Propagate the drawer size so fullscreen apps reflow.
    pub fn resize(&self, rows: u16, cols: u16) -> Result<(), String> {
        let guard = self.session.lock().unwrap();
        match guard.as_ref() {
            None => Err("no terminal running".to_string()),
            Some(s) => s
                .master
                .lock()
                .unwrap()
                .resize(portable_pty::PtySize {
                    rows,
                    cols,
                    pixel_width: 0,
                    pixel_height: 0,
                })
                .map_err(|e| e.to_string()),
        }
    }

    /// Drain output buffered so far, oldest first.
    pub fn drain(&self) -> Vec<Vec<u8>> {
        let guard = self.session.lock().unwrap();
        let Some(s) = guard.as_ref() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        while let Ok(chunk) = s.rx.try_recv() {
            out.push(chunk);
        }
        out
    }

    /// Wait up to `timeout` for the next output chunk.
    pub fn recv_timeout(&self, timeout: Duration) -> Option<Vec<u8>> {
        let guard = self.session.lock().unwrap();
        let s = guard.as_ref()?;
        s.rx.recv_timeout(timeout).ok()
    }

    /// Kill the shell and close the PTY. Idempotent: dropping the
    /// session closes the master side, and the reader thread (killed
    /// child: EOF) exits on its own.
    pub fn stop(&self) {
        let mut guard = self.session.lock().unwrap();
        if let Some(mut s) = guard.take() {
            let _ = s.child.kill();
            let _ = s.child.wait();
        }
    }
}

impl Drop for TerminalManager {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Shell script runner for tests: `sh -c <script>` on Unix,
/// `powershell -Command <script>` on Windows.
#[cfg(test)]
fn test_shell() -> (String, Vec<String>) {
    if cfg!(windows) {
        (
            "powershell.exe".to_string(),
            vec![
                "-NoProfile".to_string(),
                "-NonInteractive".to_string(),
                "-Command".to_string(),
            ],
        )
    } else {
        ("sh".to_string(), vec!["-c".to_string()])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn collect(m: &TerminalManager, want: &str, timeout: Duration) -> String {
        let deadline = Instant::now() + timeout;
        let mut got = Vec::new();
        while Instant::now() < deadline {
            if let Some(chunk) = m.recv_timeout(Duration::from_millis(50)) {
                got.extend_from_slice(&chunk);
                if String::from_utf8_lossy(&got).contains(want) {
                    break;
                }
            }
        }
        String::from_utf8_lossy(&got).into_owned()
    }

    #[test]
    fn shell_round_trip() {
        let (sh, mut args) = test_shell();
        args.push("echo hello-term".to_string());
        let m = TerminalManager::new(std::env::temp_dir());
        m.start(&sh, &args, 24, 80).unwrap();
        assert!(m.running());
        let out = collect(&m, "hello-term", Duration::from_secs(10));
        assert!(out.contains("hello-term"), "got: {out:?}");
        m.stop();
        assert!(!m.running());
    }

    #[test]
    fn stdin_reaches_shell() {
        let (sh, args) = test_shell();
        let m = TerminalManager::new(std::env::temp_dir());
        m.start(&sh, &args, 24, 80).unwrap();
        m.write(b"echo via-stdin\nexit\n").unwrap();
        let out = collect(&m, "via-stdin", Duration::from_secs(10));
        assert!(out.contains("via-stdin"), "got: {out:?}");
        m.stop();
    }

    #[test]
    fn second_start_refused_and_resize_ok() {
        let (sh, args) = test_shell();
        let m = TerminalManager::new(std::env::temp_dir());
        m.start(&sh, &args, 24, 80).unwrap();
        assert!(m.start(&sh, &args, 24, 80).is_err());
        m.resize(30, 100).unwrap();
        m.stop();
        // After stop the manager accepts a fresh session.
        m.start(&sh, &args, 24, 80).unwrap();
        m.stop();
    }

    #[test]
    fn write_without_session_errors() {
        let m = TerminalManager::new(std::env::temp_dir());
        assert!(!m.running());
        assert!(m.write(b"x").is_err());
        assert!(m.resize(24, 80).is_err());
        assert!(m.drain().is_empty());
        m.stop(); // idempotent, must not panic
    }

    #[test]
    fn default_shell_is_nonempty() {
        assert!(!default_shell().trim().is_empty());
    }
}
