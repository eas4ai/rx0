//! Editing through a coding harness. rx0 never authors a change itself:
//! it composes an instruction anchored to a line range, hands it to a
//! harness already installed on this machine, and reloads whatever moved
//! once that harness exits. The harness edits; rx0 stays the reader that
//! knows exactly when to look again.
//!
//! Ports `agent.go`. Harnesses are discovered the same way language
//! servers are, and the one to use is chosen in the UI. Discovery alone
//! never enables editing: running a general-purpose agent over a
//! workspace is a decision the user makes once, remembered in the
//! settings file rather than a flag.

use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const AGENT_TIMEOUT: Duration = Duration::from_secs(10 * 60);
pub const AGENT_LOG_BYTES: usize = 32 << 10;

// ---------------------------------------------------------------- presets

/// A harness rx0 knows and the argv that runs it headless. Each of
/// these starts an interactive session by default and would sit forever
/// waiting for approval, so every preset carries the flag that turns
/// that off and the one that lets it apply edits without asking.
#[derive(Clone, Debug)]
pub struct AgentPreset {
    pub name: String,
    pub args: Vec<String>,
    pub model_flag: String,
    pub default_model: String,
    pub models: Vec<String>,
}

fn preset(
    name: &str,
    args: &[&str],
    model_flag: &str,
    default_model: &str,
    models: &[&str],
) -> AgentPreset {
    AgentPreset {
        name: name.to_string(),
        args: args.iter().map(|s| s.to_string()).collect(),
        model_flag: model_flag.to_string(),
        default_model: default_model.to_string(),
        models: models.iter().map(|s| s.to_string()).collect(),
    }
}

/// Verbatim port of Go `agentPresets`.
pub fn agent_presets() -> Vec<AgentPreset> {
    vec![
        preset(
            "claude",
            &[
                "claude",
                "--permission-mode",
                "acceptEdits",
                "-p",
                "{prompt}",
            ],
            "--model",
            "haiku",
            &["haiku", "sonnet", "opus"],
        ),
        preset(
            "gemini",
            &["gemini", "--approval-mode", "auto_edit", "-p", "{prompt}"],
            "-m",
            "gemini-2.5-flash-lite",
            &[
                "gemini-2.5-flash-lite",
                "gemini-2.5-flash",
                "gemini-2.5-pro",
            ],
        ),
        preset(
            "cursor-agent",
            &["cursor-agent", "--force", "-p", "{prompt}"],
            "--model",
            "gemini-3.6-flash-minimal",
            &[
                "gemini-3.6-flash-minimal",
                "gemini-3.6-flash-low",
                "gemini-3.7-flash-low",
                "gemini-3.8-flash-low",
                "gpt-5.4-nano-none",
                "gpt-5.4-mini-none",
                "claude-sonnet-5-low",
                "claude-opus-4-8-thinking-low",
            ],
        ),
        preset(
            "agy",
            &[
                "agy",
                "--dangerously-skip-permissions",
                "--mode",
                "accept-edits",
                "-p",
                "{prompt}",
            ],
            "--model",
            "gemini-3.6-flash-low",
            &[
                "gemini-3.6-flash-low",
                "gemini-3.6-flash-medium",
                "gemini-3.6-flash-high",
                "gemini-3.7-flash-low",
                "gemini-3.7-flash-medium",
                "gemini-3.7-flash-high",
                "gemini-3.8-flash-low",
                "gemini-3.8-flash-medium",
                "gemini-3.8-flash-high",
                "gemini-3.1-pro-low",
                "gemini-3.1-pro-high",
            ],
        ),
        preset(
            "opencode",
            &["opencode", "run", "{prompt}"],
            "-m",
            "opencode/big-pickle",
            &[
                "opencode/big-pickle",
                "opencode/gpt-5-nano",
                "opencode/minimax-m2.5-free",
                "opencode/trinity-large-preview-free",
                "github-copilot/claude-haiku-4.5",
                "github-copilot/claude-sonnet-4.5",
                "github-copilot/claude-opus-4.5",
                "google/gemini-2.5-flash",
                "google/gemini-2.5-pro",
            ],
        ),
        preset(
            "codex",
            &["codex", "exec", "--ask-for-approval", "never", "{prompt}"],
            "-m",
            "gpt-5-codex",
            &[
                "gpt-5-codex",
                "gpt-5-mini",
                "gpt-5.1-codex",
                "gpt-5.1-codex-max",
                "gpt-5.1-codex-mini",
                "gpt-5.2-codex",
                "gpt-4.1",
                "o3-mini",
                "o1",
            ],
        ),
        preset(
            "aider",
            &[
                "aider",
                "--yes-always",
                "--no-auto-commits",
                "--message",
                "{prompt}",
            ],
            "--model",
            "claude-3-7-sonnet",
            &[
                "claude-3-7-sonnet",
                "claude-3-5-haiku",
                "claude-3-opus",
                "gpt-4o",
                "gpt-4o-mini",
                "o3-mini",
                "gemini/gemini-2.5-flash",
                "deepseek/deepseek-chat",
                "ollama/qwen2.5-coder",
            ],
        ),
        preset(
            "goose",
            &["goose", "run", "--no-session", "-t", "{prompt}"],
            "--model",
            "gpt-4o",
            &[
                "gpt-4o",
                "gpt-4o-mini",
                "claude-3-5-sonnet",
                "claude-3-5-haiku",
                "gemini-2.5-flash",
            ],
        ),
    ]
}

pub fn agent_preset_names() -> Vec<String> {
    agent_presets().iter().map(|p| p.name.clone()).collect()
}

// ---------------------------------------------------------------- errors

/// The refusals the UI reacts to rather than merely reporting. Ports
/// Go's `errAgentBusy`/`errAgentDirty`/`errAgentNone`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentError {
    /// An overlapping edit is already running.
    Busy,
    /// Reserved for the uncommitted-work guard the UI maps to 409.
    Dirty,
    /// No coding harness is selected.
    None,
    Other(String),
}

