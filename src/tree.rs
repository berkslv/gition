//! The page tree shown on the left.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::paths;
use crate::repo::INDEX;

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
        if name.starts_with('.') || name == INDEX {
            return false;
        }
        if is_dir {
            // Only the top-level assets dir is hidden.
            return !(rel.parent() == Some(Path::new("")) && name == self.assets_dir);
        }
        paths::is_markdown(rel)
    }
}

pub fn build(root: &Path, filter: &Filter) -> Node {
    let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "/".into());
    Node { path: PathBuf::new(), name, is_dir: true, children: children(root, Path::new(""), filter) }
}

fn children(root: &Path, rel: &Path, filter: &Filter) -> Vec<Node> {
    let Ok(rd) = fs::read_dir(root.join(rel)) else { return Vec::new() };
    let mut nodes: Vec<Node> = rd
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let is_dir = e.file_type().ok()?.is_dir();
            let child = rel.join(&name);
            if !filter.keep(&child, &name, is_dir) {
                return None;
            }
            let label = if !is_dir && !filter.show_all && paths::is_markdown(&child) {
                name[..name.len() - 3].to_string()
            } else {
                name
            };
            let kids = if is_dir { children(root, &child, filter) } else { Vec::new() };
            Some(Node { path: child, name: label, is_dir, children: kids })
        })
        .collect();
    nodes.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    nodes
}

/// Flattens the tree into display rows; the root is always expanded.
pub fn flatten(root: &Node, expanded: &HashSet<PathBuf>) -> Vec<Visible> {
    let mut out =
        vec![Visible { path: PathBuf::new(), name: root.name.clone(), depth: 0, is_dir: true, expanded: true }];
    fn walk(nodes: &[Node], depth: usize, expanded: &HashSet<PathBuf>, out: &mut Vec<Visible>) {
        for n in nodes {
            let open = n.is_dir && expanded.contains(&n.path);
            out.push(Visible { path: n.path.clone(), name: n.name.clone(), depth, is_dir: n.is_dir, expanded: open });
            if open {
                walk(&n.children, depth + 1, expanded, out);
            }
        }
    }
    walk(&root.children, 1, expanded, &mut out);
    out
}

/// All markdown files under `root`, skipping dot-directories.
pub fn markdown_files(root: &Path) -> Vec<PathBuf> {
    fn walk(root: &Path, rel: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = fs::read_dir(root.join(rel)) else { return };
        for e in rd.flatten() {
            let name = e.file_name();
            if name.to_string_lossy().starts_with('.') {
                continue;
            }
            let child = rel.join(&name);
            match e.file_type() {
                Ok(t) if t.is_dir() => walk(root, &child, out),
                Ok(_) if paths::is_markdown(&child) => out.push(child),
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    walk(root, Path::new(""), &mut out);
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

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
        ] {
            let p = root.join(f);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, "x").unwrap();
        }
        let tree = build(root, &Filter { show_all: false, assets_dir: "assets" });
        let mut expanded = HashSet::new();
        expanded.insert(PathBuf::from("Projects"));
        let names: Vec<_> = flatten(&tree, &expanded).into_iter().skip(1).map(|v| (v.name, v.depth)).collect();
        assert_eq!(names, [("Projects".into(), 1), ("plan".into(), 2), ("A".into(), 1), ("b".into(), 1)]);

        let all = build(root, &Filter { show_all: true, assets_dir: "assets" });
        let names: Vec<_> = all.children.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, [".hidden", "assets", "Projects", "A.md", "b.md", "INDEX.md", "notes.txt"]);

        assert_eq!(markdown_files(root).len(), 5);
    }
}
