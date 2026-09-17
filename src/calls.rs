//! Call trails: who calls a function, and what it calls, one level at a
//! time. The server identifies each function by an opaque
//! CallHierarchyItem handed back verbatim to expand the next level; rx0
//! keeps no state between requests, so every node carries its item to
//! the browser, which sends it back on expansion.
//!
//! Ports `calls.go`.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use std::time::Instant;

use crate::lsp::LspRange;
use crate::lspnav::utf16_to_byte;
use crate::lspservers::{LspError, LspManager};

/// One function in a call trail. Ports Go `CallNode`.
#[derive(Clone, Debug, Serialize)]
pub struct CallNode {
    pub name: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub detail: String,
    pub kind: String,
    /// Where the function is declared.
    pub path: String,
    pub line: usize,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub ext: bool,
    /// File holding the call expressions.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub site_path: String,
    /// Lines of those calls, ascending.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub sites: Vec<usize>,
    /// CallHierarchyItem, returned as-is to expand.
    pub item: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LspCallItem {
    #[serde(default)]
    name: String,
    #[serde(default)]
    kind: i64,
    #[serde(default)]
    detail: String,
    #[serde(default)]
    uri: String,
    #[serde(default)]
    selection_range: LspRange,
}

impl LspManager {
    fn call_node(&self, raw: &Value, allow: bool) -> Result<(CallNode, LspCallItem), String> {
        let item: LspCallItem = serde_json::from_value(raw.clone())
            .map_err(|_| "bad call hierarchy item".to_string())?;
        if item.uri.is_empty() {
            return Err("bad call hierarchy item".to_string());
        }
        let abs = crate::lsp::uri_to_path(&item.uri)?;
        let (rel, ext) = self.tree_path(&abs, allow);
        Ok((
            CallNode {
                name: item.name.clone(),
                detail: item.detail.clone(),
                kind: crate::lsp::symbol_kind_name(item.kind).to_string(),
                path: rel,
                line: (item.selection_range.start.line + 1).max(1) as usize,
                ext,
                site_path: String::new(),
                sites: Vec::new(),
                item: serde_json::to_string(raw).map_err(|e| e.to_string())?,
            },
            item,
        ))
    }

    /// Resolve the function at a position into trail root(s). Ports Go
    /// `PrepareCalls`.
    pub fn prepare_calls(
        self: &Arc<Self>,
        deadline: Instant,
        abs: &std::path::Path,
        rel: &str,
        line: usize,
        u16col: i64,
    ) -> Result<Vec<CallNode>, LspError> {
        let client = self.client(deadline, rel)?;
        client.ensure_open(abs, rel).map_err(LspError::Other)?;
        let disk = match crate::highlight::open(abs, rel) {
            Ok(doc) => (1..=doc.total)
                .map(|n| doc.raw(n).to_string())
                .collect::<Vec<_>>(),
            Err(_) => Vec::new(),
        };
        let line_text = if line >= 1 && line <= disk.len() {
            disk[line - 1].as_str()
        } else {
            ""
        };
        let byte_col = utf16_to_byte(line_text, u16col);
        let result = client
            .call(
                "textDocument/prepareCallHierarchy",
                serde_json::json!({
                    "textDocument": {"uri": crate::lsp::path_to_uri(abs)},
                    "position": client.to_lsp(line_text, line, byte_col),
                }),
                deadline,
            )
            .map_err(LspError::Other)?;
        let mut out = Vec::new();
        if let Value::Array(items) = result {
            for r in &items {
                if let Ok((n, _)) = self.call_node(r, true) {
                    out.push(n);
                }
            }
        }
        Ok(out)
    }

