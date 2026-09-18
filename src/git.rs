//! Git awareness via CLI shell-out: never stages, commits, or writes.
//!
//! Ports `git.go`: memoized availability probe, porcelain v2 `-z` status
//! map, `HEAD` unified diffs, and new-file line ranges for the change
//! gutter. Every failure is quiet (`None`/`""`/`false`), never an error.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex, OnceLock,
};

/// Process-wide `-no-git` switch, set once in main. Ports Go `gitDisabled`.
static DISABLED: AtomicBool = AtomicBool::new(false);

pub fn set_disabled(disabled: bool) {
    DISABLED.store(disabled, Ordering::Relaxed);
}

#[derive(Clone, Default)]
struct GitInfo {
    ok: bool,
    toplevel: String,
}

fn probe_cache() -> &'static Mutex<HashMap<String, GitInfo>> {
    static CACHE: OnceLock<Mutex<HashMap<String, GitInfo>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Root for prefix comparison: `rev-parse --show-toplevel` resolves
/// symlinks (macOS TMPDIR lives under /var -> /private/var), so a
/// symlinked served root would otherwise match no status keys.
fn canonical_root(root: &Path) -> PathBuf {
    let canon = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    #[cfg(windows)]
    {
        // canonicalize yields \\?\C:\... while git reports C:/...; strip
        // the prefix only for drive paths (never \\?\UNC\...).
        let s = canon.to_string_lossy();
        if let Some(stripped) = s.strip_prefix(r"\\?\") {
            let b = stripped.as_bytes();
            if b.len() > 2
                && b[0].is_ascii_alphabetic()
                && b[1] == b':'
                && (b[2] == b'\\' || b[2] == b'/')
            {
                return PathBuf::from(stripped);
            }
        }
    }
    canon
}

fn git_probe(root: &Path) -> GitInfo {
    if DISABLED.load(Ordering::Relaxed) {
        return GitInfo::default();
    }
    let key = root.to_string_lossy().into_owned();
    if let Some(info) = probe_cache().lock().unwrap().get(&key) {
        return info.clone();
    }
    let mut info = GitInfo::default();
    if let Ok(out) = Command::new("git")
        .args([
            "-C",
            &root.to_string_lossy(),
            "rev-parse",
            "--show-toplevel",
        ])
        .output()
    {
        if out.status.success() {
            info = GitInfo {
                ok: true,
                toplevel: String::from_utf8_lossy(&out.stdout).trim().to_string(),
            };
        }
    }
    probe_cache().lock().unwrap().insert(key, info.clone());
    info
}

/// Whether git is on PATH and root sits inside a working tree. Ports Go
/// `gitAvailable`.
pub fn git_available(root: &Path) -> bool {
    git_probe(root).ok
}

/// Collapse a porcelain v2 XY code, preferring the staged side. Ports Go
/// `mapXY`.
fn map_xy(xy: &str) -> &'static str {
    let b = xy.as_bytes();
    if b.len() < 2 {
        return "M";
    }
    let mut c = b[0];
    if c == b'.' {
        c = b[1];
    }
    match c {
        b'A' => "A",
        b'D' => "D",
        b'R' => "R",
        b'C' => "C",
        // Unmerged / conflict.
        b'U' => "!",
        // Modified, typechange, and anything else read as modified.
        _ => "M",
    }
}

