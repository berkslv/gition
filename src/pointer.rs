//! Pointer input (touch and mouse) on top of the keyboard actions. `ui::draw` records
//! what it drew where; these methods find the target under the pointer and run the
//! same actions a key would.

use ratatui::layout::{Position, Rect};
use ratatui_textarea::{Input, Key};
use unicode_width::UnicodeWidthChar;

use crate::app::{App, Editor, Focus, Modal, PageKind, Row, jump_to};
use crate::keymap::{Action, KeySpec};
use crate::markdown::Block;

/// Something drawn on screen that reacts to the pointer.
#[derive(Debug, Clone)]
pub enum Target {
    TreeRow(usize),
    PageRow(usize),
    /// The page pane outside any row.
    Page,
    Editor(Rect),
    Status,
    /// A modal's body; blocks the panes below it.
    Modal,
    /// A hint like "y delete": tapping it presses the key.
    Key(Input),
    SearchResult(usize),
    MenuItem(usize),
}

/// Text selected by dragging the mouse. The editor's selection lives in its text area;
/// the viewer's is a range of drawn rows and columns, both ends included.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Selection {
    Editor,
    View { anchor: (usize, usize), head: (usize, usize) },
}

/// Actions offered by the touch menu, in order, while viewing and while editing.
const MENU_VIEW: &[Action] = &[
    Action::Edit,
    Action::Search,
    Action::Sync,
    Action::NewPage,
    Action::NewFolder,
    Action::Rename,
    Action::Delete,
    Action::OpenExternal,
    Action::ToggleTree,
    Action::ToggleAllFiles,
    Action::Help,
];
const MENU_EDIT: &[Action] =
    &[Action::Save, Action::ExitEdit, Action::Undo, Action::Redo, Action::Sync, Action::ToggleTree, Action::Help];

/// The menu's name for an action; shorter than the help text.
pub fn menu_label(a: Action) -> &'static str {
    match a {
        Action::Edit => "Edit page",
        Action::Search => "Find page",
        Action::Sync => "Sync",
        Action::NewPage => "New page",
        Action::NewFolder => "New folder",
        Action::Rename => "Rename / move",
        Action::Delete => "Delete",
        Action::OpenExternal => "Open externally",
        Action::ToggleTree => "Show / hide pages",
        Action::ToggleAllFiles => "Show all files",
        Action::Help => "Keyboard shortcuts",
        Action::Save => "Save",
        Action::ExitEdit => "Done editing",
        Action::Undo => "Undo",
        Action::Redo => "Redo",
        a => a.description(),
    }
}

/// The key a hint segment like "y delete" or "Esc cancel" stands for.
pub fn hint_key(segment: &str) -> Option<Input> {
    let word = segment.split_whitespace().next()?;
    let spec = if word == "any" { KeySpec::parse("esc") } else { KeySpec::parse(word) }.ok()?;
    Some(Input { key: spec.key, ctrl: spec.ctrl, alt: spec.alt, shift: spec.shift })
}

fn key(key: Key) -> Input {
    Input { key, ..Input::default() }
}

/// Moves the editor's cursor to the text under a cell, clamped to the editor's area.
fn jump(ta: &mut ratatui_textarea::TextArea, area: Rect, x: u16, y: u16) {
    let (top, left) = ta.scroll_offset();
    let x = x.clamp(area.x, area.right().saturating_sub(1));
    // Past the top or bottom edge reaches one row further, so dragging there scrolls.
    let row = if y < area.y { top.saturating_sub(1) } else { y.min(area.bottom()) - area.y + top };
    let c = ta.screen_to_data(row as usize, (x - area.x + left) as usize);
    ta.move_cursor(jump_to(c.0, c.1));
}

/// The characters of `s` drawn in display columns `from..to`.
fn columns(s: &str, from: usize, to: usize) -> String {
    let mut col = 0;
    let mut out = String::new();
    for ch in s.chars() {
        if col >= from && col < to {
            out.push(ch);
        }
        col += ch.width().unwrap_or(0);
    }
    out
}

