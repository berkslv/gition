//! A markdown page as blocks, for the web editor. Each top-level element is a block, and so is
//! each item of a top-level list. [`Doc`] keeps their exact source and the text between them,
//! so editing one block leaves the rest of the file as it was. [`render`] turns a block into
//! HTML that is safe to put in the page: raw HTML shows as text, and only web links get an `href`.

use std::ops::Range;
use std::path::Path;

use pulldown_cmark::{Event, HeadingLevel, Parser, Tag, TagEnd, html};

use crate::links::{self, md_options};
use crate::paths;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Paragraph,
    Heading(u8),
    /// An item of a list.
    Item,
    Code,
    Quote,
    Table,
    Rule,
    /// Front matter.
    Meta,
    Html,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Piece {
    pub src: String,
    /// What follows the block up to the next one: line breaks, mostly.
    pub gap: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Doc {
    /// What comes before the first block.
    pub lead: String,
    pub pieces: Vec<Piece>,
}

/// The byte ranges of the blocks in `src`, without their trailing line breaks.
fn ranges(src: &str) -> Vec<Range<usize>> {
    let mut out: Vec<Range<usize>> = Vec::new();
    let mut push = |r: Range<usize>| {
        let end = r.start + src[r.clone()].trim_end_matches(['\n', '\r']).len();
        if end > r.start && out.last().is_none_or(|l| l.end <= r.start) {
            out.push(r.start..end);
        }
    };
    let (mut depth, mut list, mut start) = (0usize, false, 0);
    for (ev, r) in Parser::new_ext(src, md_options()).into_offset_iter() {
        match ev {
            Event::Start(tag) => {
                if depth == 0 {
                    list = matches!(tag, Tag::List(_));
                }
                if depth == 0 || depth == 1 && list {
                    start = r.start;
                }
                depth += 1;
            }
            Event::End(_) => {
                depth -= 1;
                if depth == 0 && !list || depth == 1 && list {
                    push(start..r.end);
                }
            }
            _ if depth == 0 => push(r),
            _ => {}
        }
    }
    out
}

impl Doc {
    pub fn parse(src: &str) -> Doc {
        let rs = ranges(src);
        let Some(first) = rs.first() else { return Doc { lead: src.into(), pieces: Vec::new() } };
        let pieces = rs
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let next = rs.get(i + 1).map_or(src.len(), |n| n.start);
                Piece { src: src[r.clone()].into(), gap: src[r.end..next].into() }
            })
            .collect();
        Doc { lead: src[..first.start].into(), pieces }
    }

    /// The page's text. Empty blocks have no markdown, so they're left out with the gap after them.
    pub fn text(&self) -> String {
        let mut out = self.lead.clone();
        let mut gap: Option<&str> = None;
        let last = self.pieces.len().saturating_sub(1);
        for (i, p) in self.pieces.iter().enumerate() {
            if p.src.trim().is_empty() {
                // The file still ends the way it did.
                if i == last && gap.is_some() {
                    gap = Some(&p.gap);
                }
                continue;
            }
            out.push_str(gap.unwrap_or(""));
            out.push_str(&p.src);
            gap = Some(&p.gap);
        }
        out.push_str(gap.unwrap_or(""));
        out
    }

    /// Re-reads piece `i` after an edit: it becomes the blocks its text now holds, or goes when
    /// empty. Returns how many pieces took its place.
    pub fn settle(&mut self, i: usize) -> usize {
        let p = self.pieces.remove(i);
        if p.src.trim().is_empty() {
            if i == self.pieces.len() && i > 0 {
                self.pieces[i - 1].gap = p.gap;
            }
            return 0;
        }
        let mut sub = Doc::parse(&p.src);
        let Some(last) = sub.pieces.last_mut() else {
            self.pieces.insert(i, p);
            return 1;
        };
        if last.gap.trim().is_empty() {
            last.gap = p.gap;
        } else {
            last.gap.push_str(&p.gap);
        }
        if !sub.lead.trim().is_empty() {
            match i.checked_sub(1) {
                Some(j) => self.pieces[j].gap.push_str(&sub.lead),
                None => self.lead.push_str(&sub.lead),
            }
        }
        let n = sub.pieces.len();
        self.pieces.splice(i..i, sub.pieces);
        n
    }

    /// Enter at byte `at` of piece `i`: a paragraph or heading splits in two, a list item starts
    /// the next item (or ends the list when empty), and a quote goes on to its next line.
    /// Returns the piece to edit next and the caret in it, or `None` for a plain line break.
    pub fn enter(&mut self, i: usize, at: usize) -> Option<(usize, usize)> {
        let src = &self.pieces[i].src;
        let line_start = src[..at].rfind('\n').map_or(0, |n| n + 1);
        let line_end = src[at..].find('\n').map_or(src.len(), |n| at + n);
        let line = &src[line_start..line_end];
        let last_line = line_end == src.len();
        match kind(src) {
            Some(Kind::Item) => {
                let (len, next) = marker(line)?;
                if at < line_start + len {
                    return None;
                }
                if line[len..].trim().is_empty() && last_line {
                    // An empty item ends the list.
                    if line_start > 0 {
                        let p = &mut self.pieces[i];
                        p.src.truncate(line_start - 1);
                        let gap = std::mem::replace(&mut p.gap, "\n\n".into());
                        self.pieces.insert(i + 1, Piece { src: String::new(), gap: blank_line(gap) });
                        return Some((i + 1, 0));
                    }
                    self.pieces[i].src.clear();
                    self.separate(i);
                    return Some((i, 0));
                }
                if line_start > 0 {
                    self.pieces[i].src.insert_str(at, &format!("\n{next}"));
                    return Some((i, at + 1 + next.len()));
                }
                let p = &mut self.pieces[i];
                let after = p.src.split_off(at);
                let gap = std::mem::replace(&mut p.gap, "\n".into());
                self.pieces.insert(i + 1, Piece { src: format!("{next}{after}"), gap });
                Some((i + 1, next.len()))
            }
            Some(Kind::Quote) => {
                let prefix: String = line.chars().take_while(|c| *c == '>' || *c == ' ').collect();
                if line.trim_end_matches(' ').trim_end_matches('>').trim().is_empty() && last_line && line_start > 0 {
                    let p = &mut self.pieces[i];
                    p.src.truncate(line_start - 1);
                    let gap = std::mem::replace(&mut p.gap, "\n\n".into());
                    self.pieces.insert(i + 1, Piece { src: String::new(), gap });
                    return Some((i + 1, 0));
                }
                let next = format!("\n{} ", prefix.trim_end());
                self.pieces[i].src.insert_str(at, &next);
                Some((i, at + next.len()))
            }
            Some(Kind::Paragraph | Kind::Heading(_)) | None => {
                let p = &mut self.pieces[i];
                let after = p.src.split_off(at).trim_start_matches('\n').to_string();
                p.src.truncate(p.src.trim_end_matches('\n').len());
                let gap = std::mem::replace(&mut p.gap, "\n\n".into());
                self.pieces.insert(i + 1, Piece { src: after, gap });
                Some((i + 1, 0))
            }
            _ => None,
        }
    }

    /// Makes sure piece `i` stands apart from its neighbours, as a paragraph between list items must.
    fn separate(&mut self, i: usize) {
        if i > 0 {
            let gap = std::mem::take(&mut self.pieces[i - 1].gap);
            self.pieces[i - 1].gap = blank_line(gap);
        }
        if i + 1 < self.pieces.len() {
            let gap = std::mem::take(&mut self.pieces[i].gap);
            self.pieces[i].gap = blank_line(gap);
        }
    }

    /// Backspace at the start of piece `i`: an empty block goes, a paragraph joins the paragraph
    /// or heading before it, anything else moves the caret to the end of the block before.
    /// Returns the piece to edit and the caret in it.
    pub fn backspace(&mut self, i: usize) -> Option<(usize, usize)> {
        let prev = i.checked_sub(1)?;
        let at = self.pieces[prev].src.len();
        let cur = &self.pieces[i];
        if cur.src.trim().is_empty() {
            let p = self.pieces.remove(i);
            if i == self.pieces.len() {
                self.pieces[prev].gap = p.gap;
            }
            return Some((prev, at));
        }
        let joins = matches!(kind(&self.pieces[prev].src), Some(Kind::Paragraph | Kind::Heading(_)))
            && kind(&cur.src) == Some(Kind::Paragraph);
        if joins {
            let p = self.pieces.remove(i);
            let before = &mut self.pieces[prev];
            before.src.push_str(&p.src);
            before.gap = p.gap;
        }
        Some((prev, at))
    }
}

