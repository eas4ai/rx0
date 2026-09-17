//! Windowed syntax highlighting with a process-wide document cache.
//!
//! Ports `highlight.go`: line table, 1000-line windows with 400 lines of
//! context, 512 KB window cap, background full-file pass under 2 MB,
//! byte-budgeted LRU eviction, and the short CSS class alphabet the
//! themes style.
//!
//! The lexer differs by decision (theme-compatible, not Chroma-exact):
//! syntect instead of Chroma. Only short class names cross the wire, so
//! the themes keep working; token boundaries will not match Go exactly.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Once, OnceLock};

use syntect::parsing::{ParseState, ScopeStack, SyntaxReference, SyntaxSet};

pub const MAX_FILE_BYTES: u64 = 64 << 20;
pub const HL_CHUNK: usize = 1000;
const HL_CONTEXT: usize = 400;
const HL_WINDOW_BYTES: usize = 512 << 10;
const MAX_REPORTED_COLS: usize = 20000;
const BG_LIMIT: usize = 2 << 20;
const CACHE_BUDGET: u64 = 512 << 20;

/// Short class names the themes style. Any lexer may produce them; nothing
/// else may cross the wire.
pub const KNOWN_CLASSES: &[&str] = &[
    "kt", "k", "nf", "nv", "nb", "nc", "no", "np", "na", "nt", "nd", "err", "gi", "gd", "gh", "ge",
    "gs", "s", "m", "o", "p", "cp", "c", "g",
];

pub(crate) fn syntax_set() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(SyntaxSet::load_defaults_newlines)
}

/// Syntax for a fence info string: by extension first, then a
/// case-insensitive name scan. Approximates Chroma `lexers.Get` (name or
/// alias); unlabelled or unknown fences stay plain.
pub fn syntax_for_fence(lang: &str) -> Option<&'static SyntaxReference> {
    if lang.is_empty() {
        return None;
    }
    let set = syntax_set();
    if let Some(s) = set.find_syntax_by_extension(lang) {
        return Some(s);
    }
    set.syntaxes()
        .iter()
        .find(|s| s.name.eq_ignore_ascii_case(lang))
        .map(|s| {
            let name = s.name.clone();
            set.find_syntax_by_name(&name).unwrap()
        })
}

/// Highlight one fenced block: exactly the code view's token classes,
/// lines joined with `\n`. Ports Go `highlightFence`.
pub fn highlight_code_block(code: &str, lang: &str) -> String {
    let code = code.strip_suffix('\n').unwrap_or(code);
    let want = code.matches('\n').count() + 1;
    let syntax = if code.len() <= 256 << 10 {
        syntax_for_fence(lang)
    } else {
        None
    };
    highlight_lines(syntax, code, want).join("\n")
}

fn escape_into(out: &mut String, text: &str) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
}

pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    escape_into(&mut out, text);
    out
}

/// Tab stops the UI uses. Ports Go `visualWidth`.
pub fn visual_width(line: &str) -> usize {
    let mut n = 0;
    for r in line.chars() {
        if r == '\t' {
            n += 4 - n % 4;
        } else {
            n += 1;
        }
    }
    n
}