impl std::fmt::Display for AgentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Busy => write!(f, "an edit is already running"),
            Self::Dirty => write!(f, "uncommitted"),
            Self::None => write!(f, "no coding harness is selected"),
            Self::Other(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for AgentError {}

// ---------------------------------------------------------------- models

struct ModelCache {
    cached: HashMap<String, Vec<String>>,
    discovering: HashSet<String>,
}

static MODELS: std::sync::LazyLock<Mutex<ModelCache>> = std::sync::LazyLock::new(|| {
    Mutex::new(ModelCache {
        cached: HashMap::new(),
        discovering: HashSet::new(),
    })
});

/// Known models for a harness, refreshing in the background when a
/// binary is present. Ports Go `discoverHarnessModels`.
pub fn discover_harness_models(
    name: &str,
    bin: Option<&str>,
    static_models: Vec<String>,
) -> Vec<String> {
    let mut cache = MODELS.lock().unwrap();
    if let Some(cached) = cache.cached.get(name) {
        return cached.clone();
    }
    if !cache.discovering.contains(name) {
        if let Some(bin) = bin {
            cache.discovering.insert(name.to_string());
            let name = name.to_string();
            let bin = bin.to_string();
            let fallback = static_models.clone();
            std::thread::spawn(move || run_model_discovery(&name, &bin, static_models));
            // Until the refresh lands, the static list.
            return fallback;
        }
    }
    static_models
}

fn run_with_timeout(
    bin: &str,
    args: &[&str],
    stdin_data: Option<&[u8]>,
    timeout: Duration,
) -> Option<Vec<u8>> {
    use std::io::Write;
    let mut cmd = std::process::Command::new(bin);
    cmd.args(args)
        .stdin(if stdin_data.is_some() {
            std::process::Stdio::piped()
        } else {
            std::process::Stdio::null()
        })
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    let mut child = cmd.spawn().ok()?;
    if let (Some(data), Some(mut stdin)) = (stdin_data, child.stdin.take()) {
        let _ = stdin.write_all(data);
        drop(stdin);
    }
    let stdout = child.stdout.take();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut out = Vec::new();
        if let Some(mut pipe) = stdout {
            use std::io::Read;
            let _ = pipe.read_to_end(&mut out);
        }
        let _ = tx.send(out);
    });
    match rx.recv_timeout(timeout) {
        Ok(out) => {
            let status = child.wait().ok();
            if status.map(|s| s.success()).unwrap_or(false) {
                Some(out)
            } else {
                None
            }
        }
        // Go's CommandContext kills the child on timeout; without this the
        // orphaned process (and its stdout pipe) would leak.
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            None
        }
    }
}

fn run_model_discovery(name: &str, bin: &str, static_models: Vec<String>) {
    let mut models = static_models;
    match name {
        "agy" => {
            if let Some(out) = run_with_timeout(bin, &["models"], None, Duration::from_secs(5)) {
                let mut list = Vec::new();
                for line in String::from_utf8_lossy(&out).lines() {
                    let line = line.trim();
                    if line.starts_with("Fetching") || line.is_empty() {
                        continue;
                    }
                    if let Some(first) = line.split_whitespace().next() {
                        if !first.contains(' ') {
                            list.push(first.to_string());
                        }
                    }
                }
                if !list.is_empty() {
                    let def = "gemini-3.6-flash-low";
                    let mut reordered = vec![def.to_string()];
                    reordered.extend(list.into_iter().filter(|m| m != def));
                    models = reordered;
                }
            }
        }
        "cursor-agent" => {
            if let Some(out) =
                run_with_timeout(bin, &["--list-models"], None, Duration::from_secs(5))
            {
                let mut list = Vec::new();
                for line in String::from_utf8_lossy(&out).lines() {
                    let line = line.trim();
                    if line.is_empty() || line.starts_with("Tip:") {
                        continue;
                    }
                    let id = line.split(" - ").next().unwrap_or("").trim();
                    if !id.is_empty() && !id.contains(' ') {
                        list.push(id.to_string());
                    }
                }
                if !list.is_empty() {
                    let def = "gemini-3.6-flash-minimal";
                    let mut reordered = vec![def.to_string()];
                    reordered.extend(list.into_iter().filter(|m| m != def));
                    models = reordered;
                }
            }
        }
        "claude" => {
            if let Some(out) =
                run_with_timeout(bin, &["-p", "/model"], Some(b""), Duration::from_secs(5))
            {
                let text = String::from_utf8_lossy(&out).into_owned();
                let mut list = Vec::new();
                if let Some(idx) = text.find("Available:") {
                    let mut avail = &text[idx + "Available:".len()..];
                    if let Some(dot) = avail.find('.') {
                        avail = &avail[..dot];
                    }
                    for part in avail.split(',') {
                        let m = part.trim().strip_prefix("or ").unwrap_or(part.trim());
                        if !m.is_empty() && !m.contains(' ') {
                            list.push(m.to_string());
                        }
                    }
                }
                if !list.is_empty() {
                    let def = "haiku";
                    if list.contains(&def.to_string()) {
                        let mut reordered = vec![def.to_string()];
                        reordered.extend(list.into_iter().filter(|m| m != def));
                        models = reordered;
                    } else {
                        models = list;
                    }
                }
            }
        }
        "opencode" => {
            if let Some(out) = run_with_timeout(bin, &["models"], None, Duration::from_secs(5)) {
                let list: Vec<String> = String::from_utf8_lossy(&out)
                    .lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty() && !l.contains(' '))
                    .map(str::to_string)
                    .collect();
                if !list.is_empty() {
                    let def = "opencode/big-pickle";
                    let mut reordered = vec![def.to_string()];
                    reordered.extend(list.into_iter().filter(|m| m != def));
                    models = reordered;
                }
            }
        }
        _ => {}
    }
    let mut cache = MODELS.lock().unwrap();
    cache.cached.insert(name.to_string(), models);
    cache.discovering.remove(name);
}

