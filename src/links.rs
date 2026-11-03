//! Finding and rewriting relative markdown links.

use std::ops::Range;
use std::path::{Path, PathBuf};

use pulldown_cmark::{Event, LinkType, Options, Parser, Tag};

use crate::paths;
use crate::storage::Storage;

pub fn md_options() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS
}

/// An inline link or image destination and its byte range in the source.
#[derive(Debug, Clone, PartialEq)]
pub struct LinkSpan {
    pub range: Range<usize>,
    pub dest: String,
}

/// All inline `[text](dest)` and `![alt](dest)` destinations in `src`.
pub fn find_links(src: &str) -> Vec<LinkSpan> {
    let mut out = Vec::new();
    for (ev, r) in Parser::new_ext(src, md_options()).into_offset_iter() {
        let link_type = match &ev {
            Event::Start(Tag::Link { link_type, .. }) | Event::Start(Tag::Image { link_type, .. }) => *link_type,
            _ => continue,
        };
        if link_type != LinkType::Inline {
            continue;
        }
        let s = &src[r.clone()];
        if let Some(d) = dest_range(s) {
            out.push(LinkSpan { range: r.start + d.start..r.start + d.end, dest: s[d].to_string() });
        }
    }
    out
}

/// Locates the destination inside the source text of one inline link.
fn dest_range(s: &str) -> Option<Range<usize>> {
    if !s.ends_with(')') {
        return None;
    }
    let mut i = s.rfind("](")? + 2;
    while s[i..].starts_with(char::is_whitespace) {
        i += s[i..].chars().next()?.len_utf8();
    }
    if s[i..].starts_with('<') {
        let end = s[i + 1..].find('>')?;
        return Some(i + 1..i + 1 + end);
    }
    let mut depth = 0usize;
    for (j, ch) in s[i..].char_indices() {
        match ch {
            '(' => depth += 1,
            ')' if depth == 0 => return Some(i..i + j),
            ')' => depth -= 1,
            c if c.is_whitespace() => return Some(i..i + j),
            _ => {}
        }
    }
    None
}

/// Splits `path#frag` / `path?q` into the path and the suffix.
fn split_suffix(dest: &str) -> (&str, &str) {
    match dest.find(['#', '?']) {
        Some(i) => (&dest[..i], &dest[i..]),
        None => (dest, ""),
    }
}

pub fn is_local(dest: &str) -> bool {
    !(dest.is_empty()
        || dest.starts_with('#')
        || dest.starts_with('/')
        || dest.contains("://")
        || dest.starts_with("mailto:"))
}

/// Resolves a link found in repo-relative `file` to a repo-relative target path
/// and the `#fragment`/`?query` suffix. `None` for external or escaping links.
pub fn resolve(file: &Path, dest: &str) -> Option<(PathBuf, String)> {
    if !is_local(dest) {
        return None;
    }
    let (path, suffix) = split_suffix(dest);
    if path.is_empty() {
        return None;
    }
    let target = paths::normalize(&paths::parent(file).join(paths::percent_decode(path)))?;
    Some((target, suffix.to_string()))
}

/// Maps `p` through a move of `old` to `new` (files and whole subtrees).
pub fn map_moved(p: &Path, old: &Path, new: &Path) -> PathBuf {
    match p.strip_prefix(old) {
        Ok(rest) if rest.as_os_str().is_empty() => new.to_path_buf(),
        Ok(rest) => new.join(rest),
        Err(_) => p.to_path_buf(),
    }
}

/// Rewrites links in the content of `file_old` (which will live at `file_new`)
/// so they keep pointing at the same targets after `old` moves to `new`.
/// Returns `None` when nothing changes.
pub fn rewrite(src: &str, file_old: &Path, file_new: &Path, old: &Path, new: &Path) -> Option<String> {
    let new_dir = paths::parent(file_new);
    let moved_file = paths::parent(file_old) != new_dir;
    let mut out = src.to_string();
    let mut changed = false;
    let mut links = find_links(src);
    // Apply back to front so earlier byte ranges stay valid.
    links.sort_by_key(|l| std::cmp::Reverse(l.range.start));
    for link in links {
        let Some((target, suffix)) = resolve(file_old, &link.dest) else { continue };
        let target_new = map_moved(&target, old, new);
        if !moved_file && target_new == target {
            continue;
        }
        let mut rel = paths::relative(&new_dir, &target_new);
        if link.dest.split(['#', '?']).next().is_some_and(|p| p.ends_with('/')) && !rel.ends_with('/') {
            rel.push('/');
        }
        let dest = format!("{}{}", paths::encode_link(&rel), suffix);
        if dest != link.dest {
            out.replace_range(link.range, &dest);
            changed = true;
        }
    }
    changed.then_some(out)
}

/// Number of markdown files (outside `target`) that link to `target` or into it.
pub fn count_incoming(fs: &dyn Storage, target: &Path, md_files: &[PathBuf]) -> usize {
    md_files
        .iter()
        .filter(|f| !f.starts_with(target))
        .filter(|f| {
            let Ok(src) = fs.read_to_string(f) else { return false };
            find_links(&src).iter().filter_map(|l| resolve(f, &l.dest)).any(|(t, _)| t.starts_with(target))
        })
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_inline_links_and_images() {
        let src = "See [plan](Projects/plan.md#top) and ![x](assets/a%20b.png \"t\").\n[ext](https://x.y)";
        let links = find_links(src);
        let dests: Vec<_> = links.iter().map(|l| l.dest.as_str()).collect();
        assert_eq!(dests, ["Projects/plan.md#top", "assets/a%20b.png", "https://x.y"]);
        for l in &links {
            assert_eq!(&src[l.range.clone()], l.dest);
        }
    }

    #[test]
    fn nested_image_in_link() {
        let src = "[![a](img.png)](page.md)";
        let dests: Vec<_> = find_links(src).into_iter().map(|l| l.dest).collect();
        assert_eq!(dests, ["page.md", "img.png"]);
    }

    #[test]
    fn rewrite_incoming_link() {
        let src = "[p](Projects/plan.md)";
        let out = rewrite(
            src,
            Path::new("INDEX.md"),
            Path::new("INDEX.md"),
            Path::new("Projects/plan.md"),
            Path::new("Archive/old plan.md"),
        );
        assert_eq!(out.as_deref(), Some("[p](Archive/old%20plan.md)"));
    }

    #[test]
    fn rewrite_outgoing_links_of_moved_file() {
        let src = "![i](../assets/x.png) [s](sibling.md) [w](https://e.com)";
        let out =
            rewrite(src, Path::new("a/p.md"), Path::new("a/b/c/p.md"), Path::new("a/p.md"), Path::new("a/b/c/p.md"));
        assert_eq!(out.as_deref(), Some("![i](../../../assets/x.png) [s](../../sibling.md) [w](https://e.com)"));
    }

    #[test]
    fn rewrite_folder_move() {
        let src = "[a](Projects/sub/x.md) [b](Projects/)";
        let out = rewrite(src, Path::new("INDEX.md"), Path::new("INDEX.md"), Path::new("Projects"), Path::new("Work"));
        assert_eq!(out.as_deref(), Some("[a](Work/sub/x.md) [b](Work/)"));
    }

    #[test]
    fn unrelated_link_untouched() {
        let src = "[a](x.md)";
        assert_eq!(
            rewrite(src, Path::new("INDEX.md"), Path::new("INDEX.md"), Path::new("y.md"), Path::new("z.md")),
            None
        );
    }
}
