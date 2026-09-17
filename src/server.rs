//! HTTP server shell: router, static assets, path sandbox, `/api/meta`.
//!
//! Ports `server.go` (`NewServer`, `ServeHTTP`, `safePath`, `resolvePath`,
//! `handleIndex`, `handleThemes`, `handleMeta`, `handleTree`,
//! `handleReindex`, `handleFind`). Search, file/highlight, outline, git,
//! markdown, settings, LSP, and agent handlers land in later slices.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use axum::{
    extract::{Path as AxumPath, Query, State},
    http::{header, HeaderValue, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use rust_embed::RustEmbed;
use serde::Deserialize;
use serde_json::json;
use tower_http::{compression::CompressionLayer, set_header::SetResponseHeaderLayer};

use crate::agent::{AgentError, AgentManager};
use crate::fuzzy::{fuzzy_find, FuzzyResult};
use crate::git::{git_available, git_diff, git_hunks};
use crate::highlight::{evict, is_image_ext, is_markdown, open as hl_open, HL_CHUNK};
use crate::index::{Index, Node};
use crate::lspservers::{lsp_deadline, warm_deadline, LspManager};
use crate::markdown::{render_markdown, MAX_MARKDOWN_BYTES};
use crate::search::{search, FileMatches, Match, SearchOpts};
use crate::symbols::{outline, Symbol};
use crate::terminal::{default_shell, TerminalManager};
use crate::VERSION;

/// Typed response envelopes. Go builds these from `map[string]any` (keys
/// wire-sorted) holding structs (keys in declaration order). `json!` would
/// sort nested struct keys too, so envelopes are structs with fields in
/// exact wire order instead.
#[derive(serde::Serialize)]
struct MetaResponse {
    #[serde(rename = "builtAt")]
    built_at: String,
    files: usize,
    git: bool,
    #[serde(rename = "indexMs")]
    index_ms: u64,
    name: String,
    ready: bool,
    root: String,
    version: &'static str,
    metrics: crate::metrics::ProcessMetrics,
    agent: String,
    #[serde(rename = "agentModel")]
    agent_model: String,
    #[serde(rename = "agentPinned")]
    agent_pinned: bool,
    agents: Vec<serde_json::Value>,
    #[serde(rename = "lspServers")]
    lsp_servers: Vec<String>,
}

#[derive(serde::Serialize)]
struct TreeResponse {
    children: Vec<Node>,
    dir: String,
}

#[derive(serde::Serialize)]
struct FindResponse {
    results: Vec<FuzzyResult>,
}

#[derive(serde::Serialize)]
struct SearchResponse {
    files: usize,
    results: Vec<FileMatches>,
    total: usize,
    truncated: bool,
}

#[derive(serde::Serialize)]
struct ReindexResponse {
    files: usize,
    #[serde(rename = "indexMs")]
    index_ms: u64,
}

#[derive(serde::Serialize)]
struct LspBrief {
    server: String,
    state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    missing: Option<String>,
}

/// The language-server summary sent with a file: its state, and the
/// language that lacks a server when none is installed. Ports Go
/// `lspBrief` (the `/api/def` shape is the same minus `missing`).
fn lsp_brief(lsp: &LspManager, rel: &str) -> LspBrief {
    let (state, server) = lsp.state(rel);
    let missing = lsp.missing_lang(rel);
    LspBrief {
        server,
        state: state.as_str().to_string(),
        missing: if missing.is_empty() {
            None
        } else {
            Some(missing)
        },
    }
}

#[derive(serde::Serialize)]
struct FileResponse {
    #[serde(rename = "diffAvailable")]
    diff_available: bool,
    exact: bool,
    lang: String,
    lines: Vec<String>,
    lsp: LspBrief,
    markdown: bool,
    #[serde(rename = "maxCols")]
    max_cols: usize,
    path: String,
    refine: bool,
    size: u64,
    start: usize,
    total: usize,
}

#[derive(serde::Serialize)]
struct ImageResponse {
    image: bool,
    path: String,
    size: u64,
}

#[derive(serde::Serialize)]
struct CloseResponse {
    ok: bool,
    path: String,
}

#[derive(serde::Serialize)]
struct MarkdownResponse {
    html: String,
    path: String,
}

#[derive(serde::Serialize)]
struct DiffResponse {
    available: bool,
    diff: String,
    path: String,
}

#[derive(serde::Serialize)]
struct GutterResponse {
    added: Vec<i64>,
    available: bool,
    deleted: Vec<i64>,
    modified: Vec<i64>,
    path: String,
}

#[derive(serde::Serialize)]
struct OutlineResponse {
    path: String,
    symbols: Vec<Symbol>,
}

#[derive(serde::Serialize)]
struct DefHit {
    path: String,
    #[serde(flatten)]
    hit: Match,
}

#[derive(serde::Serialize)]
struct DefResponse {
    defs: Vec<DefHit>,
    lsp: LspBrief,
    #[serde(rename = "refCount")]
    ref_count: usize,
    symbol: String,
}

/// Embedded `web/` directory, mirroring `//go:embed web` in `server.go`.
#[derive(RustEmbed)]
#[folder = "web/"]
struct EmbeddedAssets;

/// Where static assets come from: the binary (`rx0` default) or disk
/// (`-dev DIR`, mirroring Go `useDiskAssets`).
#[derive(Clone, Debug)]
pub enum AssetSource {
    Embedded,
    Disk(PathBuf),
}

impl AssetSource {
    fn read(&self, name: &str) -> Option<Vec<u8>> {
        match self {
            AssetSource::Embedded => EmbeddedAssets::get(name).map(|f| f.data.to_vec()),
            AssetSource::Disk(dir) => std::fs::read(dir.join("web").join(name)).ok(),
        }
    }

    /// Names of `themes/*.css` in file-name order, mirroring
    /// `fs.Glob(assets, "web/themes/*.css")` in `handleThemes`.
    fn theme_names(&self) -> Vec<String> {
        let mut names: Vec<String> = match self {
            AssetSource::Embedded => EmbeddedAssets::iter()
                .filter(|n| {
                    n.starts_with("themes/")
                        && n.ends_with(".css")
                        && !n["themes/".len()..].contains('/')
                })
                .map(|n| n.to_string())
                .collect(),
            AssetSource::Disk(dir) => {
                let mut out = Vec::new();
                if let Ok(rd) = std::fs::read_dir(dir.join("web").join("themes")) {
                    for entry in rd.flatten() {
                        let p = entry.path();
                        if p.extension().map(|e| e == "css").unwrap_or(false) {
                            if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                                out.push(format!("themes/{name}"));
                            }
                        }
                    }
                }
                out
            }
        };
        names.sort();
        names
    }
}

