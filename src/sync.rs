//! Sync state and commit messages, shared by the git CLI and GitHub backends.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::paths;
use crate::storage::{Blob, Files};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Remote {
    None,
    NoUpstream,
    Tracking { ahead: usize, behind: usize },
    Offline(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct State {
    pub remote: Remote,
    /// Number of uncommitted changed paths under the gition root.
    pub changes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    Added(String),
    Modified(String),
    Deleted(String),
    Renamed(String, String),
}

pub fn commit_message(prefix: &str, changes: &[Change]) -> String {
    let parts: Vec<String> = changes
        .iter()
        .map(|c| match c {
            Change::Added(p) => format!("add {p}"),
            Change::Modified(p) => format!("edit {p}"),
            Change::Deleted(p) => format!("delete {p}"),
            Change::Renamed(a, b) => format!("rename {a} -> {b}"),
        })
        .collect();
    format!("{prefix} {}", parts.join(", ")).trim().to_string()
}

pub fn committed(n: usize) -> String {
    match n {
        0 => "nothing to commit".to_string(),
        1 => "committed 1 file".to_string(),
        n => format!("committed {n} files"),
    }
}

/// Paths whose content differs between `base` and `work`; `None` means deleted.
pub fn changed(base: &Files, work: &Files) -> BTreeMap<PathBuf, Option<Blob>> {
    let mut out = BTreeMap::new();
    for (p, blob) in work {
        if base.get(p) != Some(blob) {
            out.insert(p.clone(), Some(blob.clone()));
        }
    }
    for p in base.keys() {
        if !work.contains_key(p) {
            out.insert(p.clone(), None);
        }
    }
    out
}

/// Changes between two snapshots, pairing a deleted and an added file with the same content as a rename.
pub fn diff(base: &Files, work: &Files) -> Vec<Change> {
    let changed = changed(base, work);
    let mut added: Vec<&PathBuf> =
        changed.iter().filter(|(p, b)| b.is_some() && !base.contains_key(*p)).map(|(p, _)| p).collect();
    let mut out = Vec::new();
    for (p, blob) in &changed {
        match blob {
            None => {
                let old = &base[p];
                match added.iter().position(|a| work.get(*a) == Some(old)) {
                    Some(i) => out.push(Change::Renamed(paths::to_slash(p), paths::to_slash(added.remove(i)))),
                    None => out.push(Change::Deleted(paths::to_slash(p))),
                }
            }
            Some(_) if base.contains_key(p) => out.push(Change::Modified(paths::to_slash(p))),
            Some(_) => {}
        }
    }
    out.extend(added.into_iter().map(|p| Change::Added(paths::to_slash(p))));
    out
}

/// Replays the changes from `base` to `ours` on top of `theirs`. A file changed
/// differently on both sides takes their version, and ours is kept next to it as
/// `name.conflict.ext`. Returns the merged files and the conflicting paths.
pub fn rebase(base: &Files, ours: &Files, theirs: &Files) -> (Files, Vec<String>) {
    let upstream = changed(base, theirs);
    let mut out = theirs.clone();
    let mut conflicts = Vec::new();
    for (p, mine) in changed(base, ours) {
        match upstream.get(&p) {
            Some(their) if *their != mine => {
                if let Some(mine) = mine {
                    out.insert(conflict_copy(&p), mine);
                }
                conflicts.push(paths::to_slash(&p));
            }
            _ => {
                match mine {
                    Some(b) => out.insert(p, b),
                    None => out.remove(&p),
                };
            }
        }
    }
    (out, conflicts)
}

fn conflict_copy(p: &std::path::Path) -> PathBuf {
    let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = p.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    paths::parent(p).join(format!("{stem}.conflict{ext}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_detects_renames() {
        let d = |s: &str| Blob::Data(s.as_bytes().to_vec());
        let base: Files = [("a.md", d("a")), ("b.md", d("b")), ("c.md", d("c"))]
            .into_iter()
            .map(|(p, b)| (PathBuf::from(p), b))
            .collect();
        let work: Files = [("a.md", d("a2")), ("x/b.md", d("b")), ("new.md", d("n"))]
            .into_iter()
            .map(|(p, b)| (PathBuf::from(p), b))
            .collect();
        assert_eq!(
            commit_message("gition:", &diff(&base, &work)),
            "gition: edit a.md, rename b.md -> x/b.md, delete c.md, add new.md"
        );
        assert!(diff(&base, &base).is_empty());
    }

    #[test]
    fn rebase_keeps_both_sides() {
        let d = |s: &str| Blob::Data(s.as_bytes().to_vec());
        let files = |list: &[(&str, &str)]| -> Files { list.iter().map(|(p, c)| (PathBuf::from(p), d(c))).collect() };
        let base = files(&[("a.md", "a"), ("b.md", "b"), ("c.md", "c")]);
        let ours = files(&[("a.md", "a-ours"), ("b.md", "b-ours"), ("new.md", "n")]);
        let theirs = files(&[("a.md", "a"), ("b.md", "b-theirs"), ("c.md", "c"), ("t.md", "t")]);
        let (merged, conflicts) = rebase(&base, &ours, &theirs);
        assert_eq!(conflicts, ["b.md"]);
        assert_eq!(
            merged,
            files(&[
                ("a.md", "a-ours"),
                ("b.md", "b-theirs"),
                ("b.conflict.md", "b-ours"),
                ("new.md", "n"),
                ("t.md", "t"),
            ])
        );
    }

    #[test]
    fn files_not_downloaded_count_by_sha() {
        let d = |s: &str| Blob::Data(s.as_bytes().to_vec());
        let r = |s: &str| Blob::Remote(s.into());
        let files =
            |list: Vec<(&str, Blob)>| -> Files { list.into_iter().map(|(p, b)| (PathBuf::from(p), b)).collect() };
        let base = files(vec![("a.md", r("sa")), ("b.md", r("sb")), ("c.md", r("sc")), ("d.md", d("d"))]);
        // a.md untouched, b.md moved, c.md edited.
        let ours = files(vec![("a.md", r("sa")), ("x/b.md", r("sb")), ("c.md", d("c-ours")), ("d.md", d("d"))]);
        assert_eq!(
            changed(&base, &ours),
            [("b.md".into(), None), ("c.md".into(), Some(d("c-ours"))), ("x/b.md".into(), Some(r("sb")))].into()
        );
        assert_eq!(commit_message("", &diff(&base, &ours)), "rename b.md -> x/b.md, edit c.md");

        // Upstream changed a.md, still not downloaded here, and d.md: theirs win, no conflict.
        let theirs = files(vec![("a.md", d("a-theirs")), ("b.md", r("sb")), ("c.md", r("sc")), ("d.md", r("sd2"))]);
        let (merged, conflicts) = rebase(&base, &ours, &theirs);
        assert!(conflicts.is_empty(), "{conflicts:?}");
        assert_eq!(
            merged,
            files(vec![("a.md", d("a-theirs")), ("c.md", d("c-ours")), ("d.md", r("sd2")), ("x/b.md", r("sb"))])
        );
    }
}
