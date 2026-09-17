//! Classification-based `.gitignore` engine.
//!
//! Ports `ignore.go`: most patterns are a bare name (`node_modules`) or a
//! suffix (`*.pyc`) answered by comparing path segments; only the rest need
//! a regexp, guarded by literal prefix/must-substring pre-tests.

use regex::Regex;
use std::path::Path;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RuleKind {
    /// Wildcards no shortcut covers; guarded by prefix/must pre-tests.
    Regex,
    /// A literal name, matched against each segment.
    SegEq,
    /// `*suffix`, matched against each segment.
    SegSuffix,
    /// A literal path, anchored at the root.
    PathEq,
}

struct Rule {
    kind: RuleKind,
    /// Literal name, suffix, or path for the fast kinds.
    lit: String,
    /// Literal head of an anchored pattern, for Regex.
    prefix: String,
    /// Longest wildcard-free run the path must contain, for Regex.
    must: String,
    /// Matches the pattern itself.
    re: Option<Regex>,
    /// Matches anything beneath a matched directory.
    sub: Option<Regex>,
    negate: bool,
    dir_only: bool,
}

impl Rule {
    /// Reports whether `rel` (slash-separated, relative to the scan root)
    /// is matched, either as the entry itself or nested under a matched
    /// directory. Ports Go `rule.hit`.
    fn hit(&self, rel: &str, is_dir: bool) -> bool {
        match self.kind {
            RuleKind::SegEq => seg_match(rel, self.dir_only, is_dir, |seg| seg == self.lit),
            RuleKind::SegSuffix => seg_match(rel, self.dir_only, is_dir, |seg| {
                seg.ends_with(self.lit.as_str())
            }),
            RuleKind::PathEq => {
                if rel == self.lit {
                    return !self.dir_only || is_dir;
                }
                rel.len() > self.lit.len()
                    && rel.starts_with(self.lit.as_str())
                    && rel.as_bytes()[self.lit.len()] == b'/'
            }
            RuleKind::Regex => {
                if !self.prefix.is_empty() && !rel.starts_with(self.prefix.as_str()) {
                    return false;
                }
                if !self.must.is_empty() && !rel.contains(self.must.as_str()) {
                    return false;
                }
                let re_hit = self.re.as_ref().is_some_and(|r| r.is_match(rel));
                let sub_hit = self.sub.as_ref().is_some_and(|r| r.is_match(rel));
                (re_hit && (!self.dir_only || is_dir)) || sub_hit
            }
        }
    }
}

/// Walks the `/`-separated segments of `rel`. A match on the final segment
/// is the entry itself (a `dir/` rule then needs a directory); a match on
/// any earlier segment is an ancestor directory, which takes the subtree.
/// Ports Go `rule.segMatch`.
fn seg_match(rel: &str, dir_only: bool, is_dir: bool, eq: impl Fn(&str) -> bool) -> bool {
    let mut start = 0;
    let bytes = rel.as_bytes();
    let mut i = 0;
    while i <= bytes.len() {
        if i < bytes.len() && bytes[i] != b'/' {
            i += 1;
            continue;
        }
        let last = i == bytes.len();
        if eq(&rel[start..i]) && (!last || !dir_only || is_dir) {
            return true;
        }
        start = i + 1;
        i += 1;
    }
    false
}

/// Decides which evaluation a pattern qualifies for. Only `*` and `?` are
/// wildcards; every other character is literal. Ports Go `classify`.
fn classify(pattern: &str, anchored: bool) -> (RuleKind, String) {
    let wild = pattern.contains(['*', '?']);
    if anchored {
        if !wild {
            return (RuleKind::PathEq, pattern.to_string());
        }
        return (RuleKind::Regex, String::new());
    }
    if !wild {
        return (RuleKind::SegEq, pattern.to_string());
    }
    if let Some(rest) = pattern.strip_prefix('*') {
        if !rest.contains(['*', '?']) {
            return (RuleKind::SegSuffix, rest.to_string());
        }
    }
    (RuleKind::Regex, String::new())
}

/// Leading run of a pattern with no wildcard. Ports Go `literalHead`.
fn literal_head(pattern: &str) -> &str {
    match pattern.find(['*', '?']) {
        Some(i) => &pattern[..i],
        None => pattern,
    }
}

/// Longest wildcard-free run in a pattern, which any matching path must
/// contain verbatim. A leading `/` is dropped. Ports Go `literalRun`.
fn literal_run(pattern: &str) -> String {
    let mut best = String::new();
    for mut part in pattern.split(['*', '?']) {
        part = part.strip_prefix('/').unwrap_or(part);
        if part.len() > best.len() {
            best = part.to_string();
        }
    }
    best
}