#[derive(Clone)]
pub struct AppState {
    pub root: PathBuf,
    pub assets: AssetSource,
    pub index: Arc<Index>,
    pub lsp: Arc<LspManager>,
    pub agent: Option<Arc<AgentManager>>,
    pub terminal: Arc<TerminalManager>,
}

/// The bottom-drawer terminal switch. Defaults on; `terminal.enabled`
/// in settings hides the feature.
fn terminal_enabled() -> bool {
    crate::settings::read_merged_map()
        .get("terminal.enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(true)
}

/// Resolve a client-supplied relative path inside `root`, refusing anything
/// that escapes it. Ports Go `safePath`: `""` and `"/"` resolve to the root
/// itself; an absolute input is NOT refused here (it resolves under the
/// root) — absolute-path refusal lives in [`resolve_path`], matching Go
/// where `resolvePath` intercepts absolutes before `safePath` sees them.
pub fn safe_path(root: &Path, rel: &str) -> Option<(PathBuf, String)> {
    let trimmed = rel.trim();
    let stripped = trimmed.strip_prefix('/').unwrap_or(trimmed);
    // Go strips exactly one leading "/" then `filepath.Clean` rejects the
    // result if it is still absolute ("//etc" -> "/etc" -> refuse).
    if stripped.starts_with('/') {
        return None;
    }
    let mut parts: Vec<&str> = Vec::new();
    for comp in stripped.split('/') {
        match comp {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            c => parts.push(c),
        }
    }
    let mut abs = PathBuf::from(root);
    abs.extend(parts.iter());
    if !abs.starts_with(root) {
        return None;
    }
    Some((abs, parts.join("/")))
}

/// `safe_path` plus the one documented exception: an absolute path a
/// language server names as a definition target. Ports Go `resolvePath`.
/// The allowlist is empty until the LSP slice lands, so absolute paths are
/// refused today.
pub fn resolve_path(
    root: &Path,
    allowed_outside: &dyn Fn(&Path) -> bool,
    p: &str,
) -> Option<(PathBuf, String)> {
    if Path::new(p.trim()).is_absolute() {
        let abs = lexical_clean(Path::new(p.trim()));
        if allowed_outside(&abs) {
            let display = abs.to_string_lossy().replace('\\', "/");
            return Some((abs, display));
        }
        return None;
    }
    safe_path(root, p)
}

/// Lexical absolute-path cleanup without touching the filesystem
/// (symlinks unresolved), matching Go `filepath.Clean` for this use.
fn lexical_clean(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in p.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            c => out.push(c.as_os_str()),
        }
    }
    if out.as_os_str().is_empty() {
        out.push("/");
    }
    out
}

fn err(status: StatusCode, msg: impl Into<String>) -> Response {
    let body = Json(json!({ "error": msg.into() }));
    (status, body).into_response()
}

fn mime_for(name: &str) -> &'static str {
    match Path::new(name).extension().and_then(|e| e.to_str()) {
        Some("html" | "htm") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json" | "map") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        Some("txt" | "md") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

async fn handle_index(State(state): State<AppState>) -> Response {
    match state.assets.read("index.html") {
        Some(bytes) => {
            ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], bytes).into_response()
        }
        None => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "index.html missing from assets",
        ),
    }
}

async fn handle_static(
    State(state): State<AppState>,
    AxumPath(path): AxumPath<String>,
) -> Response {
    // The sandbox governs workspace files, not bundled assets; still, never
    // serve dotfiles or escaped paths from the bundle.
    if path
        .split('/')
        .any(|c| c.is_empty() || c == "." || c == "..")
    {
        return err(StatusCode::NOT_FOUND, "not found");
    }
    match state.assets.read(&path) {
        Some(bytes) => ([(header::CONTENT_TYPE, mime_for(&path))], bytes).into_response(),
        None => err(StatusCode::NOT_FOUND, "not found"),
    }
}

async fn handle_themes(State(state): State<AppState>) -> Response {
    let mut css = String::new();
    for name in state.assets.theme_names() {
        let body = state.assets.read(&name).unwrap_or_default();
        css.push_str(&format!("/* {name} */\n"));
        css.push_str(&String::from_utf8_lossy(&body));
        css.push('\n');
    }
    ([(header::CONTENT_TYPE, "text/css; charset=utf-8")], css).into_response()
}

async fn handle_meta(State(state): State<AppState>) -> Response {
    let name = state
        .root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (files, built_at, index_ms) = state.index.stats();
    Json(MetaResponse {
        built_at,
        files,
        git: git_available(&state.root),
        index_ms,
        name,
        ready: state.index.ready(),
        root: state.root.to_string_lossy().into_owned(),
        version: VERSION,
        metrics: crate::metrics::process_metrics(),
        agent: state.agent.as_ref().map(|a| a.name()).unwrap_or_default(),
        agent_model: state.agent.as_ref().map(|a| a.model()).unwrap_or_default(),
        agent_pinned: state.agent.as_ref().map(|a| a.pinned()).unwrap_or(false),
        agents: state
            .agent
            .as_ref()
            .map(|a| {
                a.detect()
                    .into_iter()
                    .map(|h| serde_json::to_value(h).unwrap_or(serde_json::Value::Null))
                    .collect()
            })
            .unwrap_or_default(),
        lsp_servers: state.lsp.available(),
    })
    .into_response()
}

async fn handle_metrics() -> Response {
    Json(crate::metrics::process_metrics()).into_response()
}

async fn handle_settings_get() -> Response {
    Json(json!({
        "settings": crate::settings::read_merged_map(),
        "defaults": crate::settings::default_settings_map(),
        "schema": crate::settings::settings_schema(),
        "raw": crate::settings::read_raw_json(),
        "path": settings_path_string(),
    }))
    .into_response()
}

