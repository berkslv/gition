//! A page that is always editable. Its blocks show rendered; the one being edited shows its
//! markdown in a textarea in its place, and every change is written to the working copy.
//! Enter starts a new block, so typing feels like writing in a document rather than in a file.

use std::path::PathBuf;

use anyhow::Result;
use gition::blocks::{self, Doc, Kind, Piece};
use gition::paste;
use gition::storage::Storage;
use wasm_bindgen::JsCast;
use web_sys::{Element, HtmlTextAreaElement, KeyboardEvent};

use crate::dom::{self, byte_at, units_at};

/// What [`Editor::shown`] holds for the block being edited.
const EDITING: &str = "\0";

pub struct Editor {
    pub path: PathBuf,
    pub doc: Doc,
    /// Not markdown: the whole file is one block, shown as it is.
    plain: bool,
    eol: &'static str,
    /// The text last written, with `\n` line breaks.
    saved: String,
    /// The block being edited, and its textarea.
    pub editing: Option<usize>,
    ta: Option<HtmlTextAreaElement>,
    /// What each block's element shows, to redraw only the ones that changed.
    shown: Vec<String>,
}

impl Editor {
    pub fn new(path: PathBuf, text: &str, plain: bool) -> Editor {
        let eol = if text.contains("\r\n") { "\r\n" } else { "\n" };
        let text = text.replace("\r\n", "\n");
        let doc = if plain {
            Doc { lead: String::new(), pieces: vec![Piece { src: text.clone(), gap: String::new() }] }
        } else {
            Doc::parse(&text)
        };
        Editor { path, doc, plain, eol, saved: text, editing: None, ta: None, shown: Vec::new() }
    }

    /// Whether `on_disk` is what this editor last wrote.
    pub fn is_current(&self, on_disk: &str) -> bool {
        on_disk.replace("\r\n", "\n") == self.saved
    }

    /// Writes the page if it changed, keeping its line endings.
    pub fn save(&mut self, fs: &dyn Storage) -> Result<()> {
        let text = self.doc.text();
        if text != self.saved {
            let out = if self.eol == "\n" { text.clone() } else { text.replace('\n', self.eol) };
            fs.write(&self.path, out.as_bytes())?;
            self.saved = text;
        }
        Ok(())
    }

    // ---- drawing ------------------------------------------------------------------

    fn html(&self, i: usize) -> String {
        let src = &self.doc.pieces[i].src;
        if self.editing == Some(i) {
            EDITING.into()
        } else if self.plain {
            format!("<pre>{}</pre>", blocks::escape(src))
        } else if src.trim().is_empty() {
            String::new()
        } else {
            blocks::render(src, &self.path)
        }
    }

    /// Forgets what's drawn, for a fresh `root`.
    pub fn reset(&mut self) {
        self.shown.clear();
        self.ta = None;
    }

    /// Brings `root`'s elements in line with the blocks, redrawing only those that changed.
    pub fn sync(&mut self, root: &Element) {
        let new: Vec<String> = (0..self.doc.pieces.len()).map(|i| self.html(i)).collect();
        let old = std::mem::take(&mut self.shown);
        let pre = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
        let room = old.len().min(new.len()) - pre;
        let suf = old.iter().rev().zip(new.iter().rev()).take(room).take_while(|(a, b)| a == b).count();
        let children = root.children();
        for gone in &old[pre..old.len() - suf] {
            if gone == EDITING {
                self.ta = None;
            }
            if let Some(c) = children.item(pre as u32) {
                c.remove();
            }
        }
        let before = children.item(pre as u32);
        let document = dom::document();
        for (i, html) in new.iter().enumerate().take(new.len() - suf).skip(pre) {
            let Ok(div) = document.create_element("div") else { continue };
            let _ = div.set_attribute("data-i", &i.to_string());
            if html == EDITING {
                div.set_class_name("blk editing");
                if let Ok(ta) = document.create_element("textarea") {
                    let ta: HtmlTextAreaElement = ta.unchecked_into();
                    ta.set_rows(1);
                    ta.set_value(&self.doc.pieces[i].src);
                    let _ = div.append_child(&ta);
                    self.ta = Some(ta);
                }
            } else {
                div.set_class_name(if html.is_empty() { "blk empty" } else { "blk" });
                div.set_inner_html(html);
            }
            let _ = root.insert_before(&div, before.as_ref().map(|b| b.unchecked_ref()));
        }
        for i in new.len() - suf..new.len() {
            if let Some(c) = children.item(i as u32) {
                let _ = c.set_attribute("data-i", &i.to_string());
            }
        }
        self.shown = new;
        if let (Some(i), Some(ta)) = (self.editing, &self.ta) {
            style(ta, &self.doc.pieces[i].src, self.plain);
        }
    }

