//! Language-server registry and lifecycle: which servers are known,
//! which are installed, and one client per server, spawned on first use.
//!
//! Ports `lspservers.go`. Discovery runs on a background thread so
//! startup stays instant; a crashed server is respawned a bounded
//! number of times.

use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::lsp::LspClient;

pub const MAX_LSP_RESTARTS: u32 = 3;
/// Cap on allowlisted outside-the-tree paths. Ports the 20000 in Go `allow`.
pub const MAX_EXTERNAL: usize = 20_000;

// ---------------------------------------------------------------- registry

#[derive(Clone, Debug)]
pub struct LspServerDef {
    pub name: String,
    pub lang: String,
    pub cmd: Vec<String>,
    /// File extensions this server handles.
    pub exts: Vec<String>,
    /// ext -> LSP languageId, when it differs.
    pub lang_ids: HashMap<String, String>,
    pub default_lang: String,
    pub init_options: Value,
    /// Ways to get the binary, best first.
    pub install: Vec<LspInstallDef>,
}

#[derive(Clone, Debug)]
pub struct LspInstallDef {
    /// "darwin", "linux" or "windows"; empty for any.
    pub os: String,
    pub cmd: Vec<String>,
    /// px0 may run it: user-level and non-interactive.
    pub auto: bool,
}

impl LspServerDef {
    pub fn language_id(&self, rel: &str) -> String {
        let ext = Path::new(rel)
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| format!(".{e}"))
            .unwrap_or_default()
            .to_lowercase();
        self.lang_ids
            .get(&ext)
            .cloned()
            .unwrap_or_else(|| self.default_lang.clone())
    }

    pub fn installs_for(&self, os: &str) -> Vec<LspInstallDef> {
        self.install
            .iter()
            .filter(|i| i.os.is_empty() || i.os == os)
            .cloned()
            .collect()
    }
}

fn def(
    name: &str,
    lang: &str,
    cmd: &[&str],
    exts: &[&str],
    lang_ids: &[(&str, &str)],
    default_lang: &str,
    install: &[(&str, &[&str], bool)],
) -> LspServerDef {
    LspServerDef {
        name: name.to_string(),
        lang: lang.to_string(),
        cmd: cmd.iter().map(|s| s.to_string()).collect(),
        exts: exts.iter().map(|s| s.to_string()).collect(),
        lang_ids: lang_ids
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        default_lang: default_lang.to_string(),
        init_options: Value::Null,
        install: install
            .iter()
            .map(|(os, cmd, auto)| LspInstallDef {
                os: os.to_string(),
                cmd: cmd.iter().map(|s| s.to_string()).collect(),
                auto: *auto,
            })
            .collect(),
    }
}