fn settings_path_string() -> String {
    crate::settings::settings_path()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Admit a mutating request only when it is a POST from rx0's own
/// page. Ports Go `localPost`: Browsers send Origin on every POST, so
/// a page from another site cannot pass. Requiring the Host to be an
/// IP address or localhost also shuts out DNS rebinding.
fn local_post(
    method: &axum::http::Method,
    headers: &axum::http::HeaderMap,
) -> Result<(), (StatusCode, String)> {
    if *method != axum::http::Method::POST {
        return Err((StatusCode::METHOD_NOT_ALLOWED, "POST only".to_string()));
    }
    local_origin(headers)
}

/// The Host/Origin half of [`local_post`], reused by the terminal
/// websocket upgrade (a GET with an `Origin` header, which browsers
/// also send on WS handshakes).
fn local_origin(headers: &axum::http::HeaderMap) -> Result<(), (StatusCode, String)> {
    let host = headers
        .get(axum::http::header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let mut bare = host;
    if let Some((h, _)) = host.rsplit_once(':') {
        // Strip the port unless this is a bare IPv6 literal.
        if !h.ends_with(']') || bare.starts_with('[') {
            bare = h;
        }
    }
    let bare = bare.trim_matches(|c| c == '[' || c == ']');
    let is_loopback =
        bare.eq_ignore_ascii_case("localhost") || bare.parse::<std::net::IpAddr>().is_ok();
    if !is_loopback {
        return Err((
            StatusCode::FORBIDDEN,
            "open rx0 by IP address or localhost to change settings".to_string(),
        ));
    }
    let origin = headers
        .get(axum::http::header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    // `Origin: http://host` must name this same host, as Go compares
    // `o.Host != r.Host` on the parsed URL.
    let origin_host = origin
        .rsplit("://")
        .next()
        .unwrap_or("")
        .trim_end_matches('/');
    if origin_host != host {
        return Err((
            StatusCode::FORBIDDEN,
            "request did not come from rx0".to_string(),
        ));
    }
    Ok(())
}

/// Apply a settings update and persist it. Ports Go `handleSettings`
/// POST: a `raw` string replaces the whole file, anything else merges
/// key-wise (nulls delete); an empty body falls back to query params.
async fn handle_settings_post(
    method: Method,
    headers: axum::http::HeaderMap,
    Query(query): Query<std::collections::HashMap<String, String>>,
    body: axum::body::Bytes,
) -> Response {
    if let Err((status, msg)) = local_post(&method, &headers) {
        return err(status, msg);
    }
    if body.len() > 1 << 20 {
        return err(StatusCode::BAD_REQUEST, "failed to read body");
    }
    let payload: serde_json::Value = if body.is_empty() {
        let mut map = serde_json::Map::new();
        for (k, v) in query {
            map.insert(k, serde_json::Value::String(v));
        }
        serde_json::Value::Object(map)
    } else {
        match serde_json::from_slice(&body) {
            Ok(v) => v,
            Err(e) => return err(StatusCode::BAD_REQUEST, format!("invalid JSON: {e}")),
        }
    };
    let Some(payload) = payload.as_object().cloned() else {
        // Go rejects non-object bodies when unmarshalling into its map.
        return err(StatusCode::BAD_REQUEST, "invalid JSON: expected an object");
    };
    if let Some(raw_str) = payload.get("raw").and_then(|v| v.as_str()) {
        if let Err(e) = crate::settings::save_raw_json(raw_str) {
            return err(
                StatusCode::BAD_REQUEST,
                format!("invalid JSON in settings: {e}"),
            );
        }
    } else if let Err(e) = crate::settings::update_settings_map(&payload) {
        return err(StatusCode::INTERNAL_SERVER_ERROR, e);
    }
    // No coding harness runs before the agent slice, so there is nothing
    // to re-select here (Go re-reads the agent choice at this point).
    Json(json!({
        "settings": crate::settings::read_merged_map(),
        "raw": crate::settings::read_raw_json(),
        "path": settings_path_string(),
        "ok": true,
    }))
    .into_response()
}

#[derive(Deserialize)]
struct TreeQuery {
    #[serde(default)]
    dir: String,
}

async fn handle_tree(State(state): State<AppState>, Query(q): Query<TreeQuery>) -> Response {
    let dir = q.dir.trim_matches('/').to_string();
    let mut kids = state.index.children(&dir);
    if kids.is_none() && !state.index.ready() {
        // Indexing still in flight: wait up to 300ms for the directory.
        for _ in 0..30 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            kids = state.index.children(&dir);
            if kids.is_some() || state.index.ready() {
                kids = state.index.children(&dir);
                break;
            }
        }
    }
    match kids {
        Some(children) => Json(TreeResponse { children, dir }).into_response(),
        None => err(StatusCode::NOT_FOUND, format!("not indexed: {dir}")),
    }
}

#[derive(Deserialize)]
struct FindQuery {
    #[serde(default)]
    q: String,
    #[serde(default)]
    limit: String,
}

async fn handle_find(State(state): State<AppState>, Query(q): Query<FindQuery>) -> Response {
    let mut limit: usize = q.limit.parse().unwrap_or(100);
    if limit == 0 || limit > 500 {
        limit = 100;
    }
    let res = fuzzy_find(&state.index.files(), &q.q, limit);
    Json(FindResponse { results: res }).into_response()
}

#[derive(Deserialize)]
struct SearchQuery {
    #[serde(default)]
    q: String,
    #[serde(default)]
    re: String,
    #[serde(default)]
    case: String,
    #[serde(default)]
    word: String,
    #[serde(default)]
    glob: String,
}

async fn handle_search(State(state): State<AppState>, Query(q): Query<SearchQuery>) -> Response {
    let opts = SearchOpts {
        query: q.q,
        regex: q.re == "1",
        case: q.case == "1",
        word: q.word == "1",
        glob: q.glob,
        max_files: 0,
        max_per_file: 0,
        classify_defs: false,
    };
    let index = state.index.clone();
    let searched = tokio::task::spawn_blocking(move || search(&index, opts)).await;
    let (res, truncated) = match searched {
        Ok(Ok(ok)) => ok,
        Ok(Err(e)) => return err(StatusCode::BAD_REQUEST, e),
        Err(_) => return err(StatusCode::INTERNAL_SERVER_ERROR, "search interrupted"),
    };
    let total: usize = res.iter().map(|f| f.matches.len()).sum();
    Json(SearchResponse {
        files: res.len(),
        results: res,
        total,
        truncated,
    })
    .into_response()
}

#[derive(Deserialize)]
struct FileQuery {
    #[serde(default)]
    path: String,
    #[serde(default)]
    start: String,
    #[serde(default)]
    count: String,
}

async fn handle_file(State(state): State<AppState>, Query(q): Query<FileQuery>) -> Response {
    let (abs, rel) = match resolve_path(&state.root, &|p| state.lsp.allowed(p), &q.path) {
        Some(v) => v,
        None => return err(StatusCode::BAD_REQUEST, "bad path"),
    };
    let size = match std::fs::metadata(&abs) {
        Ok(m) => m.len(),
        Err(e) => return err(StatusCode::NOT_FOUND, e.to_string()),
    };
    let ext = Path::new(&rel)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    if is_image_ext(&format!(".{ext}").to_lowercase()) {
        return Json(ImageResponse {
            image: true,
            path: rel,
            size,
        })
        .into_response();
    }
    let doc = match hl_open(&abs, &rel) {
        Ok(d) => d,
        Err(e) => return err(StatusCode::UNSUPPORTED_MEDIA_TYPE, e),
    };
    let mut start: i64 = q.start.parse().unwrap_or(0);
    let mut count: i64 = q.count.parse().unwrap_or(0);
    if count <= 0 {
        count = HL_CHUNK as i64;
    }
    if start < 0 {
        start = 0;
    }
    if start as usize > doc.total {
        start = doc.total as i64;
    }
    let start = start as usize;
    let (lines, exact) = doc.lines(start, start + count as usize);
    let (_, coming) = doc.exact_state();
    let diff_available = git_available(&state.root) && !git_diff(&state.root, &rel).is_empty();
    Json(FileResponse {
        diff_available,
        exact,
        lang: doc.lang.clone(),
        lines,
        lsp: lsp_brief(&state.lsp, &rel),
        markdown: is_markdown(&rel),
        max_cols: doc.max_cols,
        path: rel,
        refine: !exact && coming,
        size,
        start,
        total: doc.total,
    })
    .into_response()
}

async fn handle_raw(State(state): State<AppState>, Query(q): Query<FileQuery>) -> Response {
    // Go uses safePath here: absolute paths resolve under the root.
    let (abs, rel) = match safe_path(&state.root, &q.path) {
        Some(v) => v,
        None => return err(StatusCode::BAD_REQUEST, "bad path"),
    };
    match std::fs::read(&abs) {
        Ok(bytes) => ([(header::CONTENT_TYPE, mime_for(&rel))], bytes).into_response(),
        Err(_) => err(StatusCode::NOT_FOUND, "not found"),
    }
}

async fn handle_outline(State(state): State<AppState>, Query(q): Query<FileQuery>) -> Response {
    let (abs, rel) = match resolve_path(&state.root, &|p| state.lsp.allowed(p), &q.path) {
        Some(v) => v,
        None => return err(StatusCode::BAD_REQUEST, "bad path"),
    };
    match outline(&abs, &rel) {
        Ok(symbols) => Json(OutlineResponse { path: rel, symbols }).into_response(),
        Err(e) => err(StatusCode::NOT_FOUND, e),
    }
}

async fn handle_def(State(state): State<AppState>, Query(q): Query<DefQuery>) -> Response {
    let sym = q.sym.trim().to_string();
    if sym.is_empty() {
        return err(StatusCode::BAD_REQUEST, "no symbol");
    }
    let opts = SearchOpts {
        query: sym.clone(),
        regex: false,
        case: true,
        word: true,
        glob: String::new(),
        max_files: 400,
        max_per_file: 20,
        classify_defs: true,
    };
    let index = state.index.clone();
    let searched = tokio::task::spawn_blocking(move || search(&index, opts)).await;
    let res = match searched {
        Ok(Ok((res, _))) => res,
        Ok(Err(e)) => return err(StatusCode::BAD_REQUEST, e),
        Err(_) => return err(StatusCode::INTERNAL_SERVER_ERROR, "search interrupted"),
    };
    let mut defs: Vec<DefHit> = Vec::new();
    let mut refs = 0;
    let mut seen = std::collections::HashSet::new();
    for f in &res {
        for m in &f.matches {
            if !m.def {
                refs += 1;
                continue;
            }
            // One entry per declaring line, however often the name repeats.
            if !seen.insert((f.path.clone(), m.line)) {
                continue;
            }
            defs.push(DefHit {
                path: f.path.clone(),
                hit: m.clone(),
            });
        }
    }
    // Prefer declarations in files whose name echoes the symbol (stable).
    let low = sym.to_lowercase();
    let mut named: Vec<DefHit> = Vec::new();
    let mut rest: Vec<DefHit> = Vec::new();
    for d in defs {
        let base = d.path.rsplit('/').next().unwrap_or(&d.path).to_lowercase();
        if base.contains(&low) {
            named.push(d);
        } else {
            rest.push(d);
        }
    }
    named.append(&mut rest);
    // Go reports the LSP state for the raw `path` query here.
    let (lsp_state, lsp_server) = state.lsp.state(&q.path);
    Json(DefResponse {
        defs: named,
        lsp: LspBrief {
            server: lsp_server,
            state: lsp_state.as_str().to_string(),
            missing: None,
        },
        ref_count: refs,
        symbol: sym,
    })
    .into_response()
}

#[derive(Deserialize)]
struct DefQuery {
    #[serde(default)]
    sym: String,
    #[serde(default)]
    path: String,
}

async fn handle_markdown(State(state): State<AppState>, Query(q): Query<FileQuery>) -> Response {
    let (abs, rel) = match resolve_path(&state.root, &|p| state.lsp.allowed(p), &q.path) {
        Some(v) => v,
        None => return err(StatusCode::BAD_REQUEST, "bad path"),
    };
    if !is_markdown(&rel) {
        return err(StatusCode::UNSUPPORTED_MEDIA_TYPE, "not a Markdown file");
    }
    let meta = match std::fs::metadata(&abs) {
        Ok(m) => m,
        Err(e) => return err(StatusCode::NOT_FOUND, e.to_string()),
    };
    if meta.is_dir() {
        return err(StatusCode::UNSUPPORTED_MEDIA_TYPE, "is a directory");
    }
    if meta.len() > MAX_MARKDOWN_BYTES {
        return err(StatusCode::PAYLOAD_TOO_LARGE, "too large to preview");
    }
    let data = match std::fs::read(&abs) {
        Ok(d) => d,
        Err(e) => return err(StatusCode::NOT_FOUND, e.to_string()),
    };
    match render_markdown(&data) {
        Ok(html) => Json(MarkdownResponse { html, path: rel }).into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

async fn handle_diff(State(state): State<AppState>, Query(q): Query<FileQuery>) -> Response {
    let (_, rel) = match resolve_path(&state.root, &|p| state.lsp.allowed(p), &q.path) {
        Some(v) => v,
        None => return err(StatusCode::BAD_REQUEST, "bad path"),
    };
    let diff = git_diff(&state.root, &rel);
    Json(DiffResponse {
        available: !diff.is_empty(),
        diff,
        path: rel,
    })
    .into_response()
}

async fn handle_gutter(State(state): State<AppState>, Query(q): Query<FileQuery>) -> Response {
    let (_, rel) = match resolve_path(&state.root, &|p| state.lsp.allowed(p), &q.path) {
        Some(v) => v,
        None => return err(StatusCode::BAD_REQUEST, "bad path"),
    };
    let hunks = git_hunks(&state.root, &rel);
    let available = hunks.is_some();
    // Marshal as [] never null, mirroring Go `nz`.
    let (added, modified, deleted) = hunks.unwrap_or_default();
    Json(GutterResponse {
        added,
        available,
        deleted,
        modified,
        path: rel,
    })
    .into_response()
}

async fn handle_close(State(state): State<AppState>, Query(q): Query<FileQuery>) -> Response {
    let (abs, rel) = match resolve_path(&state.root, &|p| state.lsp.allowed(p), &q.path) {
        Some(v) => v,
        None => return err(StatusCode::BAD_REQUEST, "bad path"),
    };
    evict(&abs.to_string_lossy());
    state.lsp.close_doc(&abs, &rel);
    Json(CloseResponse {
        ok: true,
        path: rel,
    })
    .into_response()
}

/// The picker's list and friends, or 404 when `--no-agent` left
/// editing out of this session. Ports Go `agentOrFail`.
fn agent_or_fail(state: &AppState) -> Result<Arc<AgentManager>, (StatusCode, String)> {
    match &state.agent {
        Some(a) => Ok(a.clone()),
        None => Err((
            StatusCode::NOT_FOUND,
            "editing is not available in this session".to_string(),
        )),
    }
}

fn agent_overview(agent: &AgentManager) -> serde_json::Value {
    json!({
        "harnesses": agent.detect(),
        "selected": agent.name(),
        "model": agent.model(),
        "pinned": agent.pinned(),
        "settings": crate::settings::settings_path()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default(),
    })
}

/// Backs the picker, re-scanning on every call so a harness installed
/// since startup appears without a restart. Ports Go
/// `handleAgentHarnesses`.
async fn handle_agent_harnesses(State(state): State<AppState>) -> Response {
    let agent = match agent_or_fail(&state) {
        Ok(a) => a,
        Err((status, msg)) => return err(status, msg),
    };
    let agent = agent.clone();
    match blocking_lsp(move || agent_overview(&agent)).await {
        Err(resp) => *resp,
        Ok(v) => Json(v).into_response(),
    }
}

#[derive(Deserialize)]
struct AgentSelectQuery {
    #[serde(default)]
    name: String,
    #[serde(default)]
    model: String,
}

async fn handle_agent_select(
    method: Method,
    headers: axum::http::HeaderMap,
    State(state): State<AppState>,
    Query(q): Query<AgentSelectQuery>,
) -> Response {
    if let Err((status, msg)) = local_post(&method, &headers) {
        return err(status, msg);
    }
    let agent = match agent_or_fail(&state) {
        Ok(a) => a,
        Err((status, msg)) => return err(status, msg),
    };
    let model = if q.model.is_empty() {
        None
    } else {
        Some(q.model.clone())
    };
    match agent.select(&q.name, model.as_deref()) {
        Ok(()) => Json(agent_overview(&agent)).into_response(),
        Err(AgentError::Busy) => err(StatusCode::CONFLICT, AgentError::Busy.to_string()),
        Err(e) => err(StatusCode::BAD_REQUEST, e.to_string()),
    }
}

#[derive(Deserialize)]
struct AgentEditQuery {
    #[serde(default)]
    path: String,
    #[serde(default)]
    l1: i64,
    #[serde(default)]
    l2: i64,
    #[serde(default)]
    instruction: String,
    #[serde(default)]
    force: String,
}

async fn handle_agent_edit(
    method: Method,
    headers: axum::http::HeaderMap,
    State(state): State<AppState>,
    Query(q): Query<AgentEditQuery>,
) -> Response {
    if let Err((status, msg)) = local_post(&method, &headers) {
        return err(status, msg);
    }
    let agent = match agent_or_fail(&state) {
        Ok(a) => a,
        Err((status, msg)) => return err(status, msg),
    };
    // Outside-the-tree files stay refused here: this is the plain
    // resolve, not the allowlisted LSP one.
    let (abs, rel) = match resolve_path(&state.root, &|_| false, &q.path) {
        Some(v) => v,
        None => return err(StatusCode::BAD_REQUEST, "bad path"),
    };
    match agent.start(&abs, &rel, q.l1, q.l2, &q.instruction, q.force == "1") {
        Ok(job) => Json(job).into_response(),
        Err(AgentError::Busy) | Err(AgentError::Dirty) => {
            err(StatusCode::CONFLICT, AgentError::Busy.to_string())
        }
        Err(e) => err(StatusCode::BAD_REQUEST, e.to_string()),
    }
}

/// Polled while an edit runs. id=0 (or missing) means the most recently
/// started job. Ports Go `handleAgentJob`.
#[derive(Deserialize)]
struct AgentJobQuery {
    #[serde(default)]
    id: i64,
}

async fn handle_agent_job(
    State(state): State<AppState>,
    Query(q): Query<AgentJobQuery>,
) -> Response {
    let agent = match agent_or_fail(&state) {
        Ok(a) => a,
        Err((status, msg)) => return err(status, msg),
    };
    match agent.job(q.id) {
        Some(job) => Json(job).into_response(),
        None => Json(json!({ "idle": true })).into_response(),
    }
}

async fn handle_agent_cancel(
    method: Method,
    headers: axum::http::HeaderMap,
    State(state): State<AppState>,
    Query(q): Query<AgentJobQuery>,
    body: axum::body::Bytes,
) -> Response {
    if let Err((status, msg)) = local_post(&method, &headers) {
        return err(status, msg);
    }
    let agent = match agent_or_fail(&state) {
        Ok(a) => a,
        Err((status, msg)) => return err(status, msg),
    };
    let mut id = q.id;
    if id == 0 {
        // The UI posts the id as JSON when it has one.
        if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&body) {
            if let Some(n) = v.get("id").and_then(|n| n.as_i64()) {
                if n != 0 {
                    id = n;
                }
            }
        }
    }
    Json(json!({ "cancelled": agent.cancel_job(id) })).into_response()
}

#[derive(Deserialize)]
struct TerminalQuery {
    #[serde(default)]
    rows: u16,
    #[serde(default)]
    cols: u16,
}

/// Upgrade to the bottom-drawer terminal socket. The `local_origin`
/// gate applies (browsers send `Origin` on WS handshakes too): only
/// rx0's own page, by IP or localhost, may open a shell.
async fn handle_terminal_ws(
    headers: axum::http::HeaderMap,
    State(state): State<AppState>,
    Query(q): Query<TerminalQuery>,
    ws: axum::extract::ws::WebSocketUpgrade,
) -> Response {
    if let Err((status, msg)) = local_origin(&headers) {
        return err(status, msg);
    }
    if !terminal_enabled() {
        return err(StatusCode::FORBIDDEN, "terminal disabled");
    }
    let rows = q.rows.clamp(5, 200);
    let cols = q.cols.clamp(20, 500);
    let term = state.terminal.clone();
    ws.on_upgrade(move |socket| terminal_session(term, socket, rows, cols))
}

/// Decode a `{"resize":[rows,cols]}` frame. Anything else is ignored.
fn parse_resize(t: &str) -> Option<(u16, u16)> {
    let v: serde_json::Value = serde_json::from_str(t).ok()?;
    let pair = v.get("resize")?.as_array()?;
    Some((
        pair.first()?.as_u64()? as u16,
        pair.get(1)?.as_u64()? as u16,
    ))
}

/// Bridge one websocket to the single shell session. Binary frames are
/// stdin; `{"resize":[rows,cols]}` text frames reflow the PTY. Output
/// chunks go back as binary frames. Socket close detaches only: the
/// shell keeps running so reopening the drawer resumes it.
async fn terminal_session(
    term: Arc<TerminalManager>,
    mut socket: axum::extract::ws::WebSocket,
    rows: u16,
    cols: u16,
) {
    if !term.running() {
        let shell = default_shell();
        if let Err(e) = term.start(&shell, &[], rows, cols) {
            let _ = socket
                .send(axum::extract::ws::Message::Text(format!("rx0: {e}").into()))
                .await;
            return;
        }
    } else if term.resize(rows, cols).is_err() {
        // Session vanished between checks; the next attach starts fresh.
    }
    loop {
        tokio::select! {
            msg = socket.recv() => {
                match msg {
                    Some(Ok(axum::extract::ws::Message::Binary(b)))
                        if term.write(&b).is_err() =>
                    {
                        break
                    }
                    Some(Ok(axum::extract::ws::Message::Text(t))) => {
                        if let Some((rows, cols)) = parse_resize(&t) {
                            if rows >= 5 && cols >= 20 {
                                let _ = term.resize(rows.min(200), cols.min(500));
                            }
                        }
                    }
                    Some(Ok(axum::extract::ws::Message::Close(_))) | None => break,
                    _ => {}
                }
            }
            _ = tokio::time::sleep(std::time::Duration::from_millis(10)) => {
                let mut failed = false;
                for chunk in term.drain() {
                    if socket.send(axum::extract::ws::Message::Binary(chunk.into())).await.is_err() {
                        failed = true;
                        break;
                    }
                }
                if failed {
                    break;
                }
            }
        }
    }
}

/// Shared path/line/col arguments. `col` arrives in UTF-16 code units
/// because that is what JavaScript string offsets count. Ports Go
/// `lspPos`.
#[derive(Deserialize)]
struct LspPosQuery {
    #[serde(default)]
    path: String,
    #[serde(default)]
    line: i64,
    #[serde(default)]
    col: i64,
    #[serde(default)]
    wait: i64,
}

fn lsp_pos(
    root: &Path,
    lsp: &LspManager,
    q: &LspPosQuery,
) -> Option<(PathBuf, String, usize, i64)> {
    let (abs, rel) = resolve_path(root, &|p| lsp.allowed(p), &q.path)?;
    Some((abs, rel, q.line.max(1) as usize, q.col.max(0)))
}

/// Answer envelope for def/refs: hits plus the server state, or the
/// error with empty hits. Ports Go `lspRespond`.
fn lsp_respond(
    lsp: &LspManager,
    rel: &str,
    result: Result<Vec<crate::lspnav::NavHit>, crate::lspservers::LspError>,
) -> Response {
    let (state, server) = lsp.state(rel);
    match result {
        Err(e) => Json(json!({
            "hits": Vec::<crate::lspnav::NavHit>::new(),
            "state": state.as_str(),
            "server": server,
            "error": e.to_string(),
        }))
        .into_response(),
        Ok(hits) => Json(json!({
            "hits": hits,
            "state": state.as_str(),
            "server": server,
        }))
        .into_response(),
    }
}

/// Run a blocking LSP operation off the async runtime. A JoinError
/// means the pool itself failed, which is a 500.
async fn blocking_lsp<T: Send + 'static>(
    f: impl FnOnce() -> T + Send + 'static,
) -> Result<T, Box<Response>> {
    match tokio::task::spawn_blocking(f).await {
        Ok(v) => Ok(v),
        // Boxed: `Response` is >=128 bytes and trips
        // `result_large_err` on newer clippy.
        Err(_) => Err(Box::new(err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "lsp task interrupted",
        ))),
    }
}

async fn handle_lsp_def(State(state): State<AppState>, Query(q): Query<LspPosQuery>) -> Response {
    let Some((abs, rel, line, col)) = lsp_pos(&state.root, &state.lsp, &q) else {
        return err(StatusCode::BAD_REQUEST, "bad path");
    };
    let deadline = lsp_deadline(q.wait);
    let lsp = state.lsp.clone();
    let rel_for_call = rel.clone();
    match blocking_lsp(move || lsp.definition(deadline, &abs, &rel_for_call, line, col)).await {
        Err(resp) => *resp,
        Ok(result) => lsp_respond(&state.lsp, &rel, result),
    }
}

async fn handle_lsp_refs(State(state): State<AppState>, Query(q): Query<LspPosQuery>) -> Response {
    let Some((abs, rel, line, col)) = lsp_pos(&state.root, &state.lsp, &q) else {
        return err(StatusCode::BAD_REQUEST, "bad path");
    };
    let deadline = lsp_deadline(q.wait);
    let lsp = state.lsp.clone();
    let rel_for_call = rel.clone();
    match blocking_lsp(move || lsp.references(deadline, &abs, &rel_for_call, line, col)).await {
        Err(resp) => *resp,
        Ok(result) => lsp_respond(&state.lsp, &rel, result),
    }
}

/// Call trails. Without `item` resolves the function at path/line/col
/// into trail roots; with `item` (a node's opaque item, echoed back)
/// expands that node into callers, or callees when `dir=out`. `path`
/// always names the file the trail started in, which picks the server.
/// Ports Go `handleLSPCalls`.
#[derive(Deserialize)]
struct CallsQuery {
    #[serde(default)]
    path: String,
    #[serde(default)]
    line: i64,
    #[serde(default)]
    col: i64,
    #[serde(default)]
    wait: i64,
    #[serde(default)]
    item: String,
    #[serde(default)]
    dir: String,
}

async fn handle_lsp_calls(State(state): State<AppState>, Query(q): Query<CallsQuery>) -> Response {
    let pos = LspPosQuery {
        path: q.path.clone(),
        line: q.line,
        col: q.col,
        wait: q.wait,
    };
    let Some((abs, rel, line, col)) = lsp_pos(&state.root, &state.lsp, &pos) else {
        return err(StatusCode::BAD_REQUEST, "bad path");
    };
    let deadline = lsp_deadline(q.wait);
    let lsp = state.lsp.clone();
    let item = q.item.clone();
    let rel_for_call = rel.clone();
    let outgoing = q.dir == "out";
    let result = if item.is_empty() {
        blocking_lsp(move || lsp.prepare_calls(deadline, &abs, &rel_for_call, line, col)).await
    } else {
        blocking_lsp(move || lsp.calls(deadline, &rel_for_call, &item, outgoing)).await
    };
    let (st, server) = state.lsp.state(&rel);
    match result {
        Err(resp) => *resp,
        Ok(Ok(nodes)) => Json(json!({
            "nodes": nodes,
            "state": st.as_str(),
            "server": server,
        }))
        .into_response(),
        Ok(Err(e)) => Json(json!({
            "nodes": Vec::<crate::calls::CallNode>::new(),
            "state": st.as_str(),
            "server": server,
            "error": e.to_string(),
        }))
        .into_response(),
    }
}

/// Start the server for this file type if it is not running and report
/// where it has got to. Ports Go `handleLSPWarm`.
async fn handle_lsp_warm(State(state): State<AppState>, Query(q): Query<LspPosQuery>) -> Response {
    let rel = match resolve_path(&state.root, &|p| state.lsp.allowed(p), &q.path) {
        Some((_, rel)) => rel,
        None => return err(StatusCode::BAD_REQUEST, "bad path"),
    };
    let deadline = warm_deadline(q.wait);
    let lsp = state.lsp.clone();
    let rel_for_call = rel.clone();
    // The spawn keeps going even when this call gives up waiting on it.
    let _ = blocking_lsp(move || lsp.client(deadline, &rel_for_call)).await;
    Json(lsp_brief(&state.lsp, &rel)).into_response()
}

async fn handle_lsp_hover(State(state): State<AppState>, Query(q): Query<LspPosQuery>) -> Response {
    let Some((abs, rel, line, col)) = lsp_pos(&state.root, &state.lsp, &q) else {
        return err(StatusCode::BAD_REQUEST, "bad path");
    };
    let deadline = lsp_deadline(q.wait);
    let lsp = state.lsp.clone();
    let rel_for_call = rel.clone();
    let result = blocking_lsp(move || lsp.hover(deadline, &abs, &rel_for_call, line, col)).await;
    let (st, server) = state.lsp.state(&rel);
    match result {
        Err(resp) => *resp,
        Ok(Err(e)) => Json(json!({
            "empty": true,
            "state": st.as_str(),
            "server": server,
            "error": e.to_string(),
        }))
        .into_response(),
        Ok(Ok(info)) => Json(json!({
            "signature": info.signature,
            "doc": info.doc,
            "empty": info.empty,
            "state": st.as_str(),
            "server": server,
        }))
        .into_response(),
    }
}

async fn handle_lsp_symbols(
    State(state): State<AppState>,
    Query(q): Query<LspPosQuery>,
) -> Response {
    let (abs, rel) = match resolve_path(&state.root, &|p| state.lsp.allowed(p), &q.path) {
        Some(v) => v,
        None => return err(StatusCode::BAD_REQUEST, "bad path"),
    };
    let deadline = lsp_deadline(q.wait);
    let lsp = state.lsp.clone();
    let rel_for_call = rel.clone();
    let result = blocking_lsp(move || lsp.symbols(deadline, &abs, &rel_for_call)).await;
    let (st, server) = state.lsp.state(&rel);
    match result {
        Err(resp) => *resp,
        Ok(Err(e)) => Json(json!({
            "symbols": Vec::<crate::symbols::Symbol>::new(),
            "state": st.as_str(),
            "server": server,
            "error": e.to_string(),
        }))
        .into_response(),
        Ok(Ok(syms)) => Json(json!({
            "symbols": syms,
            "state": st.as_str(),
            "server": server,
        }))
        .into_response(),
    }
}

async fn handle_lsp_setup(State(state): State<AppState>, Query(q): Query<LspPosQuery>) -> Response {
    let (_, rel) = match resolve_path(&state.root, &|p| state.lsp.allowed(p), &q.path) {
        Some(v) => v,
        None => return err(StatusCode::BAD_REQUEST, "bad path"),
    };
    Json(state.lsp.setup(&rel)).into_response()
}

#[derive(Deserialize)]
struct InstallQuery {
    #[serde(default)]
    server: String,
    #[serde(default)]
    option: String,
}

async fn handle_lsp_install(
    method: Method,
    headers: axum::http::HeaderMap,
    State(state): State<AppState>,
    Query(q): Query<InstallQuery>,
) -> Response {
    if let Err((status, msg)) = local_post(&method, &headers) {
        return err(status, msg);
    }
    // Go ignores Atoi failures, so garbage means option 0 there too.
    let option = q.option.parse::<i64>().unwrap_or(0);
    match state.lsp.install(&q.server, option) {
        Ok(job) => Json(job).into_response(),
        Err(e) => err(StatusCode::BAD_REQUEST, e),
    }
}

/// Find servers installed since startup, clear earlier start failures
/// and start the server for path, reporting where it has got to.
/// Ports Go `handleLSPStart`.
async fn handle_lsp_start(
    method: Method,
    headers: axum::http::HeaderMap,
    State(state): State<AppState>,
    Query(q): Query<LspPosQuery>,
) -> Response {
    if let Err((status, msg)) = local_post(&method, &headers) {
        return err(status, msg);
    }
    let (_, rel) = match resolve_path(&state.root, &|p| state.lsp.allowed(p), &q.path) {
        Some(v) => v,
        None => return err(StatusCode::BAD_REQUEST, "bad path"),
    };
    state.lsp.rescan();
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(1500);
    let lsp = state.lsp.clone();
    let rel_for_call = rel.clone();
    // The spawn continues if this gives up waiting.
    let _ = blocking_lsp(move || lsp.client(deadline, &rel_for_call)).await;
    Json(lsp_brief(&state.lsp, &rel)).into_response()
}

async fn handle_reindex(State(state): State<AppState>) -> Response {
    let index = state.index.clone();
    tokio::task::spawn_blocking(move || index.build())
        .await
        .unwrap_or(());
    let (files, _, index_ms) = state.index.stats();
    Json(ReindexResponse { files, index_ms }).into_response()
}

async fn handle_not_found() -> Response {
    err(StatusCode::NOT_FOUND, "not found")
}

/// Build the slice-1 router. Later slices register the remaining `/api/*`.
pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/", get(handle_index))
        .route("/static/themes.css", get(handle_themes))
        .route("/static/{*path}", get(handle_static))
        .route("/api/meta", get(handle_meta))
        .route("/api/tree", get(handle_tree))
        .route("/api/find", get(handle_find))
        .route("/api/search", get(handle_search))
        .route("/api/file", get(handle_file))
        .route("/api/raw", get(handle_raw))
        .route("/api/close", get(handle_close))
        .route("/api/outline", get(handle_outline))
        .route("/api/def", get(handle_def))
        .route("/api/diff", get(handle_diff))
        .route("/api/gutter", get(handle_gutter))
        .route("/api/lsp/def", get(handle_lsp_def))
        .route("/api/lsp/refs", get(handle_lsp_refs))
        .route("/api/lsp/calls", get(handle_lsp_calls))
        .route("/api/lsp/symbols", get(handle_lsp_symbols))
        .route("/api/lsp/hover", get(handle_lsp_hover))
        .route("/api/lsp/warm", get(handle_lsp_warm))
        .route("/api/lsp/setup", get(handle_lsp_setup))
        .route("/api/lsp/install", post(handle_lsp_install))
        .route("/api/lsp/start", post(handle_lsp_start))
        .route("/api/agent/harnesses", get(handle_agent_harnesses))
        .route("/api/agent/select", post(handle_agent_select))
        .route("/api/agent/edit", post(handle_agent_edit))
        .route("/api/agent/job", get(handle_agent_job))
        .route("/api/agent/cancel", post(handle_agent_cancel))
        .route("/api/terminal", get(handle_terminal_ws))
        .route("/api/markdown", get(handle_markdown))
        .route("/api/reindex", get(handle_reindex).post(handle_reindex))
        .route("/api/metrics", get(handle_metrics))
        .route(
            "/api/settings",
            get(handle_settings_get).post(handle_settings_post),
        )
        .fallback(handle_not_found)
        .layer(SetResponseHeaderLayer::overriding(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-store"),
        ))
        .layer(CompressionLayer::new())
        .with_state(state)
}

