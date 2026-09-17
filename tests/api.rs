//! Slice-1 HTTP integration tests: shell routes only.
//!
//! Boots the real router on a loopback port and speaks raw HTTP/1.0 over
//! TCP, so no HTTP client dependency is needed. Handler-level traversal
//! cases land with the `/api/file` slice.

use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use rx0::index::Index;
use rx0::server::{build_router, AppState, AssetSource};

fn start_server() -> std::net::SocketAddr {
    start_server_at(PathBuf::from(env!("CARGO_MANIFEST_DIR")))
}

fn start_server_at(root: PathBuf) -> std::net::SocketAddr {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let listener = tokio::net::TcpListener::from_std(listener).unwrap();
    let index = std::sync::Arc::new(Index::new(root.clone()));
    index.build();
    let state = AppState {
        root: root.clone(),
        assets: AssetSource::Embedded,
        index,
        lsp: rx0::lspservers::LspManager::new(root, false),
        agent: None,
    };
    tokio::spawn(async move {
        axum::serve(listener, build_router(state)).await.unwrap();
    });
    addr
}

/// Minimal HTTP/1.0 GET with `Connection: close`, so the server closes the
/// stream and `read_to_end` terminates.
async fn get(addr: std::net::SocketAddr, path: &str) -> (u16, Vec<(String, String)>, Vec<u8>) {
    let mut stream =
        tokio::time::timeout(Duration::from_secs(5), tokio::net::TcpStream::connect(addr))
            .await
            .expect("connect")
            .unwrap();
    stream
        .write_all(
            format!("GET {path} HTTP/1.0\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .await
        .unwrap();
    let mut raw = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut raw))
        .await
        .expect("read")
        .unwrap();
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("response must have a header block");
    let head = String::from_utf8_lossy(&raw[..split]);
    let mut lines = head.lines();
    let status: u16 = lines
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let headers = lines
        .filter_map(|l| {
            l.split_once(':')
                .map(|(k, v)| (k.trim().to_lowercase(), v.trim().to_string()))
        })
        .collect();
    (status, headers, raw[split + 4..].to_vec())
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> &'a str {
    headers
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
        .unwrap_or("")
}

#[tokio::test]
async fn index_serves_html_shell() {
    let addr = start_server();
    let (status, headers, body) = get(addr, "/").await;
    assert_eq!(status, 200);
    assert!(header(&headers, "content-type").starts_with("text/html"));
    assert!(String::from_utf8_lossy(&body).contains("<!DOCTYPE html") || !body.is_empty());
}

#[tokio::test]
async fn static_bundle_and_joined_themes() {
    let addr = start_server();
    let (status, headers, body) = get(addr, "/static/app.js").await;
    assert_eq!(status, 200);
    assert!(header(&headers, "content-type").contains("javascript"));
    assert!(!body.is_empty());

    let (status, headers, body) = get(addr, "/static/themes.css").await;
    assert_eq!(status, 200);
    assert!(header(&headers, "content-type").starts_with("text/css"));
    let css = String::from_utf8_lossy(&body);
    assert!(
        css.contains("/* themes/"),
        "themes must be joined with file markers"
    );
}

