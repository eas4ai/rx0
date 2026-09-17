//! Navigation over LSP answers: definitions, references, document
//! symbols, hover cards, and the UTF-16 bookkeeping between JavaScript
//! columns and server offsets.
//!
//! Ports `lspnav.go`. Positions arrive from the UI in UTF-16 code units
//! (what JavaScript string offsets count); the client converts to the
//! negotiated encoding before asking the server.

use serde::Serialize;
use serde_json::Value;
use std::path::Path;
use std::time::Instant;

use crate::lsp::{LspClient, LspLocation, LspPosition, LspRange};
use crate::lspservers::LspManager;
use crate::symbols::Symbol;

// ---------------------------------------------------------------- NavHit

/// One navigation result, shaped like a search Match so the UI renders
/// LSP answers and regex answers with the same code. Ports Go `NavHit`.
#[derive(Clone, Debug, Serialize)]
pub struct NavHit {
    pub path: String,
    pub line: usize,
    pub pre: String,
    pub mid: String,
    pub post: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub def: bool,
    /// Outside the indexed tree, e.g. stdlib.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub ext: bool,
}

/// A column measured in UTF-16 code units into a byte offset.
/// Ports Go `utf16ToByte`.
pub fn utf16_to_byte(line: &str, u16col: i64) -> usize {
    if u16col <= 0 {
        return 0;
    }
    let mut units = 0i64;
    let mut bytes = 0usize;
    for c in line.chars() {
        if units >= u16col {
            break;
        }
        units += c.len_utf16() as i64;
        bytes += c.len_utf8();
    }
    bytes.min(line.len())
}

/// Read the file through the same cache the editor uses.
fn disk_lines(abs: &Path, rel: &str) -> Vec<String> {
    match crate::highlight::open(abs, rel) {
        Ok(doc) => (1..=doc.total).map(|n| doc.raw(n).to_string()).collect(),
        Err(_) => Vec::new(),
    }
}

/// Slice a line safely on any byte offsets, flooring to char
/// boundaries rather than panicking (Go would panic; the server must
/// not).
fn safe_slice(text: &str, from: usize, to: usize) -> (usize, usize) {
    let mut from = from.min(text.len());
    let mut to = to.min(text.len());
    while from > 0 && !text.is_char_boundary(from) {
        from -= 1;
    }
    while to > 0 && !text.is_char_boundary(to) {
        to -= 1;
    }
    if from > to {
        from = to;
    }
    (from, to)
}

impl LspManager {
    /// Turn LSP ranges into display-ready hits. Ports Go `resolve`.
    pub fn resolve(&self, client: &LspClient, locs: &[LspLocation], mark_def: bool) -> Vec<NavHit> {
        use std::collections::HashMap;
        let mut by_file: HashMap<(String, String, bool), Vec<LspRange>> = HashMap::new();
        let mut order: Vec<(String, String, bool)> = Vec::new();
        for l in locs {
            let abs = match crate::lsp::uri_to_path(&l.uri) {
                Ok(p) => p,
                Err(_) => continue,
            };
            // Outside the indexed tree is still worth jumping to, so
            // tree_path allowlists this exact file even though the tree
            // guard would refuse it.
            let (rel, ext) = self.tree_path(&abs, true);
            let key = (abs.to_string_lossy().into_owned(), rel, ext);
            if !by_file.contains_key(&key) {
                order.push(key.clone());
            }
            by_file.entry(key).or_default().push(l.range);
        }
        let mut out = Vec::new();
        for key in order {
            let (abs_str, rel, ext) = &key;
            let lines = disk_lines(Path::new(abs_str), rel);
            let mut ranges = by_file.remove(&key).unwrap_or_default();
            ranges.sort_by(|a, b| {
                a.start
                    .line
                    .cmp(&b.start.line)
                    .then(a.start.character.cmp(&b.start.character))
            });
            let mut seen = std::collections::HashSet::new();
            for r in ranges {
                let (line, from) = client.from_lsp(&lines, r.start);
                if !seen.insert(line) {
                    continue; // one entry per line keeps the list readable
                }
                let text = if line >= 1 && line <= lines.len() {
                    lines[line - 1].as_str()
                } else {
                    ""
                };
                let mut to = from;
                if r.end.line == r.start.line {
                    (_, to) = client.from_lsp(&lines, r.end);
                }
                let mut from = from;
                if to <= from || to > text.len() {
                    to = text.len();
                    if from > to {
                        from = to;
                    }
                }
                let (from, to) = safe_slice(text, from, to);
                let snip = crate::search::snip(text.as_bytes(), from, to);
                out.push(NavHit {
                    path: rel.clone(),
                    line,
                    pre: snip.pre,
                    mid: snip.mid,
                    post: snip.post,
                    def: mark_def,
                    ext: *ext,
                });
            }
        }
        out
    }