/// Serve on an already-bound listener.
pub async fn serve(
    listener: tokio::net::TcpListener,
    state: AppState,
) -> Result<(), std::io::Error> {
    serve_until(listener, state, std::future::pending::<()>()).await
}

/// Serve until `shutdown` resolves, draining in-flight requests first.
/// Ports Go's `srv.Shutdown` on SIGINT/SIGTERM.
pub async fn serve_until(
    listener: tokio::net::TcpListener,
    state: AppState,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> Result<(), std::io::Error> {
    axum::serve(listener, build_router(state))
        .with_graceful_shutdown(shutdown)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> PathBuf {
        if cfg!(windows) {
            PathBuf::from("C:\\work\\proj")
        } else {
            PathBuf::from("/work/proj")
        }
    }

    #[test]
    fn safe_path_root_forms() {
        let r = root();
        assert_eq!(safe_path(&r, ""), Some((r.clone(), String::new())));
        assert_eq!(safe_path(&r, "/"), Some((r.clone(), String::new())));
        assert_eq!(safe_path(&r, "  "), Some((r.clone(), String::new())));
    }

    #[test]
    fn safe_path_keeps_inside_paths() {
        let r = root();
        let (abs, rel) = safe_path(&r, "a/b.go").unwrap();
        assert_eq!(rel, "a/b.go");
        assert!(abs.starts_with(&r));
        assert_eq!(safe_path(&r, "a/./b.go").unwrap().1, "a/b.go");
        assert_eq!(safe_path(&r, "a/sub/../b.go").unwrap().1, "a/b.go");
    }

    #[test]
    fn safe_path_refuses_escapes() {
        let r = root();
        for bad in [
            "..",
            "../x",
            "sub/../../outside",
            "a/../../../b",
            "../..",
            // Go strips one "/" then Clean->IsAbs refuses a still-absolute rest.
            "//etc/passwd",
            "///",
        ] {
            assert_eq!(safe_path(&r, bad), None, "{bad} must be refused");
        }
    }

    #[test]
    fn resolve_path_refuses_absolute_without_allowlist() {
        let r = root();
        let deny = |_: &Path| false;
        assert_eq!(resolve_path(&r, &deny, "/etc/passwd"), None);
        assert_eq!(resolve_path(&r, &deny, "../../../etc/passwd"), None);
        // Percent-decoding happens at the HTTP layer: by the time a path
        // reaches resolve_path, "%2F" is already "/". A literal "%" in a
        // name is a harmless in-root file, matching Go filepath.Clean.
        assert!(resolve_path(&r, &deny, "..%2F..%2Fetc%2Fpasswd").is_some());
    }

    #[test]
    fn resolve_path_allows_relative() {
        let r = root();
        let deny = |_: &Path| false;
        assert!(resolve_path(&r, &deny, "greet.go").is_some());
    }

    /// Only a POST from rx0's own page, addressed by IP or localhost,
    /// may mutate. Ports Go `TestLocalPost`.
    #[test]
    fn local_post_gate() {
        use axum::http::{HeaderMap, HeaderValue, Method};
        for (name, method, host, origin, want) in [
            (
                "same origin",
                Method::POST,
                "127.0.0.1:7777",
                Some("http://127.0.0.1:7777"),
                true,
            ),
            (
                "localhost",
                Method::POST,
                "localhost:7777",
                Some("http://localhost:7777"),
                true,
            ),
            (
                "ipv6 loopback",
                Method::POST,
                "[::1]:7777",
                Some("http://[::1]:7777"),
                true,
            ),
            (
                "get",
                Method::GET,
                "127.0.0.1:7777",
                Some("http://127.0.0.1:7777"),
                false,
            ),
            ("no origin", Method::POST, "127.0.0.1:7777", None, false),
            (
                "other site",
                Method::POST,
                "127.0.0.1:7777",
                Some("https://evil.example"),
                false,
            ),
            (
                "dns rebinding",
                Method::POST,
                "evil.example:7777",
                Some("http://evil.example:7777"),
                false,
            ),
        ] {
            let mut headers = HeaderMap::new();
            headers.insert("host", HeaderValue::from_str(host).unwrap());
            if let Some(o) = origin {
                headers.insert("origin", HeaderValue::from_str(o).unwrap());
            }
            assert_eq!(local_post(&method, &headers).is_ok(), want, "{name}");
        }
    }

    /// Only paths a server actually named may be opened outside the
    /// indexed tree. Ports Go `TestExternalAllowlist`.
    #[test]
    fn external_allowlist() {
        use crate::lspservers::LspManager;
        let r = root();
        let lsp = LspManager::new(r.clone(), false);
        let allow = |p: &Path| lsp.allowed(p);
        assert!(resolve_path(&r, &allow, "/etc/passwd").is_none());
        lsp.allow(Path::new("/usr/lib/go/src/strings/builder.go"));
        let (abs, _) =
            resolve_path(&r, &allow, "/usr/lib/go/src/strings/builder.go").expect("allowlisted");
        assert_eq!(abs.to_string_lossy(), "/usr/lib/go/src/strings/builder.go");
        assert!(resolve_path(&r, &allow, "/usr/lib/go/src/strings/other.go").is_none());
        assert!(resolve_path(&r, &allow, "../../../etc/shadow").is_none());
    }
}