// ---------------------------------------------------------------- types

/// One row of the picker. Ports Go `agentHarness`.
#[derive(Clone, Debug, Serialize)]
pub struct AgentHarness {
    pub name: String,
    pub cmd: String,
    pub installed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<String>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub model: String,
}

/// One dispatch, snapshot-able while it runs. Ports Go `agentJob`
/// (the unexported anchor/cancel/tail fields stay inside).
#[derive(Clone, Debug, Serialize)]
pub struct AgentJob {
    pub id: i64,
    pub harness: String,
    pub path: String,
    pub lines: String,
    pub running: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub error: String,
    pub log: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub stdout: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub stderr: String,
    pub changed: Vec<String>,
    pub ms: i64,
    /// False outside a git repository, where rx0 cannot tell which
    /// files a harness touched. An empty Changed then means "unknown",
    /// not "nothing", and the client reloads regardless.
    pub tracked: bool,
}

struct JobInner {
    id: i64,
    harness: String,
    path: String,
    lines: String,
    l1: i64,
    l2: i64,
    running: std::sync::atomic::AtomicBool,
    error: Mutex<String>,
    changed: Mutex<Vec<String>>,
    ms: std::sync::atomic::AtomicI64,
    tracked: bool,
    start: Instant,
    out: Arc<crate::lspsetup::TailBuffer>,
    err_out: Arc<crate::lspsetup::TailBuffer>,
    child: Mutex<Option<std::process::Child>>,
    cancelled: std::sync::atomic::AtomicBool,
}

/// Forwards harness output line by line: always into the tail log, and
/// to the terminal unless quiet. Ports Go `lineStreamer` (without the
/// terminal styling the Rust binary does not do elsewhere either).
struct LineStreamer {
    tail: Arc<crate::lspsetup::TailBuffer>,
    prefix: &'static str,
    echo: bool,
    line: Vec<u8>,
}

impl LineStreamer {
    fn push(&mut self, data: &[u8]) {
        self.tail.push(data);
        if !self.echo {
            return;
        }
        for &b in data {
            if b == b'\n' {
                if !self.line.is_empty() {
                    println!("  {} {}", self.prefix, String::from_utf8_lossy(&self.line));
                    self.line.clear();
                }
            } else if b != b'\r' {
                self.line.push(b);
            }
        }
    }

    fn flush(&mut self) {
        if self.echo && !self.line.is_empty() {
            println!("  {} {}", self.prefix, String::from_utf8_lossy(&self.line));
            self.line.clear();
        }
    }
}

// ---------------------------------------------------------------- manager

struct AgentInner {
    selected: String,
    args: Vec<String>,
    pinned: bool,
    models: HashMap<String, String>,
    jobs: HashMap<i64, Arc<JobInner>>,
    seq: i64,
}

/// Discovery, the current choice, and every edit in flight. Several
/// harnesses can run at once as long as they touch disjoint line
/// ranges. Ports Go `agentManager`.
pub struct AgentManager {
    root: PathBuf,
    lsp: Option<Arc<crate::lspservers::LspManager>>,
    quiet: bool,
    inner: Mutex<AgentInner>,
}

impl AgentManager {
    /// Wire discovery and restore the remembered choice. A flag value
    /// pins a harness (or an arbitrary command template) for this run
    /// and is the only case that can fail: a bad `-agent` stops
    /// startup, while a stale settings file just leaves nothing
    /// selected. Ports Go `newAgentManager`.
    pub fn new(
        root: PathBuf,
        flag_spec: &str,
        lsp: Option<Arc<crate::lspservers::LspManager>>,
        quiet: bool,
    ) -> Result<Arc<Self>, String> {
        let this = Arc::new(Self {
            root,
            lsp,
            quiet,
            inner: Mutex::new(AgentInner {
                selected: String::new(),
                args: Vec::new(),
                pinned: false,
                models: HashMap::new(),
                jobs: HashMap::new(),
                seq: 0,
            }),
        });
        let settings = crate::settings::read_settings();
        let flag_spec = flag_spec.trim().to_string();
        // Resolve everything before touching the manager, so a bad
        // `-agent` fails without borrowing `this` across the return.
        if !flag_spec.is_empty() {
            let saved = settings.models.get(&flag_spec).cloned().unwrap_or_default();
            let (name, args, chosen) = resolve_agent_spec(&flag_spec, &saved)?;
            let mut inner = this.inner.lock().unwrap();
            inner.models = settings.models.clone();
            inner.selected = name.clone();
            inner.args = args;
            inner.pinned = true;
            if !chosen.is_empty() {
                inner.models.insert(name, chosen);
            }
            return Ok(Arc::clone(&this));
        }
        if !settings.agent.is_empty() {
            let saved = settings
                .models
                .get(&settings.agent)
                .cloned()
                .unwrap_or_default();
            if let Ok((name, args, chosen)) = resolve_agent_spec(&settings.agent, &saved) {
                let mut inner = this.inner.lock().unwrap();
                inner.models = settings.models.clone();
                inner.selected = name.clone();
                inner.args = args;
                if !chosen.is_empty() {
                    inner.models.insert(name, chosen);
                }
                return Ok(Arc::clone(&this));
            }
        }
        this.inner.lock().unwrap().models = settings.models.clone();
        Ok(this)
    }

    pub fn name(&self) -> String {
        self.inner.lock().unwrap().selected.clone()
    }

    pub fn model(&self) -> String {
        let inner = self.inner.lock().unwrap();
        if inner.selected.is_empty() {
            return String::new();
        }
        inner
            .models
            .get(&inner.selected)
            .cloned()
            .unwrap_or_default()
    }

    pub fn pinned(&self) -> bool {
        self.inner.lock().unwrap().pinned
    }