    /// Run one position-based request, normalising the two shapes a
    /// server may answer with (Location or LocationLink). Ports Go
    /// `locate`, including its retry-the-other-shape fallback.
    // Seven call-site arguments, mirroring Go's `locate` one-to-one.
    #[allow(clippy::too_many_arguments)]
    pub fn locate(
        self: &std::sync::Arc<Self>,
        deadline: Instant,
        method: &str,
        abs: &Path,
        rel: &str,
        line: usize,
        u16col: i64,
        extra: Value,
    ) -> Result<Vec<NavHit>, crate::lspservers::LspError> {
        let client = self.client(deadline, rel)?;
        client
            .ensure_open(abs, rel)
            .map_err(crate::lspservers::LspError::Other)?;
        let lines = disk_lines(abs, rel);
        let line_text = if line >= 1 && line <= lines.len() {
            lines[line - 1].as_str()
        } else {
            ""
        };
        let byte_col = utf16_to_byte(line_text, u16col);
        let mut params = serde_json::json!({
            "textDocument": {"uri": crate::lsp::path_to_uri(abs)},
            "position": client.to_lsp(line_text, line, byte_col),
        });
        if let (Some(map), Value::Object(ex)) = (params.as_object_mut(), extra) {
            for (k, v) in ex {
                map.insert(k, v);
            }
        }
        let mark_def = method != "textDocument/references";
        match client.call(method, params.clone(), deadline) {
            Ok(result) => Ok(self.resolve(&client, &decode_locations(&result), mark_def)),
            Err(first) => {
                // A single Location, not an array, is also legal.
                match client.call(method, params, deadline) {
                    Ok(result) => match serde_json::from_value::<LspLocation>(result) {
                        Ok(one) if !one.uri.is_empty() => {
                            Ok(self.resolve(&client, std::slice::from_ref(&one), mark_def))
                        }
                        _ => Err(crate::lspservers::LspError::Other(first)),
                    },
                    Err(_) => Err(crate::lspservers::LspError::Other(first)),
                }
            }
        }
    }

    pub fn definition(
        self: &std::sync::Arc<Self>,
        deadline: Instant,
        abs: &Path,
        rel: &str,
        line: usize,
        col: i64,
    ) -> Result<Vec<NavHit>, crate::lspservers::LspError> {
        self.locate(
            deadline,
            "textDocument/definition",
            abs,
            rel,
            line,
            col,
            Value::Null,
        )
    }

    pub fn references(
        self: &std::sync::Arc<Self>,
        deadline: Instant,
        abs: &Path,
        rel: &str,
        line: usize,
        col: i64,
    ) -> Result<Vec<NavHit>, crate::lspservers::LspError> {
        self.locate(
            deadline,
            "textDocument/references",
            abs,
            rel,
            line,
            col,
            serde_json::json!({"context": {"includeDeclaration": true}}),
        )
    }