    // ---- editing ------------------------------------------------------------------

    /// Edits block `i` with the caret at byte `at`.
    pub fn edit(&mut self, root: &Element, i: usize, at: usize) {
        self.editing = Some(i);
        self.sync(root);
        let Some(ta) = &self.ta else { return };
        let src = &self.doc.pieces[i].src;
        if ta.value() != *src {
            ta.set_value(src);
        }
        let _ = ta.focus();
        let caret = units_at(src, at);
        let _ = ta.set_selection_range(caret, caret);
        dom::fit(ta);
    }

    /// Stops editing without redrawing: the block is read again, and may become several or none.
    /// Returns where it was and how many blocks took its place.
    fn settle(&mut self) -> Option<(usize, usize)> {
        let i = self.editing.take()?;
        let n = if self.plain { 1 } else { self.doc.settle(i) };
        Some((i, n))
    }

    /// Stops editing, and draws the block.
    pub fn leave(&mut self, root: &Element) {
        if self.settle().is_some() {
            self.sync(root);
        }
    }

    /// Moves to block `j` (as numbered before the current block is settled).
    pub fn goto(&mut self, root: &Element, mut j: usize, at: usize) {
        if let Some((i, n)) = self.settle()
            && j > i
        {
            j = j + n - 1;
        }
        if j < self.doc.pieces.len() {
            self.edit(root, j, at);
        } else {
            self.sync(root);
        }
    }

    /// Edits a new empty block at the end, or the empty one already there.
    pub fn append(&mut self, root: &Element) {
        self.settle();
        let j = self.push(String::new());
        self.edit(root, j, 0);
    }

    fn push(&mut self, src: String) -> usize {
        let pieces = &mut self.doc.pieces;
        match pieces.last_mut() {
            Some(last) if last.src.trim().is_empty() => last.src = src,
            Some(last) => {
                let gap = std::mem::replace(&mut last.gap, "\n\n".into());
                pieces.push(Piece { src, gap });
            }
            None => pieces.push(Piece { src, gap: "\n".into() }),
        }
        pieces.len() - 1
    }

    /// Takes in what was typed in the textarea.
    pub fn input(&mut self) {
        let (Some(i), Some(ta)) = (self.editing, &self.ta) else { return };
        let value = ta.value();
        if self.doc.pieces[i].src != value {
            style(ta, &value, self.plain);
            self.doc.pieces[i].src = value;
        }
        dom::fit(ta);
    }

    /// The textarea's selection, in bytes.
    fn selection(&self) -> Option<(usize, usize)> {
        let (i, ta) = (self.editing?, self.ta.as_ref()?);
        let src = &self.doc.pieces[i].src;
        let start = ta.selection_start().ok()??;
        let end = ta.selection_end().ok()??;
        Some((byte_at(src, start), byte_at(src, end)))
    }