/// Ordered: the first entry whose binary exists wins for an extension,
/// so a more capable server listed earlier takes precedence. Ports Go
/// `lspRegistry` verbatim.
pub fn lsp_registry() -> Vec<LspServerDef> {
    vec![
        def(
            "gopls",
            "Go",
            &["gopls"],
            &[".go"],
            &[],
            "go",
            &[
                (
                    "",
                    &["go", "install", "golang.org/x/tools/gopls@latest"],
                    true,
                ),
                ("darwin", &["brew", "install", "gopls"], true),
            ],
        ),
        def(
            "rust-analyzer",
            "Rust",
            &["rust-analyzer"],
            &[".rs"],
            &[],
            "rust",
            &[
                ("", &["rustup", "component", "add", "rust-analyzer"], true),
                ("darwin", &["brew", "install", "rust-analyzer"], true),
            ],
        ),
        def(
            "pyright",
            "Python",
            &["pyright-langserver", "--stdio"],
            &[".py", ".pyi"],
            &[],
            "python",
            &[
                ("", &["npm", "install", "-g", "pyright"], true),
                ("darwin", &["brew", "install", "pyright"], true),
            ],
        ),
        def(
            "pylsp",
            "Python",
            &["pylsp"],
            &[".py", ".pyi"],
            &[],
            "python",
            &[
                ("", &["pipx", "install", "python-lsp-server"], true),
                ("darwin", &["brew", "install", "python-lsp-server"], true),
            ],
        ),
        // Not offered for install: it lints, but answers no call hierarchy.
        def(
            "ruff",
            "Python",
            &["ruff", "server"],
            &[".py"],
            &[],
            "python",
            &[],
        ),
        def(
            "typescript",
            "TypeScript and JavaScript",
            &["typescript-language-server", "--stdio"],
            &[".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs"],
            &[
                (".ts", "typescript"),
                (".tsx", "typescriptreact"),
                (".jsx", "javascriptreact"),
            ],
            "javascript",
            &[
                (
                    "",
                    &[
                        "npm",
                        "install",
                        "-g",
                        "typescript-language-server",
                        "typescript",
                    ],
                    true,
                ),
                (
                    "darwin",
                    &["brew", "install", "typescript-language-server"],
                    true,
                ),
            ],
        ),
        def(
            "clangd",
            "C and C++",
            &["clangd", "--background-index"],
            &[
                ".c", ".h", ".cc", ".cpp", ".cxx", ".hpp", ".hh", ".m", ".mm",
            ],
            &[(".c", "c"), (".h", "c")],
            "cpp",
            &[
                ("darwin", &["brew", "install", "llvm"], true),
                // System package managers want an administrator: shown, not run.
                ("linux", &["sudo", "apt", "install", "clangd"], false),
                ("windows", &["winget", "install", "LLVM.LLVM"], false),
            ],
        ),
        def(
            "zls",
            "Zig",
            &["zls"],
            &[".zig"],
            &[],
            "zig",
            &[("darwin", &["brew", "install", "zls"], true)],
        ),
        def(
            "lua",
            "Lua",
            &["lua-language-server"],
            &[".lua"],
            &[],
            "lua",
            &[("darwin", &["brew", "install", "lua-language-server"], true)],
        ),
        def(
            "solargraph",
            "Ruby",
            &["solargraph", "stdio"],
            &[".rb"],
            &[],
            "ruby",
            &[
                ("", &["gem", "install", "solargraph"], true),
                ("darwin", &["brew", "install", "solargraph"], true),
            ],
        ),
        def(
            "jdtls",
            "Java",
            &["jdtls"],
            &[".java"],
            &[],
            "java",
            &[("darwin", &["brew", "install", "jdtls"], true)],
        ),
        def(
            "omnisharp",
            "C#",
            &["omnisharp", "-lsp"],
            &[".cs"],
            &[],
            "csharp",
            &[],
        ),
        def(
            "texlab",
            "LaTeX",
            &["texlab"],
            &[".tex"],
            &[],
            "latex",
            &[("darwin", &["brew", "install", "texlab"], true)],
        ),
    ]
}

// ---------------------------------------------------------------- errors

/// Ports Go's `errNoServer`/`errFailed`/plain errors. `Failed` is what
/// a known-bad server reports; anything else (a crash past the restart
/// budget, a timeout) is `Other`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LspError {
    NoServer,
    Failed(String),
    Other(String),
}

impl std::fmt::Display for LspError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoServer => write!(f, "no language server for this file type"),
            Self::Failed(why) | Self::Other(why) => write!(f, "{why}"),
        }
    }
}

impl std::error::Error for LspError {}

// ---------------------------------------------------------------- state

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LspState {
    /// Disabled, or no server installed for this type.
    Off,
    /// Process spawned, handshake in flight.
    Starting,
    /// Up, but still chewing through the workspace.
    Indexing,
    Ready,
    Failed,
}

impl LspState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Starting => "starting",
            Self::Indexing => "indexing",
            Self::Ready => "ready",
            Self::Failed => "failed",
        }
    }
}

// ---------------------------------------------------------------- manager

struct Starter {
    done: Mutex<bool>,
    cond: Condvar,
}

struct MgrInner {
    by_ext: HashMap<String, LspServerDef>,
    clients: HashMap<String, Arc<LspClient>>,
    starting: HashMap<String, Arc<Starter>>,
    failed: HashMap<String, String>,
    available: Vec<String>,
    restarts: HashMap<String, u32>,
    discovered: bool,
}

pub struct LspManager {
    root: PathBuf,
    enabled: bool,
    registry: Vec<LspServerDef>,
    inner: Mutex<MgrInner>,
    pub(crate) jobs: Mutex<HashMap<String, crate::lspsetup::JobSlot>>,
    external: Mutex<HashSet<String>>,
}