    /// The document outline, flattened with indentation mirroring the
    /// server's nesting. Ports Go `Symbols`.
    pub fn symbols(
        self: &std::sync::Arc<Self>,
        deadline: Instant,
        abs: &Path,
        rel: &str,
    ) -> Result<Vec<Symbol>, crate::lspservers::LspError> {
        use crate::lsp::LspDocumentSymbol;
        let client = self.client(deadline, rel)?;
        client
            .ensure_open(abs, rel)
            .map_err(crate::lspservers::LspError::Other)?;
        let result = client
            .call(
                "textDocument/documentSymbol",
                serde_json::json!({"textDocument": {"uri": crate::lsp::path_to_uri(abs)}}),
                deadline,
            )
            .map_err(crate::lspservers::LspError::Other)?;
        let raw: Vec<LspDocumentSymbol> = serde_json::from_value(result)
            .map_err(|e| crate::lspservers::LspError::Other(e.to_string()))?;
        let mut out = Vec::new();
        fn walk(syms: &[LspDocumentSymbol], depth: usize, out: &mut Vec<Symbol>) {
            for s in syms {
                let mut rng = s.selection_range;
                if let Some(loc) = &s.location {
                    rng = loc.range; // symbolInformation form
                }
                out.push(Symbol {
                    name: s.name.clone(),
                    kind: crate::lsp::symbol_kind_name(s.kind).to_string(),
                    line: (rng.start.line + 1).max(1) as usize,
                    indent: depth * 2,
                });
                if !s.children.is_empty() {
                    walk(&s.children, depth + 1, out);
                }
            }
        }
        walk(&raw, 0, &mut out);
        // symbolInformation comes back unordered often enough to fix.
        out.sort_by_key(|s| s.line);
        Ok(out)
    }

    /// Ask the server what a position is and render the answer.
    /// Ports Go `Hover`.
    pub fn hover(
        self: &std::sync::Arc<Self>,
        deadline: Instant,
        abs: &Path,
        rel: &str,
        line: usize,
        col: i64,
    ) -> Result<HoverInfo, crate::lspservers::LspError> {
        let client = self.client(deadline, rel)?;
        client
            .ensure_open(abs, rel)
            .map_err(crate::lspservers::LspError::Other)?;
        let lines = disk_lines(abs, rel);
        let line_text = if line >= 1 && line <= lines.len() {
            lines[line - 1].as_str()
        } else {
            ""
        };
        let byte_col = utf16_to_byte(line_text, col);
        let result = client
            .call(
                "textDocument/hover",
                serde_json::json!({
                    "textDocument": {"uri": crate::lsp::path_to_uri(abs)},
                    "position": client.to_lsp(line_text, line, byte_col),
                }),
                deadline,
            )
            .map_err(crate::lspservers::LspError::Other)?;
        let contents = result.get("contents").cloned().unwrap_or(Value::Null);
        if contents.is_null() {
            return Ok(HoverInfo::empty());
        }
        let (code, prose) = parse_hover_contents(&contents);
        if code.is_empty() && prose.is_empty() {
            return Ok(HoverInfo::empty());
        }
        Ok(HoverInfo {
            signature: highlight_snippet(&code, rel),
            doc: prose,
            empty: false,
        })
    }
}

/// Decode a Location array that may mix Location and LocationLink
/// items. Ports the item loop in Go `locate`.
pub fn decode_locations(result: &Value) -> Vec<LspLocation> {
    let mut locs = Vec::new();
    let items = match result {
        Value::Array(items) => items.clone(),
        _ => return locs,
    };
    for item in &items {
        if let Some(uri) = item.get("uri").and_then(|u| u.as_str()) {
            locs.push(LspLocation {
                uri: uri.to_string(),
                range: decode_range(item.get("range").cloned().unwrap_or(Value::Null)),
            });
            continue;
        }
        if let Some(uri) = item.get("targetUri").and_then(|u| u.as_str()) {
            let rng = item
                .get("targetSelectionRange")
                .or_else(|| item.get("targetRange"))
                .cloned()
                .unwrap_or(Value::Null);
            locs.push(LspLocation {
                uri: uri.to_string(),
                range: decode_range(rng),
            });
        }
    }
    locs
}

pub fn decode_range(v: Value) -> LspRange {
    let pos = |key: &str| {
        let (l, c) = v
            .get(key)
            .map(|p| {
                (
                    p.get("line").and_then(|x| x.as_i64()).unwrap_or(0),
                    p.get("character").and_then(|x| x.as_i64()).unwrap_or(0),
                )
            })
            .unwrap_or((0, 0));
        LspPosition {
            line: l,
            character: c,
        }
    };
    LspRange {
        start: pos("start"),
        end: pos("end"),
    }
}