    /// Expand one trail node: its callers, or with outgoing its
    /// callees. Ports Go `Calls`. `rel` picks the server; `item` is a
    /// node's Item as the browser sent it.
    pub fn calls(
        self: &Arc<Self>,
        deadline: Instant,
        rel: &str,
        item: &str,
        outgoing: bool,
    ) -> Result<Vec<CallNode>, LspError> {
        let client = self.client(deadline, rel)?;
        let raw_item: Value = serde_json::from_str(item)
            .map_err(|_| LspError::Other("bad call hierarchy item".to_string()))?;
        let (_, parent) = self.call_node(&raw_item, false).map_err(LspError::Other)?;
        // Some servers only answer about documents they were handed.
        // Only files in the tree are read for this: the item came from
        // the browser.
        if let Ok(pabs) = crate::lsp::uri_to_path(&parent.uri) {
            let (prel, ext) = self.tree_path(&pabs, false);
            if !ext {
                let _ = client.ensure_open(&pabs, &prel);
            }
        }
        let method = if outgoing {
            "callHierarchy/outgoingCalls"
        } else {
            "callHierarchy/incomingCalls"
        };
        let result = client
            .call(method, serde_json::json!({"item": raw_item}), deadline)
            .map_err(LspError::Other)?;
        let mut out = Vec::new();
        if let Value::Array(entries) = result {
            for r in &entries {
                let child = if outgoing { r.get("to") } else { r.get("from") };
                let Some(child) = child else { continue };
                let (mut n, it) = match self.call_node(child, true) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                // fromRanges sit inside the caller: the returned
                // function for incoming calls, the function being
                // expanded for outgoing ones.
                let site_uri = if outgoing {
                    parent.uri.clone()
                } else {
                    it.uri.clone()
                };
                if let Ok(sabs) = crate::lsp::uri_to_path(&site_uri) {
                    n.site_path = self.tree_path(&sabs, false).0;
                }
                n.sites = site_lines(
                    &r.get("fromRanges")
                        .and_then(|v| serde_json::from_value::<Vec<LspRange>>(v.clone()).ok())
                        .unwrap_or_default(),
                );
                out.push(n);
            }
        }
        if outgoing {
            out.sort_by_key(first_site); // callees in call order
        } else {
            out.sort_by(|a, b| a.path.cmp(&b.path).then(a.line.cmp(&b.line)));
        }
        Ok(out)
    }
}

pub fn site_lines(rs: &[LspRange]) -> Vec<usize> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for r in rs {
        let line = (r.start.line + 1).max(1) as usize;
        if seen.insert(line) {
            out.push(line);
        }
    }
    out.sort_unstable();
    out
}

