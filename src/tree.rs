//! File tree — scans a root directory for markdown files and produces a
//! flat display list honoring per-folder expand/collapse state.

use std::cell::RefCell;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TreeKind {
    Folder,
    Markdown,
    /// Synthetic ".." row shown above the root when the root has a parent.
    /// Clicking it re-roots the tree one level up.
    Parent,
}

#[derive(Debug, Clone)]
pub struct TreeEntry {
    pub path: PathBuf,
    pub depth: u32,
    pub kind: TreeKind,
    pub expanded: bool,
}

pub struct FileTree {
    pub root: PathBuf,
    pub expanded: HashSet<PathBuf>,
    /// Memoised flatten() result + the instant it was built. Rebuilt on
    /// any structural change (toggle / expand / set_root) and refreshed
    /// after CACHE_TTL even without changes so the tree picks up files
    /// that appeared on disk between renders.
    cache: RefCell<Option<(Vec<TreeEntry>, Instant)>>,
}

/// How long a cached flatten() is reused before re-walking the disk.
/// Long enough to coalesce a burst of paints; short enough that the
/// sidebar still notices new files within a beat.
const CACHE_TTL: Duration = Duration::from_millis(1500);

impl FileTree {
    pub fn new(root: PathBuf) -> Self {
        let mut expanded = HashSet::new();
        expanded.insert(root.clone());
        Self { root, expanded, cache: RefCell::new(None) }
    }

    pub fn toggle(&mut self, path: &Path) {
        if !self.expanded.insert(path.to_path_buf()) {
            self.expanded.remove(path);
        }
        self.invalidate();
    }

    /// Mark `path` expanded without toggling. Used by quick-open to
    /// ensure ancestors of the newly-opened file are visible.
    pub fn expand(&mut self, path: PathBuf) {
        if self.expanded.insert(path) {
            self.invalidate();
        }
    }

    /// Replace the tree's root directory. Forgets previous expand state.
    pub fn set_root(&mut self, path: PathBuf) {
        self.expanded.clear();
        self.expanded.insert(path.clone());
        self.root = path;
        self.invalidate();
    }

    pub fn invalidate(&self) {
        *self.cache.borrow_mut() = None;
    }

    pub fn is_expanded(&self, path: &Path) -> bool {
        self.expanded.contains(path)
    }

    /// Return a flat, ordered display list. Folders first, then files,
    /// alphabetical. Hidden entries (starting with '.') are skipped. If
    /// the root has a parent directory, a synthetic ".." entry is
    /// prepended so the user can navigate up.
    ///
    /// Cached: the underlying disk walk runs at most once per
    /// CACHE_TTL, or whenever structural state changes (toggle / expand
    /// / set_root). Without the cache a sidebar of a few hundred
    /// entries can flood the compositor with read_dir traffic during
    /// redraw bursts.
    pub fn flatten(&self) -> Vec<TreeEntry> {
        if let Some((cached, when)) = self.cache.borrow().as_ref() {
            if when.elapsed() < CACHE_TTL {
                return cached.clone();
            }
        }
        let v = self.compute_flatten();
        *self.cache.borrow_mut() = Some((v.clone(), Instant::now()));
        v
    }

    fn compute_flatten(&self) -> Vec<TreeEntry> {
        let mut out = Vec::new();
        if let Some(parent) = self.root.parent() {
            out.push(TreeEntry {
                path: parent.to_path_buf(),
                depth: 0,
                kind: TreeKind::Parent,
                expanded: false,
            });
        }
        let root = self.root.clone();
        if root.is_dir() {
            out.push(TreeEntry {
                path: root.clone(),
                depth: 0,
                kind: TreeKind::Folder,
                expanded: self.is_expanded(&root),
            });
            if self.is_expanded(&root) {
                self.push_children(&root, 1, &mut out);
            }
        } else if root.is_file() && is_md(&root) {
            out.push(TreeEntry {
                path: root,
                depth: 0,
                kind: TreeKind::Markdown,
                expanded: false,
            });
        }
        out
    }

    fn push_children(&self, dir: &Path, depth: u32, out: &mut Vec<TreeEntry>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        let mut folders: Vec<PathBuf> = Vec::new();
        let mut files: Vec<PathBuf> = Vec::new();
        for entry in rd.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.starts_with('.') {
                continue;
            }
            let p = entry.path();
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_dir() {
                folders.push(p);
            } else if ft.is_file() && is_md(&p) {
                files.push(p);
            }
        }
        folders.sort();
        files.sort();

