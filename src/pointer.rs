//! Pointer input (touch and mouse) on top of the keyboard actions. `ui::draw` records
//! what it drew where; these methods find the target under the pointer and run the
//! same actions a key would.

use ratatui::layout::{Position, Rect};
use ratatui_textarea::{CursorMove, Input, Key};

use crate::app::{App, Focus, Modal};
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

/// Actions offered by the touch menu, in order, while viewing and while editing.
const MENU_VIEW: &[Action] = &[
    Action::Edit,
    Action::Search,
    Action::Sync,
    Action::NewPage,
    Action::NewFolder,
    Action::Rename,
    Action::Delete,
    Action::InsertImage,
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
        Action::InsertImage => "Insert image",
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
                    let (top, left) = e.ta.scroll_offset();
                    let row = (y - area.y + top) as usize;
                    let col = (x - area.x + left) as usize;
                    let c = e.ta.screen_to_data(row, col);
                    e.ta.move_cursor(CursorMove::Jump(c.0 as u16, c.1 as u16));
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
            Modal::Prompt { .. } | Modal::Search { .. } => true,
            Modal::None => self.editor.is_some() && self.focus == Focus::Page,
            _ => false,
        }
    }

    fn row_has_link(&self, i: usize) -> bool {
        let Some(b) = self.view.rows.get(i).and_then(|r| r.block()) else { return false };
        match &self.page.as_ref().map(|p| &p.kind) {
            Some(crate::app::PageKind::Markdown(blocks)) => match blocks.get(b) {
                Some(Block::Text { links, .. }) => !links.is_empty(),
                Some(Block::Image { .. }) => true,
                None => false,
            },
            _ => false,
        }
    }
}
