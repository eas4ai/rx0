//! rx0: a fast, ultra-light code navigator in the browser.
//!
//! CLI mirrors the Go flags in `main.go`: same names, same defaults.

use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;
use tokio::net::TcpListener;

use rx0::{
    git::set_disabled as set_git_disabled,
    index::Index,
    server::{serve_until, AppState, AssetSource},
    telemetry::TelemetryService,
    VERSION,
};

/// Resolve when the first SIGINT/SIGTERM arrives, mirroring Go's
/// `signal.Notify(stop, os.Interrupt, syscall.SIGTERM)`. A second signal
/// forces immediate exit(130), as in Go.
async fn shutdown_signal() {
    #[cfg(unix)]
    async fn first_signal() {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = signal(SignalKind::terminate()).expect("rx0: listen for SIGTERM");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = term.recv() => {},
        }
    }
    #[cfg(not(unix))]
    async fn first_signal() {
        let _ = tokio::signal::ctrl_c().await;
    }
    first_signal().await;
    eprint!("\r");
    eprintln!("rx0: stopped");
    tokio::spawn(async {
        first_signal().await;
        std::process::exit(130);
    });
}

/// `std` OS/arch names in Go's `GOOS/GOARCH` vocabulary, matching the
/// `rx0 <version> (<os>/<arch>)` line and release asset names.
fn go_platform() -> (&'static str, &'static str) {
    let os = match std::env::consts::OS {
        "linux" => "linux",
        "macos" => "darwin",
        "windows" => "windows",
        other => other,
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        "x86" => "386",
        other => other,
    };
    (os, arch)
}

#[derive(Parser, Debug)]
#[command(name = "rx0", about = "a code navigator", disable_version_flag = true)]
struct Args {
    /// Port to listen on (0 picks a free one).
    #[arg(long, default_value_t = 7777)]
    port: u16,
    /// Address to bind.
    #[arg(long, default_value = "127.0.0.1")]
    host: String,
    /// Do not launch a browser.
    #[arg(long)]
    no_open: bool,
    /// Do not use language servers, even if installed.
    #[arg(long)]
    no_lsp: bool,
    /// Disable git awareness.
    #[arg(long)]
    no_git: bool,
    /// Serve the UI from this source directory instead of the embedded copy.
    #[arg(long)]
    dev: Option<PathBuf>,
    /// Print version and exit.
    #[arg(long)]
    version: bool,
    /// Print version and exit (shorthand).
    #[arg(short = 'v')]
    v: bool,
    /// Check for and install the latest version of rx0.
    #[arg(long)]
    update: bool,
    /// Disable colour output.
    #[arg(long)]
    no_color: bool,
    /// Suppress narration.
    #[arg(long)]
    quiet: bool,
    /// Log requests and internal activity to the terminal.
    #[arg(long)]
    verbose: bool,
    /// Disable anonymous usage telemetry.
    #[arg(long)]
    no_telemetry: bool,
    /// Pin the coding harness used for edits.
    #[arg(long)]
    agent: Option<String>,
    /// Do not offer editing through a coding harness.
    #[arg(long)]
    no_agent: bool,
    /// File or directory to open.
    target: Option<String>,
}

/// Resolve the CLI target to a workspace root, mirroring Go
/// `resolveTarget`/`splitTargetLine`: a file target opens its parent
/// directory, and a trailing `:line` suffix is stripped.
fn resolve_target(target: &str) -> PathBuf {
    let stripped = strip_line_suffix(target);
    let path = PathBuf::from(stripped);
    if path.is_file() {
        path.parent()
            .map(PathBuf::from)
            .unwrap_or(PathBuf::from("."))
    } else {
        path
    }
    .canonicalize()
    .unwrap_or_else(|_| PathBuf::from(stripped))
}

fn strip_line_suffix(target: &str) -> &str {
    match target.rsplit_once(':') {
        Some((head, digits))
            if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) =>
        {
            // Windows drive prefixes ("C:\...") never look like ":digits".
            head
        }
        _ => target,
    }
}

