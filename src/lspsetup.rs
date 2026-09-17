//! Setting up a language server from the UI: report what is missing
//! for a file, run a known installer for it, and pick the result up
//! without restarting px0.
//!
//! Ports `lspsetup.go` (minus the HTTP layer, which lives in
//! `server.rs` next to the other handlers).

use serde::Serialize;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::lspservers::{current_os, look_path_in, lsp_bin_dirs, LspManager, LspState};

/// Install runs may take a real build; Go allows 15 minutes.
pub const LSP_INSTALL_TIMEOUT: Duration = Duration::from_secs(15 * 60);

#[derive(Clone, Debug, Serialize)]
pub struct LspSetupOption {
    pub cmd: String,
    pub auto: bool,
    pub tool: String,
    #[serde(rename = "hasTool")]
    pub has_tool: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct LspSetupServer {
    pub name: String,
    pub options: Vec<LspSetupOption>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job: Option<LspJob>,
}

#[derive(Clone, Debug, Serialize)]
pub struct LspSetup {
    pub enabled: bool,
    pub lang: String,
    pub state: String,
    pub server: String,
    /// Why a server failed to start.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub reason: String,
    pub servers: Vec<LspSetupServer>,
}

#[derive(Clone, Debug, Serialize)]
pub struct LspJob {
    pub server: String,
    pub cmd: String,
    pub running: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub error: String,
    pub log: String,
}

/// Keeps the last `max` bytes written: enough installer output to
/// explain a failure without holding a whole build log. Ports Go
/// `tailBuffer`.
pub struct TailBuffer {
    inner: Mutex<TailInner>,
}

struct TailInner {
    buf: Vec<u8>,
    max: usize,
}

impl TailBuffer {
    pub fn new(max: usize) -> Self {
        Self {
            inner: Mutex::new(TailInner {
                buf: Vec::new(),
                max,
            }),
        }
    }

    pub fn push(&self, data: &[u8]) {
        let mut inner = self.inner.lock().unwrap();
        inner.buf.extend_from_slice(data);
        let overflow = inner.buf.len().saturating_sub(inner.max);
        inner.buf.drain(..overflow);
    }

