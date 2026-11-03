//! Renders markdown into styled terminal lines.

use pulldown_cmark::{Event, HeadingLevel, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::links::md_options;

#[derive(Debug, Clone)]
pub enum Block {
    /// One logical line; `hang` is the indent used for wrapped continuation rows.
    Text {
        line: Line<'static>,
        links: Vec<String>,
        hang: u16,
    },
    Image {
        dest: String,
        alt: String,
    },
}

impl Block {
    fn is_blank(&self) -> bool {
        matches!(self, Block::Text { line, .. } if line.spans.is_empty())
    }
}

const CODE: Style = Style::new().fg(Color::Yellow);
const DIM: Style = Style::new().fg(Color::DarkGray);

#[derive(Default)]
struct Renderer {
    blocks: Vec<Block>,
    spans: Vec<Span<'static>>,
    links: Vec<String>,
    styles: Vec<Style>,
    quote: usize,
    lists: Vec<Option<u64>>,
    item_indents: Vec<usize>,
    bullet: Option<String>,
    in_code: bool,
    image: Option<(String, String)>,
}

pub fn render(src: &str) -> Vec<Block> {
    let mut r = Renderer::default();
    for ev in Parser::new_ext(src, md_options()) {
        r.event(ev);
    }
    r.flush();
    while r.blocks.last().is_some_and(Block::is_blank) {
        r.blocks.pop();
    }
    r.blocks
}

impl Renderer {
    fn style(&self) -> Style {
        self.styles.iter().fold(Style::default(), |acc, s| acc.patch(*s))
    }

    fn prefix(&mut self) -> Vec<Span<'static>> {
        let mut out = Vec::new();
        if self.quote > 0 {
            out.push(Span::styled("│ ".repeat(self.quote), DIM));
        }
        if let Some(b) = self.bullet.take() {
            out.push(Span::styled(b, Style::new().fg(Color::Cyan)));
        } else if let Some(&w) = self.item_indents.last() {
            out.push(Span::raw(" ".repeat(w)));
        }
        out
    }

    fn flush(&mut self) {
        if self.spans.is_empty() && self.bullet.is_none() {
            return;
        }
        let mut spans = self.prefix();
        let hang = spans.iter().map(|s| s.content.width()).sum::<usize>() as u16;
        spans.append(&mut self.spans);
        self.blocks.push(Block::Text { line: Line::from(spans), links: std::mem::take(&mut self.links), hang });
    }

    fn blank(&mut self) {
        if self.blocks.last().is_some_and(|b| !b.is_blank()) {
            self.blocks.push(Block::Text { line: Line::default(), links: Vec::new(), hang: 0 });
        }
    }

    fn push_line(&mut self, spans: Vec<Span<'static>>) {
        self.spans = spans;
        self.flush();
    }

    fn event(&mut self, ev: Event) {
        match ev {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(t) => {
                if let Some((_, alt)) = &mut self.image {
                    alt.push_str(&t);
                } else if self.in_code {
                    for l in t.split_terminator('\n') {
                        self.push_line(vec![Span::raw("  "), Span::styled(l.to_string(), CODE)]);
                    }
                } else {
                    self.spans.push(Span::styled(t.into_string(), self.style()));
                }
            }
            Event::Code(t) => self.spans.push(Span::styled(t.into_string(), self.style().patch(CODE))),
            Event::Html(t) => {
                for l in t.split_terminator('\n') {
                    self.push_line(vec![Span::styled(l.to_string(), DIM)]);
                }
            }
            Event::InlineHtml(t) => self.spans.push(Span::styled(t.into_string(), DIM)),
            Event::SoftBreak => self.spans.push(Span::raw(" ")),
            Event::HardBreak => self.flush(),
            Event::Rule => {
                self.flush();
                self.push_line(vec![Span::styled("─".repeat(40), DIM)]);
                self.blank();
            }
            Event::TaskListMarker(done) => {
                self.spans.push(Span::styled(if done { "☑ " } else { "☐ " }, Style::new().fg(Color::Cyan)))
            }
            _ => {}
        }
    }

    fn start(&mut self, tag: Tag) {
        match tag {
            Tag::Heading { level, .. } => {
                self.flush();
                let style = match level {
                    HeadingLevel::H1 => {
                        Style::new().fg(Color::Magenta).add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
                    }
                    HeadingLevel::H2 => Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                    HeadingLevel::H3 => Style::new().fg(Color::Blue).add_modifier(Modifier::BOLD),
                    _ => Style::new().add_modifier(Modifier::BOLD),
                };
                self.styles.push(style);
            }
            Tag::BlockQuote(_) => {
                self.flush();
                self.quote += 1;
            }
            Tag::CodeBlock(_) => {
                self.flush();
                self.in_code = true;
            }
            Tag::List(start) => {
                self.flush();
                self.lists.push(start);
            }
            Tag::Item => {
                self.flush();
                let depth = self.lists.len().max(1);
                let marker = match self.lists.last_mut() {
                    Some(Some(n)) => {
                        let m = format!("{n}. ");
                        *n += 1;
                        m
                    }
                    _ => "• ".to_string(),
                };
                let bullet = format!("{}{}", "  ".repeat(depth - 1), marker);
                self.item_indents.push(bullet.width());
                self.bullet = Some(bullet);
            }
            Tag::Emphasis => self.styles.push(Style::new().add_modifier(Modifier::ITALIC)),
            Tag::Strong => self.styles.push(Style::new().add_modifier(Modifier::BOLD)),
            Tag::Strikethrough => self.styles.push(Style::new().add_modifier(Modifier::CROSSED_OUT)),
            Tag::Link { dest_url, .. } => {
                self.links.push(dest_url.into_string());
                self.styles.push(Style::new().fg(Color::Blue).add_modifier(Modifier::UNDERLINED));
            }
            Tag::Image { dest_url, .. } => {
                self.flush();
                self.image = Some((dest_url.into_string(), String::new()));
            }
            Tag::Table(_) => self.flush(),
            Tag::TableHead => self.styles.push(Style::new().add_modifier(Modifier::BOLD)),
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => {
                self.flush();
                if self.lists.is_empty() {
                    self.blank();
                }
            }
            TagEnd::Heading(_) => {
                self.styles.pop();
                self.flush();
                self.blank();
            }
            TagEnd::BlockQuote(_) => {
                self.flush();
                self.quote = self.quote.saturating_sub(1);
                if self.quote == 0 {
                    self.blank();
                }
            }
            TagEnd::CodeBlock => {
                self.in_code = false;
                self.blank();
            }
            TagEnd::List(_) => {
                self.flush();
                self.lists.pop();
                if self.lists.is_empty() {
                    self.blank();
                }
            }
            TagEnd::Item => {
                self.flush();
                self.item_indents.pop();
            }
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough | TagEnd::Link => {
                self.styles.pop();
            }
            TagEnd::Image => {
                if let Some((dest, alt)) = self.image.take() {
                    self.blocks.push(Block::Image { dest, alt });
                }
            }
            TagEnd::TableHead => {
                self.styles.pop();
                self.flush();
                self.push_line(vec![Span::styled("─".repeat(40), DIM)]);
            }
            TagEnd::TableCell => self.spans.push(Span::styled(" │ ", DIM)),
            TagEnd::TableRow => self.flush(),
            TagEnd::Table => self.blank(),
            _ => {}
        }
    }
}