/// A gap that holds a blank line, so the blocks on either side stay apart.
fn blank_line(gap: String) -> String {
    if gap.matches('\n').count() >= 2 || !gap.trim().is_empty() { gap } else { "\n\n".into() }
}

/// What kind of block `src` starts with; `None` when it's empty.
pub fn kind(src: &str) -> Option<Kind> {
    let ev = Parser::new_ext(src, md_options()).next()?;
    Some(match ev {
        Event::Start(Tag::Heading { level, .. }) => Kind::Heading(heading(level)),
        Event::Start(Tag::List(_) | Tag::Item) => Kind::Item,
        Event::Start(Tag::CodeBlock(_)) => Kind::Code,
        Event::Start(Tag::BlockQuote(_)) => Kind::Quote,
        Event::Start(Tag::Table(_)) => Kind::Table,
        Event::Start(Tag::MetadataBlock(_)) => Kind::Meta,
        Event::Start(Tag::HtmlBlock) => Kind::Html,
        Event::Rule => Kind::Rule,
        _ => Kind::Paragraph,
    })
}

fn heading(level: HeadingLevel) -> u8 {
    level as u8
}

/// A list item's marker at the start of `line`: its length, with a task box, and the marker the
/// next item starts with. `None` if the line isn't a list item.
pub fn marker(line: &str) -> Option<(usize, String)> {
    let indent = line.len() - line.trim_start_matches(' ').len();
    let rest = &line[indent..];
    let (len, next) = match rest.chars().next()? {
        c @ ('-' | '*' | '+') => (1, c.to_string()),
        _ => {
            let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
            let delim = *rest.as_bytes().get(digits)?;
            if digits == 0 || digits > 9 || !matches!(delim, b'.' | b')') {
                return None;
            }
            let n: u64 = rest[..digits].parse().ok()?;
            (digits + 1, format!("{}{}", n + 1, delim as char))
        }
    };
    let after = &rest[len..];
    let spaces = after.len() - after.trim_start_matches(' ').len();
    if spaces == 0 && !after.is_empty() {
        return None;
    }
    let mut len = indent + len + spaces;
    let mut next = format!("{}{next} ", " ".repeat(indent));
    let task = &line.as_bytes()[len..];
    if task.len() >= 3 && task[0] == b'[' && matches!(task[1], b' ' | b'x' | b'X') && task[2] == b']' {
        if task.len() == 3 {
            len += 3;
        } else if task[3] == b' ' {
            len += 4;
        }
        next.push_str("[ ] ");
    }
    Some((len, next))
}

