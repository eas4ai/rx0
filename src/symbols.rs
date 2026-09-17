//! Language families, declaration recognisers, and symbol outlines.
//!
//! Ports `symbols.go`: regex outline rules per family, markdown headings,
//! and the `declPatterns` used to float declarations above references.

use regex::Regex;
use serde::Serialize;
use std::collections::HashMap;
use std::path::Path;

const EXT_FAMILY: &[(&str, &str)] = &[
    (".go", "go"),
    (".py", "py"),
    (".pyi", "py"),
    (".js", "js"),
    (".jsx", "js"),
    (".mjs", "js"),
    (".cjs", "js"),
    (".ts", "js"),
    (".tsx", "js"),
    (".svelte", "js"),
    (".vue", "js"),
    (".rs", "rust"),
    (".java", "jvm"),
    (".kt", "jvm"),
    (".kts", "jvm"),
    (".scala", "jvm"),
    (".groovy", "jvm"),
    (".cs", "jvm"),
    (".c", "c"),
    (".h", "c"),
    (".cc", "c"),
    (".cpp", "c"),
    (".cxx", "c"),
    (".hpp", "c"),
    (".hh", "c"),
    (".m", "c"),
    (".mm", "c"),
    (".rb", "rb"),
    (".php", "php"),
    (".sh", "sh"),
    (".bash", "sh"),
    (".zsh", "sh"),
    (".lua", "lua"),
    (".ex", "elixir"),
    (".exs", "elixir"),
    (".swift", "swift"),
    (".dart", "swift"),
];

const DECL_TEMPLATES: &[(&str, &[&str])] = &[
    ("go", &["\\b(?:func|type|var|const)\\s+(?:\\([^)]*\\)\\s*)?%s\\b", "\\b%s\\s*:?=[^=]"]),
    ("py", &["\\b(?:def|class)\\s+%s\\b", "^\\s*%s\\s*(?::[^=]*)?=[^=]"]),
    (
        "js",
        &[
            "\\b(?:function|class|interface|type|enum|const|let|var)\\s+%s\\b",
            "\\b%s\\s*[:=]\\s*(?:async\\s*)?(?:function|\\(|\\w+\\s*=>)",
            "^\\s*(?:async\\s+)?%s\\s*\\(",
        ],
    ),
    (
        "rust",
        &[
            "\\b(?:fn|struct|enum|trait|mod|type|const|static|union|macro_rules!)\\s+(?:mut\\s+)?%s\\b",
            "\\bimpl(?:<[^>]*>)?\\s+%s\\b",
            "\\blet\\s+(?:mut\\s+)?%s\\b",
        ],
    ),
    (
        "jvm",
        &[
            "\\b(?:class|interface|enum|record|object|trait|fun|def|val|var)\\s+%s\\b",
            "\\b[\\w<>\\[\\],.?]+\\s+%s\\s*\\(",
        ],
    ),
    (
        "c",
        &[
            "\\b(?:struct|union|enum|class|typedef|namespace)\\s+%s\\b",
            "^[\\w\\s\\*&:<>,~]*\\b%s\\s*\\([^;]*$",
            "^\\s*#\\s*define\\s+%s\\b",
        ],
    ),
    ("rb", &["\\b(?:def|class|module)\\s+(?:self\\.)?%s\\b", "^\\s*%s\\s*=[^=]"]),
    ("php", &["\\b(?:function|class|interface|trait|const)\\s+%s\\b", "\\$%s\\s*=[^=]"]),
    ("sh", &["^\\s*(?:function\\s+)?%s\\s*\\(\\s*\\)", "^\\s*(?:export\\s+)?%s="]),
    ("lua", &["\\bfunction\\s+[\\w.:]*%s\\b", "\\blocal\\s+%s\\b"]),
    ("elixir", &["\\b(?:def|defp|defmodule|defstruct|defmacro)\\s+%s\\b"]),
    (
        "swift",
        &["\\b(?:func|class|struct|enum|protocol|extension|let|var|typealias)\\s+%s\\b"],
    ),
    (
        "default",
        &["\\b(?:func|function|def|class|type|struct|interface|enum|fn|const|let|var|module|trait|impl|package)\\s+%s\\b"],
    ),
];