fn open_browser(url: &str) {
    let result = if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(url).spawn()
    } else if cfg!(target_os = "windows") {
        std::process::Command::new("cmd")
            .args(["/c", "start", "", url])
            .spawn()
    } else {
        std::process::Command::new("xdg-open").arg(url).spawn()
    };
    if let Err(e) = result {
        eprintln!("rx0: could not open browser: {e}");
    }
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    if args.version || args.v {
        let (os, arch) = go_platform();
        println!("rx0 {VERSION} ({os}/{arch})");
        return;
    }

    if args.update {
        if let Err(e) = rx0::update::run_self_update(VERSION, &|line| println!("rx0: {line}")) {
            eprintln!("rx0: {e}");
            std::process::exit(1);
        }
        return;
    }

    // Anonymous usage telemetry: on by build key, off by flag/env.
    // `close` on every exit path flushes the queue first.
    let telemetry = TelemetryService::new(args.no_telemetry);

    let assets = match &args.dev {
        Some(dir) => {
            let probe = dir.join("web").join("index.html");
            if !probe.is_file() {
                eprintln!(
                    "rx0: -dev {}: {}/web/index.html not found",
                    dir.display(),
                    dir.display()
                );
                std::process::exit(1);
            }
            AssetSource::Disk(dir.clone())
        }
        None => AssetSource::Embedded,
    };

    let target = args.target.as_deref().unwrap_or(".");
    let root = resolve_target(target);
    set_git_disabled(args.no_git);
    // Language servers are discovered in the background, like Go's
    // `newLSPManager`, so startup stays instant.
    let lsp = rx0::lspservers::LspManager::new(root.clone(), !args.no_lsp);
    // The harness choice restores from settings; a bad `-agent` stops
    // startup, like Go's fatal. `--no-agent` leaves editing out.
    let agent = if args.no_agent {
        None
    } else {
        match rx0::agent::AgentManager::new(
            root.clone(),
            args.agent.as_deref().unwrap_or(""),
            Some(lsp.clone()),
            args.quiet,
        ) {
            Ok(m) => Some(m),
            Err(e) => {
                eprintln!("rx0: -agent: {e}");
                std::process::exit(1);
            }
        }
    };
    let _ = &args.no_telemetry;

    let addr = format!("{}:{}", args.host, args.port);
    let listener = match TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("rx0: cannot listen on {addr}: {e}");
            std::process::exit(1);
        }
    };
    let url = format!("http://{}", listener.local_addr().unwrap());

    if !args.quiet {
        println!("rx0 {} serving {} at {url}", VERSION, root.display());
    }
    if !args.no_open {
        open_browser(&url);
    }

    // Check for updates asynchronously once a day without delaying
    // startup, exactly like Go's `go checkDailyUpdate(version)`.
    {
        let quiet = args.quiet;
        let current = VERSION.to_string();
        std::thread::spawn(move || {
            if let Some(note) = rx0::update::check_daily_update(&current, quiet) {
                eprintln!("rx0: {note}");
            }
        });
    }

    // Index the workspace asynchronously so the server responds immediately.
    let index = Arc::new(Index::new(root.clone()));
    {
        let index = index.clone();
        let quiet = args.quiet;
        let root_for_track = root.clone();
        let telemetry = telemetry.clone();
        let lsp_for_track = lsp.clone();
        std::thread::spawn(move || {
            index.build();
            let (n, _, ms) = index.stats();
            if !quiet {
                println!("indexed {n} files in {ms}ms");
            }
            telemetry.track(
                "session_started",
                [
                    (
                        "files_bucket".to_string(),
                        serde_json::Value::String(rx0::telemetry::files_bucket(n).to_string()),
                    ),
                    ("index_ms".to_string(), serde_json::Value::from(ms)),
                    (
                        "has_git".to_string(),
                        serde_json::Value::Bool(rx0::git::git_available(&root_for_track)),
                    ),
                    (
                        "has_lsp".to_string(),
                        serde_json::Value::Bool(!lsp_for_track.available().is_empty()),
                    ),
                ]
                .into_iter()
                .collect(),
            );
        });
    }

    let state = AppState {
        root,
        assets,
        index,
        lsp: lsp.clone(),
        agent: agent.clone(),
    };
    if args.verbose {
        eprintln!("rx0: listening on {url}");
    }
    // Go drains the server on SIGINT/SIGTERM (`srv.Shutdown`), then closes
    // children and reports "interrupted" with exit 130. `serve_until`
    // returning Ok means the shutdown signal fired (it never resolves
    // otherwise); Err is a real listener failure.
    let serve_result = serve_until(listener, state, shutdown_signal()).await;
    let interrupted = serve_result.is_ok();
    // Language servers are children that can hold gigabytes. Shut them
    // down on the way out rather than leaving them for the OS to reap.
    lsp.close();
    if let Some(agent) = &agent {
        agent.close();
    }
    if interrupted {
        telemetry.close("interrupted");
        std::process::exit(130);
    }
    telemetry.close("normal");
    if let Err(e) = serve_result {
        eprintln!("rx0: server error: {e}");
        std::process::exit(1);
    }
}