impl App {
    fn hit(&self, x: u16, y: u16) -> Option<Target> {
        let p = Position::new(x, y);
        self.hits.iter().rev().find(|(r, _)| r.contains(p)).map(|(_, t)| t.clone())
    }

    /// Opens the touch menu with what can be done right now.
    pub fn open_menu(&mut self) {
        let items = if self.editor.is_some() { MENU_EDIT } else { MENU_VIEW };
        self.modal = Modal::Menu { items: items.to_vec(), sel: 0 };
    }

    /// Runs an action picked from the menu as if its key was pressed in the right pane.
    pub(crate) fn run_action(&mut self, a: Action) {
        if self.editor.is_some() && self.focus == Focus::Page {
            return self.edit_action(a);
        }
        match a {
            Action::Edit | Action::OpenExternal | Action::Follow => {
                self.focus = Focus::Page;
                self.view_action(a);
            }
            Action::NewPage
            | Action::NewFolder
            | Action::Rename
            | Action::Delete
            | Action::InsertImage
            | Action::ToggleAllFiles => self.tree_action(a),
            _ => self.global(a),
        }
    }

    /// A tap or click at a cell.
    pub fn tap(&mut self, x: u16, y: u16) {
        self.selection = None;
        let target = self.hit(x, y);
        if !matches!(self.modal, Modal::None) {
            return match target {
                Some(Target::Key(k)) => self.on_key(k),
                Some(Target::SearchResult(n)) => {
                    if let Modal::Search { sel, .. } = &mut self.modal {
                        *sel = n;
                    }
                    self.on_key(key(Key::Enter));
                }
                Some(Target::MenuItem(n)) => {
                    if let Modal::Menu { items, .. } = std::mem::replace(&mut self.modal, Modal::None)
                        && let Some(&a) = items.get(n)
                    {
                        self.run_action(a);
                    }
                }
                // Help closes on any key; elsewhere a tap outside the modal cancels it.
                Some(Target::Modal) if !matches!(self.modal, Modal::Help) => {}
                _ => self.on_key(key(Key::Esc)),
            };
        }
        match target {
            Some(Target::TreeRow(i)) => {
                self.sel = i;
                self.focus = Focus::Tree;
                let is_dir = self.visible.get(i).is_some_and(|v| v.is_dir);
                if is_dir && self.visible[i].expanded && !self.visible[i].path.as_os_str().is_empty() {
                    self.tree_action(Action::Collapse);
                } else {
                    self.tree_action(Action::Open);
                }
                // On a narrow screen the page needs the room once a page is picked.
                if !is_dir && self.narrow() && self.modal_is_none() {
                    self.toggle_tree();
                }
            }
            Some(Target::PageRow(i)) => {
                self.focus = Focus::Page;
                self.view.cursor = i;
                if self.row_has_link(i) {
                    self.view_action(Action::Follow);
                }
            }
            Some(Target::Page) => self.focus = Focus::Page,
            Some(Target::Editor(area)) => {
                self.focus = Focus::Page;
                if let Some(e) = &mut self.editor {
                    e.ta.cancel_selection();
                    jump(&mut e.ta, area, x, y);
                }
            }
            Some(Target::Status) => self.open_menu(),
            _ => {}
        }
    }

    /// A double tap or double click: edit what's there.
    pub fn double_tap(&mut self, x: u16, y: u16) {
        if !self.modal_is_none() {
            return;
        }
        match self.hit(x, y) {
            Some(Target::TreeRow(i)) => {
                self.sel = i;
                self.focus = Focus::Tree;
                self.tree_action(Action::Edit);
            }
            Some(Target::PageRow(_) | Target::Page) if self.editor.is_none() => {
                self.focus = Focus::Page;
                self.view_action(Action::Edit);
            }
            _ => {}
        }
    }