/// Family name for a lowercase dotted extension, else `"default"`.
/// Ports Go `familyFor`.
pub fn family_for(ext: &str) -> &'static str {
    for (e, fam) in EXT_FAMILY {
        if *e == ext {
            return fam;
        }
    }
    "default"
}

/// Per-extension declaration recognisers for a query identifier. Ports Go
/// `declPatterns` (identifier-shaped queries only).
pub fn decl_patterns(ident: &str) -> HashMap<String, Regex> {
    let ident_ok = {
        let mut cs = ident.bytes();
        match cs.next() {
            Some(b) if b.is_ascii_alphabetic() || b == b'_' || b == b'$' => {}
            _ => return HashMap::new(),
        }
        cs.all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'$')
    };
    if !ident_ok {
        return HashMap::new();
    }
    // NOTE: Go `[\w$]*` is Unicode-aware; identifiers reaching this path
    // are ASCII in practice. Non-ASCII idents get no def classification.
    let q = regex::escape(ident);
    let mut by_family: HashMap<&str, Regex> = HashMap::new();
    for (fam, tpls) in DECL_TEMPLATES {
        let body = tpls
            .iter()
            .map(|t| format!("(?:{})", t.replace("%s", &q)))
            .collect::<Vec<_>>()
            .join("|");
        if let Ok(re) = Regex::new(&body) {
            by_family.insert(fam, re);
        }
    }
    let mut out = HashMap::new();
    if let Some(default) = by_family.get("default") {
        out.insert(String::new(), default.clone());
    }
    for (ext, fam) in EXT_FAMILY {
        if let Some(re) = by_family.get(fam) {
            out.insert(ext.to_string(), re.clone());
        }
    }
    out
}

#[derive(Clone, Debug, Serialize)]
pub struct Symbol {
    pub name: String,
    pub kind: String,
    pub line: usize,
    pub indent: usize,
}

struct SymRule {
    re: Regex,
    /// Literal kind, or `$N` to take it from capture group N.
    kind: &'static str,
    /// Capture group holding the name.
    name: usize,
}

macro_rules! sym_rules {
    ($($kind:expr, $name:expr, $pat:expr);* $(;)?) => {
        vec![$(SymRule { re: Regex::new($pat).unwrap(), kind: $kind, name: $name }),*]
    };
}