    pub fn contents(&self) -> String {
        String::from_utf8_lossy(&self.inner.lock().unwrap().buf).into_owned()
    }
}

impl std::io::Write for TailBuffer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.push(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Live install state: the serializable [`LspJob`] plus the output
/// tail snapshots read from. Stored on the manager; snapshots cross
/// the HTTP boundary.
pub struct JobSlot {
    pub server: String,
    pub cmd: String,
    pub running: bool,
    pub error: String,
    pub out: Arc<TailBuffer>,
}

impl LspManager {
    /// Snapshot of the latest install run for a server, if any.
    /// Ports Go `job`.
    pub fn job(&self, name: &str) -> Option<LspJob> {
        let jobs = self.jobs.lock().unwrap();
        let slot = jobs.get(name)?;
        Some(LspJob {
            server: slot.server.clone(),
            cmd: slot.cmd.clone(),
            running: slot.running,
            error: slot.error.clone(),
            log: slot.out.contents(),
        })
    }

    /// What the UI can offer for rel: the server's state, and each
    /// known server with the install options for this system. Ports Go
    /// `Setup`.
    pub fn setup(&self, rel: &str) -> LspSetup {
        let (state, server) = self.state(rel);
        let mut s = LspSetup {
            enabled: self.is_enabled(),
            lang: String::new(),
            state: state.as_str().to_string(),
            server: server.clone(),
            reason: String::new(),
            servers: Vec::new(),
        };
        if state == LspState::Failed {
            // State reports a failure's reason in place of the name.
            s.reason = server;
            if let Some(def) = self.def_for(rel) {
                s.server = def.name;
            }
        }
        let dirs = lsp_bin_dirs();
        for def in crate::lspservers::registry_for(rel, self.registry()) {
            if s.lang.is_empty() {
                s.lang = def.lang.clone();
            }
            let mut entry = LspSetupServer {
                name: def.name.clone(),
                options: Vec::new(),
                job: self.job(&def.name),
            };
            for opt in def.installs_for(current_os()) {
                let has = look_path_in(&opt.cmd[0], &dirs).is_some();
                entry.options.push(LspSetupOption {
                    cmd: opt.cmd.join(" "),
                    auto: opt.auto,
                    tool: opt.cmd[0].clone(),
                    has_tool: has,
                });
            }
            s.servers.push(entry);
        }
        s
    }

    /// Names the language of rel when px0 knows servers for it but none
    /// is installed, empty otherwise. Ports Go `MissingLang`.
    pub fn missing_lang(&self, rel: &str) -> String {
        if !self.is_enabled() || !self.is_discovered() || self.def_for(rel).is_some() {
            return String::new();
        }
        crate::lspservers::registry_for(rel, self.registry())
            .first()
            .map(|d| d.lang.clone())
            .unwrap_or_default()
    }

    /// Look for servers again and forget earlier start failures, so a
    /// server installed while px0 runs is used on the next request.
    /// Ports Go `Rescan`.
    pub fn rescan(self: &Arc<Self>) {
        if !self.is_enabled() {
            return;
        }
        self.discover();
        self.clear_failures();
    }

    /// Start the option-th installer for a server in the background and
    /// return at once. Only registry recipes ever run, never a command
    /// from the request. Ports Go `Install`.
    pub fn install(self: &Arc<Self>, name: &str, option: i64) -> Result<LspJob, String> {
        if !self.is_enabled() {
            return Err("language servers are turned off (-no-lsp)".to_string());
        }
        let def = self
            .registry()
            .iter()
            .find(|d| d.name == name)
            .cloned()
            .ok_or_else(|| format!("unknown language server {name:?}"))?;
        let opts = def.installs_for(current_os());
        let opt = usize::try_from(option)
            .ok()
            .and_then(|o| opts.get(o))
            .ok_or_else(|| format!("{name} has no install option {option} on {}", current_os()))?;
        if !opt.auto {
            return Err(format!(
                "px0 does not run {:?}; run it in a terminal",
                opt.cmd.join(" ")
            ));
        }
        let tool = look_path_in(&opt.cmd[0], &lsp_bin_dirs())
            .ok_or_else(|| format!("{} is not installed", opt.cmd[0]))?;

        {
            let jobs = self.jobs.lock().unwrap();
            if let Some(slot) = jobs.get(name) {
                if slot.running {
                    drop(jobs);
                    return Ok(self.job(name).unwrap_or(LspJob {
                        server: name.to_string(),
                        cmd: opt.cmd.join(" "),
                        running: true,
                        error: String::new(),
                        log: String::new(),
                    }));
                }
            }
        }
        let out = Arc::new(TailBuffer::new(16 << 10));
        {
            let mut jobs = self.jobs.lock().unwrap();
            jobs.insert(
                name.to_string(),
                JobSlot {
                    server: name.to_string(),
                    cmd: opt.cmd.join(" "),
                    running: true,
                    error: String::new(),
                    out: out.clone(),
                },
            );
        }
        let this = Arc::clone(self);
        let args = opt.cmd[1..].to_vec();
        let name = name.to_string();
        let thread_name = name.clone();
        std::thread::spawn(move || this.run_install(&def, &thread_name, &tool, &args, out));
        self.job(&name)
            .ok_or_else(|| "install did not start".to_string())
    }

    fn run_install(
        self: &Arc<Self>,
        def: &crate::lspservers::LspServerDef,
        name: &str,
        tool: &str,
        args: &[String],
        out: Arc<TailBuffer>,
    ) {
        // Outside the workspace, so its go.mod or package.json cannot
        // change what is installed. Stdin stays empty: a prompt fails
        // instead of hanging.
        let home = std::env::var("HOME").ok().filter(|h| !h.is_empty());
        let mut cmd = std::process::Command::new(tool);
        cmd.args(args);
        if let Some(home) = home {
            cmd.current_dir(home);
        }
        cmd.stdin(std::process::Stdio::null());
        let result = run_with_output(cmd, out.clone(), LSP_INSTALL_TIMEOUT);
        let mut err: Option<String> = None;
        match result {
            RunOutcome::Timeout => {
                err = Some(format!("gave up after {}s", LSP_INSTALL_TIMEOUT.as_secs()));
            }
            RunOutcome::Failed(e) => {
                err = Some(e);
            }
            RunOutcome::Ok => {
                self.rescan();
                if look_path_in(&def.cmd[0], &lsp_bin_dirs()).is_none() {
                    err = Some(format!(
                        "installed, but {} is not on PATH or in the usual install folders",
                        def.cmd[0]
                    ));
                }
            }
        }
        let mut jobs = self.jobs.lock().unwrap();
        if let Some(slot) = jobs.get_mut(name) {
            slot.running = false;
            if let Some(e) = err {
                slot.error = e;
            }
        }
    }
}

enum RunOutcome {
    Ok,
    Failed(String),
    Timeout,
}

/// Run `cmd` to completion with piped output into `out`, killing it
/// after `timeout`. The wait loop polls so the kill is prompt.
fn run_with_output(
    mut cmd: std::process::Command,
    out: Arc<TailBuffer>,
    timeout: Duration,
) -> RunOutcome {
    use std::io::Read;
    let mut child = match cmd
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return RunOutcome::Failed(e.to_string()),
    };
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let out_clone = out.clone();
    let drain = std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        if let Some(mut pipe) = stdout.take() {
            while let Ok(n) = pipe.read(&mut buf) {
                if n == 0 {
                    break;
                }
                out_clone.push(&buf[..n]);
            }
        }
        if let Some(mut pipe) = stderr.take() {
            while let Ok(n) = pipe.read(&mut buf) {
                if n == 0 {
                    break;
                }
                out.push(&buf[..n]);
            }
        }
    });
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let _ = drain.join();
                if status.success() {
                    return RunOutcome::Ok;
                }
                return RunOutcome::Failed(format!("installer exited with {status}"));
            }
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = drain.join();
                    return RunOutcome::Timeout;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                let _ = drain.join();
                return RunOutcome::Failed(e.to_string());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ports Go `TestTailBufferKeepsTheEnd`.
    #[test]
    fn tail_buffer_keeps_the_end() {
        let buf = TailBuffer::new(8);
        buf.push(b"0123456789");
        buf.push(b"ab");
        assert_eq!(buf.contents(), "456789ab");
    }
}