        for f in folders {
            let expanded = self.is_expanded(&f);
            out.push(TreeEntry {
                path: f.clone(),
                depth,
                kind: TreeKind::Folder,
                expanded,
            });
            if expanded {
                self.push_children(&f, depth + 1, out);
            }
        }
        for f in files {
            out.push(TreeEntry {
                path: f,
                depth,
                kind: TreeKind::Markdown,
                expanded: false,
            });
        }
    }
}

fn is_md(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("md") || e.eq_ignore_ascii_case("markdown"))
        .unwrap_or(false)
}

/// Depth-first search for the first markdown file under `dir`, using the
/// same "folders first then files, alphabetical, no dotfiles" ordering as
/// the sidebar. Returns `None` if nothing is found. Used to auto-open
/// something on startup so `mdrdr <dir>` lands the user on real content.
pub fn first_markdown_in(dir: &Path) -> Option<PathBuf> {
    let mut folders: Vec<PathBuf> = Vec::new();
    let mut files: Vec<PathBuf> = Vec::new();
    let rd = std::fs::read_dir(dir).ok()?;
    for entry in rd.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with('.') {
            continue;
        }
        let p = entry.path();
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_dir() {
            folders.push(p);
        } else if ft.is_file() && is_md(&p) {
            files.push(p);
        }
    }
    files.sort();
    if let Some(f) = files.into_iter().next() {
        return Some(f);
    }
    folders.sort();
    for d in folders {
        if let Some(f) = first_markdown_in(&d) {
            return Some(f);
        }
    }
    None
}

/// Recursively walk `root` and stream batches of markdown files to
/// `on_batch` as they're discovered. Honors `cancel` — set the atomic
/// to `true` from another thread to stop the walk at the next
/// directory boundary; `on_batch` returning `false` does the same.
///
/// Used by the Ctrl+P quick-open panel: a worker thread runs this and
/// flushes batches into shared state, so the UI shows results streaming
/// in without ever blocking the event loop. Even a misdirected Ctrl+P
/// at `$HOME` stays responsive — the user can type, scroll, or close
/// the panel while the walk is still running.
///
/// Hidden entries (dot-prefixed) and well-known build/cache directory
/// names are skipped wholesale; depth is capped at 12.
pub fn walk_streaming<F>(root: &Path, cancel: &AtomicBool, mut on_batch: F)
where
    F: FnMut(Vec<PathBuf>) -> bool,
{
    let mut batch: Vec<PathBuf> = Vec::new();
    let mut last_flush = Instant::now();
    walk_rec(root, 0, &mut batch, cancel, &mut on_batch, &mut last_flush);
    if !batch.is_empty() && !cancel.load(Ordering::Relaxed) {
        let _ = on_batch(batch);
    }
}

/// Flush a batch when it reaches this size, even if the time budget
/// hasn't been hit yet. Keeps the UI's "files found so far" count moving
/// even on fast disks.
const FLUSH_BATCH: usize = 64;

/// Or flush this often, even with a small batch. Keeps the panel feeling
/// alive on slow disks where one read_dir can take many ms.
const FLUSH_INTERVAL: Duration = Duration::from_millis(80);

/// Directory names we skip wholesale because they're never source the
/// user wants to fuzzy-find and they're often *enormous*. Hidden
/// (dot-prefixed) names are filtered separately so `.git`, `.venv`,
/// `.cache` etc. don't need to be listed here.
const SKIP_DIR_NAMES: &[&str] = &[
    "node_modules",
    "target",
    "build",
    "dist",
    "out",
    "vendor",
    "__pycache__",
    "Pods",
    "bower_components",
];

fn walk_rec<F>(
    dir: &Path,
    depth: u32,
    batch: &mut Vec<PathBuf>,
    cancel: &AtomicBool,
    on_batch: &mut F,
    last_flush: &mut Instant,
) -> bool
where
    F: FnMut(Vec<PathBuf>) -> bool,
{
    // Cap — matches common editor defaults and bounds even pathological
    // symlink loops or deeply-nested generated trees.
    if depth > 12 {
        return true;
    }
    if cancel.load(Ordering::Relaxed) {
        return false;
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return true };
    for entry in rd.flatten() {
        if cancel.load(Ordering::Relaxed) {
            return false;
        }
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with('.') {
            continue;
        }
        let p = entry.path();
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_dir() {
            if SKIP_DIR_NAMES.iter().any(|&s| name_str == s) {
                continue;
            }
            if !walk_rec(&p, depth + 1, batch, cancel, on_batch, last_flush) {
                return false;
            }
        } else if ft.is_file() && is_md(&p) {
            batch.push(p);
            if batch.len() >= FLUSH_BATCH || last_flush.elapsed() >= FLUSH_INTERVAL {
                let to_send = std::mem::take(batch);
                if !on_batch(to_send) {
                    return false;
                }
                *last_flush = Instant::now();
            }
        }
    }
    true
}