/// Quote one character for the regex builder, mirroring Go
/// `regexp.QuoteMeta` on a single byte/char.
fn quote_char(c: char, out: &mut String) {
    const META: &str = r"\.+*?()|[]{}^$";
    if c.is_ascii() && META.contains(c) {
        out.push('\\');
    }
    out.push(c);
}

/// Compile one pattern. Ports Go `compile` (fast path always on; the
/// `compilePatternRegex` differential twin has no runtime role).
fn compile(pattern: &str) -> Option<Rule> {
    let mut p = pattern.trim_end_matches(' ');
    if p.is_empty() || p.starts_with('#') {
        return None;
    }
    let mut rule = Rule {
        kind: RuleKind::Regex,
        lit: String::new(),
        prefix: String::new(),
        must: String::new(),
        re: None,
        sub: None,
        negate: false,
        dir_only: false,
    };
    if let Some(rest) = p.strip_prefix('!') {
        rule.negate = true;
        p = rest;
    }
    if let Some(rest) = p.strip_suffix('/') {
        rule.dir_only = true;
        p = rest;
    }
    // A pattern without an interior slash matches at any depth.
    let anchored = p.trim_end_matches('/').contains('/');
    p = p.strip_prefix('/').unwrap_or(p);

    let (kind, lit) = classify(p, anchored);
    if kind != RuleKind::Regex {
        rule.kind = kind;
        rule.lit = lit;
        return Some(rule);
    }
    if anchored {
        rule.prefix = literal_head(p).to_string();
    }
    rule.must = literal_run(p);

    let mut body = String::from("^");
    if !anchored {
        body.push_str("(?:.*/)?");
    }
    let chars: Vec<char> = p.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '*' => {
                if i + 1 < chars.len() && chars[i + 1] == '*' {
                    // "**/" spans directories, bare "**" spans anything.
                    if i + 2 < chars.len() && chars[i + 2] == '/' {
                        body.push_str("(?:.*/)?");
                        i += 3;
                    } else {
                        body.push_str(".*");
                        i += 2;
                    }
                } else {
                    body.push_str("[^/]*");
                    i += 1;
                }
            }
            '?' => {
                body.push_str("[^/]");
                i += 1;
            }
            c => {
                quote_char(c, &mut body);
                i += 1;
            }
        }
    }
    // `\z` is end-of-haystack only; Go `$` (no `m` flag) is end-of-text,
    // while Rust `$` also matches before a trailing newline.
    let re = Regex::new(&format!("{body}\\z")).ok()?;
    let sub = Regex::new(&format!("{body}/.*\\z")).ok()?;
    rule.re = Some(re);
    rule.sub = Some(sub);
    Some(rule)
}

/// The stack of rules applying at a directory, outermost-first; later rules
/// win, matching git semantics. Ports Go `ignoreSet`.
#[derive(Default)]
pub struct IgnoreSet {
    rules: Vec<Rule>,
}

impl IgnoreSet {
    /// Defaults plus extras, mirroring Go `newIgnoreSet`.
    pub fn new(extra: &[String]) -> Self {
        let mut set = Self::default();
        set.add_patterns(DEFAULT_IGNORES.iter().copied());
        set.add_patterns(extra.iter().map(String::as_str));
        set
    }

    /// Inherit parent rules and append a subdirectory's patterns. Ports Go
    /// `ignoreSet.child`. The walker only calls this with non-empty
    /// patterns and otherwise clones its `Arc<IgnoreSet>`, matching Go
    /// returning the same pointer.
    pub fn child_set(&self, patterns: &[String]) -> IgnoreSet {
        let mut rules = Vec::with_capacity(self.rules.len() + patterns.len());
        rules.extend(self.rules.iter().map(clone_rule));
        let mut child = Self { rules };
        child.add_patterns(patterns.iter().map(String::as_str));
        child
    }

    fn add_patterns<'a>(&mut self, patterns: impl IntoIterator<Item = &'a str>) {
        for p in patterns {
            if let Some(rule) = compile(p) {
                self.rules.push(rule);
            }
        }
    }

    /// Whether `rel` (slash-separated, scan-root-relative) is ignored.
    /// Ports Go `ignoreSet.match`.
    pub fn matches(&self, rel: &str, is_dir: bool) -> bool {
        let mut ignored = false;
        for rule in &self.rules {
            if rule.hit(rel, is_dir) {
                ignored = !rule.negate;
            }
        }
        ignored
    }
}

/// A single compiled glob for path filtering (workspace search `--glob`,
/// match-through-rule semantics). A plain pattern such as `server.go` is
/// answered without a regexp, exactly like Go `compilePattern`.
pub struct GlobRule {
    inner: Rule,
}

impl GlobRule {
    pub fn compile(pattern: &str) -> Option<Self> {
        compile(pattern).map(|inner| Self { inner })
    }