// ---------------------------------------------------------------- hover

/// What the UI shows in the hover card: a syntax-highlighted signature
/// and whatever documentation the server attached. Ports Go `HoverInfo`.
#[derive(Clone, Debug, Serialize)]
pub struct HoverInfo {
    pub signature: String,
    pub doc: String,
    pub empty: bool,
}

impl HoverInfo {
    pub fn empty() -> Self {
        Self {
            signature: String::new(),
            doc: String::new(),
            empty: true,
        }
    }
}

/// Flatten the three shapes the spec allows: a MarkupContent object, a
/// MarkedString, or an array of MarkedStrings. Ports Go
/// `parseHoverContents`.
pub fn parse_hover_contents(raw: &Value) -> (String, String) {
    // MarkupContent: {"kind": ..., "value": ...}. Note a bare string
    // also matches this shape with kind missing, so require nothing of
    // kind — Go unmarshals into the struct regardless and only checks
    // Value != "". A JSON string never unmarshals into a struct, so an
    // explicit string cannot arrive here.
    if let Value::Object(map) = raw {
        if let Some(Value::String(value)) = map.get("value") {
            if !value.is_empty() {
                return split_markdown(value);
            }
        }
        // A one-element... no: fall through to the array/string shapes.
        let _ = map;
    }
    if let Value::String(one) = raw {
        return split_markdown(one);
    }
    if let Value::Array(many) = raw {
        let mut codes = Vec::new();
        let mut proses = Vec::new();
        for item in many {
            if let Value::String(s) = item {
                let (c, p) = split_markdown(s);
                if !c.is_empty() {
                    codes.push(c);
                }
                if !p.is_empty() {
                    proses.push(p);
                }
                continue;
            }
            if let Value::Object(map) = item {
                let value = map.get("value").and_then(|v| v.as_str()).unwrap_or("");
                if value.is_empty() {
                    continue;
                }
                let lang = map.get("language").and_then(|v| v.as_str()).unwrap_or("");
                if !lang.is_empty() {
                    codes.push(value.to_string());
                } else {
                    proses.push(value.to_string());
                }
            }
        }
        return (codes.join("\n"), proses.join("\n\n"));
    }
    (String::new(), String::new())
}

/// Pull fenced code blocks out of a hover body; what is left is
/// documentation prose. Ports Go `splitMarkdown`.
pub fn split_markdown(s: &str) -> (String, String) {
    let mut codes: Vec<String> = Vec::new();
    let mut text: Vec<&str> = Vec::new();
    let mut in_fence = false;
    let mut fence: Vec<&str> = Vec::new();
    for line in s.split('\n') {
        if line.trim_start().starts_with("```") {
            if in_fence {
                codes.push(fence.join("\n"));
                fence.clear();
            }
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            fence.push(line);
        } else {
            text.push(line);
        }
    }
    if !fence.is_empty() {
        codes.push(fence.join("\n"));
    }
    let mut code = codes.join("\n").trim().to_string();
    let mut prose = strip_markdown(&text.join("\n"));

    // A plaintext-only server sends no fences. Its first line is still
    // the declaration, so promote it rather than showing nothing.
    if code.is_empty() && !prose.is_empty() {
        let (first, rest) = match prose.find('\n') {
            Some(i) => (prose[..i].to_string(), prose[i + 1..].to_string()),
            None => (prose.clone(), String::new()),
        };
        if looks_like_declaration(&first) {
            code = first;
            prose = rest.trim().to_string();
        }
    }
    const MAX_PROSE: usize = 900;
    if prose.len() > MAX_PROSE {
        // Floor to a char boundary first: byte slicing must never panic
        // (Go tolerates splitting a rune; we keep valid UTF-8 instead).
        let mut end = MAX_PROSE;
        while end > 0 && !prose.is_char_boundary(end) {
            end -= 1;
        }
        match prose[..end].rfind(' ') {
            Some(cut) if cut > 0 => prose = prose[..cut].to_string(),
            _ => prose = prose[..end].to_string(),
        }
        prose.push('…');
    }
    (code, prose)
}

