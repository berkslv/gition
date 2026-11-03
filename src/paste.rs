//! What a paste turns into: a bare URL becomes a link titled after its page, and
//! an image path or clipboard image becomes a file under the assets dir.
//! Pure functions, shared by the terminal app, the web app and the worker.

use std::path::PathBuf;

/// Extensions a pasted path must have to be imported as an image.
pub const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "svg"];

/// The paste, trimmed, when it is exactly one http(s) URL.
pub fn bare_url(s: &str) -> Option<&str> {
    let s = s.trim();
    let rest = s.strip_prefix("https://").or_else(|| s.strip_prefix("http://"))?;
    let ok = !rest.is_empty() && !rest.starts_with('/') && !s.chars().any(|c| c.is_whitespace() || c.is_control());
    ok.then_some(s)
}

/// Whether text typed after `before` (the line up to the cursor) lands inside link
/// syntax, where a pasted URL must stay as it is.
pub fn in_link(before: &str) -> bool {
    before.ends_with("](") || before.rfind('<').is_some_and(|i| !before[i..].contains('>'))
}

/// `[title](url)`, with the brackets and backslashes in the title escaped.
pub fn link(title: &str, url: &str) -> String {
    let mut text = String::with_capacity(title.len());
    for c in title.chars() {
        if matches!(c, '[' | ']' | '\\') {
            text.push('\\');
        }
        text.push(c);
    }
    format!("[{text}]({url})")
}

/// A page's title: `og:title` if it has one, otherwise `<title>`. Only looks at
/// what comes before `<body`, entities decoded and whitespace collapsed.
pub fn page_title(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let head = lower.find("<body").unwrap_or(lower.len());
    let og = tags(&lower[..head], "<meta").find_map(|(start, end)| {
        let tag = &html[start..end];
        let prop = attr(tag, "property").or_else(|| attr(tag, "name"))?;
        prop.eq_ignore_ascii_case("og:title").then(|| attr(tag, "content")).flatten()
    });
    let title = || {
        let open = lower[..head].find("<title")?;
        let start = open + lower[open..].find('>')? + 1;
        let end = start + lower[start..].find("</title")?;
        Some(&html[start..end])
    };
    [og, title()].into_iter().flatten().map(clean).find(|t| !t.is_empty())
}

/// Byte ranges of each `<name …>` tag in `lower`.
fn tags<'a>(lower: &'a str, name: &'a str) -> impl Iterator<Item = (usize, usize)> + 'a {
    let mut at = 0;
    std::iter::from_fn(move || {
        let start = at + lower[at..].find(name)?;
        let end = start + lower[start..].find('>')? + 1;
        at = end;
        Some((start, end))
    })
}

/// The value of attribute `name` in a tag, quoted or not.
fn attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let lower = tag.to_ascii_lowercase();
    let mut from = 0;
    while let Some(i) = lower[from..].find(name).map(|i| i + from) {
        from = i + name.len();
        let boundary = lower[..i].ends_with(|c: char| c.is_whitespace());
        let rest = lower[from..].trim_start();
        if !boundary || !rest.starts_with('=') {
            continue;
        }
        let v = tag[tag.len() - rest.len() + 1..].trim_start();
        return Some(match v.chars().next()? {
            q @ ('"' | '\'') => &v[1..1 + v[1..].find(q)?],
            _ => &v[..v.find(|c: char| c.is_whitespace() || c == '>' || c == '/').unwrap_or(v.len())],
        });
    }
    None
}