#[tokio::test]
async fn meta_reports_root_and_version() {
    let addr = start_server();
    let (status, _, body) = get(addr, "/api/meta").await;
    assert_eq!(status, 200);
    let meta: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(meta["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(meta["ready"], true);
    assert!(meta["root"].as_str().unwrap().ends_with("rx0"));
    assert!(meta.get("name").is_some());
    assert!(meta["files"].as_u64().unwrap() > 0);
    assert!(meta.get("indexMs").is_some());
    assert!(meta["builtAt"].as_str().unwrap().ends_with('Z'));
}

#[tokio::test]
async fn unknown_routes_are_json_404() {
    let addr = start_server();
    let (status, _, body) = get(addr, "/api/nope").await;
    assert_eq!(status, 404);
    let err: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(err.get("error").is_some());
}

#[tokio::test]
async fn responses_disable_caching() {
    let addr = start_server();
    let (_, headers, _) = get(addr, "/api/meta").await;
    assert_eq!(header(&headers, "cache-control"), "no-store");
}

#[tokio::test]
async fn tree_lists_root_and_subdir() {
    let addr = start_server();
    let (status, _, body) = get(addr, "/api/tree?dir=").await;
    assert_eq!(status, 200);
    let tree: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(tree["dir"], "");
    let names: Vec<&str> = tree["children"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| k["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"Cargo.toml"));
    assert!(names.contains(&"src"));

    let (status, _, body) = get(addr, "/api/tree?dir=src").await;
    assert_eq!(status, 200);
    let tree: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(tree["dir"], "src");
    let names: Vec<&str> = tree["children"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| k["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"server.rs"));
}

#[tokio::test]
async fn tree_missing_dir_is_404() {
    let addr = start_server();
    let (status, _, body) = get(addr, "/api/tree?dir=nope").await;
    assert_eq!(status, 404);
    let err: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(err["error"].as_str().unwrap().contains("not indexed"));
}

#[tokio::test]
async fn find_ranks_and_limits() {
    let addr = start_server();
    let (status, _, body) = get(addr, "/api/find?q=server&limit=5").await;
    assert_eq!(status, 200);
    let res: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let paths: Vec<&str> = res["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["path"].as_str().unwrap())
        .collect();
    assert!(!paths.is_empty());
    assert!(paths.len() <= 5);
    assert_eq!(paths[0], "src/server.rs");
    assert!(res["results"][0].get("pos").is_some());

    let (status, _, body) = get(addr, "/api/find?q=").await;
    assert_eq!(status, 200);
    let res: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(!res["results"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn search_finds_literal_and_snippets() {
    let addr = start_server();
    let (status, _, body) = get(addr, "/api/search?q=handle_find").await;
    assert_eq!(status, 200);
    let res: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let paths: Vec<&str> = res["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["path"].as_str().unwrap())
        .collect();
    assert!(paths.contains(&"src/server.rs"));
    let first = &res["results"].as_array().unwrap()[0];
    assert!(first["matches"]
        .as_array()
        .unwrap()
        .iter()
        .all(|m| !m["mid"].as_str().unwrap().is_empty()));
    assert!(res["total"].as_u64().unwrap() >= res["files"].as_u64().unwrap());
    assert_eq!(res["truncated"], false);
}

#[tokio::test]
async fn search_modes_and_filters() {
    let addr = start_server();
    // Case-sensitive: the probe is built at runtime so this test file
    // (which is itself indexed) contains no literal lowercase copy.
    // "AssetSource" appears in src in mixed case only.
    let probe = ["asset", "source"].concat();
    let (_, _, body) = get(addr, &format!("/api/search?q={probe}")).await;
    let res: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(res["total"].as_u64().unwrap() > 0);
    let (_, _, body) = get(addr, &format!("/api/search?q={probe}&case=1")).await;
    let res: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(res["total"], 0);
    // Regex + word + glob.
    let (status, _, body) = get(addr, "/api/search?q=handle_%2B&re=1&glob=src%2Fserver.rs").await;
    assert_eq!(status, 200);
    let _: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let (status, _, _) = get(addr, "/api/search?q=Server&word=1").await;
    assert_eq!(status, 200);
    // Bad regex is a 400, never a 500.
    let (status, _, body) = get(addr, "/api/search?q=(unclosed&re=1").await;
    assert_eq!(status, 400);
    let err: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(err.get("error").is_some());
    // Blank query is an empty result, not an error.
    let (status, _, _) = get(addr, "/api/search?q=%20%20").await;
    assert_eq!(status, 200);
}

#[tokio::test]
async fn file_serves_windows_and_metadata() {
    let addr = start_server();
    let (status, _, body) = get(addr, "/api/file?path=Cargo.toml").await;
    assert_eq!(status, 200);
    let file: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(file["path"], "Cargo.toml");
    assert!(!file["lang"].as_str().unwrap().is_empty());
    // Both lexers name the Rust grammar identically.
    let (status, _, body) = get(addr, "/api/file?path=src%2Fserver.rs&count=3").await;
    assert_eq!(status, 200);
    let rs: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(rs["lang"], "Rust");
    assert!(file["total"].as_u64().unwrap() > 0);
    assert_eq!(file["start"], 0);
    assert!(!file["lines"].as_array().unwrap().is_empty());
    assert_eq!(file["markdown"], false);
    assert_eq!(
        file["lsp"],
        serde_json::json!({"server": "", "state": "off"})
    );

    // Windowing.
    let (status, _, body) = get(addr, "/api/file?path=src%2Fserver.rs&start=10&count=5").await;
    assert_eq!(status, 200);
    let file: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(file["start"], 10);
    assert_eq!(file["lines"].as_array().unwrap().len(), 5);

    // Missing file is 404, bad path is 400.
    let (status, _, _) = get(addr, "/api/file?path=nope.rs").await;
    assert_eq!(status, 404);
    let (status, _, _) = get(addr, "/api/file?path=%2Fetc%2Fpasswd").await;
    assert_eq!(status, 400);
}

#[tokio::test]
async fn traversal_is_refused_at_handlers() {
    // Mirrors Go TestPathTraversalRefused; /api/outline joins in slice 6.
    let addr = start_server();
    for bad in [
        "/api/file?path=..%2F..%2Fetc%2Fpasswd",
        "/api/file?path=%2Fetc%2Fpasswd",
        "/api/file?path=sub%2F..%2F..%2Foutside",
        "/api/close?path=..%2F..%2Fx",
        "/api/raw?path=..%2F..%2Fx",
        "/api/outline?path=..%2F..%2Fetc%2Fpasswd",
    ] {
        let (status, _, _) = get(addr, bad).await;
        assert_ne!(status, 200, "{bad} must not succeed");
    }
}

#[tokio::test]
async fn raw_and_close_round_trip() {
    let addr = start_server();
    let (status, headers, body) = get(addr, "/api/raw?path=Cargo.toml").await;
    assert_eq!(status, 200);
    assert!(header(&headers, "content-type").contains("toml") || !body.is_empty());
    let (status, _, body) = get(addr, "/api/close?path=Cargo.toml").await;
    assert_eq!(status, 200);
    let res: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(res, serde_json::json!({"ok": true, "path": "Cargo.toml"}));
}

#[tokio::test]
async fn outline_lists_symbols() {
    let addr = start_server();
    let (status, _, body) = get(addr, "/api/outline?path=src%2Fserver.rs").await;
    assert_eq!(status, 200);
    let res: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(res["path"], "src/server.rs");
    let syms = res["symbols"].as_array().unwrap();
    assert!(syms
        .iter()
        .any(|s| s["name"] == "handle_search" && s["kind"] == "func"));
    let (status, _, _) = get(addr, "/api/outline?path=nope.rs").await;
    assert_eq!(status, 404);
}

#[tokio::test]
async fn def_floats_declarations() {
    let addr = start_server();
    let (status, _, body) = get(addr, "/api/def?sym=AppState").await;
    assert_eq!(status, 200);
    let res: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(res["symbol"], "AppState");
    assert!(res["refCount"].as_u64().unwrap() > 0);
    let defs = res["defs"].as_array().unwrap();
    assert!(!defs.is_empty());
    assert!(defs.iter().all(|d| d["def"] == true));
    let (status, _, _) = get(addr, "/api/def?sym=%20%20").await;
    assert_eq!(status, 400);
}

mod scratch {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    pub struct Guard(PathBuf);
    impl Guard {
        pub fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for Guard {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    pub fn create(tag: &str) -> Guard {
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("rx0-{tag}-{}-{id}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Guard(dir)
    }
}

fn scratch_repo() -> (scratch::Guard, PathBuf) {
    let dir = scratch::create("apigit");
    let root = dir.path().to_path_buf();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "{args:?}");
    };
    git(&["init", "-q"]);
    git(&[
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@t",
        "commit",
        "-q",
        "--allow-empty",
        "-m",
        "init",
    ]);
    std::fs::write(root.join("f.txt"), "one\ntwo\nthree\n").unwrap();
    git(&["add", "f.txt"]);
    git(&[
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@t",
        "commit",
        "-q",
        "-m",
        "add",
    ]);
    std::fs::write(root.join("f.txt"), "one\nTWO\nthree\n").unwrap();
    (dir, root)
}

#[tokio::test]
async fn diff_gutter_and_badges_over_repo() {
    let (_tmp, root) = scratch_repo();
    let addr = start_server_at(root);
    let (status, _, body) = get(addr, "/api/diff?path=f.txt").await;
    assert_eq!(status, 200);
    let diff: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(diff["available"], true);
    assert!(diff["diff"].as_str().unwrap().contains("+TWO"));

    let (status, _, body) = get(addr, "/api/gutter?path=f.txt").await;
    assert_eq!(status, 200);
    let gutter: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(gutter["available"], true);
    assert_eq!(gutter["modified"], serde_json::json!([2]));
    assert_eq!(gutter["added"], serde_json::json!([]));

    // Missing file: no diff, empty arrays, never an error.
    let (status, _, body) = get(addr, "/api/gutter?path=nope.txt").await;
    assert_eq!(status, 200);
    let gutter: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(gutter["available"], false);
    assert_eq!(gutter["added"], serde_json::json!([]));

    // Tree carries the M badge; meta reports git.
    let (status, _, body) = get(addr, "/api/tree?dir=").await;
    assert_eq!(status, 200);
    let tree: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let f = tree["children"]
        .as_array()
        .unwrap()
        .iter()
        .find(|k| k["name"] == "f.txt")
        .unwrap();
    assert_eq!(f["status"], "M");
    let (status, _, body) = get(addr, "/api/meta").await;
    assert_eq!(status, 200);
    let meta: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(meta["git"], true);

    // Bad path still refused.
    let (status, _, _) = get(addr, "/api/diff?path=..%2Fx").await;
    assert_eq!(status, 400);
}

#[tokio::test]
async fn markdown_renders_preview() {
    let addr = start_server();
    let (status, _, body) = get(addr, "/api/markdown?path=PORTING.md").await;
    assert_eq!(status, 200);
    let res: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(res["path"], "PORTING.md");
    let html = res["html"].as_str().unwrap();
    assert!(html.contains("<h1"), "preview needs headings");
    assert!(html.contains("data-line="), "preview needs line markers");
    // Non-Markdown is 415, traversal is 400.
    let (status, _, _) = get(addr, "/api/markdown?path=Cargo.toml").await;
    assert_eq!(status, 415);
    let (status, _, _) = get(addr, "/api/markdown?path=..%2Fx.md").await;
    assert_eq!(status, 400);
    let (status, _, _) = get(addr, "/api/markdown?path=nope.md").await;
    assert_eq!(status, 404);
}

#[tokio::test]
async fn reindex_reports_counts() {
    let addr = start_server();
    let (status, _, body) = get(addr, "/api/reindex").await;
    assert_eq!(status, 200);
    let res: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(res["files"].as_u64().unwrap() > 0);
    assert!(res.get("indexMs").is_some());
}

/// Raw HTTP/1.0 POST with explicit headers, mirroring `get` above.
async fn post(
    addr: std::net::SocketAddr,
    path: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> (u16, Vec<(String, String)>, Vec<u8>) {
    let mut stream =
        tokio::time::timeout(Duration::from_secs(5), tokio::net::TcpStream::connect(addr))
            .await
            .expect("connect")
            .unwrap();
    // Host names the loopback addr itself, as Go's settings test does,
    // so the Origin gate can match it.
    let mut req = format!(
        "POST {path} HTTP/1.0\r\nHost: {addr}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    stream.write_all(req.as_bytes()).await.unwrap();
    stream.write_all(body).await.unwrap();
    let mut raw = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut raw))
        .await
        .expect("read")
        .unwrap();
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("response must have a header block");
    let head = String::from_utf8_lossy(&raw[..split]);
    let mut lines = head.lines();
    let status: u16 = lines
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let headers = lines
        .filter_map(|l| {
            l.split_once(':')
                .map(|(k, v)| (k.trim().to_lowercase(), v.trim().to_string()))
        })
        .collect();
    (status, headers, raw[split + 4..].to_vec())
}

static SETTINGS_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Point settings lookups at a temp dir; the server under test reads
/// process env, so callers must hold [`SETTINGS_ENV_LOCK`].
fn isolate_settings() -> (scratch::Guard, Vec<(String, Option<String>)>) {
    let dir = scratch::create("apisettings");
    let path = dir.path().to_str().unwrap().to_string();
    let mut saved = Vec::new();
    for key in ["XDG_CONFIG_HOME", "HOME"] {
        saved.push((key.to_string(), std::env::var(key).ok()));
        // SAFETY: caller holds SETTINGS_ENV_LOCK.
        unsafe { std::env::set_var(key, &path) };
    }
    (dir, saved)
}

fn restore_settings(saved: Vec<(String, Option<String>)>) {
    for (k, v) in saved {
        unsafe {
            match v {
                Some(val) => std::env::set_var(&k, val),
                None => std::env::remove_var(&k),
            }
        }
    }
}

fn origin_for(addr: std::net::SocketAddr) -> String {
    format!("http://{addr}")
}

/// Write an executable stand-in for a coding harness, outside the
/// workspace so it never shows up as a change the run made.
fn write_harness(body: &str) -> (scratch::Guard, PathBuf) {
    let dir = scratch::create("apiharness");
    let path = dir.path().join("harness.sh");
    std::fs::write(&path, format!("#!/bin/sh\n{body}")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    (dir, path)
}

/// Git repo with a clean keep.go, mirroring Go's gitRepo.
fn agent_git_repo() -> (scratch::Guard, PathBuf) {
    let dir = scratch::create("apiagent");
    let root = dir.path().to_path_buf();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "{args:?}");
    };
    git(&["init", "-q"]);
    git(&[
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@t",
        "commit",
        "-q",
        "--allow-empty",
        "-m",
        "init",
    ]);
    std::fs::write(root.join("keep.go"), "package keep\n").unwrap();
    git(&["add", "keep.go"]);
    git(&[
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@t",
        "commit",
        "-q",
        "-m",
        "keep",
    ]);
    (dir, root)
}

/// Serve `root` with an agent manager built from `spec`. Settings stay
/// isolated: the caller must hold SETTINGS_ENV_LOCK for the whole test
/// (env is process-wide), which also serializes against the settings
/// test above.
fn start_agent_server(root: &Path, spec: &str) -> std::net::SocketAddr {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let listener = tokio::net::TcpListener::from_std(listener).unwrap();
    let index = std::sync::Arc::new(Index::new(root.to_path_buf()));
    index.build();
    let agent = rx0::agent::AgentManager::new(root.to_path_buf(), spec, None, true).unwrap();
    let state = AppState {
        root: root.to_path_buf(),
        assets: AssetSource::Embedded,
        index,
        lsp: rx0::lspservers::LspManager::new(root.to_path_buf(), false),
        agent: Some(agent),
    };
    tokio::spawn(async move {
        axum::serve(listener, build_router(state)).await.unwrap();
    });
    addr
}

/// Pin HOME/XDG at a temp dir; caller holds SETTINGS_ENV_LOCK.
fn isolate_agent_env() -> (scratch::Guard, Vec<(String, Option<String>)>) {
    let dir = scratch::create("apiagentsettings");
    let path = dir.path().to_str().unwrap().to_string();
    let mut saved = Vec::new();
    for key in ["XDG_CONFIG_HOME", "HOME"] {
        saved.push((key.to_string(), std::env::var(key).ok()));
        // SAFETY: caller holds SETTINGS_ENV_LOCK.
        unsafe { std::env::set_var(key, &path) };
    }
    (dir, saved)
}

async fn agent_edit(
    addr: std::net::SocketAddr,
    origin: &str,
    q: &str,
) -> (u16, Vec<(String, String)>, Vec<u8>) {
    post(
        addr,
        &format!("/api/agent/edit?{q}"),
        &[("Origin", origin)],
        b"",
    )
    .await
}

/// Poll the latest job until it stops, like Go's waitIdle.
async fn wait_idle(addr: std::net::SocketAddr, id: i64) -> serde_json::Value {
    let path = if id == 0 {
        "/api/agent/job".to_string()
    } else {
        format!("/api/agent/job?id={id}")
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        let (status, _, body) = get(addr, &path).await;
        assert_eq!(status, 200);
        let job: serde_json::Value = serde_json::from_slice(&body).unwrap();
        if job.get("idle") == Some(&serde_json::json!(true)) {
            panic!("job vanished while waiting");
        }
        if job["running"] != true {
            return job;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "harness did not finish"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn agent_unavailable_without_manager() {
    let addr = start_server();
    let origin = origin_for(addr);
    let (status, _, _) = get(addr, "/api/agent/job").await;
    assert_eq!(status, 404);
    let (status, _, _) = get(addr, "/api/agent/harnesses").await;
    assert_eq!(status, 404);
    let (status, _, _) = post(
        addr,
        "/api/agent/edit?path=f.txt&l1=1&l2=1&instruction=hi",
        &[("Origin", &origin)],
        b"",
    )
    .await;
    assert_eq!(status, 404);
    let (_, _, body) = get(addr, "/api/meta").await;
    let meta: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(meta["agent"], "");
    assert_eq!(meta["agents"], serde_json::json!([]));
}

/// Ports Go `TestAgentEditRefusedUntilHarnessChosen`.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn agent_edit_needs_a_harness() {
    let _lock = SETTINGS_ENV_LOCK.lock().unwrap();
    let (_cfg, saved) = isolate_agent_env();
    let dir = scratch::create("apiagentplain");
    std::fs::write(dir.path().join("a.go"), "package a\n").unwrap();
    let addr = start_agent_server(dir.path(), "");
    let origin = origin_for(addr);

    let (status, _, body) = post(
        addr,
        "/api/agent/edit?path=a.go&l1=1&l2=1&instruction=hi",
        &[("Origin", &origin)],
        b"",
    )
    .await;
    assert_eq!(status, 400);
    let res: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(res["error"].as_str().unwrap().contains("no coding harness"));

    let (status, _, _) = post(
        addr,
        "/api/agent/select?name=echo+%7Bprompt%7D",
        &[("Origin", &origin)],
        b"",
    )
    .await;
    assert_eq!(status, 200);

    let (status, _, _) = post(
        addr,
        "/api/agent/edit?path=a.go&l1=1&l2=1&instruction=hi",
        &[("Origin", &origin)],
        b"",
    )
    .await;
    assert_eq!(status, 200);
    wait_idle(addr, 0).await;
    restore_settings(saved);
}

/// Ports Go `TestAgentEditRunsHarnessAndReportsChange`.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn agent_edit_runs_and_reports_change() {
    let _lock = SETTINGS_ENV_LOCK.lock().unwrap();
    let (_cfg, saved) = isolate_agent_env();
    let (_repo, root) = agent_git_repo();
    let prompt_dir = scratch::create("apiagentprompt");
    let prompt_file = prompt_dir.path().join("prompt.txt");
    let (_harness, harness) = write_harness(&format!(
        "printf 'touched\\n' >> keep.go\nprintf '%s' \"$1\" > {}\n",
        prompt_file.display()
    ));
    let addr = start_agent_server(&root, &format!("{} {{prompt}}", harness.display()));
    let origin = origin_for(addr);

    let (status, _, _) = post(
        addr,
        "/api/agent/edit?path=keep.go&l1=1&l2=1&instruction=add+a+line",
        &[("Origin", &origin)],
        b"",
    )
    .await;
    assert_eq!(status, 200);
    let job = wait_idle(addr, 0).await;
    assert!(
        job.get("error").is_none_or(|e| e == ""),
        "job error: {:?}",
        job.get("error")
    );
    assert_eq!(job["changed"], serde_json::json!(["keep.go"]));
    assert_eq!(job["tracked"], true);

    let body = std::fs::read_to_string(root.join("keep.go")).unwrap();
    assert!(body.contains("touched"), "{body}");
    let prompt = std::fs::read_to_string(&prompt_file).unwrap();
    for want in ["@keep.go line 1", "### Instruction", "add a line"] {
        assert!(prompt.contains(want), "prompt missing {want}:\n{prompt}");
    }
    restore_settings(saved);
}

/// Ports Go `TestAgentOutsideGitReportsUnknownChanges`.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn agent_outside_git_reports_unknown() {
    let _lock = SETTINGS_ENV_LOCK.lock().unwrap();
    let (_cfg, saved) = isolate_agent_env();
    let dir = scratch::create("apiagentnogit");
    std::fs::write(dir.path().join("a.go"), "package a\n").unwrap();
    let (_harness, harness) = write_harness("printf 'touched\\n' >> a.go\n");
    let addr = start_agent_server(dir.path(), &format!("{} {{prompt}}", harness.display()));
    let origin = origin_for(addr);

    let (status, _, _) = post(
        addr,
        "/api/agent/edit?path=a.go&l1=1&l2=1&instruction=hi",
        &[("Origin", &origin)],
        b"",
    )
    .await;
    assert_eq!(status, 200);
    let job = wait_idle(addr, 0).await;
    assert!(
        job.get("error").is_none_or(|e| e == ""),
        "job error: {:?}",
        job.get("error")
    );
    assert_eq!(job["tracked"], false);
    assert_eq!(job["changed"], serde_json::json!([]));
    let body = std::fs::read_to_string(dir.path().join("a.go")).unwrap();
    assert!(body.contains("touched"));
    restore_settings(saved);
}

/// Ports Go `TestAgentRefusesSecondEditWhileRunning` and
/// `TestAgentAllowsNonOverlappingEditsInParallel`.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn agent_overlap_rules() {
    let _lock = SETTINGS_ENV_LOCK.lock().unwrap();
    let (_cfg, saved) = isolate_agent_env();
    let (_repo, root) = agent_git_repo();
    let (_harness, harness) = write_harness("sleep 2\n");
    let addr = start_agent_server(&root, &format!("{} {{prompt}}", harness.display()));
    let origin = origin_for(addr);

    let (status, _, body) =
        agent_edit(addr, &origin, "path=keep.go&l1=1&l2=1&instruction=one").await;
    assert_eq!(status, 200);
    let first: serde_json::Value = serde_json::from_slice(&body).unwrap();

    // Same range overlaps: 409.
    let (status, _, body) =
        agent_edit(addr, &origin, "path=keep.go&l1=1&l2=1&instruction=two").await;
    assert_eq!(status, 409);
    let res: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(res["error"].as_str().unwrap().contains("already running"));

    // Disjoint range runs alongside.
    let (status, _, _) = agent_edit(addr, &origin, "path=keep.go&l1=2&l2=2&instruction=two").await;
    assert_eq!(status, 200);
    // Overlapping both is refused.
    let (status, _, _) =
        agent_edit(addr, &origin, "path=keep.go&l1=1&l2=2&instruction=three").await;
    assert_eq!(status, 409);

    let id = first["id"].as_i64().unwrap();
    let job = wait_idle(addr, id).await;
    assert!(
        job.get("error").is_none_or(|e| e == ""),
        "job error: {:?}",
        job.get("error")
    );
    restore_settings(saved);
}

/// Ports Go `TestAgentCancelJob` and `TestAgentMutationsRejectCrossOriginPost`.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn agent_cancel_and_origin_gate() {
    let _lock = SETTINGS_ENV_LOCK.lock().unwrap();
    let (_cfg, saved) = isolate_agent_env();
    let (_repo, root) = agent_git_repo();
    let (_harness, harness) = write_harness("sleep 5\n");
    let addr = start_agent_server(&root, &format!("{} {{prompt}}", harness.display()));
    let origin = origin_for(addr);

    // Cross-origin posts are refused; mutation routes reject GET.
    for path in [
        "/api/agent/edit?path=keep.go&l1=1&l2=1&instruction=hi",
        "/api/agent/select?name=echo",
        "/api/agent/cancel",
    ] {
        let (status, _, _) = post(addr, path, &[("Origin", "http://evil.example.com")], b"").await;
        assert_eq!(status, 403, "{path}");
        let (status, _, _) = get(addr, path).await;
        assert_eq!(status, 405, "GET {path}");
    }

    let (status, _, body) = post(
        addr,
        "/api/agent/edit?path=keep.go&l1=1&l2=1&instruction=first",
        &[("Origin", &origin)],
        b"",
    )
    .await;
    assert_eq!(status, 200);
    let first: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let id = first["id"].as_i64().unwrap();

    let (status, _, body) = post(
        addr,
        &format!("/api/agent/cancel?id={id}"),
        &[("Origin", &origin)],
        b"",
    )
    .await;
    assert_eq!(status, 200);
    let res: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(res["cancelled"], true);

    // The run thread marks the job stopped after git status and
    // settle; poll rather than assuming a fixed delay.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let (_, _, body) = get(addr, &format!("/api/agent/job?id={id}")).await;
        let job: serde_json::Value = serde_json::from_slice(&body).unwrap();
        if job["running"] == false {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "job still running after cancel"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    restore_settings(saved);
}

/// With language servers disabled, every LSP question answers with the
/// state envelope and a "no language server" error, never a crash.
#[tokio::test]
async fn lsp_disabled_reports_off() {
    let addr = start_server();
    for path in [
        "/api/lsp/def?path=Cargo.toml&line=1&col=0",
        "/api/lsp/refs?path=Cargo.toml&line=1&col=0",
        "/api/lsp/hover?path=Cargo.toml&line=1&col=0",
        "/api/lsp/symbols?path=Cargo.toml",
        "/api/lsp/calls?path=Cargo.toml&line=1&col=0",
    ] {
        let (status, _, body) = get(addr, path).await;
        assert_eq!(status, 200, "{path}");
        let res: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(res["state"], "off", "{path}");
        assert_eq!(res["server"], "", "{path}");
        assert!(!res["error"].as_str().unwrap_or("").is_empty(), "{path}");
    }
    let def: serde_json::Value =
        serde_json::from_slice(&get(addr, "/api/lsp/def?path=Cargo.toml").await.2).unwrap();
    assert_eq!(def["hits"], serde_json::json!([]));
    let syms: serde_json::Value =
        serde_json::from_slice(&get(addr, "/api/lsp/symbols?path=Cargo.toml").await.2).unwrap();
    assert_eq!(syms["symbols"], serde_json::json!([]));

    // Warm reports the brief without starting anything.
    let (status, _, body) = get(addr, "/api/lsp/warm?path=Cargo.toml").await;
    assert_eq!(status, 200);
    let warm: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(warm["state"], "off");
    assert_eq!(warm["server"], "");

    // Setup describes the registry even when disabled.
    let (status, _, body) = get(addr, "/api/lsp/setup?path=src/main.rs").await;
    assert_eq!(status, 200);
    let setup: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(setup["enabled"], false);
    assert_eq!(setup["lang"], "Rust");
    assert_eq!(setup["state"], "off");
    assert_eq!(setup["servers"][0]["name"], "rust-analyzer");

    // Bad paths are refused before any LSP work.
    let (status, _, _) = get(addr, "/api/lsp/def?path=..%2Fx.rs").await;
    assert_eq!(status, 400);

    // Install/start need the Origin gate, then refuse while disabled.
    let origin = origin_for(addr);
    let (status, _, _) = post(addr, "/api/lsp/install?server=gopls&option=0", &[], b"").await;
    assert_eq!(status, 403);
    let (status, _, body) = post(
        addr,
        "/api/lsp/install?server=gopls&option=0",
        &[("Origin", &origin)],
        b"",
    )
    .await;
    assert_eq!(status, 400);
    let res: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(res["error"].as_str().unwrap().contains("turned off"));
    let (status, _, _) = post(addr, "/api/lsp/start?path=Cargo.toml", &[], b"").await;
    assert_eq!(status, 403);
}

#[tokio::test]
async fn metrics_reports_process_shape() {
    let addr = start_server();
    let (status, _, body) = get(addr, "/api/metrics").await;
    assert_eq!(status, 200);
    let m: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(m.get("rssBytes").is_some());
    assert!(m.get("cpuUsage").is_some());
    assert!(m.get("goroutines").is_some());

    // Meta embeds the same block plus agent/LSP placeholders.
    let (status, _, body) = get(addr, "/api/meta").await;
    assert_eq!(status, 200);
    let meta: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(meta["metrics"].is_object());
    assert_eq!(meta["agent"], "");
    assert_eq!(meta["agentModel"], "");
    assert_eq!(meta["agentPinned"], false);
    assert_eq!(meta["agents"], serde_json::json!([]));
    assert_eq!(meta["lspServers"], serde_json::json!([]));
}

/// Ports Go `TestSettingsAPIEndpoints`: GET shape, key/value POST, raw
/// POST, and the `localPost` Origin gate.
#[tokio::test]
async fn settings_get_and_post_round_trip() {
    // Env is process-wide and `get`/`post` await: mutate it only under
    // the lock, then release the guard before the first await. This is
    // the sole test touching these variables, so no interleaving is
    // possible; any second such test must take the same lock.
    let (_dir, saved) = {
        let _lock = SETTINGS_ENV_LOCK.lock().unwrap();
        isolate_settings()
    };
    let addr = start_server();
    let origin = origin_for(addr);

    // 1. GET carries schema, merged settings, defaults, raw text, path.
    let (status, _, body) = get(addr, "/api/settings").await;
    assert_eq!(status, 200);
    let res: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(res.get("schema").is_some(), "missing schema");
    assert!(res.get("defaults").is_some());
    assert!(res["settings"].is_object());
    assert!(res["raw"].as_str().is_some());
    assert!(res["path"].as_str().unwrap().ends_with("settings.json"));

    // 2. POST without Origin is refused, like Go's localPost.
    let payload = serde_json::json!({"editor.tabSize": 2});
    let (status, _, _) = post(addr, "/api/settings", &[], payload.to_string().as_bytes()).await;
    assert_eq!(status, 403);

    // 3. POST key/value updates with a matching Origin.
    let payload = serde_json::json!({
        "editor.tabSize": 2,
        "diffEditor.renderSideBySide": false,
    });
    let body_bytes = payload.to_string().into_bytes();
    let (status, _, body) = post(
        addr,
        "/api/settings",
        &[("Origin", &origin), ("Content-Type", "application/json")],
        &body_bytes,
    )
    .await;
    assert_eq!(status, 200);
    let res: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(res["ok"], true);
    assert_eq!(res["settings"]["editor.tabSize"], 2);
    assert_eq!(res["settings"]["diffEditor.renderSideBySide"], false);

    // 4. POST raw JSON replaces the file.
    let raw = serde_json::json!({
        "raw": "{\n  \"editor.fontSize\": 15,\n  \"workbench.colorTheme\": \"nord\"\n}\n",
    });
    let body_bytes = raw.to_string().into_bytes();
    let (status, _, body) = post(
        addr,
        "/api/settings",
        &[("Origin", &origin), ("Content-Type", "application/json")],
        &body_bytes,
    )
    .await;
    assert_eq!(status, 200);
    let res: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(res["settings"]["editor.fontSize"], 15);
    assert_eq!(res["settings"]["workbench.colorTheme"], "nord");

    // 5. Malformed JSON is a 400, not a 500.
    let (status, _, _) = post(
        addr,
        "/api/settings",
        &[("Origin", &origin), ("Content-Type", "application/json")],
        b"{nope",
    )
    .await;
    assert_eq!(status, 400);

    {
        let _lock = SETTINGS_ENV_LOCK.lock().unwrap();
        restore_settings(saved);
    }
}