/// Deliberately conservative: short, no trailing sentence punctuation,
/// carrying syntax prose would not. Ports Go `looksLikeDeclaration`.
pub fn looks_like_declaration(s: &str) -> bool {
    let s = s.trim();
    if s.is_empty() || s.len() > 200 || s.ends_with('.') {
        return false;
    }
    // Go: ContainsAny "(){}[]<>:=*&" — byte-wise. Non-ASCII bytes can
    // never match those, so chars() is equivalent here.
    s.contains(['(', ')', '{', '}', '[', ']', '<', '>', ':', '=', '*', '&']) || s.contains(' ')
}

/// Flatten a hover body into tooltip text. Ports Go `stripMarkdown`.
pub fn strip_markdown(s: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut blank = 0;
    for line in s.split('\n') {
        if is_md_rule(line) || is_md_only_link(line) {
            continue; // horizontal rules and bare "see also" links are noise
        }
        let line = strip_inline_md(line).trim_end().to_string();
        if line.trim().is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        out.push(line);
    }
    out.join("\n").trim().to_string()
}

fn strip_inline_md(line: &str) -> String {
    // [text](url) -> text. Go's RE2 has no backreferences; a small
    // scanner is the faithful equivalent.
    let mut out = String::with_capacity(line.len());
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'[' {
            if let Some(end_text) = line[i..].find(']') {
                let after = i + end_text + 1;
                if line[after..].starts_with('(') {
                    if let Some(end_url) = line[after..].find(')') {
                        out.push_str(&line[i + 1..i + end_text]);
                        i = after + end_url + 1;
                        continue;
                    }
                }
            }
            // Not a link: emit the bracket and move past it, or the
            // copy-through below would stall on the same `[` forever.
            out.push('[');
            i += 1;
            continue;
        }
        // Copy through to the next candidate: `[` is ASCII, so every
        // slice edge is a char boundary and multibyte runes stay intact.
        let next = line[i..].find('[').map(|k| i + k).unwrap_or(bytes.len());
        out.push_str(&line[i..next]);
        i = next;
    }
    out.replace("**", "").replace("__", "").replace('`', "")
}

/// `^\s*(?:(?:-\s*){3,}|(?:\*\s*){3,}|(?:_\s*){3,})$` without regex.
fn is_md_rule(line: &str) -> bool {
    let mut dash = 0;
    let mut star = 0;
    let mut under = 0;
    let mut other = false;
    for c in line.chars() {
        match c {
            '-' => dash += 1,
            '*' => star += 1,
            '_' => under += 1,
            ' ' | '\t' => {}
            _ => {
                other = true;
                break;
            }
        }
    }
    !other && (dash >= 3 || star >= 3 || under >= 3)
}

/// A line that is nothing but one markdown link.
fn is_md_only_link(line: &str) -> bool {
    let t = line.trim();
    if !(t.starts_with('[') && t.ends_with(')')) {
        return false;
    }
    if let Some(end_text) = t.find(']') {
        let after = &t[end_text + 1..];
        return after.starts_with('(') && !after[1..].contains([' ', '\t']);
    }
    false
}

const MAX_SIGNATURE_LINES: usize = 12;

/// Run a hover signature through the same lexer the file uses, so the
/// card matches the editor. Ports Go `highlightSnippet`.
pub fn highlight_snippet(code: &str, rel: &str) -> String {
    if code.is_empty() {
        return String::new();
    }
    let code = if code.split('\n').count() > MAX_SIGNATURE_LINES {
        let mut lines: Vec<&str> = code.split('\n').collect();
        lines.truncate(MAX_SIGNATURE_LINES);
        lines.push("…");
        lines.join("\n")
    } else {
        code.to_string()
    };
    let doc = crate::highlight::snippet_doc(&code, rel);
    let (out, _) = doc.lines(0, doc.total);
    out.join("\n")
}

