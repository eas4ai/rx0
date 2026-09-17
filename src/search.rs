//! Parallel workspace grep over indexed files.
//!
//! Ports `search.go`: literal fast path with ASCII-only case folding,
//! regex/word modes, glob path filter, per-file 8 MB cap, binary skip,
//! declaration classification, `{Pre, Mid, Post}` snippet elision, and
//! result sort + truncation. Client-disconnect cancellation is the one
//! documented divergence (see PORTING.md).

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use regex::Regex;
use serde::Serialize;

use crate::ignore::GlobRule;
use crate::index::{FileEntry, Index};
use crate::symbols::decl_patterns;

/// A hit split into text before, matched text, and text after, so the
/// client never reconciles byte offsets. Ports Go `Match`.
#[derive(Clone, Debug, Serialize)]
pub struct Match {
    /// 1-based line number.
    pub line: usize,
    pub pre: String,
    pub mid: String,
    pub post: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub def: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct FileMatches {
    pub path: String,
    pub matches: Vec<Match>,
}

const SNIP_LEAD: usize = 32;
const SNIP_KEEP: usize = 16;
const SNIP_MAX: usize = 240;

/// One raw line plus a byte range into display-ready text. Ports Go `snip`.
pub(crate) fn snip(line: &[u8], from: usize, to: usize) -> Match {
    let mut pre = String::from_utf8_lossy(&line[..from]).into_owned();
    let mut mid = String::from_utf8_lossy(&line[from..to]).into_owned();
    let mut post = String::from_utf8_lossy(&line[to..]).into_owned();

    let trimmed = pre.trim_start_matches([' ', '\t']).to_string();
    if trimmed.len() != pre.len() {
        pre = trimmed;
    }
    if pre.chars().count() > SNIP_LEAD {
        let tail: String = pre
            .chars()
            .rev()
            .take(SNIP_KEEP)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        pre = format!("…{tail}");
    }
    if mid.chars().count() > SNIP_MAX {
        mid = format!("{}…", mid.chars().take(SNIP_MAX).collect::<String>());
    }
    let used = pre.chars().count() + mid.chars().count();
    if SNIP_MAX > used {
        let budget = SNIP_MAX - used;
        if post.chars().count() > budget {
            post = format!("{}…", post.chars().take(budget).collect::<String>());
        }
    } else {
        post.clear();
    }
    post = post.trim_end_matches([' ', '\t']).to_string();
    Match {
        line: 0,
        pre,
        mid,
        post,
        def: false,
    }
}

/// True when the first 8 KB contain a NUL byte. Ports Go `isBinary`.
pub fn is_binary(data: &[u8]) -> bool {
    let n = data.len().min(8000);
    data[..n].contains(&0)
}

fn ascii_lower<'a>(dst: &'a mut Vec<u8>, src: &[u8]) -> &'a [u8] {
    if dst.len() < src.len() {
        dst.resize(src.len() + src.len() / 2, 0);
    }
    for (d, &c) in dst.iter_mut().zip(src.iter()) {
        *d = if c.is_ascii_uppercase() {
            c + b'a' - b'A'
        } else {
            c
        };
    }
    &dst[..src.len()]
}

pub const SEARCH_FILE_CAP: u64 = 8 << 20;

#[derive(Clone, Debug, Default)]
pub struct SearchOpts {
    pub query: String,
    pub regex: bool,
    pub case: bool,
    pub word: bool,
    pub glob: String,
    pub max_files: usize,
    pub max_per_file: usize,
    /// Mark hits whose line declares the query. Only `/api/def` sets
    /// this (Go `classifyDefs`); plain search never classifies.
    pub classify_defs: bool,
}

struct Searcher {
    opts: SearchOpts,
    re: Option<Regex>,
    lit: Vec<u8>,
    glob: Option<GlobRule>,
    def_res: HashMap<String, Regex>,
}