/// Ticks or unticks the task box at byte `at` of `src` (where [`render`] put its `data-at`).
pub fn toggle_task(src: &mut String, at: usize) -> bool {
    let tick = match src.get(at..at + 3) {
        Some("[ ]") => "x",
        Some("[x]" | "[X]") => " ",
        _ => return false,
    };
    src.replace_range(at + 1..at + 2, tick);
    true
}

/// Where in `src` the caret goes after the `rendered` text before a click: each character
/// shown is found in turn, skipping the markdown around it.
pub fn source_offset(src: &str, rendered: &str) -> usize {
    let mut pos = 0;
    for c in rendered.chars().filter(|c| !c.is_whitespace()) {
        match src[pos..].find(c) {
            Some(i) => pos += i + c.len_utf8(),
            None => break,
        }
    }
    pos
}

/// The HTML of one block of `page`. Raw HTML is shown as text. Links to pages carry their
/// repo path in `data-page`, images in the repo theirs in `data-src`, and task boxes the byte
/// offset of their `[ ]` in `data-at`.
pub fn render(src: &str, page: &Path) -> String {
    let mut events = Vec::new();
    let mut image: Option<(String, String)> = None;
    for (ev, r) in Parser::new_ext(src, md_options()).into_offset_iter() {
        if let Some((attrs, alt)) = &mut image {
            match ev {
                Event::End(TagEnd::Image) => {
                    events.push(Event::Html(format!("<img {attrs} alt=\"{}\">", escape(alt)).into()));
                    image = None;
                }
                Event::Text(t) | Event::Code(t) => alt.push_str(&t),
                _ => {}
            }
            continue;
        }
        let ev = match ev {
            Event::Html(t) | Event::InlineHtml(t) => Event::Text(t),
            Event::Start(Tag::HtmlBlock) => Event::Html("<pre class=\"raw\">".into()),
            Event::End(TagEnd::HtmlBlock) => Event::Html("</pre>".into()),
            Event::Start(Tag::MetadataBlock(_)) => Event::Html("<pre class=\"meta\">".into()),
            Event::End(TagEnd::MetadataBlock(_)) => Event::Html("</pre>".into()),
            Event::Start(Tag::Link { dest_url, .. }) => Event::Html(link(&dest_url, page).into()),
            Event::End(TagEnd::Link) => Event::Html("</a>".into()),
            Event::Start(Tag::Image { dest_url, .. }) => {
                image = Some((image_source(&dest_url, page), String::new()));
                continue;
            }
            Event::TaskListMarker(done) => {
                let checked = if done { " checked" } else { "" };
                Event::Html(format!("<input type=\"checkbox\" data-at=\"{}\"{checked}>", r.start).into())
            }
            ev => ev,
        };
        events.push(ev);
    }
    let mut out = String::new();
    html::push_html(&mut out, events.into_iter());
    out
}