    /// Files are never directories in this use. Ports the
    /// `s.glob.hit(f.Path, false)` call in `SearchContext`.
    pub fn hit(&self, rel: &str) -> bool {
        self.inner.hit(rel, false)
    }
}

fn clone_rule(rule: &Rule) -> Rule {
    Rule {
        kind: rule.kind,
        lit: rule.lit.clone(),
        prefix: rule.prefix.clone(),
        must: rule.must.clone(),
        re: rule.re.clone(),
        sub: rule.sub.clone(),
        negate: rule.negate,
        dir_only: rule.dir_only,
    }
}

pub const DEFAULT_IGNORES: &[&str] = &[
    ".git/",
    ".hg/",
    ".svn/",
    "node_modules/",
    ".venv/",
    "venv/",
    "__pycache__/",
    "target/",
    "dist/",
    "build/",
    ".next/",
    ".nuxt/",
    "vendor/",
    ".idea/",
    ".vscode/",
    ".mypy_cache/",
    ".pytest_cache/",
    ".ruff_cache/",
    ".gradle/",
    ".tox/",
    "*.pyc",
    "*.class",
    "*.o",
    "*.so",
    "*.dylib",
    "*.a",
    "*.exe",
    "*.pdb",
    ".DS_Store",
    "*.lock",
];

/// Raw patterns in `dir/.gitignore`, re-anchored to the scan root. Ports Go
/// `readGitignore`: a rule found in `sub/` only applies under `sub/`.
pub fn read_gitignore(dir: &Path, rel_dir: &str) -> Vec<String> {
    let text = std::fs::read_to_string(dir.join(".gitignore")).unwrap_or_default();
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if rel_dir.is_empty() {
            out.push(line.to_string());
            continue;
        }
        let neg = line.starts_with('!');
        let body = line
            .strip_prefix('!')
            .unwrap_or(line)
            .trim_start_matches('/');
        let scoped = format!("{rel_dir}/{body}");
        out.push(if neg { format!("!{scoped}") } else { scoped });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(patterns: &[&str]) -> IgnoreSet {
        let mut s = IgnoreSet::default();
        s.add_patterns(patterns.iter().copied());
        s
    }

    #[test]
    fn bare_name_matches_at_any_depth() {
        let s = set(&["node_modules"]);
        assert!(s.matches("node_modules", true));
        assert!(s.matches("a/node_modules", true));
        assert!(s.matches("a/node_modules/x.js", false));
        assert!(!s.matches("a/not-modules/x.js", false));
    }

    #[test]
    fn dir_only_rule_needs_a_directory() {
        let s = set(&["build/"]);
        assert!(s.matches("build", true));
        assert!(s.matches("build/x", false));
        assert!(!s.matches("build", false));
        assert!(!s.matches("abuild", true));
    }

    #[test]
    fn suffix_rule_matches_files_and_dirs() {
        let s = set(&["*.pyc"]);
        assert!(s.matches("a.pyc", false));
        assert!(s.matches("sub/b.pyc", false));
        assert!(!s.matches("a.py", false));
    }

    #[test]
    fn anchored_path_stays_at_root() {
        let s = set(&["dist/bundle.js"]);
        assert!(s.matches("dist/bundle.js", false));
        assert!(!s.matches("sub/dist/bundle.js", false));
        assert!(s.matches("dist/bundle.js/map", false));
    }

    #[test]
    fn negation_re_includes() {
        let s = set(&["*.log", "!keep.log"]);
        assert!(s.matches("a.log", false));
        assert!(!s.matches("keep.log", false));
        assert!(!s.matches("sub/keep.log", false));
    }

    #[test]
    fn double_star_spans_directories() {
        let s = set(&["src/**/gen"]);
        assert!(s.matches("src/gen", true));
        assert!(s.matches("src/a/b/gen", true));
        assert!(!s.matches("other/a/gen", true));
    }

    #[test]
    fn defaults_cover_tool_dirs_and_artifacts() {
        let s = IgnoreSet::new(&[]);
        for (rel, dir) in [
            (".git/config", false),
            ("node_modules/x.js", false),
            ("target/debug/px0", false),
            ("a.pyc", false),
            ("pkg/yarn.lock", false),
        ] {
            assert!(s.matches(rel, dir), "{rel} should be ignored by default");
        }
        assert!(!s.matches("src/main.rs", false));
        assert!(!s.matches("Cargo.toml", false));
    }

    #[test]
    fn child_scopes_subdir_rules() {
        let parent = IgnoreSet::new(&[]);
        let child = parent.child_set(&["sub/*.tmp".to_string()]);
        assert!(child.matches("sub/a.tmp", false));
        assert!(!parent.matches("sub/a.tmp", false));
        // Parent rules still apply in the child.
        assert!(child.matches("sub/node_modules/x.js", false));
    }
}