/// Soft-wraps a line to `width` columns, indenting continuation rows by `hang`.
pub fn wrap(line: &Line<'static>, hang: u16, width: u16) -> Vec<Line<'static>> {
    let width = width.max(1) as usize;
    let hang = if (hang as usize) + 4 >= width { 0 } else { hang as usize };
    // Split spans into word and whitespace tokens that keep their style.
    let mut tokens: Vec<(String, Style)> = Vec::new();
    for span in &line.spans {
        let mut cur = String::new();
        let mut cur_ws = None;
        for ch in span.content.chars() {
            let ws = ch.is_whitespace();
            if cur_ws.is_some_and(|w| w != ws) {
                tokens.push((std::mem::take(&mut cur), span.style));
            }
            cur_ws = Some(ws);
            cur.push(ch);
        }
        if !cur.is_empty() {
            tokens.push((cur, span.style));
        }
    }

    let mut rows: Vec<Line<'static>> = Vec::new();
    let mut row: Vec<Span<'static>> = Vec::new();
    let mut row_w = 0usize;
    let new_row = |rows: &mut Vec<Line<'static>>, row: &mut Vec<Span<'static>>, row_w: &mut usize| {
        rows.push(Line::from(std::mem::take(row)).style(line.style));
        if hang > 0 {
            row.push(Span::raw(" ".repeat(hang)));
        }
        *row_w = hang;
    };
    for (tok, style) in tokens {
        let w = tok.width();
        let is_ws = tok.starts_with(char::is_whitespace);
        if row_w + w <= width {
            row.push(Span::styled(tok, style));
            row_w += w;
        } else if is_ws {
            new_row(&mut rows, &mut row, &mut row_w);
        } else if w <= width - hang && row_w > hang {
            new_row(&mut rows, &mut row, &mut row_w);
            row.push(Span::styled(tok, style));
            row_w += w;
        } else {
            // Word longer than a row: break it by characters.
            let mut buf = String::new();
            for ch in tok.chars() {
                let cw = ch.width().unwrap_or(0);
                if row_w + cw > width && row_w > hang {
                    row.push(Span::styled(std::mem::take(&mut buf), style));
                    new_row(&mut rows, &mut row, &mut row_w);
                }
                buf.push(ch);
                row_w += cw;
            }
            row.push(Span::styled(buf, style));
        }
    }
    rows.push(Line::from(row).style(line.style));
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(blocks: &[Block]) -> Vec<String> {
        blocks
            .iter()
            .map(|b| match b {
                Block::Text { line, .. } => line.spans.iter().map(|s| s.content.as_ref()).collect(),
                Block::Image { dest, alt } => format!("[img {alt} {dest}]"),
            })
            .collect()
    }

    #[test]
    fn renders_common_blocks() {
        let src = "# Title\n\nHello **world** and [a link](x.md).\n\n- one\n- two\n  1. nested\n\n> quote\n\n![cat](assets/cat.png)\n\n```\ncode\n```\n";
        let blocks = render(src);
        assert_eq!(
            text(&blocks),
            [
                "Title",
                "",
                "Hello world and a link.",
                "",
                "• one",
                "• two",
                "  1. nested",
                "",
                "│ quote",
                "",
                "[img cat assets/cat.png]",
                "",
                "  code",
            ]
        );
        let Block::Text { links, .. } = &blocks[2] else { panic!() };
        assert_eq!(links, &["x.md"]);
    }

    #[test]
    fn wraps_with_hang() {
        let line = Line::from(vec![Span::raw("• "), Span::raw("alpha beta gamma delta")]);
        let rows: Vec<String> = wrap(&line, 2, 12).iter().map(|l| l.to_string()).collect();
        assert_eq!(rows, ["• alpha beta", "  gamma ", "  delta"]);
    }

    #[test]
    fn wraps_long_words() {
        let rows: Vec<String> = wrap(&Line::raw("abcdefghij"), 0, 4).iter().map(|l| l.to_string()).collect();
        assert_eq!(rows, ["abcd", "efgh", "ij"]);
    }
}
