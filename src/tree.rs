//! The page tree shown on the left.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::paths;
use crate::repo::{AGENTS, INDEX};
use crate::storage::Storage;

#[derive(Debug, Clone)]
pub struct Node {
    /// Repo-relative path ("" for the root).
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    pub children: Vec<Node>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Visible {
    pub path: PathBuf,
    pub name: String,
    pub depth: usize,
    pub is_dir: bool,
    pub expanded: bool,
}

pub struct Filter<'a> {
    pub show_all: bool,
    pub assets_dir: &'a str,
}

impl Filter<'_> {
    fn keep(&self, rel: &Path, name: &str, is_dir: bool) -> bool {
        if name == ".git" {
            return false;
        }
        if self.show_all {
            return true;
        }
        let top = rel.parent() == Some(Path::new(""));
        // A folder's INDEX.md is the folder's own page; the root has no row, so its INDEX.md gets one.
        if name.starts_with('.') || name == INDEX && !top {
            return false;
        }
        if is_dir {
            // Only the top-level assets dir is hidden.
            return !(top && name == self.assets_dir);
        }
        if top && name == AGENTS {
            return false;
        }
        paths::is_markdown(rel)
    }
}

pub fn build(fs: &dyn Storage, filter: &Filter) -> Node {
    Node { path: PathBuf::new(), name: fs.name(), is_dir: true, children: children(fs, Path::new(""), filter) }
}

fn children(fs: &dyn Storage, rel: &Path, filter: &Filter) -> Vec<Node> {
    let Ok(entries) = fs.read_dir(rel) else { return Vec::new() };
    let mut nodes: Vec<Node> = entries
        .into_iter()
        .filter_map(|e| {
            let (name, is_dir) = (e.name, e.is_dir);
            let child = rel.join(&name);
            if !filter.keep(&child, &name, is_dir) {
                return None;
            }
            let label = if !is_dir && !filter.show_all && paths::is_markdown(&child) {
                name[..name.len() - 3].to_string()
            } else {
                name
            };
            let kids = if is_dir { children(fs, &child, filter) } else { Vec::new() };
            Some(Node { path: child, name: label, is_dir, children: kids })
        })
        .collect();
    nodes.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    nodes
}

/// Flattens the tree into display rows; the root is always expanded.
pub fn flatten(root: &Node, expanded: &HashSet<PathBuf>) -> Vec<Visible> {
    let mut out = Vec::new();
    fn walk(nodes: &[Node], depth: usize, expanded: &HashSet<PathBuf>, out: &mut Vec<Visible>) {
        for n in nodes {
            let open = n.is_dir && expanded.contains(&n.path);
            out.push(Visible { path: n.path.clone(), name: n.name.clone(), depth, is_dir: n.is_dir, expanded: open });
            if open {
                walk(&n.children, depth + 1, expanded, out);
            }
        }
    }
    walk(&root.children, 0, expanded, &mut out);
    out
}

/// All markdown files, skipping dot-directories.
pub fn markdown_files(fs: &dyn Storage) -> Vec<PathBuf> {
    fn walk(fs: &dyn Storage, rel: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = fs.read_dir(rel) else { return };
        for e in entries {
            if e.name.starts_with('.') {
                continue;
            }
            let child = rel.join(&e.name);
            if e.is_dir {
                walk(fs, &child, out);
            } else if paths::is_markdown(&child) {
                out.push(child);
            }
        }
    }
    let mut out = Vec::new();
    walk(fs, Path::new(""), &mut out);
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::FsStorage;
    use std::fs;

    #[test]
    fn builds_filtered_tree() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for f in [
            "INDEX.md",
            "b.md",
            "A.md",
            "notes.txt",
            "assets/x.png",
            "Projects/INDEX.md",
            "Projects/plan.md",
            ".hidden/x.md",
            "AGENTS.md",
            "Projects/AGENTS.md",
        ] {
            let p = root.join(f);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, "x").unwrap();
        }
        let store = FsStorage::new(root.to_path_buf());
        let tree = build(&store, &Filter { show_all: false, assets_dir: "assets" });
        let mut expanded = HashSet::new();
        expanded.insert(PathBuf::from("Projects"));
        let names: Vec<_> = flatten(&tree, &expanded).into_iter().map(|v| (v.name, v.depth)).collect();
        assert_eq!(
            names,
            [
                ("Projects".into(), 0),
                ("AGENTS".into(), 1),
                ("plan".into(), 1),
                ("A".into(), 0),
                ("b".into(), 0),
                ("INDEX".into(), 0)
            ]
        );

        let all = build(&store, &Filter { show_all: true, assets_dir: "assets" });
        let names: Vec<_> = all.children.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, [".hidden", "assets", "Projects", "A.md", "AGENTS.md", "b.md", "INDEX.md", "notes.txt"]);

        assert_eq!(markdown_files(&store).len(), 7);
    }
}