fn outline_rules(family: &str) -> Vec<SymRule> {
    match family {
        "go" => sym_rules!(
            "func", 2, r"^func\s+(\([^)]*\)\s*)?([\w]+)\s*[\(\[]";
            "$2", 1, r"^type\s+([\w]+)\s+(struct|interface)\b";
            "type", 1, r"^type\s+([\w]+)\s";
            "$1", 2, r"^(var|const)\s+([\w]+)\s";
        ),
        "py" => sym_rules!(
            "func", 1, r"^\s*(?:async\s+)?def\s+([\w]+)\s*\(";
            "class", 1, r"^\s*class\s+([\w]+)\s*[\(:]";
        ),
        "js" => sym_rules!(
            "func", 1, r"^\s*(?:export\s+)?(?:default\s+)?(?:async\s+)?function\s*\*?\s*([\w$]+)";
            "class", 1, r"^\s*(?:export\s+)?(?:default\s+)?(?:abstract\s+)?class\s+([\w$]+)";
            "type", 2, r"^\s*(?:export\s+)?(?:declare\s+)?(interface|type|enum|namespace)\s+([\w$]+)";
            "func", 1, r"^\s*(?:export\s+)?(?:const|let|var)\s+([\w$]+)\s*(?::[^=]+)?=\s*(?:async\s*)?(?:function|\([^)]*\)\s*(?::[^=]*)?=>|[\w$]+\s*=>)";
            "const", 1, r"^\s*(?:export\s+)?(?:const|let|var)\s+([\w$]+)\s*=";
            "method", 2, r"^\s{2,}(?:(?:public|private|protected|static|readonly|async|get|set)\s+)*([\w$]*\s*)?([\w$]+)\s*\([^)]*\)\s*(?::[^{;]+)?\{";
        ),
        "rust" => sym_rules!(
            "func", 1, r#"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:const\s+|async\s+|unsafe\s+|extern\s+"[^"]*"\s+)*fn\s+([\w]+)"#;
            "$1", 2, r"^\s*(?:pub(?:\([^)]*\))?\s+)?(struct|enum|trait|union|mod|type)\s+([\w]+)";
            "impl", 1, r"^\s*impl(?:\s*<[^>]*>)?\s+(?:[\w:<>, ]+\s+for\s+)?([\w:]+)";
            "macro", 1, r"^\s*macro_rules!\s+([\w]+)";
        ),
        "jvm" => sym_rules!(
            "$1", 2, r"^\s*(?:(?:public|private|protected|internal|final|abstract|sealed|static|open|data|case)\s+)*(class|interface|enum|record|object|trait|struct)\s+([\w]+)";
            "func", 1, r"^\s*(?:(?:public|private|protected|internal|static|final|override|suspend|async|virtual)\s+)*(?:fun|def)\s+([\w]+)";
            "method", 2, r"^\s+(?:(?:public|private|protected|static|final|synchronized|abstract|override|virtual)\s+)+(?:[\w<>\[\],.?]+\s+)?([\w<>\[\],.?]+\s+)?([\w]+)\s*\(";
        ),
        "c" => sym_rules!(
            "$1", 2, r"^\s*(?:typedef\s+)?(struct|union|enum|class|namespace)\s+([\w]+)";
            "func", 1, r"^[\w][\w\s\*&:<>,~]*?([\w~]+)\s*\([^;]*$";
            "macro", 1, r"^\s*#\s*define\s+([\w]+)";
        ),
        "rb" => sym_rules!(
            "$1", 2, r"^\s*(def|class|module)\s+((?:self\.)?[\w?!=]+)";
        ),
        "php" => sym_rules!(
            "$1", 2, r"^\s*(?:(?:public|private|protected|static|abstract|final)\s+)*(function|class|interface|trait)\s+([\w]+)";
        ),
        "sh" => sym_rules!(
            "func", 1, r"^\s*(?:function\s+)?([\w-]+)\s*\(\s*\)\s*\{";
        ),
        "lua" => sym_rules!(
            "func", 1, r"^\s*(?:local\s+)?function\s+([\w.:]+)";
        ),
        "elixir" => sym_rules!(
            "$1", 2, r"^\s*(defmodule|def|defp|defmacro|defstruct)\s+([\w.?!]+)";
        ),
        "swift" => sym_rules!(
            "$1", 2, r"^\s*(?:(?:public|private|internal|fileprivate|open|final|static|override|@objc)\s+)*(func|class|struct|enum|protocol|extension|typealias)\s+([\w]+)";
        ),
        _ => sym_rules!(
            "$1", 2, r"^\s*(func|function|def|class|type|struct|interface|enum|fn|module|trait)\s+([\w.$:?!-]+)";
        ),
    }
}

fn is_noise_symbol(name: &str) -> bool {
    matches!(
        name,
        "if" | "for"
            | "while"
            | "switch"
            | "return"
            | "else"
            | "catch"
            | "try"
            | "do"
            | "case"
            | "with"
            | "match"
            | "defer"
            | "go"
            | "in"
    )
}