    /// A long press: the menu, for the tree row under the finger if there is one.
    pub fn long_press(&mut self, x: u16, y: u16) {
        if !self.modal_is_none() {
            return;
        }
        match self.hit(x, y) {
            Some(Target::TreeRow(i)) => {
                self.sel = i;
                self.focus = Focus::Tree;
            }
            Some(Target::PageRow(_) | Target::Page | Target::Editor(_)) => self.focus = Focus::Page,
            _ => {}
        }
        self.open_menu();
    }

    /// Scrolls whatever is under the pointer by `rows` (positive is down).
    pub fn scroll(&mut self, x: u16, y: u16, rows: isize) {
        match self.hit(x, y) {
            Some(Target::TreeRow(_)) => {
                let max = self.visible.len().saturating_sub(1) as isize;
                self.sel = (self.sel as isize + rows).clamp(0, max.max(0)) as usize;
            }
            Some(Target::PageRow(_) | Target::Page) => {
                let max = self.view.rows.len().saturating_sub(1) as isize;
                let clamp = |v: usize| (v as isize + rows).clamp(0, max.max(0)) as usize;
                self.view.scroll = clamp(self.view.scroll);
                self.view.cursor = clamp(self.view.cursor);
            }
            Some(Target::Editor(_)) => {
                if let Some(e) = &mut self.editor {
                    e.ta.scroll((rows.clamp(i16::MIN as isize, i16::MAX as isize) as i16, 0));
                }
            }
            Some(Target::SearchResult(_)) | Some(Target::MenuItem(_)) | Some(Target::Modal) => {
                let k = key(if rows > 0 { Key::Down } else { Key::Up });
                for _ in 0..rows.unsigned_abs() {
                    self.on_key(k.clone());
                }
            }
            _ => {}
        }
    }

    /// A mouse drag from the cell where the button went down to the one under it now:
    /// selects text in the editor or the viewer. Call it for every move; `drag_end` copies.
    pub fn drag(&mut self, from: (u16, u16), to: (u16, u16)) {
        if !self.modal_is_none() {
            return;
        }
        if !self.dragging {
            self.selection = None;
            match self.hit(from.0, from.1) {
                Some(Target::Editor(area)) => {
                    self.focus = Focus::Page;
                    let Some(e) = &mut self.editor else { return };
                    e.ta.cancel_selection();
                    jump(&mut e.ta, area, from.0, from.1);
                    e.ta.start_selection();
                    self.selection = Some(Selection::Editor);
                }
                Some(Target::PageRow(_) | Target::Page) if self.shows_text() => {
                    self.focus = Focus::Page;
                    let at = self.view_cell(from.0, from.1);
                    self.selection = Some(Selection::View { anchor: at, head: at });
                }
                _ => return,
            }
            self.dragging = true;
        }
        match self.selection {
            Some(Selection::Editor) => {
                let area = self.hits.iter().find_map(|(_, t)| match t {
                    Target::Editor(a) => Some(*a),
                    _ => None,
                });
                if let (Some(e), Some(area)) = (&mut self.editor, area) {
                    jump(&mut e.ta, area, to.0, to.1);
                }
            }
            Some(Selection::View { anchor, .. }) => {
                let head = self.view_cell(to.0, to.1);
                // The cursor follows, so the view scrolls when the drag leaves it.
                self.view.cursor = head.0;
                self.selection = Some(Selection::View { anchor, head });
            }
            None => {}
        }
    }

    /// The mouse button went up after a drag: copies what's selected. The selection stays
    /// visible until the next click or key.
    pub fn drag_end(&mut self) {
        self.dragging = false;
        let text = match self.selection {
            Some(Selection::Editor) => {
                self.selection = None;
                self.editor.as_ref().and_then(Editor::selected_text)
            }
            Some(Selection::View { .. }) => Some(self.selected_text()),
            None => None,
        };
        let Some(text) = text.filter(|t| !t.is_empty()) else { return };
        self.copy(&text);
    }