impl LspManager {
    pub fn new(root: PathBuf, enabled: bool) -> Arc<Self> {
        let this = Arc::new(Self {
            root,
            enabled,
            registry: lsp_registry(),
            inner: Mutex::new(MgrInner {
                by_ext: HashMap::new(),
                clients: HashMap::new(),
                starting: HashMap::new(),
                failed: HashMap::new(),
                available: Vec::new(),
                restarts: HashMap::new(),
                discovered: false,
            }),
            jobs: Mutex::new(HashMap::new()),
            external: Mutex::new(HashSet::new()),
        });
        if enabled {
            // Discover in the background so startup is instantaneous.
            let bg = this.clone();
            std::thread::spawn(move || bg.discover());
        }
        this
    }

    /// Test seam: a manager with a fixed extension table and no
    /// discovery. Mirrors the struct literals in Go's LSP tests.
    #[cfg(test)]
    pub fn for_test(root: PathBuf, by_ext: HashMap<String, LspServerDef>) -> Arc<Self> {
        Arc::new(Self {
            root,
            enabled: true,
            registry: lsp_registry(),
            inner: Mutex::new(MgrInner {
                by_ext,
                clients: HashMap::new(),
                starting: HashMap::new(),
                failed: HashMap::new(),
                available: Vec::new(),
                restarts: HashMap::new(),
                discovered: true,
            }),
            jobs: Mutex::new(HashMap::new()),
            external: Mutex::new(HashSet::new()),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn allow(&self, abs: &Path) {
        let mut ext = self.external.lock().unwrap();
        if ext.len() < MAX_EXTERNAL {
            ext.insert(abs.to_string_lossy().into_owned());
        }
    }

    /// Whether a language server has named this exact file.
    pub fn allowed(&self, abs: &Path) -> bool {
        self.external
            .lock()
            .unwrap()
            .contains(abs.to_string_lossy().as_ref())
    }

    /// Resolve which registry servers are installed. Runs again on
    /// Rescan; the result swaps in whole so readers never see a
    /// half-built table. Ports Go `discover`.
    pub fn discover(&self) {
        if !self.enabled {
            return;
        }
        let dirs = lsp_bin_dirs();
        let mut by_ext: HashMap<String, LspServerDef> = HashMap::new();
        let mut available = Vec::new();
        for def in &self.registry {
            let mut def = def.clone(); // Cmd[0] becomes the resolved path
            let Some(bin) = look_path_in(&def.cmd[0], &dirs) else {
                continue;
            };
            def.cmd[0] = bin;
            let mut claimed = false;
            for ext in &def.exts {
                if !by_ext.contains_key(ext) {
                    by_ext.insert(ext.clone(), def.clone());
                    claimed = true;
                }
            }
            if claimed {
                available.push(def.name.clone());
            }
        }
        let mut inner = self.inner.lock().unwrap();
        inner.by_ext = by_ext;
        inner.available = available;
        inner.discovered = true;
    }

    pub fn is_discovered(&self) -> bool {
        self.inner.lock().unwrap().discovered
    }

    pub fn available(&self) -> Vec<String> {
        self.inner.lock().unwrap().available.clone()
    }

    pub fn def_for(&self, rel: &str) -> Option<LspServerDef> {
        if !self.enabled {
            return None;
        }
        let ext = Path::new(rel)
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| format!(".{e}"))
            .unwrap_or_default()
            .to_lowercase();
        self.inner.lock().unwrap().by_ext.get(&ext).cloned()
    }

    /// What a caller can expect for this file without starting
    /// anything, so the UI can say "indexing" instead of silently
    /// showing regex hits. Ports Go `State`.
    pub fn state(&self, rel: &str) -> (LspState, String) {
        let Some(def) = self.def_for(rel) else {
            // Discovery runs in the background at startup. Until it
            // finishes, a file type px0 knows may still get a server.
            if self.enabled
                && !self.is_discovered()
                && !registry_for(rel, &self.registry).is_empty()
            {
                return (LspState::Starting, String::new());
            }
            return (LspState::Off, String::new());
        };
        let inner = self.inner.lock().unwrap();
        let client = inner.clients.get(&def.name);
        let failed = inner.failed.get(&def.name).cloned();
        let pending = inner.starting.contains_key(&def.name);
        if let Some(why) = failed {
            return (LspState::Failed, why);
        }
        if pending {
            return (LspState::Starting, def.name);
        }
        let Some(client) = client else {
            return (LspState::Starting, def.name); // not spawned yet; the next call will
        };
        if client.alive().is_some() {
            return (LspState::Failed, def.name);
        }
        if client.busy() {
            return (LspState::Indexing, def.name);
        }
        (LspState::Ready, def.name)
    }

    /// A started client for rel, spawning one on first use. Bounded by
    /// `deadline`; the spawn continues regardless so the next request
    /// finds it ready. Ports Go `client`. Takes `&Arc<Self>` so the
    /// background spawn thread holds a sound reference.
    pub fn client(
        self: &Arc<Self>,
        deadline: Instant,
        rel: &str,
    ) -> Result<Arc<LspClient>, LspError> {
        let def = self.def_for(rel).ok_or(LspError::NoServer)?;
        loop {
            enum Next {
                Failed(LspError),
                Ready(Arc<LspClient>),
                Wait(Arc<Starter>),
                Spawn(Arc<Starter>),
            }
            let next = {
                let mut inner = self.inner.lock().unwrap();
                if let Some(why) = inner.failed.get(&def.name) {
                    Next::Failed(LspError::Failed(why.clone()))
                } else if let Some(c) = inner.clients.get(&def.name) {
                    if c.alive().is_none() {
                        Next::Ready(c.clone())
                    } else {
                        // A crashed server would otherwise take hover,
                        // definitions and references down for the rest
                        // of the session. Start a fresh one, but stop
                        // after a few crashes.
                        let n = inner.restarts.get(&def.name).copied().unwrap_or(0);
                        if n >= MAX_LSP_RESTARTS {
                            // Past the budget the crash itself is
                            // reported, not a Failed marker.
                            Next::Failed(LspError::Other(c.alive().unwrap_or_default()))
                        } else {
                            inner.restarts.insert(def.name.clone(), n + 1);
                            inner.clients.remove(&def.name);
                            continue;
                        }
                    }
                } else if let Some(starter) = inner.starting.get(&def.name) {
                    Next::Wait(starter.clone())
                } else {
                    let starter = Arc::new(Starter {
                        done: Mutex::new(false),
                        cond: Condvar::new(),
                    });
                    inner.starting.insert(def.name.clone(), starter.clone());
                    Next::Spawn(starter)
                }
            };
            // Wait for a spawn (ours or another caller's), giving up at
            // the deadline while the spawn itself continues.
            let starter = match next {
                Next::Failed(e) => return Err(e),
                Next::Ready(c) => return Ok(c),
                Next::Wait(starter) => starter,
                Next::Spawn(starter) => {
                    let this = Arc::clone(self);
                    let def = def.clone();
                    std::thread::spawn(move || this.spawn(def));
                    starter
                }
            };
            let (lock, cvar) = (&starter.done, &starter.cond);
            let guard = lock.lock().unwrap();
            let timeout = deadline.saturating_duration_since(Instant::now());
            let (guard, res) = cvar
                .wait_timeout_while(guard, timeout, |done| !*done)
                .unwrap();
            drop(guard);
            if res.timed_out() {
                return Err(LspError::Other(format!("{}: request timed out", def.name)));
            }
            // Loop back and pick up the result.
        }
    }

    /// Spawn one server; the handshake gets 30 s of its own. Ports Go
    /// `spawn`. Runs on a background thread; the starter is always
    /// signalled, before it leaves the map.
    fn spawn(self: Arc<Self>, def: LspServerDef) {
        let name = def.name.clone();
        let result = LspClient::start(&def, &self.root);
        let starter = {
            let mut inner = self.inner.lock().unwrap();
            match result {
                Err(e) => {
                    inner.failed.insert(name.clone(), e);
                }
                Ok(c) => {
                    inner.clients.insert(name.clone(), c);
                }
            }
            match inner.starting.remove(&name) {
                Some(s) => s,
                None => return,
            }
        };
        *starter.done.lock().unwrap() = true;
        starter.cond.notify_all();
    }

    pub(crate) fn clear_failures(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.failed.clear();
        inner.restarts.clear();
    }

    /// Test seam: pretend `name` spawned successfully, so nav plumbing
    /// runs without a real language server.
    #[cfg(test)]
    pub fn insert_client(&self, name: &str, client: Arc<LspClient>) {
        self.inner
            .lock()
            .unwrap()
            .clients
            .insert(name.to_string(), client);
    }

    pub fn close_doc(&self, abs: &Path, rel: &str) {
        let def = match self.def_for(rel) {
            Some(d) => d,
            None => return,
        };
        let client = self.inner.lock().unwrap().clients.get(&def.name).cloned();
        if let Some(c) = client {
            c.close_doc(abs);
        }
    }

    /// Shut every server down rather than leaving the OS to reap them.
    /// Ports Go `Close`.
    pub fn close(&self) {
        let clients: Vec<Arc<LspClient>> = {
            let mut inner = self.inner.lock().unwrap();
            std::mem::take(&mut inner.clients).into_values().collect()
        };
        for c in clients {
            c.shutdown();
        }
    }

    pub fn registry(&self) -> &[LspServerDef] {
        &self.registry
    }
}

// ---------------------------------------------------------------- treePath

impl LspManager {
    /// An absolute path to what the UI shows and opens: relative inside
    /// the indexed tree, absolute outside it. Ports Go `treePath`.
    pub fn tree_path(&self, abs: &Path, allow: bool) -> (String, bool) {
        match abs.strip_prefix(&self.root) {
            Ok(rel) if rel.components().next().is_some() => {
                (rel.to_string_lossy().replace('\\', "/"), false)
            }
            _ => {
                // The standard library, or a dependency in the module
                // cache. Only paths a server named may be allowlisted.
                if allow {
                    self.allow(abs);
                }
                (abs.to_string_lossy().replace('\\', "/"), true)
            }
        }
    }
}

// ---------------------------------------------------------------- lookup

/// Every server px0 knows for rel's extension, best first, installed
/// or not. Ports Go `registryFor`.
pub fn registry_for(rel: &str, registry: &[LspServerDef]) -> Vec<LspServerDef> {
    let ext = Path::new(rel)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| format!(".{e}"))
        .unwrap_or_default()
        .to_lowercase();
    registry
        .iter()
        .filter(|d| d.exts.iter().any(|e| e == &ext))
        .cloned()
        .collect()
}

/// Folders installers put binaries in that are often missing from
/// PATH. Ports Go `lspBinDirs`.
pub fn lsp_bin_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(v) = std::env::var("GOBIN") {
        if !v.is_empty() {
            dirs.push(PathBuf::from(v));
        }
    }
    if let Ok(gopath) = std::env::var("GOPATH") {
        for p in std::env::split_paths(&gopath) {
            dirs.push(p.join("bin"));
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            for sub in [
                "go/bin",
                ".cargo/bin",
                ".local/bin",
                ".opencode/bin",
                ".codex/bin",
            ] {
                dirs.push(PathBuf::from(&home).join(sub));
            }
        }
    }
    // npm puts global packages beside its own executable.
    if let Some(npm) = look_path_in("npm", &[]) {
        if let Some(dir) = Path::new(&npm).parent() {
            dirs.push(dir.to_path_buf());
        }
    }
    if cfg!(target_os = "macos") {
        dirs.push(PathBuf::from("/opt/homebrew/bin"));
        dirs.push(PathBuf::from("/usr/local/bin"));
        // Homebrew's llvm is keg-only: clangd is installed but never linked.
        dirs.push(PathBuf::from("/opt/homebrew/opt/llvm/bin"));
        dirs.push(PathBuf::from("/usr/local/opt/llvm/bin"));
    }
    if cfg!(windows) {
        if let Ok(v) = std::env::var("APPDATA") {
            if !v.is_empty() {
                dirs.push(PathBuf::from(v).join("npm"));
            }
        }
        if let Ok(v) = std::env::var("ProgramFiles") {
            if !v.is_empty() {
                dirs.push(PathBuf::from(v).join("LLVM").join("bin"));
            }
        }
    }
    dirs
}

