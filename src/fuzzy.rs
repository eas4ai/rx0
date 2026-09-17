//! Two-pass bounded fuzzy matcher for file paths.
//!
//! Ports `fuzzy.go`: forward pass proves every query byte present, backward
//! pass pulls positions tight, then weighted scoring (consecutive runs,
//! basename, boundaries, camel humps, exact case, verbatim bonus). No
//! `O(n*m)` dynamic program. Query and index sides are byte-oriented exactly
//! like Go (lowercase comparison, original-case bonuses).

use serde::Serialize;

use crate::index::FileEntry;

fn is_boundary(b: u8) -> bool {
    matches!(b, b'/' | b'_' | b'-' | b'.' | b' ' | b'@')
}

#[derive(Clone, Debug, Serialize)]
pub struct FuzzyResult {
    pub path: String,
    pub name: String,
    pub pos: Option<Vec<usize>>,
    #[serde(skip_serializing)]
    pub score: i64,
}

/// Score one entry. Ports Go `fuzzyScore`.
fn fuzzy_score(q: &[u8], entry: &FileEntry, pos: &mut Vec<usize>) -> Option<i64> {
    let p = entry.path.as_bytes();
    let lp = entry.lower.as_bytes();
    let name_start = entry.path.len() - entry.name.len();

    let mut qi = 0;
    let mut end = 0;
    let mut found = false;
    for (i, _) in lp.iter().enumerate() {
        if qi < q.len() && lp[i] == q[qi] {
            qi += 1;
            end = i;
            found = true;
        }
    }
    if qi < q.len() {
        return None;
    }
    let _ = found;

    pos.clear();
    let mut qi = q.len();
    let mut i = end as isize;
    while i >= 0 && qi > 0 {
        if lp[i as usize] == q[qi - 1] {
            pos.push(i as usize);
            qi -= 1;
        }
        i -= 1;
    }
    pos.reverse();

    let mut score: i64 = 0;
    let mut prev: isize = -2;
    for (k, &at) in pos.iter().enumerate() {
        let at = at as isize;
        if at == prev + 1 {
            score += 12;
        } else if k > 0 {
            score -= (at - prev).min(12) as i64;
        }
        if at as usize >= name_start {
            score += 14;
        }
        if at == 0 || is_boundary(p[at as usize - 1]) {
            score += 16;
        } else if p[at as usize].is_ascii_uppercase() && p[at as usize - 1].is_ascii_lowercase() {
            score += 14;
        }
        if p[at as usize] == q[k] {
            score += 4;
        }
        prev = at;
    }
    score -= p.len() as i64 / 8;
    score -= p.iter().filter(|&&b| b == b'/').count() as i64 * 2;
    if let Some(idx) = entry.lower[name_start..].find(q_as_str(q)) {
        score += 40;
        if idx == 0 {
            score += 20;
        }
    }
    Some(score)
}

// The query is lowercase text; the byte slice borrows it.
fn q_as_str(q: &[u8]) -> &str {
    // SAFETY: constructed from `str::to_lowercase` output, always UTF-8.
    unsafe { std::str::from_utf8_unchecked(q) }
}

/// Rank every indexed path against the query, best `limit` first. Ports Go
/// `FuzzyFind`, including worker-chunked scoring and score-desc/path-asc
/// order.
pub fn fuzzy_find(files: &[FileEntry], query: &str, limit: usize) -> Vec<FuzzyResult> {
    let q: Vec<u8> = query.to_lowercase().trim().replace(' ', "").into_bytes();

    if q.is_empty() {
        return files
            .iter()
            .take(limit)
            .map(|f| FuzzyResult {
                path: f.path.clone(),
                name: f.name.clone(),
                pos: None,
                score: 0,
            })
            .collect();
    }

    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    let mut chunk = files.len().div_ceil(workers);
    if chunk == 0 {
        chunk = 1;
    }
    let mut parts: Vec<Vec<FuzzyResult>> = Vec::new();
    // Shared slices: `&[u8]` is Copy, so each moved closure gets its own.
    let qr: &[u8] = &q;
    std::thread::scope(|scope| {
        let mut handles = Vec::new();
        let mut lo = 0;
        while lo < files.len() {
            let hi = (lo + chunk).min(files.len());
            let slice = &files[lo..hi];
            handles.push(scope.spawn(move || {
                let mut local = Vec::with_capacity(64);
                let mut scratch = Vec::with_capacity(64);
                for entry in slice {
                    if let Some(score) = fuzzy_score(qr, entry, &mut scratch) {
                        local.push(FuzzyResult {
                            path: entry.path.clone(),
                            name: entry.name.clone(),
                            pos: Some(scratch.clone()),
                            score,
                        });
                    }
                }
                local
            }));
            lo = hi;
        }
        for h in handles {
            parts.push(h.join().unwrap());
        }
    });

    let mut all: Vec<FuzzyResult> = parts.into_iter().flatten().collect();
    all.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.path.cmp(&b.path)));
    all.truncate(limit);
    all
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str) -> FileEntry {
        let path = path.to_string();
        let name = path.rsplit('/').next().unwrap().to_string();
        let lower = path.to_lowercase();
        FileEntry {
            path,
            name,
            size: 0,
            lower,
        }
    }

    fn files() -> Vec<FileEntry> {
        [
            "src/server.rs",
            "src/main.rs",
            "web/src/state.js",
            "README.md",
            "src/index.rs",
        ]
        .iter()
        .map(|p| entry(p))
        .collect()
    }

    #[test]
    fn empty_query_returns_index_order() {
        let res = fuzzy_find(&files(), "  ", 3);
        assert_eq!(res.len(), 3);
        assert_eq!(res[0].path, "src/server.rs");
        assert!(res.iter().all(|r| r.pos.is_none()));
    }

    #[test]
    fn basename_and_boundary_win() {
        let res = fuzzy_find(&files(), "srv", 5);
        assert_eq!(res[0].path, "src/server.rs");
    }

    #[test]
    fn verbatim_basename_ranks_first() {
        let res = fuzzy_find(&files(), "state", 5);
        assert_eq!(res[0].path, "web/src/state.js");
    }

    #[test]
    fn no_match_gives_empty() {
        assert!(fuzzy_find(&files(), "zzzqqq", 10).is_empty());
    }

    #[test]
    fn positions_stay_within_path() {
        for r in fuzzy_find(&files(), "sr", 10) {
            for &at in r.pos.as_ref().unwrap() {
                assert!(at < r.path.len());
            }
        }
    }
}