    /// Remember a harness for this workspace and every later run.
    /// An empty name turns editing back off. Ports Go `Select`.
    pub fn select(&self, name: &str, model_opt: Option<&str>) -> Result<(), AgentError> {
        {
            let inner = self.inner.lock().unwrap();
            if inner.pinned {
                return Err(AgentError::Other(
                    "rx0 was started with -agent, so the harness is fixed for this run".to_string(),
                ));
            }
            if inner
                .jobs
                .values()
                .any(|j| j.running.load(std::sync::atomic::Ordering::SeqCst))
            {
                return Err(AgentError::Busy);
            }
        }
        let name = name.trim().to_string();
        if name.is_empty() {
            self.inner.lock().unwrap().selected.clear();
            self.inner.lock().unwrap().args.clear();
            return crate::settings::write_settings(&crate::settings::Settings::default())
                .map_err(AgentError::Other);
        }
        let req_model = match model_opt {
            Some(m) if !m.trim().is_empty() => m.trim().to_string(),
            _ => self
                .inner
                .lock()
                .unwrap()
                .models
                .get(&name)
                .cloned()
                .unwrap_or_default(),
        };
        let (display, args, chosen) =
            resolve_agent_spec(&name, &req_model).map_err(AgentError::Other)?;
        let saved = {
            let mut inner = self.inner.lock().unwrap();
            inner.selected = display.clone();
            inner.args = args;
            if !chosen.is_empty() {
                inner.models.insert(display.clone(), chosen);
            }
            inner.models.clone()
        };
        // Persist the spec as given, not the display name: a command
        // template shortens to its binary for display and would not
        // survive the round trip.
        crate::settings::write_settings(&crate::settings::Settings {
            agent: name,
            models: saved,
            ..Default::default()
        })
        .map_err(AgentError::Other)
    }

    /// Every harness rx0 knows and whether it is installed right now,
    /// so a tool installed since startup shows up without a restart.
    /// Ports Go `Detect`.
    pub fn detect(&self) -> Vec<AgentHarness> {
        let saved = self.inner.lock().unwrap().models.clone();
        let mut out = Vec::with_capacity(agent_presets().len());
        for p in agent_presets() {
            let bin =
                crate::lspservers::look_path_in(&p.args[0], &crate::lspservers::lsp_bin_dirs());
            let models = discover_harness_models(&p.name, bin.as_deref(), p.models.clone());
            let mut cur = saved.get(&p.name).cloned().unwrap_or_default();
            if cur.is_empty() {
                cur = p.default_model.clone();
            }
            // Go re-resolves and joins; the template keeps {prompt}
            // either way, so show the resolved argv when it resolves.
            let mut cmd = p.args.join(" ");
            if let Ok((_, args, _)) = resolve_agent_spec(&p.name, &cur) {
                cmd = args.join(" ");
            }
            out.push(AgentHarness {
                name: p.name.clone(),
                cmd,
                installed: bin.is_some(),
                path: bin,
                models,
                model: cur,
            });
        }
        out
    }

    /// Snapshot of a job, or of the most recently started one for
    /// id 0, or None when there isn't one. Ports Go `Job`.
    pub fn job(&self, id: i64) -> Option<AgentJob> {
        let inner = self.inner.lock().unwrap();
        let found = if id == 0 {
            inner.jobs.values().max_by_key(|j| j.id).cloned()
        } else {
            inner.jobs.get(&id).cloned()
        };
        let j = found?;
        use std::sync::atomic::Ordering;
        let running = j.running.load(Ordering::SeqCst);
        let log = j.out.contents();
        // Bind the guarded fields first: temporaries in the tail
        // expression would otherwise outlive `j`.
        let error = j.error.lock().unwrap().clone();
        let changed = j.changed.lock().unwrap().clone();
        let ms = if running {
            j.start.elapsed().as_millis() as i64
        } else {
            j.ms.load(Ordering::SeqCst)
        };
        Some(AgentJob {
            id: j.id,
            harness: j.harness.clone(),
            path: j.path.clone(),
            lines: j.lines.clone(),
            running,
            error,
            log: log.clone(),
            stdout: log,
            stderr: j.err_out.contents(),
            changed,
            ms,
            tracked: j.tracked,
        })
    }

    fn overlap_locked(jobs: &HashMap<i64, Arc<JobInner>>, rel: &str, l1: i64, l2: i64) -> bool {
        jobs.values().any(|j| {
            j.running.load(std::sync::atomic::Ordering::SeqCst)
                && j.path == rel
                && l1 <= j.l2
                && j.l1 <= l2
        })
    }

    /// Dispatch an instruction anchored to abs:l1-l2, returning as soon
    /// as the harness is running. Ports Go `Start`.
    pub fn start(
        self: &Arc<Self>,
        abs: &Path,
        rel: &str,
        l1: i64,
        l2: i64,
        instruction: &str,
        _force: bool,
    ) -> Result<AgentJob, AgentError> {
        let instruction = instruction.trim().to_string();
        if instruction.is_empty() {
            return Err(AgentError::Other("instruction is empty".to_string()));
        }
        let (args, harness) = {
            let inner = self.inner.lock().unwrap();
            if inner.args.is_empty() {
                return Err(AgentError::None);
            }
            if Self::overlap_locked(&inner.jobs, rel, l1, l2) {
                return Err(AgentError::Busy);
            }
            (inner.args.clone(), inner.selected.clone())
        };
        let snippet = read_line_range(abs, l1, l2).map_err(AgentError::Other)?;
        let job = {
            let mut inner = self.inner.lock().unwrap();
            // Re-check under lock: another dispatch may have raced
            // while this one was reading the file.
            if Self::overlap_locked(&inner.jobs, rel, l1, l2) {
                return Err(AgentError::Busy);
            }
            inner.seq += 1;
            let job = Arc::new(JobInner {
                id: inner.seq,
                harness: harness.clone(),
                path: rel.to_string(),
                lines: line_ref(l1, l2),
                l1,
                l2,
                running: std::sync::atomic::AtomicBool::new(true),
                error: Mutex::new(String::new()),
                changed: Mutex::new(Vec::new()),
                ms: std::sync::atomic::AtomicI64::new(0),
                tracked: crate::git::git_available(&self.root),
                start: Instant::now(),
                out: Arc::new(crate::lspsetup::TailBuffer::new(AGENT_LOG_BYTES)),
                err_out: Arc::new(crate::lspsetup::TailBuffer::new(AGENT_LOG_BYTES)),
                child: Mutex::new(None),
                cancelled: std::sync::atomic::AtomicBool::new(false),
            });
            inner.jobs.insert(job.id, job.clone());
            job
        };
        let this = Arc::clone(self);
        let prompt = agent_prompt(rel, l1, l2, &snippet, &instruction);
        let id = job.id;
        std::thread::spawn(move || this.run(job, args, prompt));
        // Snapshot what Start returns: Go returns m.Job(id), whose Ms
        // is ~0 while running and whose Log is empty so far.
        self.job(id)
            .ok_or_else(|| AgentError::Other("job vanished".to_string()))
    }

