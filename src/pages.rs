//! Creating, moving and filling pages through [`Storage`], shared by the terminal app and the web.

use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow, bail};

use crate::repo::INDEX;
use crate::storage::Storage;
use crate::{links, paths, tree};

/// Validates user input as a path relative to some dir in the repo.
pub fn clean_rel(input: &str) -> Result<PathBuf> {
    let trimmed = input.trim().trim_matches('/').trim_matches('\\');
    paths::normalize(Path::new(trimmed))
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| anyhow!("Invalid path: {input}"))
}

/// Where a new page named `input` goes in `dir`, with `.md` added.
pub fn new_page_path(dir: &Path, input: &str) -> Result<PathBuf> {
    let rel = dir.join(clean_rel(input)?);
    Ok(if paths::is_markdown(&rel) { rel } else { PathBuf::from(format!("{}.md", rel.to_string_lossy())) })
}

/// The folder a new folder page named `input` goes in `dir`; fails if it exists.
pub fn new_folder_path(fs: &dyn Storage, dir: &Path, input: &str) -> Result<PathBuf> {
    let folder = dir.join(clean_rel(input)?);
    if fs.exists(&folder) {
        bail!("'{}' already exists", paths::to_slash(&folder));
    }
    Ok(folder)
}

/// Writes a new page at `rel` that starts with its title as a heading.
pub fn create(fs: &dyn Storage, rel: &Path, title: &str) -> Result<()> {
    if fs.exists(rel) {
        bail!("'{}' already exists", paths::to_slash(rel));
    }
    fs.write(rel, format!("# {title}\n").as_bytes())?;
    Ok(())
}

/// The title a new page or folder at `rel` starts with: its name without `.md`.
pub fn title(rel: &Path) -> String {
    let rel = if paths::is_index(rel) { paths::parent(rel) } else { rel.to_path_buf() };
    let name = rel.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    if paths::is_markdown(&rel) { name[..name.len() - 3].to_string() } else { name }
}

/// Where renaming `old` to the typed `input` moves it. Like `mv`, naming an existing
/// folder moves into it, and a page keeps its `.md`.
pub fn rename_target(fs: &dyn Storage, old: &Path, input: &str) -> Result<PathBuf> {
    let mut new = clean_rel(input)?;
    let same = paths::to_slash(&new).to_lowercase() == paths::to_slash(old).to_lowercase();
    if fs.is_dir(&new) && !same {
        new = new.join(old.file_name().unwrap_or_default());
    } else if !fs.is_dir(old) && paths::is_markdown(old) && !paths::is_markdown(&new) {
        new = PathBuf::from(format!("{}.md", new.to_string_lossy()));
    }
    Ok(new)
}

/// Renames/moves a page or folder and rewrites every affected relative link. Reads every page,
/// so callers check that none are still downloading. Returns how many pages had links updated.
pub fn move_path(fs: &dyn Storage, old: &Path, new: &Path) -> Result<usize> {
    if new == old {
        return Ok(0);
    }
    if new.starts_with(old) {
        bail!("Can't move a folder into itself");
    }
    let case_only = paths::to_slash(new).to_lowercase() == paths::to_slash(old).to_lowercase();
    if fs.exists(new) && !case_only {
        bail!("'{}' already exists", paths::to_slash(new));
    }
    let mut edits = Vec::new();
    for f in tree::markdown_files(fs) {
        let Ok(src) = fs.read_to_string(&f) else { continue };
        let f_new = links::map_moved(&f, old, new);
        if let Some(out) = links::rewrite(&src, &f, &f_new, old, new) {
            edits.push((f_new, out));
        }
    }
    fs.rename(old, new)?;
    for (f, content) in &edits {
        fs.write(f, content.as_bytes())?;
    }
    Ok(edits.len())
}

/// Turns the page `name.md` into the folder page `name/INDEX.md`, so it can hold subpages.
/// Returns the folder. Reads every page, like [`move_path`].
pub fn make_folder(fs: &dyn Storage, page: &Path) -> Result<PathBuf> {
    let folder = page.with_extension("");
    move_path(fs, page, &folder.join(INDEX))?;
    Ok(folder)
}

/// Copies an image into the assets dir and returns the markdown that shows it on `page`.
pub fn save_image(fs: &dyn Storage, assets_dir: &str, page: &Path, name: &str, bytes: &[u8]) -> Result<String> {
    let name = Path::new(name).file_name().ok_or_else(|| anyhow!("Invalid file name: {name}"))?;
    let assets = PathBuf::from(assets_dir);
    fs.create_dir_all(&assets)?;
    let rel = unique_asset(fs, &assets, Path::new(name), bytes);
    if !fs.exists(&rel) {
        fs.write(&rel, bytes)?;
    }
    let alt = rel.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    Ok(format!("![{alt}]({})", paths::encode_link(&paths::relative(&paths::parent(page), &rel))))
}

/// Picks `assets/<name>`, reusing an identical existing file or adding `-1`, `-2`, … .
fn unique_asset(fs: &dyn Storage, assets: &Path, name: &Path, bytes: &[u8]) -> PathBuf {
    let stem = name.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = name.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    for n in 0.. {
        let candidate = if n == 0 { assets.join(name) } else { assets.join(format!("{stem}-{n}{ext}")) };
        if !fs.exists(&candidate) {
            return candidate;
        }
        match fs.read(&candidate) {
            Err(_) => continue,
            Ok(existing) if existing == bytes => return candidate,
            Ok(_) => continue,
        }
    }
    unreachable!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{Files, MemStorage};

    fn store(files: &[(&str, &str)]) -> MemStorage {
        let files: Files =
            files.iter().map(|(p, c)| (PathBuf::from(p), crate::storage::Blob::Data(c.as_bytes().to_vec()))).collect();
        MemStorage::new("t", files)
    }

    #[test]
    fn page_becomes_folder_and_keeps_links() {
        let fs = store(&[("a.md", "# A\n[b](b.md)\n"), ("b.md", "[a](a.md)\n")]);
        let folder = make_folder(&fs, Path::new("a.md")).unwrap();
        assert_eq!(folder, PathBuf::from("a"));
        assert_eq!(fs.read_to_string(Path::new("a/INDEX.md")).unwrap(), "# A\n[b](../b.md)\n");
        assert_eq!(fs.read_to_string(Path::new("b.md")).unwrap(), "[a](a/INDEX.md)\n");
    }

    #[test]
    fn titles_and_new_paths() {
        assert_eq!(title(Path::new("x/My page.md")), "My page");
        assert_eq!(title(Path::new("x/Folder/INDEX.md")), "Folder");
        assert_eq!(new_page_path(Path::new("x"), "Plan").unwrap(), PathBuf::from("x/Plan.md"));
        assert!(new_page_path(Path::new(""), "../up").is_err());
    }
}