/// Find a command on PATH, then in dirs. Ports Go `lookPathIn`.
pub fn look_path_in(name: &str, dirs: &[PathBuf]) -> Option<String> {
    if let Ok(paths) = std::env::var("PATH") {
        for dir in std::env::split_paths(&paths) {
            if dir.as_os_str().is_empty() {
                continue;
            }
            if let Some(p) = executable_in(&dir.join(name)) {
                return Some(p);
            }
        }
    }
    for d in dirs {
        if let Some(p) = which_in_dir(d, name) {
            return Some(p);
        }
    }
    None
}

fn which_in_dir(dir: &Path, name: &str) -> Option<String> {
    executable_in(&dir.join(name))
}

#[cfg(unix)]
fn executable_in(path: &Path) -> Option<String> {
    use std::os::unix::fs::PermissionsExt;
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() {
        return None;
    }
    if meta.permissions().mode() & 0o111 == 0 {
        return None;
    }
    Some(path.to_string_lossy().into_owned())
}

#[cfg(windows)]
fn executable_in(path: &Path) -> Option<String> {
    if path.is_file() {
        return Some(path.to_string_lossy().into_owned());
    }
    // exec.LookPath tries PATHEXT extensions for a joined path.
    for ext in ["exe", "cmd", "bat", "com"] {
        let with_ext = path.with_extension(ext);
        if with_ext.is_file() {
            return Some(with_ext.to_string_lossy().into_owned());
        }
    }
    None
}