/// Repo-relative-to-served-root path to single-letter status. Ports Go
/// `gitStatus` (porcelain v2 `-z`, quiet on any failure).
pub fn git_status(root: &Path) -> Option<HashMap<String, String>> {
    let info = git_probe(root);
    if !info.ok {
        return None;
    }
    let out = Command::new("git")
        .args([
            "-C",
            &root.to_string_lossy(),
            "status",
            "--porcelain=v2",
            "-z",
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    // Porcelain paths are repo-root-relative regardless of -C; strip the
    // served root's offset to match the index's keys.
    let prefix = rel_to_slash(Path::new(&info.toplevel), &canonical_root(root))
        .filter(|rel| rel != ".")
        .map(|rel| rel + "/")
        .unwrap_or_default();
    let key = |p: &str| -> Option<String> {
        if prefix.is_empty() {
            return Some(p.to_string());
        }
        p.strip_prefix(&prefix).map(str::to_string)
    };

    let mut status: HashMap<String, String> = HashMap::new();
    let fields: Vec<&str> = std::str::from_utf8(&out.stdout)
        .unwrap_or("")
        .split('\0')
        .collect();
    let mut i = 0;
    while i < fields.len() {
        let f = fields[i];
        if f.is_empty() {
            i += 1;
            continue;
        }
        match f.as_bytes()[0] {
            // "? <path>"
            b'?' => {
                if let Some(k) = key(f.get(2..).unwrap_or("")) {
                    status.insert(k, "U".to_string());
                }
            }
            // "1 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <path>"
            b'1' => {
                let p: Vec<&str> = f.splitn(9, ' ').collect();
                if p.len() == 9 {
                    if let Some(k) = key(p[8]) {
                        status.insert(k, map_xy(p[1]).to_string());
                    }
                }
            }
            // "2 <XY> ... <path>", original path follows as its own field.
            b'2' => {
                let p: Vec<&str> = f.splitn(10, ' ').collect();
                if p.len() == 10 {
                    if let Some(k) = key(p[9]) {
                        status.insert(k, map_xy(p[1]).to_string());
                    }
                }
                i += 1;
            }
            // "u <XY> ... <path>"
            b'u' => {
                let p: Vec<&str> = f.splitn(11, ' ').collect();
                if p.len() == 11 {
                    if let Some(k) = key(p[10]) {
                        status.insert(k, "!".to_string());
                    }
                }
            }
            _ => {}
        }
        i += 1;
    }
    if status.is_empty() {
        return None;
    }
    Some(status)
}

/// `target` relative to `base`, slash-separated, mirroring
/// `filepath.Rel` for the absolute-path cases that reach it.
fn rel_to_slash(base: &Path, target: &Path) -> Option<String> {
    use std::path::Component;
    let mut b = base.components().peekable();
    let mut t = target.components().peekable();
    while matches!((b.peek(), t.peek()), (Some(x), Some(y)) if x == y) {
        b.next();
        t.next();
    }
    let mut rel = PathBuf::new();
    for comp in b {
        if !matches!(comp, Component::Normal(_)) {
            return None;
        }
        rel.push("..");
    }
    for comp in t {
        match comp {
            Component::Normal(s) => rel.push(s),
            Component::CurDir => {}
            _ => return None,
        }
    }
    if rel.as_os_str().is_empty() {
        return Some(".".to_string());
    }
    Some(rel.to_string_lossy().replace('\\', "/"))
}

/// Unified diff of a served-root-relative path against HEAD. Ports Go
/// `gitDiff` (quiet `""` on any failure).
pub fn git_diff(root: &Path, relpath: &str) -> String {
    if !git_available(root) {
        return String::new();
    }
    Command::new("git")
        .args([
            "-C",
            &root.to_string_lossy(),
            "diff",
            "--no-color",
            "HEAD",
            "--",
            relpath,
        ])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

/// New-file line numbers for the change gutter: added, modified, and one
/// marker per pure-deletion run. Signed like Go `[]int`: a fully deleted
/// file yields `-1`, no saturation. Ports Go `gitHunks` (`None` when git
/// is off or the file is clean).
pub fn git_hunks(root: &Path, relpath: &str) -> Option<(Vec<i64>, Vec<i64>, Vec<i64>)> {
    let diff = git_diff(root, relpath);
    if diff.is_empty() {
        return None;
    }
    // Current block: a maximal run of consecutive '+'/'-' lines.
    #[derive(Default)]
    struct Block {
        dels: usize,
        adds: Vec<i64>,
        start: i64,
    }
    impl Block {
        fn begin(&mut self, new_line: i64) {
            if self.dels == 0 && self.adds.is_empty() {
                self.start = new_line;
            }
        }
        fn flush(&mut self, added: &mut Vec<i64>, modified: &mut Vec<i64>, deleted: &mut Vec<i64>) {
            if self.dels > 0 && !self.adds.is_empty() {
                modified.append(&mut self.adds);
            } else if !self.adds.is_empty() {
                added.append(&mut self.adds);
            } else if self.dels > 0 {
                deleted.push(self.start - 1);
            }
            self.dels = 0;
        }
    }
    let mut added: Vec<i64> = Vec::new();
    let mut modified = Vec::new();
    let mut deleted = Vec::new();
    let mut block = Block::default();
    let mut new_line = 0;
    let mut in_hunk = false;
    for line in diff.split('\n') {
        if line.starts_with("@@") {
            block.flush(&mut added, &mut modified, &mut deleted);
            in_hunk = true;
            new_line = parse_new_start(line);
        } else if !in_hunk || line.starts_with('\\') {
            // Pre-hunk header / "\ No newline": neither +/- nor a line.
        } else if line.starts_with('+') {
            block.begin(new_line);
            block.adds.push(new_line);
            new_line += 1;
        } else if line.starts_with('-') {
            block.begin(new_line);
            block.dels += 1;
        } else {
            block.flush(&mut added, &mut modified, &mut deleted);
            new_line += 1;
        }
    }
    block.flush(&mut added, &mut modified, &mut deleted);
    if added.is_empty() && modified.is_empty() && deleted.is_empty() {
        return None;
    }
    Some((added, modified, deleted))
}

/// New-file start of a `@@ -a,b +c,d @@` header. Ports Go `parseNewStart`.
fn parse_new_start(hdr: &str) -> i64 {
    let Some(i) = hdr.find('+') else { return 1 };
    let mut rest = &hdr[i + 1..];
    if let Some(end) = rest.find([',', ' ']) {
        rest = &rest[..end];
    }
    rest.parse().unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Scratch repo: one committed file, one modified, one staged-new,
    /// one untracked. Unique per process so parallel tests don't collide.
    fn scratch_repo(tag: &str) -> (crate::testutil::TempDir, PathBuf) {
        let dir = crate::testutil::tempdir(tag);
        let root = dir.path().to_path_buf();
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(args)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
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
        std::fs::write(root.join("tracked.txt"), "one\ntwo\nthree\n").unwrap();
        git(&["add", "tracked.txt"]);
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
        std::fs::write(root.join("tracked.txt"), "one\nTWO\nthree\nfour\n").unwrap();
        std::fs::write(root.join("staged.txt"), "new\n").unwrap();
        git(&["add", "staged.txt"]);
        std::fs::write(root.join("untracked.txt"), "u\n").unwrap();
        (dir, root)
    }

    #[test]
    fn status_maps_porcelain_codes() {
        let (_tmp, root) = scratch_repo("status");
        let status = git_status(&root).unwrap();
        assert_eq!(status.get("tracked.txt").map(String::as_str), Some("M"));
        assert_eq!(status.get("staged.txt").map(String::as_str), Some("A"));
        assert_eq!(status.get("untracked.txt").map(String::as_str), Some("U"));
    }

    #[test]
    fn diff_and_hunks_cover_edit() {
        let (_tmp, root) = scratch_repo("hunks");
        let diff = git_diff(&root, "tracked.txt");
        assert!(diff.contains("-two") && diff.contains("+TWO"));
        let (added, modified, deleted) = git_hunks(&root, "tracked.txt").unwrap();
        // Line 2 replaced, line 4 inserted.
        assert_eq!(modified, vec![2]);
        assert_eq!(added, vec![4]);
        assert!(deleted.is_empty());
        // Staged-new files diff against HEAD as fully added (same as Go).
        let (added, _, _) = git_hunks(&root, "staged.txt").unwrap();
        assert_eq!(added, vec![1]);
    }

    #[test]
    fn fully_deleted_file_marks_minus_one() {
        let (_tmp, root) = scratch_repo("del");
        std::fs::write(root.join("doomed.txt"), "a\nb\n").unwrap();
        let git = |args: &[&str]| {
            Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(args)
                .output()
                .unwrap()
        };
        git(&["add", "doomed.txt"]);
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
        std::fs::remove_file(root.join("doomed.txt")).unwrap();
        assert_eq!(
            git_hunks(&root, "doomed.txt"),
            Some((vec![], vec![], vec![-1]))
        );
    }

    #[test]
    fn unavailable_repo_fails_quiet() {
        let dir = crate::testutil::tempdir("nogit");
        let root = dir.path().to_path_buf();
        assert!(!git_available(&root));
        assert!(git_status(&root).is_none());
        assert_eq!(git_diff(&root, "x"), "");
        assert!(git_hunks(&root, "x").is_none());
    }

    #[test]
    fn rel_to_slash_strips_repo_prefix() {
        assert_eq!(
            rel_to_slash(Path::new("/r"), Path::new("/r/sub")).as_deref(),
            Some("sub")
        );
        assert_eq!(
            rel_to_slash(Path::new("/r"), Path::new("/r")).as_deref(),
            Some(".")
        );
        assert_eq!(
            rel_to_slash(Path::new("/r"), Path::new("/other")).as_deref(),
            Some("../other")
        );
    }

    #[test]
    fn map_xy_prefers_staged() {
        assert_eq!(map_xy("M."), "M");
        assert_eq!(map_xy(".M"), "M");
        assert_eq!(map_xy("A."), "A");
        assert_eq!(map_xy("R."), "R");
        assert_eq!(map_xy("UU"), "!");
        assert_eq!(map_xy("T."), "M");
        assert_eq!(map_xy(""), "M");
    }

    #[test]
    fn parse_new_start_reads_plus_range() {
        assert_eq!(parse_new_start("@@ -1,3 +4,5 @@"), 4);
        assert_eq!(parse_new_start("@@ -0,0 +1 @@"), 1);
        assert_eq!(parse_new_start("garbage"), 1);
    }
}