    /// Handles the keys that cross blocks; true when it did.
    pub fn key(&mut self, root: &Element, e: &KeyboardEvent) -> bool {
        if e.is_composing() || e.ctrl_key() || e.meta_key() || e.alt_key() {
            return false;
        }
        let (Some(i), Some((start, end))) = (self.editing, self.selection()) else { return false };
        let len = self.doc.pieces[i].src.len();
        let caret = (start == end).then_some(start);
        let last = self.doc.pieces.len() - 1;
        let next = match e.key().as_str() {
            "Enter" if !e.shift_key() && !self.plain => caret.and_then(|at| self.doc.enter(i, at)),
            "Backspace" if caret == Some(0) && !self.plain => self.doc.backspace(i),
            "ArrowUp" | "ArrowLeft" if caret == Some(0) && i > 0 => {
                let at = self.doc.pieces[i - 1].src.len();
                self.goto(root, i - 1, at);
                return true;
            }
            "ArrowDown" | "ArrowRight" if caret == Some(len) && i < last => {
                self.goto(root, i + 1, 0);
                return true;
            }
            "Tab" if !self.plain && blocks::kind(&self.doc.pieces[i].src) == Some(Kind::Item) => {
                Some((i, indent(&mut self.doc.pieces[i].src, start, e.shift_key())))
            }
            _ => None,
        };
        match next {
            Some((j, at)) => {
                self.edit(root, j, at);
                true
            }
            None => false,
        }
    }

    /// Puts `text` at the caret, or in a new block at the end when nothing is being edited.
    pub fn insert(&mut self, root: &Element, text: &str) {
        match (self.editing, self.selection()) {
            (Some(i), Some((start, end))) => {
                self.doc.pieces[i].src.replace_range(start..end, text);
                self.edit(root, i, start + text.len());
            }
            _ => {
                self.push(text.into());
                self.sync(root);
            }
        }
    }

    /// Swaps the first bare `url` on the page for a link titled `title`.
    pub fn link_title(&mut self, root: &Element, url: &str, title: &str) -> bool {
        let link = paste::link(title, url);
        let caret = self.selection().map(|(s, _)| s);
        for (i, p) in self.doc.pieces.iter_mut().enumerate() {
            let Some(k) = p.src.match_indices(url).map(|(k, _)| k).find(|&k| !paste::in_link(&p.src[..k])) else {
                continue;
            };
            p.src.replace_range(k..k + url.len(), &link);
            if self.editing == Some(i) {
                let at = caret.map_or(0, |c| if c >= k + url.len() { c + link.len() - url.len() } else { c });
                self.edit(root, i, at);
            } else {
                self.sync(root);
            }
            return true;
        }
        false
    }

    /// Ticks or unticks the task box at byte `at` of block `i`.
    pub fn toggle_task(&mut self, root: &Element, i: usize, at: usize) {
        if let Some(p) = self.doc.pieces.get_mut(i)
            && blocks::toggle_task(&mut p.src, at)
        {
            self.sync(root);
        }
    }

    /// A click on block `j`'s element `el`: edits it with the caret where the click was.
    pub fn click(&mut self, root: &Element, j: usize, el: &Element, x: f64, y: f64) {
        let Some(src) = self.doc.pieces.get(j).map(|p| &p.src) else { return };
        let at = match dom::text_before_click(el, x, y) {
            Some(t) if self.plain => byte_at(src, t.encode_utf16().count() as u32),
            Some(t) => blocks::source_offset(src, &t),
            None => src.len(),
        };
        self.goto(root, j, at);
    }
}

/// Indents the line holding byte `at` by two spaces, or takes up to two away. Returns where
/// the caret goes.
fn indent(src: &mut String, at: usize, out: bool) -> usize {
    let start = src[..at].rfind('\n').map_or(0, |n| n + 1);
    if !out {
        src.insert_str(start, "  ");
        return at + 2;
    }
    let n = src[start..].bytes().take(2).take_while(|b| *b == b' ').count();
    src.replace_range(start..start + n, "");
    at.saturating_sub(n).max(start)
}

/// Sizes the textarea's font for the kind of block it holds.
fn style(ta: &HtmlTextAreaElement, src: &str, plain: bool) {
    let class = match blocks::kind(src) {
        _ if plain => "mono",
        Some(Kind::Heading(1)) => "h1",
        Some(Kind::Heading(2)) => "h2",
        Some(Kind::Heading(_)) => "h3",
        Some(Kind::Code | Kind::Table | Kind::Meta | Kind::Html) => "mono",
        _ => "",
    };
    ta.set_class_name(&format!("src {class}"));
}
