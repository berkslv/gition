//! Helpers for repo-relative paths and markdown link destinations.

use std::path::{Component, Path, PathBuf};

/// Lexically resolves `.` and `..`. Returns `None` for absolute paths or
/// paths that climb above their starting point.
pub fn normalize(p: &Path) -> Option<PathBuf> {
    let mut out: Vec<String> = Vec::new();
    for c in p.components() {
        match c {
            Component::Normal(s) => out.push(s.to_string_lossy().into_owned()),
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop()?;
            }
            _ => return None,
        }
    }
    Some(out.iter().collect())
}

/// Renders a relative path with forward slashes, as used in markdown links.
pub fn to_slash(p: &Path) -> String {
    p.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect::<Vec<_>>().join("/")
}

/// Relative path (forward slashes) from directory `from_dir` to `to`.
/// Both must be relative to the same root.
pub fn relative(from_dir: &Path, to: &Path) -> String {
    let a: Vec<_> = from_dir.components().collect();
    let b: Vec<_> = to.components().collect();
    let mut i = 0;
    while i < a.len() && i < b.len() && a[i] == b[i] {
        i += 1;
    }
    let mut parts: Vec<String> = vec!["..".to_string(); a.len() - i];
    parts.extend(b[i..].iter().map(|c| c.as_os_str().to_string_lossy().into_owned()));
    parts.join("/")
}

pub fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = |b: u8| (b as char).to_digit(16);
            if let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Escapes the characters that would break an inline markdown link destination.
pub fn encode_link(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            ' ' => out.push_str("%20"),
            '(' => out.push_str("%28"),
            ')' => out.push_str("%29"),
            '<' => out.push_str("%3C"),
            '>' => out.push_str("%3E"),
            _ => out.push(ch),
        }
    }
    out
}

/// Parent directory of a repo-relative path (`""` for top-level entries).
pub fn parent(p: &Path) -> PathBuf {
    p.parent().map(Path::to_path_buf).unwrap_or_default()
}

pub fn is_markdown(p: &Path) -> bool {
    p.extension().is_some_and(|e| e.eq_ignore_ascii_case("md"))
}

pub fn is_index(p: &Path) -> bool {
    p.file_name().is_some_and(|n| n == "INDEX.md")
}

pub fn is_image(p: &Path) -> bool {
    const EXTS: [&str; 8] = ["png", "jpg", "jpeg", "gif", "bmp", "webp", "tif", "tiff"];
    p.extension().and_then(|e| e.to_str()).is_some_and(|e| EXTS.iter().any(|x| x.eq_ignore_ascii_case(e)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths() {
        assert_eq!(relative(Path::new(""), Path::new("assets/a.png")), "assets/a.png");
        assert_eq!(relative(Path::new("a/b"), Path::new("assets/a.png")), "../../assets/a.png");
        assert_eq!(relative(Path::new("a"), Path::new("a/x.md")), "x.md");
    }

    #[test]
    fn normalize_rejects_escape() {
        assert_eq!(normalize(Path::new("a/../b.md")), Some(PathBuf::from("b.md")));
        assert_eq!(normalize(Path::new("../b.md")), None);
    }

    #[test]
    fn percent_roundtrip() {
        assert_eq!(percent_decode(&encode_link("my page (1).md")), "my page (1).md");
        assert_eq!(percent_decode("100%"), "100%");
    }
}