impl Searcher {
    fn new(mut opts: SearchOpts) -> Result<Self, String> {
        if opts.max_files == 0 {
            opts.max_files = 200;
        }
        if opts.max_per_file == 0 {
            opts.max_per_file = 50;
        }
        let glob = if opts.glob.is_empty() {
            None
        } else {
            GlobRule::compile(&opts.glob)
        };
        let (re, lit) = if opts.regex || opts.word {
            let mut pat = opts.query.clone();
            if !opts.regex {
                pat = regex::escape(&pat);
            }
            if opts.word {
                pat = format!(r"\b(?:{pat})\b");
            }
            if !opts.case {
                pat = format!("(?i){pat}");
            }
            (
                Some(Regex::new(&pat).map_err(|e| e.to_string())?),
                Vec::new(),
            )
        } else {
            let needle = if opts.case {
                opts.query.clone()
            } else {
                opts.query.to_ascii_lowercase()
            };
            (None, needle.into_bytes())
        };
        let def_res = if opts.classify_defs && !opts.query.is_empty() {
            decl_patterns(&opts.query)
        } else {
            HashMap::new()
        };
        Ok(Self {
            opts,
            re,
            lit,
            glob,
            def_res,
        })
    }

    /// Every match in one file's bytes, capped at `max`. Ports Go
    /// `searcher.scan` (line splitting on `\n`, offsets into the original
    /// line even on the case-folded path).
    fn scan(&self, data: &[u8], ext: &str, max: usize, lower: &mut Vec<u8>) -> Vec<Match> {
        let hay: &[u8] = if self.re.is_none() && !self.opts.case {
            ascii_lower(lower, data);
            let n = data.len();
            &lower[..n]
        } else {
            data
        };
        if self.re.is_none() && !memmem(hay, &self.lit) {
            return Vec::new();
        }
        let mut out = Vec::new();
        let def_re = self.def_res.get(ext);
        let mut line_no = 1;
        let mut start = 0;
        while start <= data.len() {
            let rel_end = data[start..].iter().position(|&b| b == b'\n');
            let line_end = match rel_end {
                Some(e) => start + e,
                None => data.len(),
            };
            let line = &data[start..line_end];
            let hline = &hay[start..line_end];

            let mut locs: Vec<(usize, usize)> = Vec::new();
            if let Some(re) = &self.re {
                // Regex matches borrow from `line`, so collect offsets first.
                if let Ok(text) = std::str::from_utf8(line) {
                    for m in re.find_iter(text).take(max) {
                        locs.push((m.start(), m.end()));
                    }
                }
            } else {
                let mut off = 0;
                while locs.len() < max {
                    match find_from(hline, &self.lit, off) {
                        Some(i) => {
                            locs.push((off + i, off + i + self.lit.len()));
                            off += i + self.lit.len();
                        }
                        None => break,
                    }
                }
            }
            if !locs.is_empty() {
                let is_def =
                    def_re.is_some_and(|r| std::str::from_utf8(line).is_ok_and(|t| r.is_match(t)));
                for (from, to) in locs {
                    let mut m = snip(line, from, to);
                    m.line = line_no;
                    m.def = is_def;
                    out.push(m);
                    if out.len() >= max {
                        return out;
                    }
                }
            }
            match rel_end {
                None => break,
                Some(_) => {
                    start = line_end + 1;
                    line_no += 1;
                }
            }
        }
        out
    }
}

fn memmem(hay: &[u8], needle: &[u8]) -> bool {
    find_from(hay, needle, 0).is_some()
}

fn find_from(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() {
        // Unreachable: blank queries return before searching. Refuse
        // rather than looping on zero-width matches.
        return None;
    }
    if from >= hay.len() {
        return None;
    }
    hay[from..].windows(needle.len()).position(|w| w == needle)
}