fn link(dest: &str, page: &Path) -> String {
    if links::is_web_url(dest) {
        format!("<a href=\"{}\" target=\"_blank\" rel=\"noopener noreferrer\">", escape(dest))
    } else if let Some((target, _)) = links::resolve(page, dest) {
        format!("<a data-page=\"{}\">", escape(&paths::to_slash(&target)))
    } else {
        "<a>".into()
    }
}

fn image_source(dest: &str, page: &Path) -> String {
    let web =
        dest.split_once(':').is_some_and(|(s, _)| s.eq_ignore_ascii_case("http") || s.eq_ignore_ascii_case("https"));
    if web {
        format!("src=\"{}\" loading=\"lazy\"", escape(dest))
    } else if let Some((target, _)) = links::resolve(page, dest) {
        format!("data-src=\"{}\"", escape(&paths::to_slash(&target)))
    } else {
        String::new()
    }
}

/// Escapes text for HTML, in content and in quoted attributes.
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn srcs(d: &Doc) -> Vec<&str> {
        d.pieces.iter().map(|p| p.src.as_str()).collect()
    }

    #[test]
    fn splits_blocks_and_list_items_and_keeps_the_text() {
        let src = "---\ntitle: x\n---\n\n# Title\n\nSome *text*\nmore\n\n- a\n- b\n  - nested\n\n1. one\n2. two\n\n```\ncode\n\n```\n\n> quote\n\n---\n| a | b |\n|---|---|\n| 1 | 2 |\n";
        let d = Doc::parse(src);
        assert_eq!(
            srcs(&d),
            [
                "---\ntitle: x\n---",
                "# Title",
                "Some *text*\nmore",
                "- a",
                "- b\n  - nested",
                "1. one",
                "2. two",
                "```\ncode\n\n```",
                "> quote",
                "---",
                "| a | b |\n|---|---|\n| 1 | 2 |",
            ]
        );
        assert_eq!(d.text(), src);
        for s in ["", "\n\n", "text", "  # indented\n\n\n", "[x]: http://a\n\npara\n", "a\r\nb\r\n\r\nc"] {
            assert_eq!(Doc::parse(s).text(), s);
        }
    }

    #[test]
    fn empty_blocks_leave_no_trace() {
        let mut d = Doc::parse("# T\n");
        assert_eq!(d.enter(0, 3), Some((1, 0)));
        assert_eq!(d.text(), "# T\n");
        d.pieces[1].src = "hello".into();
        assert_eq!(d.text(), "# T\n\nhello\n");
    }

    #[test]
    fn enter_splits_paragraphs_and_continues_lists() {
        let mut d = Doc::parse("Hello world\n");
        assert_eq!(d.enter(0, 6), Some((1, 0)));
        assert_eq!(d.text(), "Hello \n\nworld\n");

        let mut d = Doc::parse("- [x] a\n- b\n\nafter\n");
        assert_eq!(d.enter(0, 7), Some((1, 6)));
        assert_eq!(d.pieces[1].src, "- [ ] ");
        d.pieces[1].src.push('c');
        assert_eq!(d.text(), "- [x] a\n- [ ] c\n- b\n\nafter\n");

        let mut d = Doc::parse("1. a\n");
        assert_eq!(d.enter(0, 4), Some((1, 3)));
        assert_eq!(d.pieces[1].src, "2. ");
        // Enter on the empty item ends the list.
        assert_eq!(d.enter(1, 3), Some((1, 0)));
        d.pieces[1].src.push_str("text");
        assert_eq!(d.text(), "1. a\n\ntext\n");

        let mut d = Doc::parse("> a\n");
        assert_eq!(d.enter(0, 3), Some((0, 6)));
        assert_eq!(d.pieces[0].src, "> a\n> ");
        assert_eq!(d.enter(0, 6), Some((1, 0)));
        assert_eq!(d.text(), "> a\n");

        let mut d = Doc::parse("```\ncode\n```\n");
        assert_eq!(d.enter(0, 8), None);
    }

    #[test]
    fn settle_splits_and_drops() {
        let mut d = Doc::parse("a\n\nb\n");
        d.pieces[0].src = "x\n\ny".into();
        assert_eq!(d.settle(0), 2);
        assert_eq!(srcs(&d), ["x", "y", "b"]);
        assert_eq!(d.text(), "x\n\ny\n\nb\n");
        d.pieces[2].src.clear();
        assert_eq!(d.settle(2), 0);
        assert_eq!(d.text(), "x\n\ny\n");
    }

    #[test]
    fn backspace_joins_paragraphs() {
        let mut d = Doc::parse("ab\n\ncd\n\n- x\n");
        assert_eq!(d.backspace(1), Some((0, 2)));
        assert_eq!(d.text(), "abcd\n\n- x\n");
        assert_eq!(d.backspace(1), Some((0, 4)));
        assert_eq!(d.text(), "abcd\n\n- x\n");
        assert_eq!(d.backspace(0), None);
    }

    #[test]
    fn markers() {
        assert_eq!(marker("- a"), Some((2, "- ".into())));
        assert_eq!(marker("  * [ ] a"), Some((8, "  * [ ] ".into())));
        assert_eq!(marker("9) x"), Some((3, "10) ".into())));
        assert_eq!(marker("-"), Some((1, "- ".into())));
        assert_eq!(marker("**bold**"), None);
        assert_eq!(marker("---"), None);
        assert_eq!(marker("2024. was"), Some((6, "2025. ".into())));
        assert_eq!(marker("text"), None);
    }

    #[test]
    fn renders_safely() {
        let page = Path::new("notes/a.md");
        let html = render("<script>x</script>", page);
        assert!(!html.contains("<script>"), "{html}");
        let html = render("[p](b.md) [w](https://x.org/?a=\"1\") [j](javascript:alert(1))", page);
        assert!(html.contains("<a data-page=\"notes/b.md\">p</a>"), "{html}");
        assert!(html.contains("href=\"https://x.org/?a=&quot;1&quot;\""), "{html}");
        assert!(!html.contains("href=\"javascript"), "{html}");
        let html = render("![a *b*](../assets/x.png)", page);
        assert!(html.contains("<img data-src=\"assets/x.png\" alt=\"a b\">"), "{html}");
        let mut src = "- [ ] task".to_string();
        let html = render(&src, page);
        assert!(html.contains("data-at=\"2\""), "{html}");
        assert!(toggle_task(&mut src, 2));
        assert_eq!(src, "- [x] task");
    }

    #[test]
    fn click_offsets() {
        assert_eq!(source_offset("# Hello", "Hel"), 5);
        assert_eq!(source_offset("a [link](x.md) b", "a lin"), 6);
        assert_eq!(source_offset("- item", ""), 0);
        assert_eq!(kind("## h"), Some(Kind::Heading(2)));
    }
}
