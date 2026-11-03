//! Locating a gition repo and initializing a new one.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use crate::git;

pub const INDEX: &str = "INDEX.md";

/// True if `dir` contains a file named exactly `INDEX.md` (case-sensitive,
/// even on case-insensitive filesystems).
pub fn has_index(dir: &Path) -> bool {
    fs::read_dir(dir).map(|rd| rd.flatten().any(|e| e.file_name() == INDEX && e.path().is_file())).unwrap_or(false)
}

/// A differently-cased index file such as `index.md`, if present.
pub fn miscased_index(dir: &Path) -> Option<PathBuf> {
    fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n != INDEX && n.eq_ignore_ascii_case(INDEX)))
}

pub enum Found {
    Repo(PathBuf),
    /// No INDEX.md anywhere up the tree; this is where init should happen.
    NeedsInit(PathBuf),
}

/// Walks up from `start` to the nearest directory holding `INDEX.md`.
/// Falls back to the enclosing git root, then to `start` itself, for init.
pub fn discover(start: &Path) -> Result<Found> {
    let start = dunce(start.canonicalize()?);
    for dir in start.ancestors() {
        if has_index(dir) {
            return Ok(Found::Repo(dir.to_path_buf()));
        }
    }
    let target = git::toplevel(&start).unwrap_or(start);
    Ok(Found::NeedsInit(target))
}

/// Strips the `\\?\` prefix Windows adds on canonicalize, so paths print nicely.
pub fn dunce(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC") => PathBuf::from(rest),
        _ => p,
    }
}

/// Creates INDEX.md, the assets dir, .gitattributes and .gitignore, runs
/// `git init` if needed and commits the new files.
pub fn init(root: &Path, assets_dir: &str) -> Result<()> {
    if let Some(p) = miscased_index(root) {
        bail!("found {}; rename it to {INDEX} and run gition again", p.display());
    }
    if !git::is_repo(root) {
        git::run(root, &["init", "-q"])?;
    }
    let title = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "Notes".into());
    let mut created: Vec<String> = Vec::new();
    let mut create = |rel: &str, content: &str| -> Result<()> {
        let p = root.join(rel);
        if !p.exists() {
            if let Some(dir) = p.parent() {
                fs::create_dir_all(dir)?;
            }
            fs::write(&p, content)?;
            created.push(rel.to_string());
        }
        Ok(())
    };
    create(INDEX, &format!("# {title}\n\nWelcome to your gition workspace.\n"))?;
    create(&format!("{assets_dir}/.gitkeep"), "")?;
    create(
        ".gitattributes",
        "*.md text eol=lf\n*.png binary\n*.jpg binary\n*.jpeg binary\n*.gif binary\n*.webp binary\n*.bmp binary\n",
    )?;
    create(".gitignore", ".DS_Store\nThumbs.db\n")?;
    if !created.is_empty() {
        let mut args = vec!["add", "--"];
        args.extend(created.iter().map(String::as_str));
        git::run(root, &args)?;
        if let Err(e) = git::run(root, &["commit", "-q", "-m", "gition: init"]) {
            eprintln!("warning: could not create the initial commit: {e}");
        }
    }
    Ok(())
}