pub fn first_site(n: &CallNode) -> usize {
    n.sites.first().copied().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lsp::LspPosition;
    use std::collections::HashMap;
    use std::io::{BufReader, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::Mutex;
    use std::time::Duration;

    /// Wire a client to an in-process responder over loopback, so the
    /// call-hierarchy plumbing runs without a real language server.
    /// Ports Go's `fakeCallServer`.
    fn fake_call_server(
        root: &std::path::Path,
        answer: impl Fn(&str, serde_json::Value) -> serde_json::Value + Send + Sync + 'static,
    ) -> std::sync::Arc<crate::lspservers::LspManager> {
        let answer = std::sync::Arc::new(answer);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (peer, _) = listener.accept().unwrap();
            peer.set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let mut reader = BufReader::new(peer.try_clone().unwrap());
            let mut writer = peer;
            for _ in 0..16 {
                let msg = match crate::lsp::read_frame(&mut reader) {
                    Ok(m) => m,
                    Err(_) => break,
                };
                if msg.id.is_null() {
                    continue; // notifications such as didOpen
                }
                let body = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": msg.id,
                    "result": answer(&msg.method, msg.params),
                });
                let body = serde_json::to_vec(&body).unwrap();
                if writer
                    .write_all(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes())
                    .is_err()
                {
                    break;
                }
                if writer.write_all(&body).is_err() {
                    break;
                }
            }
        });
        let peer = TcpStream::connect(addr).unwrap();
        let reader: Box<dyn std::io::BufRead + Send> =
            Box::new(BufReader::new(peer.try_clone().unwrap()));
        let writer: Box<dyn std::io::Write + Send> = Box::new(peer);
        let client = crate::lsp::LspClient::for_test(reader, writer);
        let mut by_ext = HashMap::new();
        by_ext.insert(
            ".go".to_string(),
            crate::lspservers::LspServerDef {
                name: "fake".to_string(),
                lang: "Go".to_string(),
                cmd: vec!["fake".to_string()],
                exts: vec![".go".to_string()],
                lang_ids: HashMap::new(),
                default_lang: "go".to_string(),
                init_options: serde_json::Value::Null,
                install: Vec::new(),
            },
        );
        let m = crate::lspservers::LspManager::for_test(root.to_path_buf(), by_ext);
        m.insert_client("fake", client);
        m
    }

    fn call_item(name: &str, uri: &str, line: i64) -> serde_json::Value {
        let pos = serde_json::json!({"line": line, "character": 5});
        let rng = serde_json::json!({"start": pos, "end": pos});
        serde_json::json!({
            "name": name, "kind": 12, "uri": uri,
            "range": rng, "selectionRange": rng,
            "data": {"token": name},
        })
    }

    fn site_range(line: i64) -> serde_json::Value {
        let pos = serde_json::json!({"line": line, "character": 1});
        serde_json::json!({"start": pos, "end": pos})
    }

    fn deadline() -> std::time::Instant {
        std::time::Instant::now() + Duration::from_secs(10)
    }

    /// Ports Go `TestCallTrail`.
    #[test]
    fn call_trail() {
        let dir = crate::testutil::tempdir("calltrail");
        std::fs::write(
            dir.path().join("a.go"),
            "package a\n\nfunc A() { C(); B() }\n\nfunc B() {}\n",
        )
        .unwrap();
        let a_uri = crate::lsp::path_to_uri(&dir.path().join("a.go"));
        let ext_path = "/usr/lib/go/src/fmt/print.go";
        let ext_uri = crate::lsp::path_to_uri(std::path::Path::new(ext_path));
        let sent: std::sync::Arc<Mutex<Vec<(String, String)>>> =
            std::sync::Arc::new(Mutex::new(Vec::new()));
        let seen = sent.clone();
        let m = fake_call_server(dir.path(), move |method, params| {
            seen.lock()
                .unwrap()
                .push((method.to_string(), params.to_string()));
            match method {
                "textDocument/prepareCallHierarchy" => {
                    serde_json::json!([call_item("B", &a_uri, 4)])
                }
                "callHierarchy/incomingCalls" => serde_json::json!([
                    {"from": call_item("A", &a_uri, 2),
                     "fromRanges": [site_range(2), site_range(2)]},
                    {"from": call_item("Println", &ext_uri, 9),
                     "fromRanges": [site_range(12)]},
                ]),
                "callHierarchy/outgoingCalls" => serde_json::json!([
                    {"to": call_item("B", &a_uri, 4), "fromRanges": [site_range(2)]},
                    {"to": call_item("C", &ext_uri, 0), "fromRanges": [site_range(1)]},
                ]),
                _ => serde_json::Value::Null,
            }
        });

        let abs = dir.path().join("a.go");
        let roots = m
            .prepare_calls(deadline(), &abs, "a.go", 5, 5)
            .expect("prepare");
        assert_eq!(roots.len(), 1);
        let b = &roots[0];
        assert_eq!(
            (b.name.as_str(), b.path.as_str(), b.line, b.kind.as_str()),
            ("B", "a.go", 5, "func")
        );

        let callers = m
            .calls(deadline(), "a.go", &b.item, false)
            .expect("incoming");
        assert_eq!(callers.len(), 2);
        let echoed = sent.lock().unwrap();
        assert!(
            echoed.iter().any(|(method, params)| {
                method == "callHierarchy/incomingCalls" && params.contains("\"token\":\"B\"")
            }),
            "item was not echoed verbatim to the server"
        );
        drop(echoed);
        // Sorted by path: the absolute external path sorts before "a.go".
        let (ext, a) = (&callers[0], &callers[1]);
        assert!(ext.ext && ext.path == ext_path && m.allowed(std::path::Path::new(ext_path)));
        assert_eq!(a.name, "A");
        assert_eq!(a.site_path, "a.go");
        assert_eq!(a.sites, vec![3]);

        let callees = m
            .calls(deadline(), "a.go", &a.item, true)
            .expect("outgoing");
        assert_eq!(callees.len(), 2);
        assert_eq!(
            (callees[0].name.as_str(), callees[1].name.as_str()),
            ("C", "B")
        );
        assert_eq!(callees[0].site_path, "a.go");
        assert_eq!(callees[0].sites, vec![2]);
    }

    /// An item arrives from the browser, so its path must not open the
    /// filesystem. Ports Go `TestCallTrailForgedItem`.
    #[test]
    fn call_trail_forged_item() {
        let dir = crate::testutil::tempdir("callforged");
        let m = fake_call_server(dir.path(), |_, _| serde_json::json!([]));
        let forged = serde_json::to_string(&call_item(
            "x",
            &crate::lsp::path_to_uri(std::path::Path::new("/etc/passwd")),
            0,
        ))
        .unwrap();
        m.calls(deadline(), "a.go", &forged, false)
            .expect("forged external item");
        assert!(!m.allowed(std::path::Path::new("/etc/passwd")));
        assert!(m
            .calls(deadline(), "a.go", r#"{"name":"x"}"#, false)
            .is_err());
    }

    #[test]
    fn site_lines_dedupe_and_sort() {
        let range = |line: i64| LspRange {
            start: LspPosition { line, character: 0 },
            end: LspPosition { line, character: 1 },
        };
        assert_eq!(site_lines(&[range(5), range(3), range(5)]), vec![4, 6]);
        assert!(site_lines(&[]).is_empty());
        let node = CallNode {
            name: "f".to_string(),
            detail: String::new(),
            kind: "func".to_string(),
            path: "a.go".to_string(),
            line: 1,
            ext: false,
            site_path: String::new(),
            sites: vec![9, 4],
            item: "{}".to_string(),
        };
        assert_eq!(first_site(&node), 9);
    }
}