    fn run(self: Arc<Self>, job: Arc<JobInner>, template: Vec<String>, prompt: String) {
        let args: Vec<String> = template
            .iter()
            .map(|tok| tok.replace("{prompt}", &prompt))
            .collect();
        let mut cmd = std::process::Command::new(&args[0]);
        cmd.args(&args[1..])
            .current_dir(&self.root)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let before = worktree_snapshot(&self.root);
        let spawn_err = match cmd.spawn() {
            Ok(mut child) => {
                let stdout = child.stdout.take();
                let stderr = child.stderr.take();
                *job.child.lock().unwrap() = Some(child);
                let out = job.out.clone();
                let echo = !self.quiet;
                let drain_out = stdout.map(|pipe| {
                    std::thread::spawn(move || {
                        drain_pipe(pipe, out, "│", echo);
                    })
                });
                let err_out = job.err_out.clone();
                let drain_err = stderr.map(|pipe| {
                    std::thread::spawn(move || {
                        drain_pipe(pipe, err_out, "│", echo);
                    })
                });
                // Wait for the exit, a user cancel, or the timeout —
                // whichever comes first. stdin stays empty: a harness
                // that still wants to ask something fails fast instead
                // of hanging until the timeout with nothing on screen.
                let deadline = Instant::now() + AGENT_TIMEOUT;
                let verdict = loop {
                    if job.cancelled.load(std::sync::atomic::Ordering::SeqCst) {
                        kill_child(&job);
                        break RunVerdict::Cancelled;
                    }
                    let done = job
                        .child
                        .lock()
                        .unwrap()
                        .as_mut()
                        .and_then(|c| c.try_wait().ok().flatten());
                    if let Some(status) = done {
                        if status.success() {
                            break RunVerdict::Ok;
                        }
                        break RunVerdict::Failed(format!(
                            "exit status {}",
                            status
                                .code()
                                .map(|c| c.to_string())
                                .unwrap_or_else(|| "?".to_string())
                        ));
                    }
                    if Instant::now() >= deadline {
                        kill_child(&job);
                        break RunVerdict::Failed(format!(
                            "gave up after {}",
                            fmt_duration(AGENT_TIMEOUT)
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(50));
                };
                if let Some(t) = drain_out {
                    let _ = t.join();
                }
                if let Some(t) = drain_err {
                    let _ = t.join();
                }
                // Reap whatever kill left behind.
                if let Some(mut child) = job.child.lock().unwrap().take() {
                    let _ = child.wait();
                }
                verdict
            }
            Err(e) => RunVerdict::Failed(e.to_string()),
        };
        let changed = changed_since(&self.root, &before);
        self.settle(&changed);
        {
            use std::sync::atomic::Ordering;
            if let RunVerdict::Failed(e) = &spawn_err {
                *job.error.lock().unwrap() = e.clone();
            }
            *job.changed.lock().unwrap() = changed;
            job.ms
                .store(job.start.elapsed().as_millis() as i64, Ordering::SeqCst);
            job.running.store(false, Ordering::SeqCst);
        }
    }

    /// Drops every trace of the old bytes and tells language servers
    /// to reopen the files on next use. Ports Go `settle`.
    fn settle(&self, changed: &[String]) {
        for rel in changed {
            let abs = self.root.join(from_slash(rel));
            crate::highlight::evict(&abs.to_string_lossy());
            if let Some(lsp) = &self.lsp {
                lsp.close_doc(&abs, rel);
            }
        }
    }

    /// Stop every harness currently running. Ports Go `Cancel`.
    pub fn cancel(&self) -> bool {
        self.cancel_job(0)
    }

    /// Stop one run by id, or every running harness for id 0. Ports Go
    /// `CancelJob`.
    pub fn cancel_job(&self, id: i64) -> bool {
        let inner = self.inner.lock().unwrap();
        if id != 0 {
            return match inner.jobs.get(&id) {
                Some(j) if j.running.load(std::sync::atomic::Ordering::SeqCst) => {
                    j.cancelled.store(true, std::sync::atomic::Ordering::SeqCst);
                    kill_child(j);
                    true
                }
                _ => false,
            };
        }
        let mut cancelled = false;
        for j in inner.jobs.values() {
            if !j.running.load(std::sync::atomic::Ordering::SeqCst) {
                continue;
            }
            j.cancelled.store(true, std::sync::atomic::Ordering::SeqCst);
            kill_child(j);
            cancelled = true;
        }
        cancelled
    }

    pub fn close(&self) {
        self.cancel();
    }
}

enum RunVerdict {
    Ok,
    Failed(String),
    Cancelled,
}

fn kill_child(job: &JobInner) {
    if let Some(child) = job.child.lock().unwrap().as_mut() {
        let _ = child.kill();
    }
}

fn drain_pipe(
    pipe: impl std::io::Read + Send + 'static,
    tail: Arc<crate::lspsetup::TailBuffer>,
    prefix: &'static str,
    echo: bool,
) {
    let mut streamer = LineStreamer {
        tail,
        prefix,
        echo,
        line: Vec::new(),
    };
    let mut pipe = pipe;
    let mut buf = [0u8; 8192];
    loop {
        match pipe.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => streamer.push(&buf[..n]),
        }
    }
    streamer.flush();
}

/// `10m0s`-style durations for timeout messages. Ports Go's
/// `fmtDuration` usage in `run` (durations over a minute).
pub fn fmt_duration(d: Duration) -> String {
    if d < Duration::from_secs(1) {
        return format!("{}ms", d.as_millis());
    }
    if d < Duration::from_secs(60) {
        return format!("{:.1}s", d.as_secs_f64());
    }
    format!("{}m {}s", d.as_secs() / 60, d.as_secs() % 60)
}

/// Turn a preset name or a command template into argv, verifying the
/// binary exists now rather than at first use. Ports Go
/// `resolveAgentSpec`.
pub fn resolve_agent_spec(
    spec: &str,
    model: &str,
) -> Result<(String, Vec<String>, String), String> {
    let spec = spec.trim();
    if spec.is_empty() {
        return Err("empty harness".to_string());
    }
    let mut name = String::new();
    let mut args: Vec<String> = Vec::new();
    let mut chosen = String::new();
    for p in agent_presets() {
        if !spec.eq_ignore_ascii_case(&p.name) {
            continue;
        }
        name = p.name.clone();
        chosen = model.to_string();
        if chosen.is_empty() {
            chosen = p.default_model.clone();
        }
        let prompt_idx = p.args.iter().position(|a| a == "{prompt}");
        let mut insert_idx = prompt_idx.unwrap_or(p.args.len());
        if prompt_idx.unwrap_or(0) > 0 && p.args[prompt_idx.unwrap() - 1].starts_with('-') {
            insert_idx = prompt_idx.unwrap() - 1;
        }
        args = Vec::with_capacity(p.args.len() + 2);
        for (i, arg) in p.args.iter().enumerate() {
            if i == insert_idx && !p.model_flag.is_empty() && !chosen.is_empty() {
                args.push(p.model_flag.clone());
                args.push(chosen.clone());
            }
            args.push(arg.clone());
        }
        break;
    }
    if args.is_empty() {
        args = spec.split_whitespace().map(str::to_string).collect();
        if args.is_empty() {
            return Err("empty harness command".to_string());
        }
        if !spec.contains("{prompt}") {
            return Err(format!(
                "a command template must contain {{prompt}} (known harnesses: {})",
                agent_preset_names().join(", ")
            ));
        }
        name = Path::new(&args[0])
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        chosen = model.to_string();
        if !chosen.is_empty() {
            for arg in args.iter_mut() {
                *arg = arg.replace("{model}", &chosen);
            }
        }
    }
    let bin = crate::lspservers::look_path_in(&args[0], &crate::lspservers::lsp_bin_dirs())
        .ok_or_else(|| format!("{} is not installed", args[0]))?;
    let mut resolved = args.clone();
    resolved[0] = bin;
    Ok((name, resolved, chosen))
}

/// `filepath.FromSlash`: git names files with forward slashes even on
/// Windows. Ports the `filepath.FromSlash` in Go `settle`.
fn from_slash(rel: &str) -> String {
    if cfg!(windows) {
        rel.replace('/', "\\")
    } else {
        rel.to_string()
    }
}

/// `4` or `4-9`. Ports Go `lineRef`.
pub fn line_ref(l1: i64, l2: i64) -> String {
    if l1 == l2 {
        l1.to_string()
    } else {
        format!("{l1}-{l2}")
    }
}

/// Lines l1..l2 of a file, 1-based and inclusive. Ports Go
/// `readLineRange`.
pub fn read_line_range(abs: &Path, l1: i64, l2: i64) -> Result<String, String> {
    let data = std::fs::read(abs).map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&data).replace("\r\n", "\n");
    let lines: Vec<&str> = text.split('\n').collect();
    let (mut l1, mut l2) = (l1, l2);
    if l1 < 1 {
        l1 = 1;
    }
    if l2 < l1 {
        l2 = l1;
    }
    if l1 as usize > lines.len() {
        return Err(format!(
            "line {l1} is past the end of {}",
            abs.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        ));
    }
    if l2 as usize > lines.len() {
        l2 = lines.len() as i64;
    }
    Ok(lines[l1 as usize - 1..l2 as usize].join("\n"))
}

