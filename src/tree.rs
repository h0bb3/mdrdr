//! File tree — scans a root directory for markdown files and produces a
//! flat display list honoring per-folder expand/collapse state.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeKind {
    Folder,
    Markdown,
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
}

impl FileTree {
    pub fn new(root: PathBuf) -> Self {
        let mut expanded = HashSet::new();
        expanded.insert(root.clone());
        Self { root, expanded }
    }

    pub fn toggle(&mut self, path: &Path) {
        if !self.expanded.insert(path.to_path_buf()) {
            self.expanded.remove(path);
        }
    }

    pub fn is_expanded(&self, path: &Path) -> bool {
        self.expanded.contains(path)
    }

    /// Return a flat, ordered display list. Folders first, then files,
    /// alphabetical. Hidden entries (starting with '.') are skipped.
    pub fn flatten(&self) -> Vec<TreeEntry> {
        let mut out = Vec::new();
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