/// Map a syntect scope stack onto px0's short class. Comment and string
/// states win wherever they appear (delimiters belong to the construct);
/// otherwise the innermost specific scope decides. Approximate by design:
/// syntect grammars are not Chroma lexers.
fn scope_class(stack: &ScopeStack) -> &'static str {
    let scopes: Vec<String> = stack.as_slice().iter().map(|s| s.build_string()).collect();
    if scopes.iter().any(|s| s.starts_with("comment.")) {
        return "c";
    }
    if scopes.iter().any(|s| s.starts_with("string.")) {
        return "s";
    }
    for scope in scopes.iter().rev() {
        let s = scope.as_str();
        let class = if s.starts_with("keyword.operator") {
            "o"
        } else if s.starts_with("keyword.") {
            "k"
        } else if s.starts_with("storage.type") {
            // Sublime puts declaration keywords (`func`) here alongside
            // type names; Chroma calls the former Keyword. The type
            // class wins: it is right for `int` et al.
            "kt"
        } else if s.starts_with("storage.") {
            "k"
        } else if s.starts_with("entity.name.function") {
            "nf"
        } else if s.starts_with("entity.name.tag") {
            "nt"
        } else if s.starts_with("entity.other.attribute-name") {
            "na"
        } else if s.starts_with("entity.name.type")
            || s.starts_with("entity.name.class")
            || s.starts_with("entity.name.struct")
            || s.starts_with("entity.name.enum")
            || s.starts_with("entity.name.trait")
            || s.starts_with("entity.name.interface")
            || s.starts_with("entity.name.namespace")
        {
            "nc"
        } else if s.starts_with("entity.name.") {
            "nv"
        } else if s.starts_with("variable.other.property")
            || s.starts_with("variable.other.member")
            || s.starts_with("variable.other.object")
        {
            "np"
        } else if s.starts_with("variable.") {
            "nv"
        } else if s.starts_with("constant.numeric") {
            "m"
        } else if s.starts_with("constant.character") {
            "s"
        } else if s.starts_with("constant.") {
            "no"
        } else if s.starts_with("meta.preprocessor") || s.starts_with("meta.annotation") {
            "cp"
        } else if s.starts_with("markup.heading") {
            "gh"
        } else if s.starts_with("markup.bold") {
            "gs"
        } else if s.starts_with("markup.italic") {
            "ge"
        } else if s.starts_with("markup.inserted") {
            "gi"
        } else if s.starts_with("markup.deleted") {
            "gd"
        } else if s.starts_with("markup.quote") {
            "g"
        } else if s.starts_with("punctuation.") {
            "p"
        } else if s.starts_with("invalid.") {
            "err"
        } else {
            continue;
        };
        return class;
    }
    ""
}

fn emit_token(out: &mut String, text: &str, class: &str) {
    if text.is_empty() {
        return;
    }
    if class.is_empty() {
        escape_into(out, text);
        return;
    }
    out.push_str("<i class=");
    out.push_str(class);
    out.push('>');
    escape_into(out, text);
    out.push_str("</i>");
}

/// Render one contiguous run of source into exactly `want` HTML lines, one
/// `<i class=…>` per token. Ports Go `highlightLines` (plus the nil-lexer
/// plain path, which syntect expresses as no syntax).
fn highlight_lines(syntax: Option<&SyntaxReference>, src: &str, want: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(want);
    let mut line_html = String::with_capacity(256);
    // A fresh state per segment mirrors Go's window approximation: the
    // leading context puts the lexer in roughly the right state.
    let mut state = syntax.map(ParseState::new);
    let set = syntax_set();
    // Go `strings.Split` semantics: a trailing newline leaves a final
    // empty piece, and Total counts it.
    let pieces: Vec<&str> = src.split('\n').collect();
    for piece in pieces {
        if let Some(st) = state.as_mut() {
            let mut stack = ScopeStack::new();
            // Feed the newline back: some constructs only end at EOL.
            let fed = format!("{piece}\n");
            if let Ok(ops) = st.parse_line(&fed, set) {
                // Ops carry byte POSITIONS where the stack changes; the
                // token is the text since the previous position.
                let mut byte = 0;
                for (pos, op) in ops {
                    let mut pos = pos.min(piece.len());
                    while pos < piece.len() && !piece.is_char_boundary(pos) {
                        pos += 1;
                    }
                    if pos > byte {
                        emit_token(&mut line_html, &piece[byte..pos], scope_class(&stack));
                        byte = pos;
                    }
                    let _ = stack.apply(&op);
                }
                if byte < piece.len() {
                    emit_token(&mut line_html, &piece[byte..], scope_class(&stack));
                }
            } else {
                escape_into(&mut line_html, piece);
            }
        } else {
            escape_into(&mut line_html, piece);
        }
        out.push(std::mem::replace(
            &mut line_html,
            String::with_capacity(256),
        ));
    }
    pad(out, want)
}

fn plain_fallback(src: &str, want: usize) -> Vec<String> {
    pad(src.split('\n').map(escape).collect(), want)
}

fn pad(mut out: Vec<String>, want: usize) -> Vec<String> {
    while out.len() < want {
        out.push(String::new());
    }
    out.truncate(want);
    out
}

fn size_of_lines(lines: &[String]) -> u64 {
    lines.len() as u64 * 16 + lines.iter().map(|l| l.len() as u64).sum::<u64>()
}

pub struct Doc {
    pub lang: String,
    pub max_cols: usize,
    pub total: usize,
    src: String,
    line_off: Vec<usize>,
    syntax: Option<&'static SyntaxReference>,
    chunks: Mutex<HashMap<usize, Vec<String>>>,
    full: AtomicBool,
    bg_once: Once,
    key: String,
    bytes: Mutex<u64>,
}