/// Extract a symbol list with per-language regexps: approximate, never
/// blocking, right often enough to navigate by. Ports Go `Outline`.
/// Lines over 1 MB fail like Go's scanner (`token too long`).
pub fn outline(abs: &Path, rel: &str) -> Result<Vec<Symbol>, String> {
    let data = std::fs::read(abs).map_err(|e| e.to_string())?;
    let ext = Path::new(rel)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| format!(".{}", e.to_ascii_lowercase()))
        .unwrap_or_default();
    // Go splits on \n without the newline; \r\n leaves \r in the text.
    let text = String::from_utf8_lossy(&data);
    let mut out = Vec::new();
    if ext == ".md" || ext == ".markdown" {
        let heading = Regex::new(r"^(#{1,6})\s+(.+?)\s*#*$").unwrap();
        for (i, line) in text.split('\n').enumerate() {
            if let Some(m) = heading.captures(line) {
                out.push(Symbol {
                    name: m[2].to_string(),
                    kind: "heading".to_string(),
                    line: i + 1,
                    indent: m[1].len() - 1,
                });
            }
        }
        return Ok(out);
    }
    let rules = outline_rules(family_for(&ext));
    for (i, line) in text.split('\n').enumerate() {
        if line.is_empty() || line.len() > 500 {
            continue;
        }
        if line.len() > 1 << 20 {
            // Unreachable given the 500-byte skip, kept to mirror the
            // scanner limit Go enforces.
            return Err("line too long".to_string());
        }
        for rule in &rules {
            let Some(caps) = rule.re.captures(line) else {
                continue;
            };
            let Some(name) = caps.get(rule.name).map(|m| m.as_str().trim()) else {
                continue;
            };
            if name.is_empty() || is_noise_symbol(name) {
                continue;
            }
            let mut kind = rule.kind.to_string();
            if let Some(rest) = rule.kind.strip_prefix('$') {
                let g: usize = rest.parse().unwrap_or(0);
                kind = caps
                    .get(g)
                    .map(|m| m.as_str().to_string())
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "sym".to_string());
            }
            let indent = line.len() - line.trim_start_matches([' ', '\t']).len();
            out.push(Symbol {
                name: name.to_string(),
                kind,
                line: i + 1,
                indent,
            });
            break;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn go_outline_names_funcs_types_and_vars() {
        let dir = std::env::temp_dir().join(format!("px0-sym-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.go");
        std::fs::write(
            &file,
            "package a\n\nfunc Foo() {}\ntype Bar struct{}\nvar baz = 1\n",
        )
        .unwrap();
        let syms = outline(&file, "a.go").unwrap();
        let names: Vec<(&str, &str)> = syms
            .iter()
            .map(|s| (s.name.as_str(), s.kind.as_str()))
            .collect();
        assert!(names.contains(&("Foo", "func")));
        assert!(names.contains(&("Bar", "struct")));
        assert!(names.contains(&("baz", "var")));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rust_outline_and_noise_filter() {
        let dir = std::env::temp_dir().join(format!("px0-sym-rs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.rs");
        std::fs::write(&file, "pub fn main() {}\nstruct S;\nif x {}\n").unwrap();
        let syms = outline(&file, "a.rs").unwrap();
        assert!(syms.iter().any(|s| s.name == "main" && s.kind == "func"));
        assert!(syms.iter().any(|s| s.name == "S"));
        assert!(!syms.iter().any(|s| s.name == "if"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn markdown_headings_carry_level_indent() {
        let dir = std::env::temp_dir().join(format!("px0-sym-md-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.md");
        std::fs::write(&file, "# Title\n\n## Sub ##\n").unwrap();
        let syms = outline(&file, "a.md").unwrap();
        assert_eq!(syms.len(), 2);
        assert_eq!((syms[0].name.as_str(), syms[0].indent), ("Title", 0));
        assert_eq!((syms[1].name.as_str(), syms[1].indent), ("Sub", 1));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