    /// Puts `text` on the clipboard and says so.
    pub(crate) fn copy(&mut self, text: &str) {
        if self.host.copy(text) {
            let n = text.chars().count();
            self.info(format!("Copied {n} character{}", if n == 1 { "" } else { "s" }));
        } else {
            self.error("Couldn't copy to the clipboard");
        }
    }

    /// The selected columns of a viewer row, end excluded, if any are.
    pub fn selected_cols(&self, row: usize) -> Option<(usize, usize)> {
        let Some(Selection::View { anchor, head }) = self.selection else { return None };
        let (start, end) = if anchor <= head { (anchor, head) } else { (head, anchor) };
        if row < start.0 || row > end.0 {
            return None;
        }
        let from = if row == start.0 { start.1 } else { 0 };
        let to = if row == end.0 { end.1 + 1 } else { usize::MAX };
        Some((from, to))
    }

    /// The viewer's selection as text. Rows wrapped from one line join back with a space.
    fn selected_text(&self) -> String {
        let mut out = String::new();
        let mut prev_block = None;
        for (i, row) in self.view.rows.iter().enumerate() {
            let Some((from, to)) = self.selected_cols(i) else { continue };
            let (text, block) = match row {
                Row::Text { line, block } => (columns(&line.to_string(), from, to), *block),
                _ => (String::new(), row.block()),
            };
            if prev_block.is_some() {
                if block.is_some() && block == prev_block.flatten() {
                    out.truncate(out.trim_end().len());
                    out.push(' ');
                    out.push_str(text.trim_start());
                    continue;
                }
                out.push('\n');
            }
            out.push_str(text.trim_end());
            prev_block = Some(block);
        }
        out
    }

    /// The viewer is showing rows of text, rather than an image or a notice.
    fn shows_text(&self) -> bool {
        let text = matches!(self.page.as_ref().map(|p| &p.kind), Some(PageKind::Markdown(_) | PageKind::Text(_)));
        text && !self.view.rows.is_empty()
    }

    /// The viewer row and column under a cell, clamped to the drawn rows.
    fn view_cell(&self, x: u16, y: u16) -> (usize, usize) {
        let a = self.view.area;
        let below = y.saturating_sub(a.y) as usize;
        let row = (self.view.scroll + below.min(a.height as usize)).min(self.view.rows.len().saturating_sub(1));
        // Above the rows, the drag reaches up past them one row at a time.
        let row = if y < a.y { self.view.scroll.saturating_sub(1) } else { row };
        let col = x.saturating_sub(a.x).min(a.width.saturating_sub(1)) as usize;
        (row, col)
    }

    /// A horizontal swipe: right shows the tree, left hides it.
    pub fn swipe(&mut self, right: bool) {
        if !self.modal_is_none() || right == self.tree_visible() {
            return;
        }
        self.toggle_tree();
        if right {
            self.focus = Focus::Tree;
        }
    }

    /// True while there's text to type: the editor, a prompt or the search box.
    pub fn wants_text(&self) -> bool {
        match self.modal {
            Modal::Prompt { .. } | Modal::Command { .. } | Modal::Search { .. } => true,
            Modal::None => self.editor.is_some() && self.focus == Focus::Page,
            _ => false,
        }
    }

    fn row_has_link(&self, i: usize) -> bool {
        let Some(b) = self.view.rows.get(i).and_then(|r| r.block()) else { return false };
        match &self.page.as_ref().map(|p| &p.kind) {
            Some(PageKind::Markdown(blocks)) => match blocks.get(b) {
                Some(Block::Text { links, .. }) => !links.is_empty(),
                Some(Block::Image { .. }) => true,
                None => false,
            },
            _ => false,
        }
    }
}