impl Doc {
    fn new(src: String, rel: &str, key: String) -> Arc<Doc> {
        let set = syntax_set();
        let base = Path::new(rel)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        let ext = Path::new(base)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        let syntax = set
            .find_syntax_by_extension(ext)
            .or_else(|| set.find_syntax_by_first_line(&src))
            .filter(|s| s.name != "Plain Text");
        let lang = syntax
            .map(|s| s.name.clone())
            .unwrap_or_else(|| "plain text".to_string());
        let raw: Vec<&str> = src.split('\n').collect();
        let mut line_off = Vec::with_capacity(raw.len() + 1);
        let mut max_cols = 0;
        let mut off = 0;
        for line in &raw {
            line_off.push(off);
            off += line.len() + 1;
            max_cols = max_cols.max(visual_width(line));
        }
        line_off.push(src.len());
        let bytes = src.len() as u64 + raw.len() as u64 * 32;
        Arc::new(Doc {
            lang,
            max_cols: max_cols.min(MAX_REPORTED_COLS),
            total: raw.len(),
            src,
            line_off,
            syntax,
            chunks: Mutex::new(HashMap::new()),
            full: AtomicBool::new(false),
            bg_once: Once::new(),
            key,
            bytes: Mutex::new(bytes),
        })
    }

    /// Source text of a 1-based line, without its newline. Ports Go `Doc.Raw`.
    pub fn raw(&self, n: usize) -> &str {
        if n < 1 || n > self.total {
            return "";
        }
        let start = self.line_off[n - 1];
        let mut end = self.line_off[n];
        if end > start && end <= self.src.len() && end > 0 && self.src.as_bytes()[end - 1] == b'\n'
        {
            end -= 1;
        }
        let end = end.min(self.src.len());
        if start > end {
            return "";
        }
        &self.src[start..end]
    }

    /// Highlighted HTML for the half-open line range `[start, end)`, plus
    /// whether every line came from the full-file pass. Ports Go
    /// `Doc.Lines`.
    pub fn lines(self: &Arc<Doc>, start: usize, end: usize) -> (Vec<String>, bool) {
        let end = end.min(self.total);
        if self.src.len() <= BG_LIMIT {
            let this = self.clone();
            self.bg_once.call_once(|| {
                std::thread::spawn(move || this.background_pass());
            });
        }
        if start >= end {
            return (Vec::new(), true);
        }
        let mut exact = true;
        let mut out = Vec::with_capacity(end - start);
        let mut c = start / HL_CHUNK;
        while c * HL_CHUNK < end {
            let (lines, ok) = self.chunk(c);
            exact = exact && ok;
            let base = c * HL_CHUNK;
            let lo = start.saturating_sub(base);
            let hi = (end - base).min(lines.len());
            if lo < hi {
                out.extend_from_slice(&lines[lo..hi]);
            }
            c += 1;
        }
        (out, exact)
    }

    /// Whether the full-file pass finished, and whether one is coming.
    /// Ports Go `Doc.Exact`.
    pub fn exact_state(&self) -> (bool, bool) {
        (
            self.full.load(Ordering::Acquire),
            self.src.len() <= BG_LIMIT,
        )
    }

    fn chunk(self: &Arc<Doc>, c: usize) -> (Vec<String>, bool) {
        {
            let chunks = self.chunks.lock().unwrap();
            if let Some(v) = chunks.get(&c) {
                return (v.clone(), self.full.load(Ordering::Acquire));
            }
        }
        let start = c * HL_CHUNK;
        let end = start.saturating_add(HL_CHUNK).min(self.total);
        let mut from = start.saturating_sub(HL_CONTEXT);
        let mut to = (end + HL_CONTEXT).min(self.total);
        if self.line_off[to] - self.line_off[from] > HL_WINDOW_BYTES {
            from = start;
            to = end;
        }
        let seg = &self.src[self.line_off[from]..self.line_off[to]];
        let rendered = if seg.len() > HL_WINDOW_BYTES {
            plain_fallback(seg, to - from)
        } else {
            highlight_lines(self.syntax, seg, to - from)
        };
        let v = rendered[start - from..end - from].to_vec();
        {
            let mut chunks = self.chunks.lock().unwrap();
            if self.full.load(Ordering::Acquire) {
                let full = chunks.get(&c).cloned().unwrap_or(v);
                return (full, true);
            }
            let had = chunks.contains_key(&c);
            chunks.insert(c, v.clone());
            drop(chunks);
            if !had {
                grow(&self.key, size_of_lines(&v));
            }
        }
        (v, self.full.load(Ordering::Acquire))
    }