/// Compose what the harness is told, shaped like the Copy-for-Agent
/// snippet in web/src/selbar.js. Ports Go `agentPrompt`.
pub fn agent_prompt(rel: &str, l1: i64, l2: i64, snippet: &str, instruction: &str) -> String {
    let ext = Path::new(rel)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    let line_str = if l1 == l2 {
        format!("line {l1}")
    } else {
        format!("lines {l1}-{l2}")
    };
    format!(
        "@{rel} {line_str}\n```{ext}\n{snippet}\n```\n\n### Instruction\n{instruction}\n\nEdit the file in place to carry out that instruction. Change only what it asks for, and do not explain the change afterwards."
    )
}

/// Quote one argv word for display. Ports Go `shellQuote`.
pub fn shell_quote(s: &str) -> String {
    if s.is_empty() {
        return "''".to_string();
    }
    let safe = s
        .chars()
        .all(|r| r.is_ascii_alphanumeric() || matches!(r, '-' | '_' | '.' | '/' | '=' | ':' | ','));
    if safe {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Display argv the way a terminal would take it. Ports Go
/// `shellCommand`.
pub fn shell_command(args: &[String]) -> String {
    args.iter()
        .map(|a| shell_quote(a))
        .collect::<Vec<_>>()
        .join(" ")
}

// ---------------------------------------------------------------- snapshots

/// `git status` with each listed file's size and mtime folded in, so
/// editing an already-modified file still shows up. Ports Go
/// `worktreeSnapshot` (missing files surface through status alone).
pub fn worktree_snapshot(root: &Path) -> HashMap<String, String> {
    let mut st = crate::git::git_status(root).unwrap_or_default();
    for (rel, code) in st.clone() {
        let abs = root.join(from_slash(&rel));
        if let Ok(meta) = std::fs::metadata(&abs) {
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            st.insert(rel, format!("{} {} {mtime}", code, meta.len()));
        }
    }
    st
}

/// Paths whose snapshot entries differ, in either direction. Ports Go
/// `changedSinceMaps`.
pub fn changed_since_maps(
    before: &HashMap<String, String>,
    after: &HashMap<String, String>,
) -> Vec<String> {
    let mut out = Vec::new();
    for (path, st) in after {
        if before.get(path) != Some(st) {
            out.push(path.clone());
        }
    }
    // A file restored to its committed state leaves the status list.
    for path in before.keys() {
        if !after.contains_key(path) {
            out.push(path.clone());
        }
    }
    out
}

pub fn changed_since(root: &Path, before: &HashMap<String, String>) -> Vec<String> {
    changed_since_maps(before, &worktree_snapshot(root))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serializes tests that share the process-wide model cache (and
    /// PATH), the way Go's sequential tests never interleave.
    static CACHE_LOCK: Mutex<()> = Mutex::new(());

    fn isolate() -> (
        crate::testutil::TempDir,
        std::sync::MutexGuard<'static, ()>,
        crate::testutil::EnvGuard,
    ) {
        let dir = crate::testutil::tempdir("agent");
        let lock = crate::testutil::ENV_LOCK.lock().unwrap();
        let path = dir.path().to_str().unwrap().to_string();
        let guard = crate::testutil::set_env(&[("XDG_CONFIG_HOME", &path), ("HOME", &path)]);
        (dir, lock, guard)
    }

    /// Ports Go `TestAgentSpecResolution`.
    #[test]
    fn spec_resolution_pins_and_refuses() {
        let (_dir, _lock, _env) = isolate();
        let root = crate::testutil::tempdir("agentroot");
        assert!(AgentManager::new(root.path().to_path_buf(), "echo hello", None, true).is_err());
        assert!(AgentManager::new(
            root.path().to_path_buf(),
            "rx0-not-a-real-binary {prompt}",
            None,
            true
        )
        .is_err());

        let m = AgentManager::new(root.path().to_path_buf(), "echo {prompt}", None, true).unwrap();
        assert_eq!(m.name(), "echo");
        assert!(m.pinned());
        assert!(m.select("echo {prompt}", None).is_err());

        let idle = AgentManager::new(root.path().to_path_buf(), "", None, true).unwrap();
        assert_eq!(idle.name(), "");
        assert!(!idle.pinned());
    }

    /// Ports Go `TestAgentDetectListsKnownHarnesses` (order and shape;
    /// model defaults are covered below).
    #[test]
    fn detect_lists_one_row_per_preset() {
        let (_dir, _lock, _env) = isolate();
        let _cache = CACHE_LOCK.lock().unwrap();
        let m = AgentManager::new(
            crate::testutil::tempdir("agentroot").path().to_path_buf(),
            "",
            None,
            true,
        )
        .unwrap();
        let got = m.detect();
        let presets = agent_presets();
        assert_eq!(got.len(), presets.len());
        for (h, p) in got.iter().zip(presets.iter()) {
            assert_eq!(h.name, p.name);
            assert!(
                h.cmd.contains("{prompt}"),
                "{} cmd shows the template",
                h.name
            );
            if h.installed {
                assert!(h.path.as_ref().map(|p| !p.is_empty()).unwrap_or(false));
            }
        }
    }

    /// Ports Go `TestAgentSelectPersistsOutsideWorkspace`.
    #[test]
    fn select_persists_and_restores() {
        let (cfg, _lock, _env) = isolate();
        let root = crate::testutil::tempdir("agentroot");
        let m = AgentManager::new(root.path().to_path_buf(), "", None, true).unwrap();
        assert!(m.select("rx0-not-a-real-binary", None).is_err());
        // echo stands in for a harness binary on every machine.
        m.select("echo {prompt}", None).unwrap();
        assert_eq!(m.name(), "echo");

        assert!(cfg.path().join("rx0").join("settings.json").exists());
        assert!(!root.path().join("settings.json").exists());

        let restored = AgentManager::new(root.path().to_path_buf(), "", None, true).unwrap();
        assert_eq!(restored.name(), "echo");
        restored.select("", None).unwrap();
        assert_eq!(restored.name(), "");
        let again = AgentManager::new(root.path().to_path_buf(), "", None, true).unwrap();
        assert_eq!(again.name(), "");
    }

    /// Ports Go `TestAgentModelSelectionAndDefaults`.
    #[test]
    fn models_default_to_least_capable() {
        let (_dir, _lock, _env) = isolate();
        let _cache = CACHE_LOCK.lock().unwrap();
        let m = AgentManager::new(
            crate::testutil::tempdir("agentroot").path().to_path_buf(),
            "",
            None,
            true,
        )
        .unwrap();
        for h in m.detect() {
            assert!(!h.models.is_empty(), "{} should list models", h.name);
            assert!(
                !h.model.is_empty(),
                "{} should have a default model",
                h.name
            );
            assert_eq!(h.model, h.models[0], "{} default != least capable", h.name);
        }
    }

    /// Ports Go `TestPresetArgvOrder`.
    #[test]
    fn preset_argv_keeps_prompt_last() {
        for p in agent_presets() {
            assert!(p.args.len() >= 2, "{} args too short", p.name);
            assert_eq!(
                *p.args.last().unwrap(),
                "{prompt}",
                "{} prompt not last",
                p.name
            );
            let Ok((_, resolved, _)) = resolve_agent_spec(&p.name, "") else {
                continue;
            };
            assert_eq!(
                *resolved.last().unwrap(),
                "{prompt}",
                "{} resolved prompt not last",
                p.name
            );
        }
    }

    /// Ports Go `TestAllPresetArgvFormatting`: with every preset binary
    /// faked on PATH, each resolves with its default model flag.
    #[test]
    fn all_presets_resolve_with_model_flag() {
        let (_dir, _lock, _env) = isolate();
        let bindir = crate::testutil::tempdir("agentbin");
        let old_path = std::env::var("PATH").unwrap_or_default();
        // PATH separator: ';' on Windows, ':' elsewhere. (join_paths is
        // wrong here: it rejects elements containing the separator, and
        // the existing PATH is full of them.)
        #[cfg(windows)]
        const PATH_SEP: char = ';';
        #[cfg(not(windows))]
        const PATH_SEP: char = ':';
        let new_path = format!("{}{}{}", bindir.path().display(), PATH_SEP, old_path);
        let _path_guard = crate::testutil::set_env(&[("PATH", &new_path)]);
        for p in agent_presets() {
            let bin = bindir.path().join(&p.args[0]);
            std::fs::write(&bin, "#!/bin/sh\nexit 0\n").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        for p in agent_presets() {
            let (name, resolved, model) = resolve_agent_spec(&p.name, "").unwrap();
            assert_eq!(name, p.name);
            assert_eq!(model, p.default_model);
            assert_eq!(*resolved.last().unwrap(), "{prompt}");
            assert!(
                resolved
                    .windows(2)
                    .any(|w| w[0] == p.model_flag && w[1] == p.default_model),
                "{} missing model flag in {resolved:?}",
                p.name
            );
        }
    }

    /// Ports Go `TestClaudeModelDiscovery`.
    #[test]
    fn claude_discovers_models() {
        let (_dir, _lock, _env) = isolate();
        let _cache = CACHE_LOCK.lock().unwrap();
        let dir = crate::testutil::tempdir("fakeclaude");
        let fake = dir.path().join("claude");
        std::fs::write(
            &fake,
            "#!/bin/sh\necho 'Current model: Sonnet 5'\necho 'Usage: /model <name>. Available: sonnet, opus, haiku, fable, best, sonnet[1m], opus[1m], fable[1m], opusplan, default, or a full model ID.'\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        {
            let mut cache = MODELS.lock().unwrap();
            cache.cached.remove("claude");
            cache.discovering.remove("claude");
        }
        run_model_discovery(
            "claude",
            fake.to_str().unwrap(),
            vec![
                "haiku".to_string(),
                "sonnet".to_string(),
                "opus".to_string(),
            ],
        );
        let models = MODELS
            .lock()
            .unwrap()
            .cached
            .get("claude")
            .cloned()
            .unwrap_or_default();
        // The extensionless shell-script fake cannot spawn under
        // CreateProcess: discovery must degrade to the static list.
        #[cfg(windows)]
        assert_eq!(
            models,
            vec![
                "haiku".to_string(),
                "sonnet".to_string(),
                "opus".to_string()
            ]
        );
        #[cfg(not(windows))]
        {
            assert!(!models.is_empty());
            assert_eq!(models[0], "haiku");
            assert!(models.contains(&"fable".to_string()), "{models:?}");
        }
    }

    /// Ports Go `TestReadLineRange`.
    #[test]
    fn line_ranges_clamp() {
        let dir = crate::testutil::tempdir("agentlines");
        let f = dir.path().join("f.txt");
        std::fs::write(&f, "one\ntwo\nthree\nfour\n").unwrap();
        for (l1, l2, want) in [
            (2, 3, "two\nthree"),
            (1, 1, "one"),
            (3, 99, "three\nfour\n"),
            (0, 1, "one"),
        ] {
            assert_eq!(read_line_range(&f, l1, l2).unwrap(), want, "{l1}-{l2}");
        }
        assert!(read_line_range(&f, 50, 60).is_err());
    }

    /// Ports Go `TestChangedSinceReportsBothDirections`.
    #[test]
    fn changed_since_maps_both_directions() {
        let (before, after): (HashMap<String, String>, HashMap<String, String>) = (
            [
                ("stays.go".to_string(), "M".to_string()),
                ("reverted.go".to_string(), "M".to_string()),
            ]
            .into_iter()
            .collect(),
            [
                ("stays.go".to_string(), "M".to_string()),
                ("new.go".to_string(), "U".to_string()),
            ]
            .into_iter()
            .collect(),
        );
        let got: std::collections::HashSet<String> =
            changed_since_maps(&before, &after).into_iter().collect();
        assert!(!got.contains("stays.go"));
        assert!(got.contains("new.go"));
        assert!(got.contains("reverted.go"));
    }

    /// Ports Go `TestLineRefAndPrompt` and `TestShellQuoteAndCommand`.
    #[test]
    fn refs_prompts_and_shell_quoting() {
        assert_eq!(line_ref(4, 4), "4");
        assert_eq!(line_ref(4, 9), "4-9");
        let p = agent_prompt("web/src/app.js", 2, 5, "const x = 1;", "rename x to count");
        for want in [
            "@web/src/app.js lines 2-5",
            "```js",
            "const x = 1;",
            "rename x to count",
        ] {
            assert!(p.contains(want), "prompt missing {want}:\n{p}");
        }
        let single = agent_prompt("a.go", 4, 4, "pkg a", "fix");
        assert!(single.contains("@a.go line 4"), "{single}");
        for (input, want) in [
            (
                vec!["agy", "--mode", "accept-edits", "-p", "hello world"],
                "agy --mode accept-edits -p 'hello world'",
            ),
            (vec!["echo", "it's working"], "echo 'it'\\''s working'"),
            (vec!["tool", ""], "tool ''"),
        ] {
            let args: Vec<String> = input.iter().map(|s| s.to_string()).collect();
            assert_eq!(shell_command(&args), want);
        }
    }
}