/// Test helper: decode positions without a client.
#[cfg(test)]
pub fn from_lsp_test(lines: &[String], p: LspPosition) -> (usize, usize) {
    crate::lsp::from_lsp_with(crate::lsp::PositionEncoding::Utf16, lines, p)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ports Go `TestUTF16ToByte`.
    #[test]
    fn utf16_columns_to_bytes() {
        for (line, u16col, want) in [
            ("abc", 0, 0),
            ("abc", 2, 2),
            ("abc", 99, 3),  // past the end clamps
            ("héllo", 2, 3), // é is two bytes
            ("→x", 1, 3),    // → is three bytes, one UTF-16 unit
            ("🎉x", 2, 4),   // emoji is a surrogate pair: two units, four bytes
            ("🎉x", 3, 5),
            ("", 5, 0),
        ] {
            assert_eq!(utf16_to_byte(line, u16col), want, "{line:?} @{u16col}");
        }
    }

    #[test]
    fn hover_markup_shapes() {
        // MarkupContent with fences: code out, prose flattened.
        let (code, prose) = parse_hover_contents(&serde_json::json!({
            "kind": "markdown",
            "value": "```rust\nfn main() {}\n```\nDo things **well**.\n\n[docs](https://example.com/x)",
        }));
        assert_eq!(code, "fn main() {}");
        assert!(prose.contains("Do things well."), "{prose:?}");
        assert!(!prose.contains("]("), "{prose:?}");

        // Plain string, no fences: first declaration-like line promoted.
        let (code, prose) =
            parse_hover_contents(&serde_json::json!("func Foo(x int) int\nCount things."));
        assert_eq!(code, "func Foo(x int) int");
        assert_eq!(prose, "Count things.");

        // Array of MarkedStrings. "just prose" carries a space, so
        // Go's declaration heuristic promotes it to code as well.
        let (code, prose) = parse_hover_contents(&serde_json::json!([
            {"language": "go", "value": "func Bar()"},
            "just prose",
        ]));
        assert_eq!(code, "func Bar()\njust prose");
        assert_eq!(prose, "");

        // Empty shapes stay empty.
        assert_eq!(
            parse_hover_contents(&serde_json::json!(null)),
            (String::new(), String::new())
        );
        assert_eq!(
            parse_hover_contents(&serde_json::json!({"kind": "markdown", "value": ""})),
            (String::new(), String::new())
        );
    }

    #[test]
    fn markdown_helpers_match_go() {
        assert!(looks_like_declaration("func Foo(x int) int"));
        assert!(looks_like_declaration("use std::io;"));
        assert!(!looks_like_declaration("Count things."));
        assert!(!looks_like_declaration(""));
        assert!(!looks_like_declaration(&"x".repeat(201)));
        // Rules and bare links are noise; blank runs collapse.
        assert_eq!(
            strip_markdown("a\n\n\n---\n[see](https://e.com)\nb"),
            "a\n\nb"
        );
        assert_eq!(strip_markdown("`code` **bold** __u__"), "code bold u");
        assert_eq!(strip_markdown("[text](https://e.com/x) tail"), "text tail");
        // Non-ASCII survives the link scanner intact.
        assert_eq!(
            strip_markdown("héllo [wörld](https://e.com) →"),
            "héllo wörld →"
        );
        // Prose truncation keeps valid UTF-8 and marks the cut.
        let long = format!("{}\n{}", "word ".repeat(200), "héllo wörld → ".repeat(10));
        let (_, prose) = split_markdown(&long);
        assert!(prose.ends_with('…'));
        assert!(prose.len() <= 905);
    }

    #[test]
    fn locations_decode_both_shapes() {
        let locs = decode_locations(&serde_json::json!([
            {"uri": "file:///a.go", "range": {
                "start": {"line": 1, "character": 2}, "end": {"line": 1, "character": 5}}},
            {"targetUri": "file:///b.go",
             "targetRange": {"start": {"line": 9, "character": 0}, "end": {"line": 9, "character": 3}},
             "targetSelectionRange": {"start": {"line": 8, "character": 4}, "end": {"line": 8, "character": 7}}},
        ]));
        assert_eq!(locs.len(), 2);
        assert_eq!(locs[0].uri, "file:///a.go");
        assert_eq!(locs[0].range.start.line, 1);
        assert_eq!(locs[1].uri, "file:///b.go");
        assert_eq!(locs[1].range.start.line, 8); // selection preferred
        assert!(decode_locations(&serde_json::json!({"uri": "x"})).is_empty());
    }
}