    fn background_pass(self: Arc<Doc>) {
        let all = highlight_lines(self.syntax, &self.src, self.total);
        let mut before = 0;
        {
            for v in self.chunks.lock().unwrap().values() {
                before += size_of_lines(v);
            }
        }
        let mut after = 0;
        {
            let mut chunks = self.chunks.lock().unwrap();
            let mut c = 0;
            while c * HL_CHUNK < self.total {
                let end = ((c + 1) * HL_CHUNK).min(self.total);
                let v = all[c * HL_CHUNK..end].to_vec();
                after += size_of_lines(&v);
                chunks.insert(c, v);
                c += 1;
            }
        }
        self.full.store(true, Ordering::Release);
        if after > before {
            grow(&self.key, after - before);
        }
    }
}

struct CacheEntry {
    doc: Arc<Doc>,
    bytes: u64,
    seq: u64,
}

struct Cache {
    items: HashMap<String, CacheEntry>,
    used: u64,
    seq: u64,
}

impl Cache {
    fn get(&mut self, key: &str) -> Option<Arc<Doc>> {
        let seq = self.seq + 1;
        self.seq = seq;
        self.items.get_mut(key).map(|e| {
            e.seq = seq;
            e.doc.clone()
        })
    }

    fn put(&mut self, key: String, doc: Arc<Doc>) {
        let seq = self.seq + 1;
        self.seq = seq;
        if let Some(e) = self.items.get_mut(&key) {
            e.seq = seq;
            return;
        }
        let bytes = *doc.bytes.lock().unwrap();
        self.used += bytes;
        self.items.insert(key, CacheEntry { doc, bytes, seq });
        self.evict();
    }

    fn grow(&mut self, key: &str, delta: u64) {
        if key.is_empty() || delta == 0 {
            return;
        }
        if let Some(e) = self.items.get_mut(key) {
            e.bytes += delta;
            *e.doc.bytes.lock().unwrap() += delta;
            self.used += delta;
            self.evict();
        }
    }

    /// Drop least-recently-used documents until the budget fits; the most
    /// recent entry never goes, so the document being read survives.
    fn evict(&mut self) {
        while self.used > CACHE_BUDGET && self.items.len() > 1 {
            let victim = self
                .items
                .iter()
                .min_by_key(|(_, e)| e.seq)
                .map(|(k, _)| k.clone());
            match victim {
                Some(k) => {
                    if let Some(e) = self.items.remove(&k) {
                        self.used -= e.bytes;
                    }
                }
                None => break,
            }
        }
    }

    fn remove(&mut self, abs: &str) -> bool {
        let prefix = format!("{abs}|");
        let dead: Vec<String> = self
            .items
            .keys()
            .filter(|k| k.as_str() == abs || k.starts_with(&prefix))
            .cloned()
            .collect();
        let mut removed = false;
        for k in dead {
            if let Some(e) = self.items.remove(&k) {
                self.used -= e.bytes;
                removed = true;
            }
        }
        removed
    }
}

fn cache() -> &'static Mutex<Cache> {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    CACHE.get_or_init(|| {
        Mutex::new(Cache {
            items: HashMap::new(),
            used: 0,
            seq: 0,
        })
    })
}

fn grow(key: &str, delta: u64) {
    cache().lock().unwrap().grow(key, delta);
}

/// Load a file ready to serve line windows from, memoised on
/// path+mtime+size. Ports Go `Open`.
pub fn open(abs: &Path, rel: &str) -> Result<Arc<Doc>, String> {
    let meta = std::fs::metadata(abs).map_err(|e| e.to_string())?;
    if meta.is_dir() {
        return Err("is a directory".to_string());
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(format!("file too large ({} bytes)", meta.len()));
    }
    let mtime = meta.modified().map_err(|e| e.to_string())?;
    let nanos = mtime
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let key = format!("{}|{nanos}|{}", abs.display(), meta.len());
    if let Some(doc) = cache().lock().unwrap().get(&key) {
        return Ok(doc);
    }
    let src = std::fs::read_to_string(abs).map_err(|e| e.to_string())?;
    let doc = Doc::new(src, rel, key.clone());
    cache().lock().unwrap().put(key, doc.clone());
    Ok(doc)
}

/// Drop a file from the cache by absolute path. Ports Go `Evict`.
pub fn evict(abs: &str) -> bool {
    cache().lock().unwrap().remove(abs)
}

