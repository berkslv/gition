//! Locating a gition repo and initializing a new one.

#[cfg(feature = "native")]
use std::fs;
use std::path::Path;
#[cfg(feature = "native")]
use std::path::PathBuf;

#[cfg(feature = "native")]
use anyhow::Result;

#[cfg(feature = "native")]
use crate::git;
use crate::storage::Storage;

pub const INDEX: &str = "INDEX.md";
/// Notes for coding agents, at the repo root. Not a page, so the tree hides it.
pub const AGENTS: &str = "AGENTS.md";

/// Files a new gition repo starts with, as (path, content).
pub fn starter_files(title: &str, assets_dir: &str) -> Vec<(String, String)> {
    vec![
        (INDEX.into(), format!("# {title}\n\nWelcome to your gition workspace.\n")),
        (format!("{assets_dir}/.gitkeep"), String::new()),
        (
            ".gitattributes".into(),
            "*.md text eol=lf\n*.png binary\n*.jpg binary\n*.jpeg binary\n*.gif binary\n*.webp binary\n*.bmp binary\n"
                .into(),
        ),
        (".gitignore".into(), ".DS_Store\nThumbs.db\n".into()),
        (crate::config::FILE_NAME.into(), starter_config(assets_dir)),
        (AGENTS.into(), starter_agents(assets_dir)),
    ]
}

fn starter_config(assets_dir: &str) -> String {
    format!(
        "# gition settings: https://github.com/berkslv/gition#config
tree_width = 30
assets_dir = \"{assets_dir}\"
show_all_files = false
commit_prefix = \"gition:\"
link_titles = true   # a pasted URL becomes [Page title](url)
compress_images = true   # pasted PNG/JPEG images are saved as WebP

[keys]
# Action name = one key or a list of keys. Names are in src/keymap.rs.
# quit = [\"q\", \"ctrl+q\"]
# sync = \"ctrl+g\"

[theme]
# A ready-made theme: default, light, nord, gruvbox, dracula or catppuccin.
# default and light use your terminal's colors; the others need true color.
preset = \"default\"
# Any color below overrides the preset's. A color is a name (cyan, dark_gray,
# light_blue...), \"#rrggbb\", or a 256-color index like \"236\".
# accent = \"cyan\"        # focused borders, prompts, help headings
# dim = \"dark_gray\"      # hints, unfocused borders, quotes, front matter
# cursor = \"236\"         # background of the cursor row
# bar = \"235\"            # background of the status bar
# selection = \"24\"       # background of selected text
# folder = \"blue\"
# current = \"green\"      # the open page in the tree
# ok = \"green\"
# warning = \"yellow\"
# error = \"red\"
# heading1 = \"magenta\"
# heading2 = \"cyan\"
# heading3 = \"blue\"
# link = \"blue\"
# code = \"yellow\"
# bullet = \"cyan\"        # list bullets and task boxes
# image = \"magenta\"      # image placeholders
"
    )
}

fn starter_agents(assets_dir: &str) -> String {
    format!(
        "# AGENTS.md

This folder is a gition notebook: markdown pages in a git repo, read and edited with
gition (https://github.com/berkslv/gition) in the terminal or the browser.

## Layout

- `INDEX.md` (exactly this case) at the root, if present, is the home page.
- `name.md` is a page. A folder is a page when it holds `folder/INDEX.md`; the other `.md` files in it are its subpages.
- Images and attachments live in `{assets_dir}/` at the root.
- `.gition.toml` holds the settings. This file is hidden from gition's page tree.

## Editing

- Pages are CommonMark with GFM tables, task lists and strikethrough. A `---` front matter block at the top shows as the page's properties.
- Link pages and images with relative paths to the file, like `[Plan](Projects/plan.md)` or `![diagram](../{assets_dir}/diagram.png)`, so they also work on GitHub.
- To turn a page into a folder, move `name.md` to `name/INDEX.md`. When you rename or move a page, update the links to and from it.
- Use LF line endings.
- Don't commit unless asked. gition commits on sync with a message starting `gition:` that lists the changed files.
"
    )
}

/// True if `dir` holds a file named exactly `INDEX.md`.
pub fn dir_has_index(fs: &dyn Storage, dir: &Path) -> bool {
    fs.read_dir(dir).is_ok_and(|entries| entries.iter().any(|e| e.name == INDEX && !e.is_dir))
}

#[cfg(feature = "native")]
/// True if `dir` contains a file named exactly `INDEX.md` (case-sensitive,
/// even on case-insensitive filesystems).
pub fn has_index(dir: &Path) -> bool {
    fs::read_dir(dir).map(|rd| rd.flatten().any(|e| e.file_name() == INDEX && e.path().is_file())).unwrap_or(false)
}

#[cfg(feature = "native")]
pub enum Found {
    Repo(PathBuf),
    /// Not inside a git repo; this is where init should happen.
    NeedsInit(PathBuf),
}

#[cfg(feature = "native")]
/// Walks up from `start` to the nearest directory holding `INDEX.md`, so a notebook can live
/// in a folder of a bigger repo. Without one, the enclosing git root is the notebook.
pub fn discover(start: &Path) -> Result<Found> {
    let start = dunce(start.canonicalize()?);
    if let Some(dir) = start.ancestors().find(|d| has_index(d)) {
        return Ok(Found::Repo(dir.to_path_buf()));
    }
    Ok(match git::toplevel(&start) {
        Some(root) => Found::Repo(root),
        None => Found::NeedsInit(start),
    })
}

#[cfg(feature = "native")]
/// Strips the `\\?\` prefix Windows adds on canonicalize, so paths print nicely.
pub fn dunce(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC") => PathBuf::from(rest),
        _ => p,
    }
}

#[cfg(feature = "native")]
/// Creates INDEX.md, the assets dir, .gitattributes, .gitignore, .gition.toml
/// and AGENTS.md, runs
/// `git init` if needed and commits the new files.
pub fn init(root: &Path, assets_dir: &str) -> Result<()> {
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
    for (rel, content) in starter_files(&title, assets_dir) {
        create(&rel, &content)?;
    }
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
