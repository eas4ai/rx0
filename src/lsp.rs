//! Minimal LSP client: enough of the protocol to answer navigation
//! questions (definition, references, document symbols, hover, call
//! hierarchy) and nothing else. Everything is best-effort — if a server
//! is missing, slow, or broken, callers fall back to the regex index,
//! which is always available.
//!
//! Ports `lsp.go`. The client is synchronous (`std` threads and pipes,
//! like Go's goroutines): axum handlers run it on a blocking thread
//! with a deadline, which plays the role of Go's request context.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------- protocol

/// 0-based line, 0-based character in the negotiated encoding.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct LspPosition {
    pub line: i64,
    pub character: i64,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct LspRange {
    pub start: LspPosition,
    pub end: LspPosition,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LspLocation {
    pub uri: String,
    pub range: LspRange,
}

/// What servers return when they support LinkSupport.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LspLocationLink {
    pub target_uri: String,
    pub target_range: LspRange,
    pub target_selection_range: LspRange,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LspDocumentSymbol {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub kind: i64,
    #[serde(default)]
    pub range: LspRange,
    #[serde(default)]
    pub selection_range: LspRange,
    #[serde(default)]
    pub children: Vec<LspDocumentSymbol>,
    /// symbolInformation form, used by servers without hierarchical support.
    #[serde(default)]
    pub location: Option<LspLocation>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RpcMessage {
    #[serde(default)]
    pub jsonrpc: String,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub id: Value,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub method: String,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub params: Value,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub result: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RpcError {
    #[serde(default)]
    pub code: i64,
    #[serde(default)]
    pub message: String,
}

/// Kind numbers come from the LSP spec; only display names are needed.
pub fn symbol_kind_name(kind: i64) -> &'static str {
    match kind {
        1 => "file",
        2 => "module",
        3 => "namespace",
        4 => "package",
        5 => "class",
        6 => "method",
        7 => "property",
        8 => "field",
        9 => "ctor",
        10 => "enum",
        11 => "interface",
        12 => "func",
        13 => "var",
        14 => "const",
        15 => "string",
        16 => "number",
        17 => "bool",
        18 => "array",
        19 => "object",
        20 => "key",
        21 => "null",
        22 => "enum",
        23 => "struct",
        24 => "event",
        25 => "operator",
        26 => "type",
        _ => "sym",
    }
}

// ---------------------------------------------------------------- uris

/// Absolute path to `file://` URI, percent-encoding what needs it.
/// Ports Go `pathToURI` (which defers to `net/url`).
pub fn path_to_uri(p: &Path) -> String {
    let mut s = p.to_string_lossy().replace('\\', "/");
    if cfg!(windows) && !s.starts_with('/') {
        s.insert(0, '/');
    }
    let mut out = String::with_capacity(s.len() + 7);
    out.push_str("file://");
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"$-_.+!*'(),;:@&=~/?".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `file://` URI back to a path. Ports Go `uriToPath`.
pub fn uri_to_path(uri: &str) -> Result<PathBuf, String> {
    let path = if let Some(rest) = uri.strip_prefix("file://") {
        rest
    } else if let Some(rest) = uri.strip_prefix("file:") {
        rest
    } else if uri.contains("://") {
        return Err(format!("not a file uri: {uri}"));
    } else {
        uri
    };
    let mut bytes = Vec::with_capacity(path.len());
    let raw = path.as_bytes();
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b'%' {
            if i + 2 >= raw.len() {
                return Err(format!("bad percent encoding in uri: {uri}"));
            }
            let hex = std::str::from_utf8(&raw[i + 1..i + 3])
                .map_err(|_| format!("bad percent encoding in uri: {uri}"))?;
            let byte = u8::from_str_radix(hex, 16)
                .map_err(|_| format!("bad percent encoding in uri: {uri}"))?;
            bytes.push(byte);
            i += 3;
        } else {
            bytes.push(raw[i]);
            i += 1;
        }
    }
    let mut s = String::from_utf8(bytes).map_err(|_| format!("non-utf8 uri: {uri}"))?;
    if cfg!(windows) {
        // file:///C:/x sheds one slash (drive path); anything else keeps
        // its root, mirroring Go filepath.FromSlash.
        let b = s.as_bytes();
        if s.starts_with('/') && b.len() > 2 && b[1].is_ascii_alphabetic() && b[2] == b':' {
            s.remove(0);
        }
        s = s.replace('/', "\\");
    }
    Ok(PathBuf::from(s))
}

// ---------------------------------------------------------------- client

/// How the server counts Character offsets: negotiated at initialize,
/// defaulting to the spec's utf-16.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PositionEncoding {
    Utf8,
    Utf16,
    Utf32,
}

impl PositionEncoding {
    fn from_name(name: &str) -> Self {
        match name {
            "utf-8" => Self::Utf8,
            "utf-32" => Self::Utf32,
            _ => Self::Utf16,
        }
    }
}

/// Read one `Content-Length`-framed message. Ports Go `readFrame`,
/// including the 64 MiB cap.
pub fn read_frame<R: BufRead>(r: &mut R) -> Result<RpcMessage, String> {
    let mut length: usize = 0;
    loop {
        let mut line = String::new();
        r.read_line(&mut line).map_err(|e| e.to_string())?;
        if line.is_empty() {
            return Err("eof reading lsp headers".to_string());
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        if let Some((name, val)) = line.split_once(':') {
            if name.trim().eq_ignore_ascii_case("Content-Length") {
                length = val.trim().parse().unwrap_or(0);
            }
        }
    }
    if length == 0 || length > 64 << 20 {
        return Err(format!("bad content length {length}"));
    }
    let mut buf = vec![0u8; length];
    r.read_exact(&mut buf).map_err(|e| e.to_string())?;
    serde_json::from_slice(&buf).map_err(|e| e.to_string())
}

struct ClientInner {
    dead: Option<String>,
    next_id: i64,
    pending: HashMap<i64, std::sync::mpsc::SyncSender<RpcMessage>>,
    opened: HashMap<String, i64>,
}

/// How to stop the server process. The live client kills its child;
/// tests substitute a no-op. Reference-counted so the deferred kill in
/// `shutdown` stays valid even if the client is dropped first.
type Killer = Arc<dyn Fn() + Send + Sync>;

pub struct LspClient {
    pub def_name: String,
    root: PathBuf,
    init_options: Value,
    lang_ids: HashMap<String, String>,
    default_lang: String,
    stdin: Mutex<Box<dyn Write + Send>>,
    killer: Killer,
    inner: Mutex<ClientInner>,
    pub encoding: Mutex<PositionEncoding>,
    indexing: Mutex<i32>,
}

impl LspClient {
    /// Spawn the server process and run the initialize handshake with a
    /// 30 s budget of its own. Ports Go `start` + `initialize`.
    pub fn start(def: &crate::lspservers::LspServerDef, root: &Path) -> Result<Arc<Self>, String> {
        let mut child = std::process::Command::new(&def.cmd[0])
            .args(&def.cmd[1..])
            .current_dir(root)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| e.to_string())?;
        let stdin = child.stdin.take().ok_or("lsp client stdin closed")?;
        let stdout = child.stdout.take().ok_or("lsp client stdout closed")?;
        let child = Arc::new(Mutex::new(child));
        let killer_child = child.clone();
        let killer: Killer = Arc::new(move || drop(killer_child.lock().unwrap().kill()));
        let this = Self::launch(
            def.name.clone(),
            root.to_path_buf(),
            def.init_options.clone(),
            def.lang_ids.clone(),
            def.default_lang.clone(),
            Box::new(std::io::BufReader::with_capacity(64 << 10, stdout)),
            Box::new(stdin),
            killer,
        );
        // A dead server must fail its callers rather than hang them.
        let reaper = this.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_millis(50));
            let gone = child.lock().unwrap().try_wait().map_err(|e| e.to_string());
            match gone {
                Ok(None) => continue,
                Ok(Some(_)) => {
                    let name = reaper.def_name.clone();
                    reaper.fail(&format!("{name} exited"));
                    return;
                }
                Err(e) => {
                    reaper.fail(&e);
                    return;
                }
            }
        });
        let deadline = Instant::now() + Duration::from_secs(30);
        this.initialize(deadline)?;
        Ok(this)
    }

    /// Test seam: a client over caller-provided pipes, with no process
    /// and no handshake. The peer drives the protocol.
    #[cfg(test)]
    pub fn for_test(reader: Box<dyn BufRead + Send>, writer: Box<dyn Write + Send>) -> Arc<Self> {
        Self::launch(
            "test".to_string(),
            PathBuf::from("/test"),
            Value::Null,
            HashMap::new(),
            String::new(),
            reader,
            writer,
            Arc::new(|| {}),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn launch(
        def_name: String,
        root: PathBuf,
        init_options: Value,
        lang_ids: HashMap<String, String>,
        default_lang: String,
        reader: Box<dyn BufRead + Send>,
        writer: Box<dyn Write + Send>,
        killer: Killer,
    ) -> Arc<Self> {
        let this = Arc::new(Self {
            def_name,
            root,
            init_options,
            lang_ids,
            default_lang,
            stdin: Mutex::new(writer),
            killer,
            inner: Mutex::new(ClientInner {
                dead: None,
                next_id: 0,
                pending: HashMap::new(),
                opened: HashMap::new(),
            }),
            encoding: Mutex::new(PositionEncoding::Utf16),
            indexing: Mutex::new(0),
        });
        let reader_this = this.clone();
        std::thread::spawn(move || reader_this.read_loop(reader));
        this
    }

    pub(crate) fn fail(&self, err: &str) {
        let mut inner = self.inner.lock().unwrap();
        if inner.dead.is_none() {
            inner.dead = Some(err.to_string());
        }
        for (_, ch) in inner.pending.drain() {
            drop(ch);
        }
    }

    pub fn alive(&self) -> Option<String> {
        self.inner.lock().unwrap().dead.clone()
    }

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

    /// Consume framed messages: responses to their waiting caller,
    /// server-initiated requests to a stub reply (a server that never
    /// hears back can stall), notifications to progress tracking.
    fn read_loop(&self, reader: Box<dyn BufRead + Send>) {
        let mut reader = reader;
        loop {
            let msg = match read_frame(&mut reader) {
                Ok(m) => m,
                Err(e) => {
                    self.fail(&e);
                    return;
                }
            };
            if msg.method.is_empty() && !msg.id.is_null() {
                if let Some(id) = rpc_id_int(&msg.id) {
                    let ch = self.inner.lock().unwrap().pending.remove(&id);
                    if let Some(ch) = ch {
                        let _ = ch.try_send(msg);
                    }
                }
            } else if !msg.id.is_null() {
                self.reply(&msg.id, &msg.method);
            } else {
                self.on_notification(&msg);
            }
        }
    }

    fn reply(&self, id: &Value, method: &str) {
        let result = match method {
            "workspace/configuration" => serde_json::json!([{}]),
            "workspace/workspaceFolders" => serde_json::json!([{
                "uri": path_to_uri(&self.root),
                "name": self.root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            }]),
            _ => Value::Null,
        };
        let body = serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result});
        let _ = self.write(&serde_json::to_vec(&body).unwrap_or_default());
    }

    fn on_notification(&self, msg: &RpcMessage) {
        if msg.method != "$/progress" {
            return;
        }
        let kind = msg
            .params
            .get("value")
            .and_then(|v| v.get("kind"))
            .and_then(|k| k.as_str())
            .unwrap_or("");
        let mut indexing = self.indexing.lock().unwrap();
        match kind {
            "begin" => *indexing += 1,
            "end" if *indexing > 0 => *indexing -= 1,
            _ => {}
        }
    }

    /// `$/progress` tokens outstanding: callers use this to tell "no
    /// result" from "the server has not finished indexing yet".
    pub fn busy(&self) -> bool {
        *self.indexing.lock().unwrap() > 0
    }

    fn write(&self, body: &[u8]) -> Result<(), String> {
        let mut stdin = self.stdin.lock().unwrap();
        if let Some(dead) = &self.inner.lock().unwrap().dead {
            return Err(dead.clone());
        }
        write!(stdin, "Content-Length: {}\r\n\r\n", body.len()).map_err(|e| e.to_string())?;
        stdin.write_all(body).map_err(|e| e.to_string())
    }

    pub fn notify(&self, method: &str, params: Value) -> Result<(), String> {
        let body = serde_json::json!({"jsonrpc": "2.0", "method": method, "params": params});
        self.write(&serde_json::to_vec(&body).map_err(|e| e.to_string())?)
    }

    /// One request/response round trip, bounded by `deadline`. On
    /// timeout the waiter is dropped and the server is told to stop
    /// working on something nobody waits for. Ports Go `call`.
    pub fn call(&self, method: &str, params: Value, deadline: Instant) -> Result<Value, String> {
        let (ch, id) = {
            let mut inner = self.inner.lock().unwrap();
            if let Some(dead) = &inner.dead {
                return Err(dead.clone());
            }
            inner.next_id += 1;
            let id = inner.next_id;
            let (tx, rx) = std::sync::mpsc::sync_channel(1);
            inner.pending.insert(id, tx);
            (rx, id)
        };
        let body =
            serde_json::json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        if let Err(e) = self.write(&serde_json::to_vec(&body).map_err(|e| e.to_string())?) {
            self.inner.lock().unwrap().pending.remove(&id);
            return Err(e);
        }
        let timeout = deadline.saturating_duration_since(Instant::now());
        match ch.recv_timeout(timeout) {
            Err(_) => {
                self.inner.lock().unwrap().pending.remove(&id);
                let _ = self.notify("$/cancelRequest", serde_json::json!({"id": id}));
                Err(format!("{}: request timed out", self.def_name))
            }
            Ok(msg) => {
                if let Some(err) = msg.error {
                    return Err(format!("lsp error {}: {}", err.code, err.message));
                }
                if msg.result.is_null() {
                    return Ok(Value::Null);
                }
                Ok(msg.result)
            }
        }
    }

    fn initialize(&self, deadline: Instant) -> Result<(), String> {
        let root_uri = path_to_uri(&self.root);
        let params = serde_json::json!({
            "processId": std::process::id(),
            "rootUri": root_uri,
            "clientInfo": {"name": "rx0", "version": crate::VERSION},
            "workspaceFolders": [{"uri": root_uri, "name": self.root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()}],
            "capabilities": {
                "general": {"positionEncodings": ["utf-8", "utf-16"]},
                "workspace": {
                    "workspaceFolders": true,
                    "configuration": true,
                    "symbol": {"dynamicRegistration": false},
                },
                "textDocument": {
                    "synchronization": {"didSave": false, "dynamicRegistration": false},
                    "definition": {"linkSupport": true},
                    "typeDefinition": {"linkSupport": true},
                    "implementation": {"linkSupport": true},
                    "references": {"dynamicRegistration": false},
                    "callHierarchy": {"dynamicRegistration": false},
                    "documentSymbol": {
                        "hierarchicalDocumentSymbolSupport": true,
                        "dynamicRegistration": false,
                    },
                    // Order matters: servers pick the first format they
                    // support, and markdown carries the fenced signature.
                    "hover": {"contentFormat": ["markdown", "plaintext"]},
                },
                "window": {"workDoneProgress": true},
            },
            "initializationOptions": self.init_options,
        });
        let res = self.call("initialize", params, deadline)?;
        if let Some(enc) = res
            .get("capabilities")
            .and_then(|c| c.get("positionEncoding"))
            .and_then(|e| e.as_str())
        {
            if !enc.is_empty() {
                *self.encoding.lock().unwrap() = PositionEncoding::from_name(enc);
            }
        }
        self.notify("initialized", serde_json::json!({}))?;
        Ok(())
    }

    /// `shutdown` + `exit` + stdin close, killing the process a second
    /// later if it lingers. Ports Go `shutdown`.
    pub fn shutdown(&self) {
        let deadline = Instant::now() + Duration::from_secs(2);
        let _ = self.call("shutdown", Value::Null, deadline);
        let _ = self.notify("exit", Value::Null);
        // Closing stdin unblocks servers waiting on input: swap in a
        // sink so later writes fail closed, not loud, and drop the pipe.
        {
            let mut stdin = self.stdin.lock().unwrap();
            let old = std::mem::replace(&mut *stdin, Box::new(Sink) as Box<dyn Write + Send>);
            drop(old);
        }
        let killer = self.killer.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(1));
            killer();
        });
    }

    /// Tell the server about a file. Most servers refuse to answer
    /// questions about a document they were never handed.
    pub fn ensure_open(&self, abs: &Path, rel: &str) -> Result<(), String> {
        let uri = path_to_uri(abs);
        if self.inner.lock().unwrap().opened.contains_key(&uri) {
            return Ok(());
        }
        let data = std::fs::read(abs).map_err(|e| e.to_string())?;
        let text = String::from_utf8_lossy(&data).into_owned();
        self.notify(
            "textDocument/didOpen",
            serde_json::json!({"textDocument": {
                "uri": uri,
                "languageId": self.language_id(rel),
                "version": 1,
                "text": text,
            }}),
        )?;
        self.inner.lock().unwrap().opened.insert(uri, 1);
        Ok(())
    }

    /// Notify the server a file was closed so it can free ASTs and memory.
    pub fn close_doc(&self, abs: &Path) {
        let uri = path_to_uri(abs);
        let was = self.inner.lock().unwrap().opened.remove(&uri).is_some();
        if !was {
            return;
        }
        let _ = self.notify(
            "textDocument/didClose",
            serde_json::json!({"textDocument": {"uri": uri}}),
        );
    }

    /// 1-based line and 0-based byte column into server offsets.
    pub fn to_lsp(&self, line_text: &str, line: usize, byte_col: usize) -> LspPosition {
        to_lsp_with(*self.encoding.lock().unwrap(), line_text, line, byte_col)
    }

    /// Server position back into a 1-based line and 0-based byte column
    /// against the file on disk.
    pub fn from_lsp(&self, lines: &[String], p: LspPosition) -> (usize, usize) {
        from_lsp_with(*self.encoding.lock().unwrap(), lines, p)
    }
}

