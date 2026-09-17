//! In-memory filesystem index: flat file list plus directory map.
//!
//! Ports `index.go`: bounded-concurrency walk (`available_parallelism *
//! 4`, mirroring `NumCPU * 4`), version-control dirs never listed, ignored
//! entries listed-but-dimmed, root published immediately, git status
//! overlaid after the walk (git overlay lands with the git slice).

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    Arc, Mutex, RwLock,
};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::ignore::{read_gitignore, IgnoreSet};

/// Version-control internals: not even listed. Ports Go `vcsDirs`.
fn is_vcs_dir(name: &str) -> bool {
    matches!(name, ".git" | ".hg" | ".svn")
}

#[derive(Clone, Debug)]
pub struct FileEntry {
    pub path: String,
    pub name: String,
    pub size: u64,
    /// Cached lowercase path for matching. Ports Go `FileEntry.lower`.
    pub lower: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Node {
    pub name: String,
    pub path: String,
    pub dir: bool,
    pub size: u64,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub ignored: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub status: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub dirty: bool,
}

/// Directories-first, then case-insensitive by name. Ports Go `sortNodes`.
fn sort_nodes(kids: &mut [Node]) {
    kids.sort_by(|a, b| {
        b.dir
            .cmp(&a.dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
}

struct Snapshot {
    files: Vec<FileEntry>,
    children: HashMap<String, Vec<Node>>,
}

pub struct Index {
    root: PathBuf,
    inner: RwLock<Snapshot>,
    built_at: RwLock<String>,
    build_ms: AtomicU64,
    ready: AtomicBool,
}

impl Index {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            inner: RwLock::new(Snapshot {
                files: Vec::new(),
                children: HashMap::new(),
            }),
            built_at: RwLock::new(String::new()),
            build_ms: AtomicU64::new(0),
            ready: AtomicBool::new(false),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn ready(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }

    pub fn stats(&self) -> (usize, String, u64) {
        let inner = self.inner.read().unwrap();
        (
            inner.files.len(),
            self.built_at.read().unwrap().clone(),
            self.build_ms.load(Ordering::Relaxed),
        )
    }

    pub fn files(&self) -> Vec<FileEntry> {
        self.inner.read().unwrap().files.clone()
    }

    /// List a directory for the tree. Ignored directories are never
    /// walked, so their contents are read from disk on demand, all marked
    /// ignored. Ports Go `Index.Children`.
    pub fn children(&self, dir: &str) -> Option<Vec<Node>> {
        let inner = self.inner.read().unwrap();
        if let Some(kids) = inner.children.get(dir) {
            return Some(kids.clone());
        }
        let under = under_ignored(&inner.children, dir);
        drop(inner);
        if !under {
            return None;
        }
        self.list_ignored(dir)
    }

    fn list_ignored(&self, dir: &str) -> Option<Vec<Node>> {
        let entries = std::fs::read_dir(self.root.join(dir)).ok()?;
        let mut kids = Vec::new();
        for entry in entries.flatten() {
            if entry.file_type().map(|t| t.is_symlink()).unwrap_or(true) {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            kids.push(Node {
                name: name.clone(),
                path: format!("{dir}/{name}"),
                dir: entry.file_type().map(|t| t.is_dir()).unwrap_or(false),
                size: 0,
                ignored: true,
                status: String::new(),
                dirty: false,
            });
        }
        sort_nodes(&mut kids);
        Some(kids)
    }

    /// Walk the tree once, honouring `.gitignore` at every level. Root
    /// entries publish immediately so the UI renders without waiting for
    /// the full scan. Ports Go `Index.Build` (minus the git overlay, which
    /// lands with the git slice).
    pub fn build(&self) {
        let start = Instant::now();
        let root_set = Arc::new(IgnoreSet::new(&read_gitignore(&self.root, "")));
        // Git status needs only the repo root, not the walk result, so it
        // runs concurrently with the walk. Ports the Go goroutine.
        let git_root = self.root.clone();
        let git_handle = std::thread::spawn(move || crate::git::git_status(&git_root));

        let files = Mutex::new(Vec::new());
        let children: Mutex<HashMap<String, Vec<Node>>> = Mutex::new(HashMap::new());
        let queue: Mutex<VecDeque<(PathBuf, String, Arc<IgnoreSet>)>> = Mutex::new(VecDeque::new());
        let pending = AtomicUsize::new(1);
        queue
            .lock()
            .unwrap()
            .push_back((self.root.clone(), String::new(), root_set));

        let workers = std::thread::available_parallelism()
            .map(|n| n.get() * 4)
            .unwrap_or(4);
        std::thread::scope(|scope| {
            for _ in 0..workers {
                scope.spawn(|| loop {
                    let task = queue.lock().unwrap().pop_front();
                    match task {
                        Some((abs, rel, ig)) => {
                            self.walk_dir(&abs, &rel, &ig, &files, &children, &queue, &pending);
                            pending.fetch_sub(1, Ordering::AcqRel);
                        }
                        None => {
                            if pending.load(Ordering::Acquire) == 0 {
                                break;
                            }
                            std::thread::yield_now();
                        }
                    }
                });
            }
        });

        let mut files = files.into_inner().unwrap();
        files.sort_by(|a: &FileEntry, b: &FileEntry| a.path.cmp(&b.path));
        let mut children = children.into_inner().unwrap();

        // Overlay git working-tree status onto file nodes. Every ancestor
        // directory of a changed file is dirty, so a collapsed folder can
        // badge without fetching its subtree.
        if let Some(status) = git_handle.join().ok().flatten() {
            let mut dirty = std::collections::HashSet::new();
            for p in status.keys() {
                let mut p = p.as_str();
                while let Some(i) = p.rfind('/') {
                    p = &p[..i];
                    dirty.insert(p.to_string());
                }
            }
            for kids in children.values_mut() {
                for kid in kids.iter_mut() {
                    if kid.dir {
                        kid.dirty = dirty.contains(&kid.path);
                    } else if let Some(code) = status.get(&kid.path) {
                        kid.status = code.clone();
                    }
                }
            }
        }

        {
            let mut inner = self.inner.write().unwrap();
            inner.files = files;
            inner.children = children;
        }
        *self.built_at.write().unwrap() = rfc3339_now();
        self.build_ms
            .store(start.elapsed().as_millis() as u64, Ordering::Relaxed);
        self.ready.store(true, Ordering::Release);
    }

    #[allow(clippy::too_many_arguments)]
    fn walk_dir(
        &self,
        abs: &Path,
        rel: &str,
        ig: &Arc<IgnoreSet>,
        files: &Mutex<Vec<FileEntry>>,
        children: &Mutex<HashMap<String, Vec<Node>>>,
        queue: &Mutex<VecDeque<(PathBuf, String, Arc<IgnoreSet>)>>,
        pending: &AtomicUsize,
    ) {
        let entries = match std::fs::read_dir(abs) {
            Ok(rd) => rd,
            Err(_) => return,
        };
        let ig: Arc<IgnoreSet> = if rel.is_empty() {
            ig.clone()
        } else {
            let extra = read_gitignore(abs, rel);
            if extra.is_empty() {
                ig.clone()
            } else {
                Arc::new(ig.child_set(&extra))
            }
        };
        let mut kids = Vec::new();
        let mut subdirs = Vec::new();
        for entry in entries.flatten() {
            // Follow nothing through symlinks; cycles are not worth the risk.
            if entry.file_type().map(|t| t.is_symlink()).unwrap_or(true) {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let child_rel = if rel.is_empty() {
                name.clone()
            } else {
                format!("{rel}/{name}")
            };
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            if ig.matches(&child_rel, is_dir) {
                // Listed so the tree can show it dimmed, but never walked
                // or indexed.
                if !(is_dir && is_vcs_dir(&name)) {
                    kids.push(Node {
                        name,
                        path: child_rel,
                        dir: is_dir,
                        size: 0,
                        ignored: true,
                        status: String::new(),
                        dirty: false,
                    });
                }
                continue;
            }
            if is_dir {
                kids.push(Node {
                    name: name.clone(),
                    path: child_rel.clone(),
                    dir: true,
                    size: 0,
                    ignored: false,
                    status: String::new(),
                    dirty: false,
                });
                subdirs.push((abs.join(&name), child_rel));
                continue;
            }
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            kids.push(Node {
                name: name.clone(),
                path: child_rel.clone(),
                dir: false,
                size,
                ignored: false,
                status: String::new(),
                dirty: false,
            });
            files.lock().unwrap().push(FileEntry {
                path: child_rel.clone(),
                name,
                size,
                lower: child_rel.to_lowercase(),
            });
        }
        sort_nodes(&mut kids);
        children
            .lock()
            .unwrap()
            .insert(rel.to_string(), kids.clone());

        // Publish the root immediately, mirroring Go.
        if rel.is_empty() {
            self.inner
                .write()
                .unwrap()
                .children
                .insert(String::new(), kids);
        }

        if !subdirs.is_empty() {
            let mut queue = queue.lock().unwrap();
            for (abs, rel) in subdirs {
                pending.fetch_add(1, Ordering::AcqRel);
                queue.push_back((abs, rel, ig.clone()));
            }
        }
    }
}

/// Whether `dir` sits inside a directory the walk listed as ignored.
/// Ports Go `underIgnoredLocked`.
fn under_ignored(children: &HashMap<String, Vec<Node>>, dir: &str) -> bool {
    if dir.is_empty() || dir.contains('\\') {
        return false;
    }
    for seg in dir.split('/') {
        if seg.is_empty() || seg == "." || seg == ".." {
            return false;
        }
    }
    // The nearest ancestor the walk visited decides: its entry for the
    // next segment down must be an ignored directory.
    let mut p = dir;
    loop {
        let parent = match p.rfind('/') {
            Some(i) => &p[..i],
            None => "",
        };
        if let Some(kids) = children.get(parent) {
            let name = p[parent.len()..].trim_start_matches('/');
            for kid in kids {
                if kid.name == name {
                    return kid.dir && kid.ignored;
                }
            }
            return false;
        }
        if parent.is_empty() {
            return false;
        }
        p = parent;
    }
}

/// Current UTC time as RFC3339 with nanoseconds, e.g.
/// `2026-09-17T08:30:00.123456789Z`. Go emits local-offset time; UTC is
/// the documented divergence.
fn rfc3339_now() -> String {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    let secs = now.as_secs() as i64;
    let nanos = now.subsec_nanos();
    let days = secs.div_euclid(86400);
    let tod = secs.rem_euclid(86400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{nanos:09}Z",
        (tod / 3600) as u8,
        ((tod % 3600) / 60) as u8,
        (tod % 60) as u8,
    )
}

/// Days since 1970-01-01 to (year, month, day). Howard Hinnant's algorithm.
fn civil_from_days(z: i64) -> (i64, u8, u8) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u8;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Best-effort symlink for the fixture. Windows creation needs
    /// Developer Mode or SeCreateSymbolicLinkPrivilege; where it fails
    /// the link is simply absent and the skip assertion below is vacuous.
    #[cfg(unix)]
    fn make_symlink(original: &Path, link: &Path) {
        std::os::unix::fs::symlink(original, link).unwrap();
    }
    #[cfg(windows)]
    fn make_symlink(original: &Path, link: &Path) {
        let _ = std::os::windows::fs::symlink_file(original, link);
    }
    #[cfg(not(any(unix, windows)))]
    fn make_symlink(_original: &Path, _link: &Path) {}

    /// Fixture: normal files, an ignored dir, VCS internals, a symlink,
    /// and a .gitignore with a negation.
    fn fixture() -> (crate::testutil::TempDir, PathBuf) {
        let dir = crate::testutil::tempdir("index");
        let root = dir.path().to_path_buf();
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::create_dir_all(root.join("node_modules")).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join("a.go"), "package main\n").unwrap();
        std::fs::write(root.join("sub/b.go"), "package sub\n").unwrap();
        std::fs::write(root.join("x.log"), "noise\n").unwrap();
        std::fs::write(root.join("keep.log"), "signal\n").unwrap();
        std::fs::write(root.join("node_modules/x.js"), "lib\n").unwrap();
        std::fs::write(root.join(".git/config"), "[core]\n").unwrap();
        std::fs::write(root.join(".gitignore"), "*.log\n!keep.log\n").unwrap();
        make_symlink(&root.join("a.go"), &root.join("link.go"));
        (dir, root)
    }

    #[test]
    fn walk_indexes_visible_and_skips_rest() {
        let (_tmp, root) = fixture();
        let ix = Index::new(root);
        ix.build();
        assert!(ix.ready());
        let mut paths: Vec<String> = ix.files().iter().map(|f| f.path.clone()).collect();
        paths.sort();
        assert_eq!(paths, vec![".gitignore", "a.go", "keep.log", "sub/b.go"]);
    }

    #[test]
    fn tree_lists_ignored_dimmed_and_hides_vcs() {
        let (_tmp, root) = fixture();
        let ix = Index::new(root);
        ix.build();
        let kids = ix.children("").unwrap();
        let by_name: HashMap<&str, &Node> = kids.iter().map(|k| (k.name.as_str(), k)).collect();
        assert!(!by_name.contains_key(".git"), ".git is never listed");
        assert!(by_name["node_modules"].ignored);
        assert!(!by_name["sub"].dir || !by_name["sub"].ignored);
        assert!(!by_name.contains_key("link.go"), "symlinks are skipped");
        // Dirs first, then names case-insensitively.
        let names: Vec<&str> = kids.iter().map(|k| k.name.as_str()).collect();
        let first_file = names
            .iter()
            .position(|n| !["node_modules", "sub"].contains(n))
            .unwrap();
        assert!(names[..first_file]
            .iter()
            .all(|n| ["node_modules", "sub"].contains(n)));
    }

    #[test]
    fn ignored_subtree_lists_on_demand() {
        let (_tmp, root) = fixture();
        let ix = Index::new(root);
        ix.build();
        let kids = ix.children("node_modules").unwrap();
        assert_eq!(kids.len(), 1);
        assert!(kids[0].ignored);
        assert!(ix.children("nope").is_none());
    }

    #[test]
    fn stats_report_counts_and_time() {
        let (_tmp, root) = fixture();
        let ix = Index::new(root);
        ix.build();
        let (n, built_at, _ms) = ix.stats();
        assert_eq!(n, 4);
        assert!(built_at.ends_with('Z') && built_at.contains('T'));
    }
}