/// Current OS in Go's vocabulary for install-recipe filtering.
pub fn current_os() -> &'static str {
    if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(windows) {
        "windows"
    } else {
        "linux"
    }
}

/// How long a caller waits for one LSP round trip by default and at
/// most. Ports Go `lspCtx` (default 10 s, cap 120 s).
pub fn lsp_deadline(wait_ms: i64) -> Instant {
    let mut ms = wait_ms;
    if ms <= 0 {
        ms = 10_000;
    }
    if ms > 120_000 {
        ms = 120_000;
    }
    Instant::now() + Duration::from_millis(ms as u64)
}

/// Warm-request budget: default 1 ms, cap 60 s. Ports Go
/// `handleLSPWarm`.
pub fn warm_deadline(wait_ms: i64) -> Instant {
    let mut ms = wait_ms;
    if ms <= 0 {
        ms = 1;
    }
    if ms > 60_000 {
        ms = 60_000;
    }
    Instant::now() + Duration::from_millis(ms as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gone_def() -> LspServerDef {
        LspServerDef {
            name: "gone".to_string(),
            lang: "Go".to_string(),
            cmd: vec!["px0-test-no-such-language-server".to_string()],
            exts: vec![".go".to_string()],
            lang_ids: HashMap::new(),
            default_lang: "go".to_string(),
            init_options: Value::Null,
            install: Vec::new(),
        }
    }

    fn dead_client() -> Arc<LspClient> {
        let c = LspClient::for_test(Box::new(std::io::empty()), Box::new(std::io::sink()));
        c.fail("gone exited");
        c
    }

    fn deadline() -> Instant {
        Instant::now() + Duration::from_secs(5)
    }

    /// A disabled manager never resolves a server, whatever is
    /// installed. Ports the tail of Go `TestManagerDisabled`.
    #[test]
    fn disabled_manager_resolves_nothing() {
        let root = std::env::temp_dir().join(format!("px0-lsp-off-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let m = LspManager::new(root, false);
        assert!(m.available().is_empty());
        assert!(m.def_for("x.go").is_none());
        assert_eq!(m.state("x.go").0, LspState::Off);
    }

    /// A crashed client is replaced by a fresh spawn, a bounded number
    /// of times. Ports Go `TestCrashedServerIsRespawned`.
    #[test]
    fn crashed_server_respawns_with_budget() {
        let def = gone_def();
        let mut by_ext = HashMap::new();
        by_ext.insert(".go".to_string(), def.clone());
        let m = LspManager::for_test(
            std::env::temp_dir().join(format!("px0-lsp-crash-{}", std::process::id())),
            by_ext,
        );
        m.inner
            .lock()
            .unwrap()
            .clients
            .insert("gone".to_string(), dead_client());

        // The respawn is attempted; the binary does not exist, so the
        // failure is a Failed marker rather than the stale crash.
        match m.client(deadline(), "x.go") {
            Err(LspError::Failed(_)) => {}
            Err(e) => panic!("client() = {e}, want a failed respawn"),
            Ok(_) => panic!("client() succeeded, want a failed respawn"),
        }
        assert_eq!(m.inner.lock().unwrap().restarts.get("gone"), Some(&1));

        // Out of budget: the crash itself is reported, nothing spawns.
        {
            let mut inner = m.inner.lock().unwrap();
            inner.failed.clear();
            inner.clients.insert("gone".to_string(), dead_client());
            inner.restarts.insert("gone".to_string(), MAX_LSP_RESTARTS);
        }
        match m.client(deadline(), "x.go") {
            Err(LspError::Other(_)) => {}
            Err(e) => panic!("past-budget client() = {e}, want the crash error"),
            Ok(_) => panic!("past-budget client() succeeded, want the crash error"),
        }
    }

    /// Ports Go `TestLanguageIDMapping`.
    #[test]
    fn typescript_language_ids() {
        let ts = lsp_registry()
            .into_iter()
            .find(|d| d.name == "typescript")
            .unwrap();
        for (file, want) in [
            ("a.ts", "typescript"),
            ("a.tsx", "typescriptreact"),
            ("a.jsx", "javascriptreact"),
            ("a.js", "javascript"),
            ("a.mjs", "javascript"),
        ] {
            assert_eq!(ts.language_id(file), want, "{file}");
        }
    }

    /// A binary outside PATH is still found in an extra folder. Ports
    /// Go `TestLookPathInExtraDir`.
    #[test]
    fn look_path_finds_extra_dirs() {
        let dir = crate::testutil::tempdir("lspbin");
        let name = "px0-test-fake-language-server";
        let path = dir.path().join(name);
        std::fs::write(&path, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert!(look_path_in(name, &[]).is_none(), "must not be on PATH");
        let found =
            look_path_in(name, &[dir.path().to_path_buf()]).expect("must find in extra dir");
        assert_eq!(Path::new(&found).parent().unwrap(), dir.path());
    }

    /// Every recipe is well formed, and nothing px0 runs by itself asks
    /// for root. Ports Go `TestInstallRecipes`.
    #[test]
    fn install_recipes_well_formed() {
        for def in lsp_registry() {
            assert!(!def.lang.is_empty(), "{}: no Lang", def.name);
            for inst in &def.install {
                assert!(inst.cmd.len() >= 2, "{}: recipe too short", def.name);
                assert!(
                    ["", "darwin", "linux", "windows"].contains(&inst.os.as_str()),
                    "{}: unknown OS {:?}",
                    def.name,
                    inst.os
                );
                if inst.auto {
                    assert!(
                        inst.cmd[0] != "sudo" && inst.cmd[0] != "winget",
                        "{}: {:?} needs an administrator and must not be Auto",
                        def.name,
                        inst.cmd.join(" ")
                    );
                }
            }
        }
    }

    /// Ports Go `TestInstallRefusals`.
    #[test]
    fn install_refusals() {
        let off = LspManager::new(
            std::env::temp_dir().join(format!("px0-lsp-install-{}", std::process::id())),
            false,
        );
        assert!(
            off.install("gopls", 0).is_err(),
            "disabled install succeeded"
        );
        let m = LspManager::for_test(std::env::temp_dir(), HashMap::new());
        assert!(m.install("px0-no-such-server", 0).is_err());
        assert!(m.install("gopls", 99).is_err());
        assert!(m.install("gopls", -1).is_err());
        for def in lsp_registry() {
            for (i, inst) in def.installs_for(current_os()).iter().enumerate() {
                if !inst.auto {
                    assert!(
                        m.install(&def.name, i as i64).is_err(),
                        "ran the manual recipe {:?}",
                        inst.cmd.join(" ")
                    );
                }
            }
        }
    }

    #[test]
    fn tree_path_splits_inside_and_outside() {
        let root = PathBuf::from("/work/proj");
        let m = LspManager::for_test(root, HashMap::new());
        assert_eq!(
            m.tree_path(Path::new("/work/proj/a/b.go"), false),
            ("a/b.go".to_string(), false)
        );
        // Outside: absolute display, flagged external, allowlisted.
        let (rel, ext) = m.tree_path(Path::new("/usr/lib/go/src/x.go"), true);
        assert_eq!((rel.as_str(), ext), ("/usr/lib/go/src/x.go", true));
        assert!(m.allowed(Path::new("/usr/lib/go/src/x.go")));
        // allow=false does not allowlist.
        let m2 = LspManager::for_test(PathBuf::from("/work/proj"), HashMap::new());
        let (_, ext) = m2.tree_path(Path::new("/usr/lib/go/src/x.go"), false);
        assert!(ext);
        assert!(!m2.allowed(Path::new("/usr/lib/go/src/x.go")));
    }
}