struct Sink;
impl Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Response ids arrive as numbers or quoted numbers; anything else is
/// not one of our calls. Ports Go's `ParseInt(Trim(id, '"'))`.
pub fn rpc_id_int(id: &Value) -> Option<i64> {
    match id {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.trim_matches('"').parse().ok(),
        _ => None,
    }
}

pub fn to_lsp_with(
    enc: PositionEncoding,
    line_text: &str,
    line: usize,
    byte_col: usize,
) -> LspPosition {
    let byte_col = byte_col.min(line_text.len());
    let prefix = &line_text[..byte_col];
    let ch = match enc {
        PositionEncoding::Utf8 => byte_col as i64,
        PositionEncoding::Utf32 => prefix.chars().count() as i64,
        PositionEncoding::Utf16 => prefix.chars().map(|c| c.len_utf16() as i64).sum(),
    };
    LspPosition {
        line: line as i64 - 1,
        character: ch,
    }
}

pub fn from_lsp_with(enc: PositionEncoding, lines: &[String], p: LspPosition) -> (usize, usize) {
    let line = (p.line + 1).max(1) as usize;
    if p.line < 0 || (p.line as usize) >= lines.len() {
        return (line, 0);
    }
    let text = &lines[p.line as usize];
    match enc {
        PositionEncoding::Utf8 => {
            if p.character > text.len() as i64 {
                return (line, text.len());
            }
            (line, p.character.max(0) as usize)
        }
        PositionEncoding::Utf32 => {
            let chars: Vec<char> = text.chars().collect();
            if p.character > chars.len() as i64 {
                return (line, text.len());
            }
            let n = p.character.max(0) as usize;
            (line, chars[..n].iter().map(|c| c.len_utf8()).sum())
        }
        PositionEncoding::Utf16 => {
            let mut units = 0i64;
            let mut bytes = 0usize;
            for c in text.chars() {
                if units >= p.character {
                    break;
                }
                units += c.len_utf16() as i64;
                bytes += c.len_utf8();
            }
            (line, bytes)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::net::{TcpListener, TcpStream};

    fn frame_bytes(msg: &Value) -> Vec<u8> {
        let body = serde_json::to_vec(msg).unwrap();
        let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
        out.extend_from_slice(&body);
        out
    }

    /// Position encodings are the classic off-by-N source: Go counts
    /// bytes, JavaScript counts UTF-16 units, and the protocol defaults
    /// to UTF-16 while some servers negotiate UTF-8. Ports Go
    /// `TestPositionEncodings`.
    #[test]
    fn position_encodings_round_trip() {
        // "héllo → wörld" mixes 1-, 2- and 3-byte runes.
        let line = "héllo → wörld";
        let byte_col = line.find("wörld").unwrap();
        for (enc, want_c) in [
            (PositionEncoding::Utf8, byte_col as i64), // bytes
            (PositionEncoding::Utf16, 8),              // h é l l o ␠ → ␠
            (PositionEncoding::Utf32, 8),              // no surrogates here
        ] {
            let got = to_lsp_with(enc, line, 1, byte_col);
            assert_eq!(got.character, want_c, "{enc:?}");
            assert_eq!(got.line, 0, "LSP lines are 0-based");
            let (rl, rb) = from_lsp_with(enc, &[line.to_string()], got);
            assert_eq!((rl, rb), (1, byte_col), "{enc:?} round trip");
        }
    }

    /// Ports Go `TestURIRoundTrip`.
    #[test]
    fn uri_round_trip() {
        // (native path, exact uri): both directions pinned, so spaces
        // must be percent-encoded and drive roots must survive.
        #[cfg(windows)]
        let cases = [
            ("C:\\Users\\me\\main.go", "file:///C:/Users/me/main.go"),
            (
                "C:\\Users\\my self\\a b.go",
                "file:///C:/Users/my%20self/a%20b.go",
            ),
            ("D:\\weird#name$x.go", "file:///D:/weird%23name$x.go"),
        ];
        #[cfg(not(windows))]
        let cases = [
            (
                "/home/user/project/main.go",
                "file:///home/user/project/main.go",
            ),
            (
                "/home/user/my project/a b.go",
                "file:///home/user/my%20project/a%20b.go",
            ),
            ("/tmp/weird#name$x.go", "file:///tmp/weird%23name$x.go"),
        ];
        for (p, want_uri) in cases {
            let uri = path_to_uri(Path::new(p));
            assert_eq!(uri, want_uri, "{p}");
            let back = uri_to_path(&uri).expect("must parse");
            assert_eq!(back.to_string_lossy(), p, "{p} -> {uri}");
        }
        assert!(uri_to_path("https://example.com/x.go").is_err());
    }

    #[test]
    fn frame_rejects_garbage() {
        let mut cur = Cursor::new(b"Content-Length: 0\r\n\r\n".to_vec());
        assert!(read_frame(&mut cur).is_err());
        let mut cur = Cursor::new(b"Content-Length: 5\r\n\r\nnul".to_vec());
        assert!(read_frame(&mut cur).is_err()); // short body
        let msg = serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "m"});
        let mut cur = Cursor::new(frame_bytes(&msg));
        let back = read_frame(&mut cur).unwrap();
        assert_eq!(back.method, "m");
        assert_eq!(rpc_id_int(&back.id), Some(1));
        // Quoted numeric ids are ours too.
        assert_eq!(rpc_id_int(&Value::String("\"42\"".to_string())), Some(42));
        assert_eq!(rpc_id_int(&Value::Null), None);
    }

    /// Pipes handed to a client under test.
    type ClientPipes = (Box<dyn BufRead + Send>, Box<dyn Write + Send>);

    /// Connected loopback pair: the test drives the server side with
    /// one persistent reader, since fresh buffered readers would
    /// swallow coalesced frames.
    struct Driver {
        read: std::io::BufReader<TcpStream>,
        write: TcpStream,
    }

    impl Driver {
        fn read_msg(&mut self) -> RpcMessage {
            read_frame(&mut self.read).expect("server side must get a frame")
        }
        fn write_msg(&mut self, msg: &Value) {
            use std::io::Write;
            self.write.write_all(&frame_bytes(msg)).unwrap();
        }
    }

    fn pipe_pair() -> (Driver, ClientPipes) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let write = TcpStream::connect(addr).unwrap();
        let (peer, _) = listener.accept().unwrap();
        let reader: Box<dyn BufRead + Send> =
            Box::new(std::io::BufReader::new(peer.try_clone().unwrap()));
        let writer: Box<dyn Write + Send> = Box::new(peer);
        let read = std::io::BufReader::new(write.try_clone().unwrap());
        // A stuck read surfaces as an error, never a hung test.
        read.get_ref()
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        write
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        (Driver { read, write }, (reader, writer))
    }

    /// The full wire conversation: initialize negotiates utf-8,
    /// definition decodes, progress flips busy, server-initiated
    /// requests get stub replies, and EOF fails the client.
    #[test]
    fn client_conversation_over_loopback() {
        let (mut driver, (reader, writer)) = pipe_pair();
        let client = LspClient::for_test(reader, writer);

        // initialize round trip, negotiated down to utf-8.
        let init = std::thread::spawn({
            let client = client.clone();
            move || client.initialize(Instant::now() + Duration::from_secs(5))
        });
        let req = driver.read_msg();
        assert_eq!(req.method, "initialize");
        assert_eq!(req.id, serde_json::json!(1));
        assert_eq!(
            req.params["clientInfo"],
            serde_json::json!({"name": "rx0", "version": crate::VERSION})
        );
        driver.write_msg(&serde_json::json!({
            "jsonrpc": "2.0", "id": 1,
            "result": {"capabilities": {"positionEncoding": "utf-8"}},
        }));
        init.join().unwrap().expect("initialize");
        assert_eq!(*client.encoding.lock().unwrap(), PositionEncoding::Utf8);
        // The handshake ends with `initialized`.
        let noted = driver.read_msg();
        assert_eq!(noted.method, "initialized");

        // A definition call decodes a Location array.
        let def = std::thread::spawn({
            let client = client.clone();
            move || {
                client.call(
                    "textDocument/definition",
                    serde_json::json!({}),
                    Instant::now() + Duration::from_secs(5),
                )
            }
        });
        let req = driver.read_msg();
        assert_eq!(req.method, "textDocument/definition");
        driver.write_msg(&serde_json::json!({
            "jsonrpc": "2.0", "id": req.id,
            "result": [{"uri": "file:///a.go", "range": {
                "start": {"line": 2, "character": 4},
                "end": {"line": 2, "character": 7},
            }}],
        }));
        let result = def.join().unwrap().expect("definition");
        assert_eq!(result[0]["uri"], "file:///a.go");

        // Progress notifications flip busy().
        assert!(!client.busy());
        driver.write_msg(
            &serde_json::json!({"jsonrpc": "2.0", "method": "$/progress",
                "params": {"token": "t", "value": {"kind": "begin"}}}),
        );
        std::thread::sleep(Duration::from_millis(200));
        assert!(client.busy());
        driver.write_msg(
            &serde_json::json!({"jsonrpc": "2.0", "method": "$/progress",
                "params": {"token": "t", "value": {"kind": "end"}}}),
        );
        std::thread::sleep(Duration::from_millis(200));
        assert!(!client.busy());

        // Server-initiated requests get a stub reply, not silence.
        driver.write_msg(
            &serde_json::json!({"jsonrpc": "2.0", "id": 99, "method": "client/registerCapability",
                "params": {}}),
        );
        let reply = driver.read_msg();
        assert_eq!(reply.id, serde_json::json!(99));
        assert!(reply.result.is_null());

        // A timed-out call cancels server-side and reports.
        let slow = client.call(
            "textDocument/hover",
            serde_json::json!({}),
            Instant::now() + Duration::from_millis(100),
        );
        assert!(slow.unwrap_err().contains("timed out"));
        // First the unanswered hover request itself, then its cancel.
        let pending = driver.read_msg();
        assert_eq!(pending.method, "textDocument/hover");
        let cancel = driver.read_msg();
        assert_eq!(cancel.method, "$/cancelRequest");

        // EOF fails pending and future callers.
        drop(driver); // EOF fails the client
        let deadline = Instant::now() + Duration::from_secs(5);
        let start = Instant::now();
        while client.alive().is_none() && start.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(client.alive().is_some());
        assert!(client.call("x", serde_json::json!({}), deadline).is_err());
    }

    /// Ports Go `TestLSPCloseDoc`.
    #[test]
    fn close_doc_forgets_uri() {
        let (_driver, (reader, writer)) = pipe_pair();
        let client = LspClient::for_test(reader, writer);
        let uri = path_to_uri(Path::new("/test.go"));
        client.inner.lock().unwrap().opened.insert(uri.clone(), 1);
        client.close_doc(Path::new("/test.go"));
        assert!(!client.inner.lock().unwrap().opened.contains_key(&uri));
        client.close_doc(Path::new("/test.go")); // no panic on repeat
    }
}