fn clean(s: &str) -> String {
    decode(s).split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Decodes the HTML entities titles use: numeric ones and a few named ones.
fn decode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let entity = rest[1..].find(';').filter(|&n| n <= 10).map(|n| &rest[1..1 + n]);
        let c = entity.and_then(|e| match e {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some(' '),
            "ndash" => Some('–'),
            "mdash" => Some('—'),
            "hellip" => Some('…'),
            _ => {
                let n = e.strip_prefix('#')?;
                let code = match n.strip_prefix(['x', 'X']) {
                    Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                    None => n.parse().ok()?,
                };
                char::from_u32(code)
            }
        });
        match (c, entity) {
            (Some(c), Some(e)) => {
                out.push(c);
                rest = &rest[e.len() + 2..];
            }
            _ => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// The image file a one-line paste names, as typed or dropped into a terminal:
/// quoted, with `\ ` escapes, or as a `file://` URL. Doesn't check that it exists.
pub fn image_path(s: &str) -> Option<PathBuf> {
    let s = s.trim();
    if s.is_empty() || s.contains('\n') {
        return None;
    }
    let unquoted = s
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| s.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')));
    let path = match unquoted {
        Some(p) => p.to_string(),
        None => unescape(s),
    };
    let path = match path.strip_prefix("file://") {
        Some(p) => percent_decode(p.strip_prefix("localhost").unwrap_or(p)),
        None => path,
    };
    let path = match path.strip_prefix('~') {
        Some(rest) => {
            let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
            PathBuf::from(home).join(rest.trim_start_matches(['/', '\\']))
        }
        None => PathBuf::from(path),
    };
    let ext = path.extension()?.to_string_lossy().to_ascii_lowercase();
    IMAGE_EXTS.contains(&ext.as_str()).then_some(path)
}

/// Undoes shell-style backslash escapes (`\ `, `\(`). Windows paths keep theirs.
fn unescape(s: &str) -> String {
    if cfg!(windows) {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.extend(chars.next()),
            c => out.push(c),
        }
    }
    out
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = (b[i] == b'%').then(|| s.get(i + 1..i + 3)).flatten();
        match hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
            Some(v) => {
                out.push(v);
                i += 3;
            }
            None => {
                out.push(b[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The file name for a pasted image: `pasted-YYYYMMDD-HHMMSS.<ext>`, in UTC.
pub fn image_name(ext: &str, unix_secs: u64) -> String {
    let (days, secs) = ((unix_secs / 86400) as i64, unix_secs % 86400);
    // Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("pasted-{y:04}{m:02}{d:02}-{:02}{:02}{:02}.{ext}", secs / 3600, secs / 60 % 60, secs % 60)
}

/// The extension for an image MIME type.
pub fn image_ext(mime: &str) -> Option<&'static str> {
    Some(match mime.split(';').next()?.trim() {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/svg+xml" => "svg",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_urls() {
        assert_eq!(bare_url(" https://medium.com\n"), Some("https://medium.com"));
        assert_eq!(bare_url("http://a.b/c?d=e#f"), Some("http://a.b/c?d=e#f"));
        assert_eq!(bare_url("see https://medium.com"), None);
        assert_eq!(bare_url("https://a.com https://b.com"), None);
        assert_eq!(bare_url("https://"), None);
        assert_eq!(bare_url("ftp://a.com"), None);
        assert_eq!(bare_url("[x](https://a.com)"), None);
    }

    #[test]
    fn link_context() {
        assert!(in_link("see [here]("));
        assert!(in_link("an <"));
        assert!(!in_link("a <b> and "));
        assert!(!in_link("plain "));
        assert!(!in_link(""));
    }

    #[test]
    fn links_escape_brackets() {
        assert_eq!(link("Medium", "https://medium.com"), "[Medium](https://medium.com)");
        assert_eq!(link(r"a [b] \c", "u"), r"[a \[b\] \\c](u)");
    }

    #[test]
    fn titles() {
        let html = "<html><head>\n<title>\n  Medium &amp;\n Co &#8211; x&#x21;\n</title></head><body><title>no</title>";
        assert_eq!(page_title(html).as_deref(), Some("Medium & Co – x!"));
        let og = r#"<head><title>Long tagline</title><meta property="og:title" content="Medium &quot;A&quot;"></head>"#;
        assert_eq!(page_title(og).as_deref(), Some(r#"Medium "A""#));
        let attrs = "<META content='Hi there' Property=og:title /><TITLE>t</TITLE>";
        assert_eq!(page_title(attrs).as_deref(), Some("Hi there"));
        assert_eq!(
            page_title("<meta property=\"og:title\" content=\"  \"><title>Fallback</title>").as_deref(),
            Some("Fallback")
        );
        assert_eq!(page_title("<meta name=\"og:title\" content=\"N\">").as_deref(), Some("N"));
        assert_eq!(page_title("<meta data-property=\"og:title\" content=\"X\">"), None);
        assert_eq!(page_title("<head></head><body><title>x</title>"), None);
        assert_eq!(page_title("<title>a &bogus; &</title>").as_deref(), Some("a &bogus; &"));
    }

    #[test]
    fn image_paths() {
        assert_eq!(image_path("/tmp/a.png"), Some(PathBuf::from("/tmp/a.png")));
        assert_eq!(image_path("'/tmp/a b.JPG' "), Some(PathBuf::from("/tmp/a b.JPG")));
        assert_eq!(image_path("\"/tmp/a b.gif\""), Some(PathBuf::from("/tmp/a b.gif")));
        assert_eq!(image_path("file:///tmp/a%20b.webp"), Some(PathBuf::from("/tmp/a b.webp")));
        assert_eq!(image_path("/tmp/a.txt"), None);
        assert_eq!(image_path("/tmp/a.png\n/tmp/b.png"), None);
        assert_eq!(image_path(""), None);
        #[cfg(not(windows))]
        assert_eq!(image_path(r"/tmp/Screen\ Shot\ (1).png"), Some(PathBuf::from("/tmp/Screen Shot (1).png")));
    }

    #[test]
    fn image_names() {
        assert_eq!(image_name("png", 0), "pasted-19700101-000000.png");
        assert_eq!(image_name("jpg", 1_793_708_465), "pasted-20261103-122105.jpg");
        assert_eq!(image_name("png", 951_782_400), "pasted-20000229-000000.png");
    }

    #[test]
    fn mime_exts() {
        assert_eq!(image_ext("image/png"), Some("png"));
        assert_eq!(image_ext("image/svg+xml"), Some("svg"));
        assert_eq!(image_ext("text/html"), None);
    }
}