/// Grep every indexed file in parallel. Ports Go `SearchContext` minus
/// context cancellation (documented in PORTING.md).
pub fn search(index: &Index, opts: SearchOpts) -> Result<(Vec<FileMatches>, bool), String> {
    if opts.query.trim().is_empty() {
        return Ok((Vec::new(), false));
    }
    let searcher = Searcher::new(opts.clone())?;
    // Defaults live on the searcher (Go mutates its copy of the opts).
    let max_files = searcher.opts.max_files;
    let glob = opts.glob.clone();
    let mut files = index.files();
    // An exactly-named but unindexed file (gitignored yet open) is
    // searched alone. Ports Go `unindexedTarget`.
    if let Some(single) = unindexed_target(index.root(), &files, &glob) {
        files = vec![single];
    }

    let next = AtomicUsize::new(0);
    let results = std::sync::Mutex::new(Vec::new());
    let hit = AtomicUsize::new(0);
    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                let mut lower = Vec::new();
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= files.len() {
                        break;
                    }
                    let f = &files[i];
                    if f.size == 0 || f.size > SEARCH_FILE_CAP {
                        continue;
                    }
                    if let Some(g) = &searcher.glob {
                        if !g.hit(&f.path) {
                            continue;
                        }
                    }
                    if hit.load(Ordering::Relaxed) >= searcher.opts.max_files {
                        continue;
                    }
                    let data = match std::fs::read(index.root().join(&f.path)) {
                        Ok(d) => d,
                        Err(_) => continue,
                    };
                    if is_binary(&data) {
                        continue;
                    }
                    let ext = Path::new(&f.path)
                        .extension()
                        .and_then(|e| e.to_str())
                        .unwrap_or("");
                    let ext = format!(".{ext}").to_lowercase();
                    let m = searcher.scan(&data, &ext, searcher.opts.max_per_file, &mut lower);
                    // Bound scratch like Go `keepBuf` (1 MB).
                    if lower.capacity() > (1 << 20) {
                        lower = Vec::new();
                    }
                    if m.is_empty() {
                        continue;
                    }
                    hit.fetch_add(1, Ordering::Relaxed);
                    results.lock().unwrap().push(FileMatches {
                        path: f.path.clone(),
                        matches: m,
                    });
                }
            });
        }
    });

    let mut results = results.into_inner().unwrap();
    results.sort_by(|a, b| a.path.cmp(&b.path));
    let mut truncated = false;
    if results.len() > max_files {
        results.truncate(max_files);
        truncated = true;
    }
    Ok((results, truncated))
}

/// The file a literal glob names when it exists under root but is not
/// indexed. Ports Go `unindexedTarget`.
fn unindexed_target(root: &Path, files: &[FileEntry], glob: &str) -> Option<FileEntry> {
    let rel = glob.strip_prefix('/').unwrap_or(glob);
    if rel.is_empty()
        || rel.contains(['*', '?', '[', '!', '\\'])
        || rel.ends_with('/')
        || rel
            .split('/')
            .any(|s| s.is_empty() || s == "." || s == "..")
    {
        return None;
    }
    if files.binary_search_by(|f| f.path.as_str().cmp(rel)).is_ok() {
        return None;
    }
    let meta = std::fs::symlink_metadata(root.join(rel)).ok()?;
    if !meta.file_type().is_file() {
        return None;
    }
    let name = rel.rsplit('/').next().unwrap_or(rel).to_string();
    Some(FileEntry {
        path: rel.to_string(),
        name: name.clone(),
        size: meta.len(),
        lower: rel.to_lowercase(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snip_keeps_match_in_view() {
        let line = b"        func fuzzyScore(q string, e *Entry, pos []int) (int, []int, bool) {";
        let m = snip(line, 13, 23);
        assert_eq!(m.mid, "fuzzyScore");
        assert!(m.pre.starts_with('…') || m.pre.len() <= 32);
        assert!(!m.pre.starts_with(' ') && !m.pre.starts_with('\t'));
    }

    #[test]
    fn snip_caps_long_match_and_post() {
        let mut line = vec![b'x'; 40];
        line.extend_from_slice(&vec![b'y'; 300]);
        let m = snip(&line, 40, 340);
        assert!(m.mid.chars().count() <= SNIP_MAX + 1);
        assert!(m.post.is_empty());
    }

    #[test]
    fn ascii_folding_keeps_offsets() {
        // U+0130 lowercases across byte lengths; ASCII folding must not move.
        let data = "ABC İDEF abc".as_bytes();
        let mut buf = Vec::new();
        let folded = ascii_lower(&mut buf, data).to_vec();
        assert_eq!(&folded[..3], b"abc");
        assert_eq!(folded.len(), data.len());
    }

    #[test]
    fn binary_probe_checks_nul_prefix() {
        assert!(is_binary(b"ab\0cd"));
        assert!(!is_binary(b"plain text\n"));
        let mut big = vec![b'x'; 9000];
        big[7999] = 0;
        assert!(is_binary(&big));
        big[7999] = b'x';
        big[8500] = 0;
        assert!(!is_binary(&big));
    }
}