/// A throwaway document for a hover signature, never cached. Ports Go
/// `newDoc` (which likewise bypasses the cache).
pub(crate) fn snippet_doc(src: &str, rel: &str) -> Arc<Doc> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    Doc::new(src.to_string(), rel, format!("lsp-snippet-{id}"))
}

/// Image extensions served as metadata instead of text. Ports Go `imageExt`.
pub fn is_image_ext(ext: &str) -> bool {
    matches!(
        ext,
        ".png" | ".jpg" | ".jpeg" | ".gif" | ".webp" | ".svg" | ".ico" | ".bmp" | ".avif"
    )
}

/// Markdown extensions. Ports Go `isMarkdown`.
pub fn is_markdown(rel: &str) -> bool {
    matches!(
        Path::new(rel)
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref(),
        Some("md") | Some("markdown")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn go_doc() -> Arc<Doc> {
        Doc::new(
            "package main\n\nfunc main() {\n\tprintln(\"hi\") // greet\n}\n".to_string(),
            "main.go",
            String::new(),
        )
    }

    #[test]
    fn lang_detection_names_languages() {
        assert_eq!(go_doc().lang, "Go");
        let rs = Doc::new("fn main() {}".to_string(), "x.rs", String::new());
        assert_eq!(rs.lang, "Rust");
        let plain = Doc::new("hello".to_string(), "LICENSE", String::new());
        assert_eq!(plain.lang, "plain text");
    }

    #[test]
    fn raw_lines_split_like_go() {
        let d = go_doc();
        // Trailing newline leaves a final empty piece, counted in Total.
        assert_eq!(d.total, 6);
        assert_eq!(d.raw(1), "package main");
        assert_eq!(d.raw(5), "}");
        assert_eq!(d.raw(6), "");
        assert_eq!(d.raw(0), "");
        assert_eq!(d.raw(99), "");
    }

    #[test]
    fn visual_width_expands_tabs() {
        assert_eq!(visual_width("\t"), 4);
        assert_eq!(visual_width("ab\t"), 4);
        assert_eq!(visual_width("abcd\t"), 8);
    }

    #[test]
    fn classes_stay_in_alphabet() {
        let (lines, _) = go_doc().lines(0, 6);
        assert_eq!(lines.len(), 6);
        for line in &lines {
            let mut rest = line.as_str();
            while let Some(i) = rest.find("<i class=") {
                rest = &rest[i + 9..];
                let end = rest.find('>').unwrap();
                let class = &rest[..end];
                assert!(KNOWN_CLASSES.contains(&class), "unknown class {class}");
                rest = &rest[end..];
            }
        }
        assert!(lines.iter().any(|l| l.contains("<i class=")));
    }

    #[test]
    fn multibyte_chars_never_split_tokens() {
        // A regex op once split '·' mid-char and panicked the worker.
        let d = Doc::new(
            "\t\tuiStatus(\"info\", \"%q · %d\", q)\n".to_string(),
            "x.go",
            String::new(),
        );
        let (lines, _) = d.lines(0, 1);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains('·'));
    }

    #[test]
    fn windows_cover_ranges_exactly() {
        let src = (0..2500)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let d = Doc::new(src, "big.txt", String::new());
        let (lines, _) = d.lines(999, 1002);
        assert_eq!(lines.len(), 3);
        assert!(lines[0].contains("line 999"));
        let (all, _) = d.lines(0, d.total);
        assert_eq!(all.len(), d.total);
    }

    #[test]
    fn escaping_never_breaks_markup() {
        // Plain text takes the escape path verbatim; a real HTML lexer
        // tokenises tags instead (same as Chroma).
        let d = Doc::new("a < b && c > d".to_string(), "data.txt", String::new());
        assert_eq!(d.lang, "plain text");
        let (lines, _) = d.lines(0, 1);
        assert_eq!(lines[0], "a &lt; b &amp;&amp; c &gt; d");
    }

    #[test]
    fn cache_memoises_and_evicts() {
        let dir = std::env::temp_dir().join(format!("px0-hl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.go");
        std::fs::write(&file, "package a\n").unwrap();
        let rel = "a.go";
        let a = open(&file, rel).unwrap();
        let b = open(&file, rel).unwrap();
        assert!(Arc::ptr_eq(&a, &b));
        assert!(evict(&file.to_string_lossy()));
        let c = open(&file, rel).unwrap();
        assert!(!Arc::ptr_eq(&a, &c));
        assert!(evict(&file.to_string_lossy()));
        assert!(!evict(&file.to_string_lossy()));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
