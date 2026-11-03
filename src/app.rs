//! Application state and input handling.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config as MatcherConfig, Matcher};
use ratatui::layout::{Rect, Size};
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::ListState;
use ratatui_textarea::{CursorMove, DataCursor, Input, Key, TextArea, WrapMode};
use web_time::Instant;

use crate::config::Config;
use crate::host::{Host, Msg, Open};
use crate::images::Images;
use crate::keymap::{Action, Keymap};
use crate::links;
use crate::markdown::{self, Block};
use crate::paste;
use crate::paths;
use crate::pointer::{Selection, Target};
use crate::repo::{self, INDEX};
use crate::storage::Storage;
use crate::sync::State;
use crate::tree::{self, Filter, Node, Visible};

/// Images are capped at this many rows in the viewer.
const MAX_IMAGE_ROWS: u16 = 20;
/// Below this terminal width the tree hides unless toggled on.
const NARROW_WIDTH: u16 = 70;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Tree,
    Page,
}

pub enum PageKind {
    Markdown(Vec<Block>),
    Text(String),
    Image,
    Binary,
    /// A folder without INDEX.md.
    Folder,
}

pub struct Page {
    pub path: PathBuf,
    pub kind: PageKind,
}

pub struct Editor {
    pub ta: TextArea<'static>,
    pub path: PathBuf,
    eol: &'static str,
    saved: Vec<String>,
    /// Pasted URLs waiting for their page title, with the row and column they start at.
    titles: Vec<(String, usize, usize)>,
    /// The lines with and without the last title swap, so that undo and redo take
    /// its two edits (delete the URL, insert the link) as one.
    swap: Option<(Vec<String>, Vec<String>)>,
}

impl Editor {
    pub fn dirty(&self) -> bool {
        self.ta.lines() != self.saved.as_slice()
    }

    /// The selected text, if any is selected.
    pub fn selected_text(&self) -> Option<String> {
        let ((r0, c0), (r1, c1)) = self.ta.selection_range().filter(|(a, b)| a != b)?;
        let lines = self.ta.lines();
        let chars = |r: usize, from: usize, to: usize| -> String {
            lines[r].chars().skip(from).take(to.saturating_sub(from)).collect()
        };
        Some(if r0 == r1 {
            chars(r0, c0, c1)
        } else {
            let mut out = vec![chars(r0, c0, usize::MAX)];
            out.extend(lines[r0 + 1..r1].iter().cloned());
            out.push(chars(r1, 0, c1));
            out.join("\n")
        })
    }
}

pub enum Row {
    Text { line: Line<'static>, block: Option<usize> },
    Image { block: usize, part: u16, size: Size },
    ImagePlaceholder { block: usize },
}

impl Row {
    pub fn block(&self) -> Option<usize> {
        match self {
            Row::Text { block, .. } => *block,
            Row::Image { block, .. } | Row::ImagePlaceholder { block } => Some(*block),
        }
    }
}

#[derive(Default)]
pub struct View {
    pub scroll: usize,
    pub cursor: usize,
    pub rows: Vec<Row>,
    pub height: u16,
    /// Where the rows were last drawn.
    pub area: Rect,
}

pub enum PromptKind {
    NewPage(PathBuf),
    NewFolder(PathBuf),
    Rename(PathBuf),
    InsertImage(PathBuf),
}

pub enum After {
    ExitEdit,
    Quit,
    Open(PathBuf),
}

pub enum Modal {
    None,
    Prompt {
        title: String,
        input: String,
        /// Byte offset of the cursor in `input`.
        cursor: usize,
        kind: PromptKind,
    },
    ConfirmDelete {
        path: PathBuf,
        message: String,
    },
    SaveChanges(After),
    Search {
        query: String,
        items: Vec<(String, PathBuf)>,
        results: Vec<usize>,
        sel: usize,
    },
    Help,
    /// A shell command being typed, in the status bar.
    Command {
        input: String,
        /// Byte offset of the cursor in `input`.
        cursor: usize,
    },
    /// What a shell command printed.
    Output {
        title: String,
        lines: Vec<String>,
        scroll: usize,
    },
    /// Actions to pick by touch (see `pointer`).
    Menu {
        items: Vec<Action>,
        sel: usize,
    },
}

pub struct App {
    pub fs: Box<dyn Storage>,
    pub host: Box<dyn Host>,
    pub cfg: Config,
    pub keys: Keymap,
    pub tree: Node,
    pub expanded: HashSet<PathBuf>,
    pub visible: Vec<Visible>,
    pub sel: usize,
    pub tree_state: ListState,
    pub show_all: bool,
    pub focus: Focus,
    pub page: Option<Page>,
    pub editor: Option<Editor>,
    pub view: View,
    pub modal: Modal,
    pub git: Option<State>,
    pub busy: bool,
    /// A shell command is running.
    pub running: bool,
    pub message: Option<(String, bool, Instant)>,
    pub images: Images,
    /// Page waiting for an image from the host's file picker.
    pending_image: Option<PathBuf>,
    tree_wide: bool,
    tree_narrow: bool,
    pub width: u16,
    pub quit: bool,
    /// What the last frame drew where, for pointer input.
    pub hits: Vec<(Rect, Target)>,
    /// Text being selected with the mouse, or selected and copied.
    pub selection: Option<Selection>,
    /// A mouse button is down and has moved; `selection` is the one it's making.
    pub(crate) dragging: bool,
    /// A touch screen: the status bar offers the menu instead of help.
    pub touch: bool,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
}

const GLOBAL: &[Action] =
    &[Action::Quit, Action::Help, Action::Search, Action::Shell, Action::Sync, Action::ToggleTree, Action::SwitchFocus];
const TREE: &[Action] = &[
    Action::Up,
    Action::Down,
    Action::Open,
    Action::Collapse,
    Action::NewPage,
    Action::NewFolder,
    Action::Rename,
    Action::Delete,
    Action::InsertImage,
    Action::ToggleAllFiles,
    Action::PageUp,
    Action::PageDown,
    Action::Edit,
];
const VIEW: &[Action] = &[
    Action::Up,
    Action::Down,
    Action::PageUp,
    Action::PageDown,
    Action::Follow,
    Action::Edit,
    Action::OpenExternal,
    Action::Collapse,
];
const EDIT: &[Action] = &[
    Action::Save,
    Action::SelectAll,
    Action::Copy,
    Action::Cut,
    Action::Undo,
    Action::Redo,
    Action::PasteImage,
    Action::ExitEdit,
    Action::Sync,
    Action::ToggleTree,
    Action::SwitchFocus,
];

impl App {
    /// A gition repo on disk, synced with the git CLI.
    #[cfg(feature = "native")]
    pub fn open_dir(root: PathBuf, cfg: Config, picker: Option<ratatui_image::picker::Picker>) -> App {
        use crate::host::GitHost;
        use crate::storage::FsStorage;
        let images = Images::new(root.clone(), picker);
        App::new(Box::new(FsStorage::new(root.clone())), Box::new(GitHost { root }), cfg, images)
    }

    pub fn new(fs: Box<dyn Storage>, host: Box<dyn Host>, cfg: Config, images: Images) -> App {
        let (keys, warnings) = Keymap::from_config(&cfg);
        let (tx, rx) = channel();
        let mut app = App {
            show_all: cfg.show_all_files,
            fs,
            host,
            cfg,
            keys,
            tree: Node { path: PathBuf::new(), name: String::new(), is_dir: true, children: Vec::new() },
            expanded: HashSet::new(),
            visible: Vec::new(),
            sel: 0,
            tree_state: ListState::default(),
            focus: Focus::Tree,
            page: None,
            editor: None,
            view: View::default(),
            modal: Modal::None,
            git: None,
            busy: false,
            running: false,
            message: None,
            images,
            pending_image: None,
            tree_wide: true,
            tree_narrow: false,
            width: 80,
            quit: false,
            hits: Vec::new(),
            selection: None,
            dragging: false,
            touch: false,
            tx,
            rx,
        };
        app.reload_tree();
        app.load_page(PathBuf::from(INDEX));
        if !warnings.is_empty() {
            app.error(format!("config: {}", warnings.join("; ")));
        }
        app
    }

    // ---- background sync -----------------------------------------------

    pub fn start_fetch(&mut self) {
        self.busy = true;
        self.host.fetch(self.tx.clone());
    }

    fn sync(&mut self) {
        if self.busy {
            return self.info("Sync already running…");
        }
        if self.editor.as_ref().is_some_and(Editor::dirty)
            && let Err(e) = self.save()
        {
            return self.error(e.to_string());
        }
        self.busy = true;
        self.host.sync(&self.cfg.commit_prefix, self.tx.clone());
    }

    /// Applies results from background work. Call once per loop iteration.
    pub fn tick(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Fetched(state, pulled) => {
                    self.busy = false;
                    self.git = Some(state);
                    if pulled > 0 {
                        self.refresh_after_pull();
                        self.info(format!("Pulled {pulled} commit(s)"));
                    }
                }
                Msg::Synced(res, state) => {
                    self.busy = false;
                    self.git = Some(state);
                    self.refresh_after_pull();
                    match res {
                        Ok(m) => self.info(m),
                        Err(m) => self.error(m),
                    }
                }
                Msg::Title(url, title) => self.title_arrived(&url, title),
                Msg::Ran(cmd, res, state) => {
                    self.running = false;
                    self.git = Some(state);
                    self.refresh_after_pull();
                    self.command_done(cmd, res);
                }
            }
        }
        if self.message.as_ref().is_some_and(|(_, _, t)| t.elapsed() > Duration::from_secs(5)) {
            self.message = None;
        }
    }

    fn refresh_after_pull(&mut self) {
        self.reload_tree();
        if self.editor.is_none() {
            self.reload_page();
        }
    }

    /// Recounts local changes after a file operation.
    fn refresh_changes(&mut self) {
        if let Some(st) = &mut self.git {
            st.changes = self.host.changes();
        }
    }

    pub fn info(&mut self, m: impl Into<String>) {
        self.message = Some((m.into(), false, Instant::now()));
    }

    pub fn error(&mut self, m: impl Into<String>) {
        self.message = Some((m.into(), true, Instant::now()));
    }

    // ---- tree ----------------------------------------------------------

    pub fn reload_tree(&mut self) {
        let selected = self.visible.get(self.sel).map(|v| v.path.clone());
        self.tree = tree::build(&*self.fs, &Filter { show_all: self.show_all, assets_dir: &self.cfg.assets_dir });
        self.visible = tree::flatten(&self.tree, &self.expanded);
        match selected.and_then(|p| self.visible.iter().position(|v| v.path == p)) {
            Some(i) => self.sel = i,
            None => self.sel = self.sel.min(self.visible.len().saturating_sub(1)),
        }
    }

    fn refilter(&mut self) {
        self.visible = tree::flatten(&self.tree, &self.expanded);
        self.sel = self.sel.min(self.visible.len().saturating_sub(1));
    }

    /// Expands ancestors of `path` and selects its tree node.
    fn reveal(&mut self, path: &Path) {
        let node = if paths::is_index(path) { paths::parent(path) } else { path.to_path_buf() };
        for a in node.ancestors().skip(1) {
            if !a.as_os_str().is_empty() {
                self.expanded.insert(a.to_path_buf());
            }
        }
        self.refilter();
        if let Some(i) = self.visible.iter().position(|v| v.path == node) {
            self.sel = i;
        }
    }

    pub fn tree_visible(&self) -> bool {
        if self.narrow() { self.tree_narrow } else { self.tree_wide }
    }

    pub(crate) fn narrow(&self) -> bool {
        self.width < NARROW_WIDTH
    }

    pub(crate) fn toggle_tree(&mut self) {
        if self.narrow() {
            self.tree_narrow = !self.tree_narrow;
        } else {
            self.tree_wide = !self.tree_wide;
        }
        if !self.tree_visible() {
            self.focus = Focus::Page;
        }
    }

    fn selected(&self) -> Option<&Visible> {
        self.visible.get(self.sel)
    }

    /// Directory new pages are created in, based on the selected node.
    fn target_dir(&self) -> PathBuf {
        match self.selected() {
            Some(v) if v.is_dir => v.path.clone(),
            Some(v) => paths::parent(&v.path),
            None => PathBuf::new(),
        }
    }

    /// The markdown page images are inserted into.
    fn current_markdown(&self) -> Option<PathBuf> {
        if let Some(e) = &self.editor {
            return Some(e.path.clone());
        }
        self.page.as_ref().filter(|p| matches!(p.kind, PageKind::Markdown(_))).map(|p| p.path.clone())
    }

    // ---- pages ---------------------------------------------------------

    /// Opens `path` (a file or a folder), prompting to save a dirty editor first.
    pub fn request_open(&mut self, path: PathBuf) {
        let target = self.resolve_folder(&path);
        if let Some(e) = &self.editor {
            if e.path == target {
                return;
            }
            if e.dirty() {
                self.modal = Modal::SaveChanges(After::Open(path));
                return;
            }
            self.editor = None;
        }
        self.load_page(path);
    }

    fn resolve_folder(&self, path: &Path) -> PathBuf {
        if self.fs.is_dir(path) && repo::dir_has_index(&*self.fs, path) { path.join(INDEX) } else { path.to_path_buf() }
    }

    pub fn load_page(&mut self, path: PathBuf) {
        self.selection = None;
        let path = self.resolve_folder(&path);
        let kind = if self.fs.is_dir(&path) {
            PageKind::Folder
        } else if paths::is_image(&path) {
            self.images.forget(&path);
            PageKind::Image
        } else {
            match self.fs.read(&path) {
                Ok(bytes) if !bytes.contains(&0) => match String::from_utf8(bytes) {
                    Ok(s) if paths::is_markdown(&path) => PageKind::Markdown(markdown::render(&s, &self.cfg.theme)),
                    Ok(s) => PageKind::Text(s),
                    Err(_) => PageKind::Binary,
                },
                Ok(_) => PageKind::Binary,
                Err(e) => {
                    self.error(format!("{}: {e}", paths::to_slash(&path)));
                    return;
                }
            }
        };
        let same = self.page.as_ref().is_some_and(|p| p.path == path);
        if !same {
            self.view.scroll = 0;
            self.view.cursor = 0;
        }
        self.page = Some(Page { path, kind });
    }

    fn reload_page(&mut self) {
        if let Some(p) = &self.page {
            let path = p.path.clone();
            if self.fs.exists(&path) {
                self.load_page(path);
            } else {
                self.page = None;
            }
        }
    }

    fn start_edit(&mut self) {
        let Some(page) = &self.page else { return };
        if !matches!(page.kind, PageKind::Markdown(_) | PageKind::Text(_)) {
            return self.error("This file can't be edited here");
        }
        let path = page.path.clone();
        let content = match self.fs.read_to_string(&path) {
            Ok(c) => c,
            Err(e) => return self.error(e.to_string()),
        };
        let eol = if content.contains("\r\n") { "\r\n" } else { "\n" };
        let lines: Vec<String> = content.replace("\r\n", "\n").split('\n').map(str::to_string).collect();
        let mut ta = TextArea::new(lines.clone());
        ta.set_wrap_mode(WrapMode::WordOrGlyph);
        ta.set_cursor_line_style(Style::default());
        ta.set_selection_style(self.cfg.theme.selected());
        self.editor = Some(Editor { ta, path, eol, saved: lines, titles: Vec::new(), swap: None });
        self.focus = Focus::Page;
    }

    fn save(&mut self) -> Result<()> {
        let Some(e) = &mut self.editor else { return Ok(()) };
        let lines = e.ta.lines().to_vec();
        self.fs.write(&e.path, lines.join(e.eol).as_bytes())?;
        e.saved = lines;
        self.reload_page();
        self.refresh_changes();
        Ok(())
    }

    fn after_save_prompt(&mut self, after: After) {
        match after {
            After::ExitEdit => {
                self.editor = None;
                self.reload_page();
            }
            After::Quit => self.quit = true,
            After::Open(p) => {
                self.editor = None;
                self.load_page(p);
            }
        }
    }

    // ---- view ----------------------------------------------------------

    /// Lays out the viewer rows for a pane of the given size.
    pub fn layout(&mut self, width: u16, height: u16) {
        let App { view, page, images, .. } = self;
        view.height = height;
        view.rows.clear();
        let Some(page) = page else { return };
        match &page.kind {
            PageKind::Markdown(blocks) => {
                for (i, b) in blocks.iter().enumerate() {
                    match b {
                        Block::Text { line, hang, .. } => {
                            for l in markdown::wrap(line, *hang, width) {
                                view.rows.push(Row::Text { line: l, block: Some(i) });
                            }
                        }
                        Block::Image { dest, .. } => {
                            // Decode lazily: only once the image is near the visible window.
                            let near = view.rows.len() < view.scroll + height as usize + MAX_IMAGE_ROWS as usize;
                            let fit = links::resolve(&page.path, dest)
                                .filter(|_| near && images.enabled())
                                .and_then(|(t, _)| images.fit(&t, Size::new(width, MAX_IMAGE_ROWS.min(height))));
                            match fit {
                                Some(size) => {
                                    for part in 0..size.height {
                                        view.rows.push(Row::Image { block: i, part, size });
                                    }
                                }
                                None => view.rows.push(Row::ImagePlaceholder { block: i }),
                            }
                        }
                    }
                }
            }
            PageKind::Text(s) => {
                for l in s.lines() {
                    for w in markdown::wrap(&Line::raw(l.to_string()), 0, width) {
                        view.rows.push(Row::Text { line: w, block: None });
                    }
                }
            }
            _ => {}
        }
        let h = height.max(1) as usize;
        view.cursor = view.cursor.min(view.rows.len().saturating_sub(1));
        view.scroll = view.scroll.min(view.rows.len().saturating_sub(1));
        if view.cursor < view.scroll {
            view.scroll = view.cursor;
        } else if view.cursor >= view.scroll + h {
            view.scroll = view.cursor + 1 - h;
        }
    }

    fn move_cursor(&mut self, delta: isize) {
        let max = self.view.rows.len().saturating_sub(1) as isize;
        self.view.cursor = (self.view.cursor as isize + delta).clamp(0, max.max(0)) as usize;
    }

    fn block_at_cursor(&self) -> Option<&Block> {
        let i = self.view.rows.get(self.view.cursor)?.block()?;
        match &self.page.as_ref()?.kind {
            PageKind::Markdown(blocks) => blocks.get(i),
            _ => None,
        }
    }

    fn follow(&mut self) {
        let dest = match self.block_at_cursor() {
            Some(Block::Text { links, .. }) => links.first().cloned(),
            Some(Block::Image { dest, .. }) => Some(dest.clone()),
            None => None,
        };
        let Some(dest) = dest else { return };
        let Some(page) = self.page.as_ref().map(|p| p.path.clone()) else { return };
        if !links::is_local(&dest) {
            if dest.contains("://") || dest.starts_with("mailto:") {
                self.open_external(Open::Url(&dest));
            }
            return;
        }
        let Some((target, _)) = links::resolve(&page, &dest) else {
            return self.error(format!("Link points outside the repo: {dest}"));
        };
        if !self.fs.exists(&target) {
            return self.error(format!("Not found: {}", paths::to_slash(&target)));
        }
        if self.fs.is_dir(&target) || paths::is_markdown(&target) {
            self.reveal(&self.resolve_folder(&target));
            self.request_open(target);
        } else {
            self.open_external(Open::File(&target));
        }
    }

    fn open_external(&mut self, target: Open) {
        if let Err(e) = self.host.open(target) {
            self.error(format!("Could not open: {e}"));
        }
    }

    // ---- input ---------------------------------------------------------

    fn action(&self, k: &Input, candidates: &[Action]) -> Option<Action> {
        candidates.iter().copied().find(|&a| self.keys.is(a, k))
    }

    pub fn on_key(&mut self, k: Input) {
        if k.key == Key::Null {
            return;
        }
        self.selection = None;
        if !matches!(self.modal, Modal::None) {
            return self.modal_key(k);
        }
        if self.focus == Focus::Page && self.editor.is_some() {
            return self.edit_key(k);
        }
        if let Some(a) = self.action(&k, GLOBAL) {
            return self.global(a);
        }
        match self.focus {
            Focus::Tree => {
                if let Some(a) = self.action(&k, TREE) {
                    self.tree_action(a);
                }
            }
            Focus::Page => {
                if let Some(a) = self.action(&k, VIEW) {
                    self.view_action(a);
                }
            }
        }
    }

    pub fn on_paste(&mut self, s: String) {
        let s = s.replace("\r\n", "\n");
        match &mut self.modal {
            Modal::Prompt { input, cursor, .. } | Modal::Command { input, cursor } => {
                let line = s.lines().next().unwrap_or("");
                input.insert_str(*cursor, line);
                *cursor += line.len();
            }
            Modal::Search { query, .. } => {
                query.push_str(s.lines().next().unwrap_or(""));
                self.search_update();
            }
            // Some terminals paste nothing when the clipboard only holds an image.
            Modal::None if self.focus == Focus::Page && self.editor.is_some() && s.is_empty() => {
                if let Some(png) = self.host.paste_image() {
                    self.paste_images(vec![("png", png)]);
                }
            }
            Modal::None if self.focus == Focus::Page && self.editor.is_some() => {
                if let Err(e) = self.paste(s) {
                    self.error(e.to_string());
                }
            }
            _ => {}
        }
    }

    /// Pastes into the editor: an image file's path imports the image, a bare URL
    /// becomes a link once its page title arrives, anything else is typed as is.
    fn paste(&mut self, s: String) -> Result<()> {
        let Some(e) = &mut self.editor else { return Ok(()) };
        if let Some(src) = paste::image_path(&s).filter(|p| p.is_file()) {
            let name = src.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let page = e.path.clone();
            return self.insert_image_bytes(&page, &name, &std::fs::read(&src)?);
        }
        let DataCursor(row, col) = e.ta.cursor();
        let before: String = e.ta.lines()[row].chars().take(col).collect();
        match paste::bare_url(&s).filter(|_| self.cfg.link_titles && !paste::in_link(&before)) {
            Some(url) => {
                e.ta.insert_str(url);
                e.titles.push((url.to_string(), row, col));
                self.host.fetch_title(url, self.tx.clone());
            }
            None => {
                e.ta.insert_str(s);
            }
        }
        Ok(())
    }

    /// Swaps a pasted URL for `[title](url)` if it's still where it was pasted.
    fn title_arrived(&mut self, url: &str, title: Option<String>) {
        let Some(e) = &mut self.editor else { return };
        let Some(i) = e.titles.iter().position(|(u, ..)| u == url) else { return };
        let (_, row, col) = e.titles.remove(i);
        let Some(title) = title else { return };
        let len = url.chars().count();
        let still = e.ta.lines().get(row).is_some_and(|l| l.chars().skip(col).take(len).eq(url.chars()));
        if !still {
            return;
        }
        let bare = e.ta.lines().to_vec();
        let link = paste::link(&title, url);
        let DataCursor(crow, ccol) = e.ta.cursor();
        let at = |c: usize| CursorMove::Jump(row as u16, c as u16);
        e.ta.cancel_selection();
        e.ta.move_cursor(at(col));
        e.ta.start_selection();
        e.ta.move_cursor(at(col + len));
        e.ta.insert_str(&link);
        let ccol = if crow == row && ccol >= col + len { ccol + link.chars().count() - len } else { ccol };
        e.ta.move_cursor(CursorMove::Jump(crow as u16, ccol as u16));
        // Titles for other URLs pasted later on this line moved too.
        for (_, r, c) in &mut e.titles {
            if *r == row && *c > col {
                *c = *c + link.chars().count() - len;
            }
        }
        e.swap = Some((e.ta.lines().to_vec(), bare));
    }

    /// Pastes images from the clipboard into the editor, each on its own line.
    pub fn paste_images(&mut self, images: Vec<(&str, Vec<u8>)>) {
        let Some(page) = self.editor.as_ref().map(|e| e.path.clone()) else { return };
        let secs = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        for (i, (ext, bytes)) in images.iter().enumerate() {
            if i > 0
                && let Some(e) = &mut self.editor
            {
                e.ta.insert_newline();
            }
            if let Err(e) = self.insert_image_bytes(&page, &paste::image_name(ext, secs), bytes) {
                return self.error(e.to_string());
            }
        }
    }

    pub(crate) fn global(&mut self, a: Action) {
        match a {
            Action::Quit => {
                if self.editor.as_ref().is_some_and(Editor::dirty) {
                    self.modal = Modal::SaveChanges(After::Quit);
                } else {
                    self.quit = true;
                }
            }
            Action::Help => self.modal = Modal::Help,
            Action::Search => self.open_search(),
            Action::Shell => self.modal = Modal::Command { input: String::new(), cursor: 0 },
            Action::Sync => self.sync(),
            Action::ToggleTree => self.toggle_tree(),
            Action::SwitchFocus => {
                self.focus = match self.focus {
                    Focus::Tree => Focus::Page,
                    Focus::Page => {
                        if !self.tree_visible() {
                            self.toggle_tree();
                        }
                        Focus::Tree
                    }
                };
            }
            _ => {}
        }
    }

    fn edit_key(&mut self, k: Input) {
        // Plain quit keys type text here; only the ones with a modifier quit.
        let quit = (k.ctrl || k.alt) && self.keys.is(Action::Quit, &k);
        // Copy shares Ctrl+C with quit: it copies a selection, and quits without one.
        let selected = self.editor.as_ref().is_some_and(|e| e.selected_text().is_some());
        let action = self.action(&k, EDIT).filter(|&a| a != Action::Copy || selected);
        match action.or(quit.then_some(Action::Quit)) {
            Some(a) => self.edit_action(a),
            None => {
                // Alt+←/→ (Option on macOS) moves by word, like Ctrl+←/→.
                let k = match k {
                    Input { key: Key::Left | Key::Right, ctrl: false, alt: true, shift } => {
                        Input { key: k.key, ctrl: true, alt: false, shift }
                    }
                    k => k,
                };
                let Some(e) = &mut self.editor else { return };
                // ←/→ on a selection lands on its start or end, like other editors.
                if let Input { key: key @ (Key::Left | Key::Right), ctrl: false, alt: false, shift: false } = k
                    && let Some((start, end)) = e.ta.selection_range().filter(|(a, b)| a != b)
                {
                    let (row, col) = if key == Key::Left { start } else { end };
                    e.ta.cancel_selection();
                    e.ta.move_cursor(CursorMove::Jump(row as u16, col as u16));
                    return;
                }
                e.ta.input(k);
            }
        }
    }

    pub(crate) fn edit_action(&mut self, a: Action) {
        match a {
            Action::Save => match self.save() {
                Ok(()) => self.info("Saved"),
                Err(e) => self.error(format!("Save failed: {e}")),
            },
            Action::SelectAll => {
                if let Some(e) = &mut self.editor {
                    e.ta.select_all();
                }
            }
            Action::Copy => {
                let Some(text) = self.editor.as_ref().and_then(Editor::selected_text) else { return };
                self.copy(&text);
            }
            Action::Cut => {
                let Some(e) = &mut self.editor else { return };
                let text = if e.ta.cut() { e.ta.yank_text() } else { cut_line(&mut e.ta) };
                self.host.copy(&text);
            }
            Action::Undo => {
                if let Some(e) = &mut self.editor {
                    let swapped = e.swap.as_ref().is_some_and(|(linked, _)| e.ta.lines() == linked.as_slice());
                    e.ta.undo();
                    if swapped {
                        e.ta.undo();
                    }
                }
            }
            Action::Redo => {
                if let Some(e) = &mut self.editor {
                    let unswapped = e.swap.as_ref().is_some_and(|(_, bare)| e.ta.lines() == bare.as_slice());
                    if e.ta.redo() && unswapped {
                        e.ta.redo();
                    }
                }
            }
            Action::PasteImage => match self.host.paste_image() {
                Some(png) => self.paste_images(vec![("png", png)]),
                None => self.info("No image on the clipboard"),
            },
            Action::ExitEdit => {
                if self.editor.as_ref().is_some_and(Editor::dirty) {
                    self.modal = Modal::SaveChanges(After::ExitEdit);
                } else {
                    self.after_save_prompt(After::ExitEdit);
                }
            }
            a => self.global(a),
        }
    }

    pub(crate) fn tree_action(&mut self, a: Action) {
        let Some(node) = self.selected().cloned() else { return };
        match a {
            Action::Up => self.sel = self.sel.saturating_sub(1),
            Action::Down => self.sel = (self.sel + 1).min(self.visible.len().saturating_sub(1)),
            Action::PageUp => self.sel = self.sel.saturating_sub(10),
            Action::PageDown => self.sel = (self.sel + 10).min(self.visible.len().saturating_sub(1)),
            Action::Open => {
                // A closed folder opens first; once open, the next press goes into its page like a file.
                if node.is_dir && !node.expanded {
                    self.expanded.insert(node.path.clone());
                    self.refilter();
                    self.request_open(node.path);
                } else {
                    self.request_open(node.path);
                    if !self.page.as_ref().is_some_and(|p| matches!(p.kind, PageKind::Folder)) {
                        self.focus = Focus::Page;
                    }
                }
            }
            Action::Collapse => {
                if node.is_dir && node.expanded && !node.path.as_os_str().is_empty() {
                    self.expanded.remove(&node.path);
                    self.refilter();
                } else if let Some(i) = self.visible.iter().position(|v| v.path == paths::parent(&node.path)) {
                    self.sel = i;
                }
            }
            Action::Edit => {
                if node.is_dir && !node.expanded {
                    self.expanded.insert(node.path.clone());
                    self.refilter();
                }
                self.request_open(node.path);
                if !self.modal_is_none() || self.page.as_ref().is_some_and(|p| matches!(p.kind, PageKind::Folder)) {
                    return;
                }
                // Editing this page already: go back to it rather than reload it from disk.
                if self.editor.is_some() {
                    self.focus = Focus::Page;
                } else {
                    self.start_edit();
                }
            }
            Action::NewPage => self.prompt("New page name", String::new(), PromptKind::NewPage(self.target_dir())),
            Action::NewFolder => {
                self.prompt("New folder name", String::new(), PromptKind::NewFolder(self.target_dir()))
            }
            Action::Rename => {
                if node.path.as_os_str().is_empty() {
                    return self.error("The root can't be renamed");
                }
                if self.editor.as_ref().is_some_and(Editor::dirty) {
                    return self.error("Save or close the editor first");
                }
                let mut current = paths::to_slash(&node.path);
                if !node.is_dir && !self.show_all && paths::is_markdown(&node.path) {
                    current.truncate(current.len() - 3);
                }
                self.prompt("Rename / move to (path from repo root)", current, PromptKind::Rename(node.path));
            }
            Action::Delete => {
                if node.path.as_os_str().is_empty() {
                    return self.error("The root can't be deleted");
                }
                let n = links::count_incoming(&*self.fs, &node.path, &tree::markdown_files(&*self.fs));
                let what = if node.is_dir { "folder and everything in it" } else { "page" };
                let refs = match n {
                    0 => String::new(),
                    1 => " 1 page links here.".into(),
                    n => format!(" {n} pages link here."),
                };
                let message = format!("Delete {what} '{}'?{refs}", paths::to_slash(&node.path));
                self.modal = Modal::ConfirmDelete { path: node.path, message };
            }
            Action::InsertImage => match self.current_markdown() {
                Some(page) if self.host.pick_image() => self.pending_image = Some(page),
                Some(page) => self.prompt("Image file to insert", String::new(), PromptKind::InsertImage(page)),
                None => self.error("Open a markdown page first"),
            },
            Action::ToggleAllFiles => {
                self.show_all = !self.show_all;
                self.reload_tree();
                self.info(if self.show_all { "Showing all files" } else { "Showing pages only" });
            }
            _ => {}
        }
    }

    pub(crate) fn view_action(&mut self, a: Action) {
        let page = self.view.height.max(2) as isize - 1;
        match a {
            Action::Up => self.move_cursor(-1),
            Action::Down => self.move_cursor(1),
            Action::PageUp => self.move_cursor(-page),
            Action::PageDown => self.move_cursor(page),
            Action::Edit => self.start_edit(),
            Action::Follow => self.follow(),
            Action::Collapse => self.focus = Focus::Tree,
            Action::OpenExternal => {
                let image = match self.block_at_cursor() {
                    Some(Block::Image { dest, .. }) => Some(dest.clone()),
                    _ => None,
                };
                let Some(page) = self.page.as_ref().map(|p| p.path.clone()) else { return };
                match image {
                    Some(dest) if !links::is_local(&dest) => self.open_external(Open::Url(&dest)),
                    Some(dest) => {
                        if let Some((t, _)) = links::resolve(&page, &dest) {
                            self.open_external(Open::File(&t));
                        }
                    }
                    None => self.open_external(Open::File(&page)),
                }
            }
            _ => {}
        }
    }

    pub(crate) fn modal_is_none(&self) -> bool {
        matches!(self.modal, Modal::None)
    }

    fn prompt(&mut self, title: &str, input: String, kind: PromptKind) {
        self.modal = Modal::Prompt { title: title.into(), cursor: input.len(), input, kind };
    }

    fn modal_key(&mut self, k: Input) {
        let ctrl = k.ctrl;
        match std::mem::replace(&mut self.modal, Modal::None) {
            Modal::Help => {}
            Modal::Menu { items, mut sel } => match k.key {
                Key::Enter => {
                    if let Some(&a) = items.get(sel) {
                        self.run_action(a);
                    }
                }
                Key::Esc => {}
                Key::Up | Key::Char('k') => self.modal = Modal::Menu { sel: sel.saturating_sub(1), items },
                Key::Down | Key::Char('j') => {
                    sel = (sel + 1).min(items.len().saturating_sub(1));
                    self.modal = Modal::Menu { items, sel };
                }
                _ => self.modal = Modal::Menu { items, sel },
            },
            Modal::Prompt { title, mut input, mut cursor, kind } => match k.key {
                Key::Esc => {}
                Key::Enter => {
                    if let Err(e) = self.submit(kind, input.trim()) {
                        self.error(e.to_string());
                    }
                }
                _ => {
                    edit_line(&mut input, &mut cursor, &k);
                    self.modal = Modal::Prompt { title, input, cursor, kind };
                }
            },
            Modal::Command { mut input, mut cursor } => match k.key {
                Key::Esc => {}
                Key::Enter => self.run_command(input.trim()),
                _ => {
                    edit_line(&mut input, &mut cursor, &k);
                    self.modal = Modal::Command { input, cursor };
                }
            },
            Modal::Output { title, lines, mut scroll } => {
                let max = lines.len().saturating_sub(1);
                match k.key {
                    Key::Up | Key::Char('k') => scroll = scroll.saturating_sub(1),
                    Key::Down | Key::Char('j') => scroll = (scroll + 1).min(max),
                    Key::PageUp => scroll = scroll.saturating_sub(10),
                    Key::PageDown => scroll = (scroll + 10).min(max),
                    _ => return,
                }
                self.modal = Modal::Output { title, lines, scroll };
            }
            Modal::ConfirmDelete { path, message } => match k.key {
                Key::Char('y') | Key::Char('Y') | Key::Enter => {
                    if let Err(e) = self.delete(&path) {
                        self.error(e.to_string());
                    }
                }
                Key::Char(_) | Key::Esc => {}
                _ => self.modal = Modal::ConfirmDelete { path, message },
            },
            Modal::SaveChanges(after) => match k.key {
                Key::Char('y') | Key::Char('Y') => match self.save() {
                    Ok(()) => self.after_save_prompt(after),
                    Err(e) => self.error(format!("Save failed: {e}")),
                },
                Key::Char('n') | Key::Char('N') => self.after_save_prompt(after),
                Key::Esc | Key::Char('c') | Key::Char('C') => {}
                _ => self.modal = Modal::SaveChanges(after),
            },
            Modal::Search { mut query, items, results, mut sel } => {
                match k.key {
                    Key::Esc => return,
                    Key::Enter => {
                        if let Some(&i) = results.get(sel) {
                            let path = items[i].1.clone();
                            self.reveal(&path);
                            self.request_open(path);
                            if self.editor.is_none() {
                                self.focus = Focus::Page;
                            }
                        }
                        return;
                    }
                    Key::Up => sel = sel.saturating_sub(1),
                    Key::Char('p') | Key::Char('k') if ctrl => sel = sel.saturating_sub(1),
                    Key::Down => sel = (sel + 1).min(results.len().saturating_sub(1)),
                    Key::Char('n') | Key::Char('j') if ctrl => sel = (sel + 1).min(results.len().saturating_sub(1)),
                    Key::Backspace => {
                        query.pop();
                    }
                    Key::Char(c) if !ctrl => query.push(c),
                    _ => {}
                }
                self.modal = Modal::Search { query, items, results, sel };
                if matches!(k.key, Key::Backspace | Key::Char(_)) && !ctrl {
                    self.search_update();
                }
            }
            Modal::None => {}
        }
    }

    // ---- shell ---------------------------------------------------------

    fn run_command(&mut self, cmd: &str) {
        if cmd.is_empty() {
            return;
        }
        if self.running {
            return self.error("A command is still running");
        }
        // The command sees the files as they are on screen.
        if self.editor.as_ref().is_some_and(Editor::dirty)
            && let Err(e) = self.save()
        {
            return self.error(e.to_string());
        }
        if !self.host.run(cmd, self.tx.clone()) {
            return self.error("Shell commands need the terminal app");
        }
        self.running = true;
    }

    /// Shows what a command printed: in the status bar if it fits on a line, else in a popup.
    fn command_done(&mut self, cmd: String, res: Result<String, String>) {
        let (text, ok) = match res {
            Ok(t) => (t, true),
            Err(t) => (t, false),
        };
        let lines: Vec<String> = text.lines().map(|l| l.replace('\t', "    ")).collect();
        match lines.len() {
            0 => self.info(format!("✓ {cmd}")),
            1 if ok => self.info(lines[0].clone()),
            1 => self.error(lines[0].clone()),
            _ => {
                let title = if ok { format!("$ {cmd}") } else { format!("$ {cmd} (failed)") };
                self.modal = Modal::Output { title, lines, scroll: 0 };
            }
        }
    }

    // ---- search --------------------------------------------------------

    fn open_search(&mut self) {
        let items: Vec<(String, PathBuf)> = tree::markdown_files(&*self.fs)
            .into_iter()
            .map(|p| {
                let label = if paths::is_index(&p) {
                    format!("{}/", paths::to_slash(&paths::parent(&p)))
                } else {
                    let s = paths::to_slash(&p);
                    s[..s.len() - 3].to_string()
                };
                (label, p)
            })
            .collect();
        self.modal = Modal::Search { query: String::new(), results: (0..items.len()).collect(), items, sel: 0 };
    }

    fn search_update(&mut self) {
        let Modal::Search { query, items, results, sel } = &mut self.modal else { return };
        let mut matcher = Matcher::new(MatcherConfig::DEFAULT.match_paths());
        let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
        let labels: Vec<(usize, &str)> = items.iter().enumerate().map(|(i, (l, _))| (i, l.as_str())).collect();
        let matched = pattern.match_list(labels.iter().map(|(i, l)| Labeled(*i, l)), &mut matcher);
        *results = matched.into_iter().map(|(l, _)| l.0).collect();
        *sel = 0;
    }

    // ---- file operations -----------------------------------------------

    fn submit(&mut self, kind: PromptKind, input: &str) -> Result<()> {
        if input.is_empty() {
            return Ok(());
        }
        match kind {
            PromptKind::NewPage(dir) => {
                let mut rel = dir.join(clean_rel(input)?);
                if !paths::is_markdown(&rel) {
                    rel = PathBuf::from(format!("{}.md", rel.to_string_lossy()));
                }
                let title = rel.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                self.create_and_edit(rel, &title)
            }
            PromptKind::NewFolder(dir) => {
                let folder = dir.join(clean_rel(input)?);
                if self.fs.exists(&folder) {
                    bail!("'{}' already exists", paths::to_slash(&folder));
                }
                let title = folder.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                self.expanded.insert(folder.clone());
                self.create_and_edit(folder.join(INDEX), &title)
            }
            PromptKind::Rename(old) => {
                let mut new = clean_rel(input)?;
                // Like `mv`: naming an existing folder moves into it.
                let same = paths::to_slash(&new).to_lowercase() == paths::to_slash(&old).to_lowercase();
                if self.fs.is_dir(&new) && !same {
                    new = new.join(old.file_name().unwrap_or_default());
                } else if !self.fs.is_dir(&old) && paths::is_markdown(&old) && !paths::is_markdown(&new) {
                    new = PathBuf::from(format!("{}.md", new.to_string_lossy()));
                }
                self.move_path(&old, &new)
            }
            PromptKind::InsertImage(page) => self.insert_image(&page, input),
        }
    }

    fn create_and_edit(&mut self, rel: PathBuf, title: &str) -> Result<()> {
        if self.fs.exists(&rel) {
            bail!("'{}' already exists", paths::to_slash(&rel));
        }
        self.fs.write(&rel, format!("# {title}\n").as_bytes())?;
        self.reload_tree();
        self.reveal(&rel);
        self.request_open(rel);
        if self.modal_is_none() {
            self.start_edit();
            if let Some(e) = &mut self.editor {
                e.ta.move_cursor(CursorMove::Bottom);
                e.ta.move_cursor(CursorMove::End);
            }
        }
        self.refresh_changes();
        Ok(())
    }

    /// Renames/moves a page or folder and rewrites every affected relative link.
    fn move_path(&mut self, old: &Path, new: &Path) -> Result<()> {
        if new == old {
            return Ok(());
        }
        if new.starts_with(old) {
            bail!("Can't move a folder into itself");
        }
        let case_only = paths::to_slash(new).to_lowercase() == paths::to_slash(old).to_lowercase();
        if self.fs.exists(new) && !case_only {
            bail!("'{}' already exists", paths::to_slash(new));
        }
        let mut edits = Vec::new();
        for f in tree::markdown_files(&*self.fs) {
            let Ok(src) = self.fs.read_to_string(&f) else { continue };
            let f_new = links::map_moved(&f, old, new);
            if let Some(out) = links::rewrite(&src, &f, &f_new, old, new) {
                edits.push((f_new, out));
            }
        }
        self.fs.rename(old, new)?;
        for (f, content) in &edits {
            self.fs.write(f, content.as_bytes())?;
        }
        self.expanded = self.expanded.iter().map(|p| links::map_moved(p, old, new)).collect();
        if let Some(p) = &mut self.page {
            p.path = links::map_moved(&p.path, old, new);
        }
        if let Some(e) = &mut self.editor {
            e.path = links::map_moved(&e.path, old, new);
        }
        self.reload_tree();
        self.reveal(new);
        self.reload_page();
        self.refresh_changes();
        match edits.len() {
            0 => self.info(format!("Moved to {}", paths::to_slash(new))),
            n => self.info(format!("Moved to {}, updated links in {n} page(s)", paths::to_slash(new))),
        }
        Ok(())
    }

    fn delete(&mut self, path: &Path) -> Result<()> {
        self.fs.remove(path)?;
        if self.page.as_ref().is_some_and(|p| p.path.starts_with(path)) {
            self.page = None;
            self.editor = None;
        }
        self.expanded.retain(|p| !p.starts_with(path));
        self.reload_tree();
        self.refresh_changes();
        self.info(format!("Deleted {}", paths::to_slash(path)));
        Ok(())
    }

    fn insert_image(&mut self, page: &Path, input: &str) -> Result<()> {
        let src = expand_user_path(input);
        if !src.is_file() {
            bail!("Not a file: {}", src.display());
        }
        let name = src.file_name().ok_or_else(|| anyhow!("Not a file: {}", src.display()))?.to_string_lossy();
        self.insert_image_bytes(page, &name, &std::fs::read(&src)?)
    }

    /// Delivers an image picked through [`Host::pick_image`].
    pub fn insert_image_data(&mut self, name: &str, bytes: &[u8]) {
        let Some(page) = self.pending_image.take() else { return };
        if let Err(e) = self.insert_image_bytes(&page, name, bytes) {
            self.error(e.to_string());
        }
    }

    /// Copies an image into the assets dir and links it from `page`.
    fn insert_image_bytes(&mut self, page: &Path, name: &str, bytes: &[u8]) -> Result<()> {
        let name = Path::new(name).file_name().ok_or_else(|| anyhow!("Invalid file name: {name}"))?;
        let assets = PathBuf::from(&self.cfg.assets_dir);
        self.fs.create_dir_all(&assets)?;
        let rel = unique_asset(&*self.fs, &assets, Path::new(name), bytes);
        if !self.fs.exists(&rel) {
            self.fs.write(&rel, bytes)?;
        }
        let alt = rel.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let md = format!("![{alt}]({})", paths::encode_link(&paths::relative(&paths::parent(page), &rel)));
        match &mut self.editor {
            Some(e) if e.path == page => {
                e.ta.insert_str(&md);
                self.info("Image inserted (unsaved)");
            }
            _ => {
                let mut content = self.fs.read_to_string(page)?;
                let eol = if content.contains("\r\n") { "\r\n" } else { "\n" };
                if !content.is_empty() && !content.ends_with('\n') {
                    content.push_str(eol);
                }
                content.push_str(&format!("{eol}{md}{eol}"));
                self.fs.write(page, content.as_bytes())?;
                self.reload_page();
                self.info("Image inserted");
            }
        }
        self.reload_tree();
        self.refresh_changes();
        Ok(())
    }
}

/// A search item that remembers its index.
struct Labeled<'a>(usize, &'a str);

impl AsRef<str> for Labeled<'_> {
    fn as_ref(&self) -> &str {
        self.1
    }
}

/// Removes the cursor's line as one undoable edit and returns it with its line break.
fn cut_line(ta: &mut TextArea) -> String {
    ta.cancel_selection();
    let DataCursor(row, _) = ta.cursor();
    let len = |ta: &TextArea, r: usize| ta.lines()[r].chars().count() as u16;
    let text = format!("{}\n", ta.lines()[row]);
    let last = row + 1 == ta.lines().len();
    // The last line takes the break before it, so no empty line is left behind.
    let (from, to) = match (last, row) {
        (true, 0) => ((0, 0), (0, len(ta, 0))),
        (true, _) => ((row - 1, len(ta, row - 1)), (row, len(ta, row))),
        (false, _) => ((row, 0), (row + 1, 0)),
    };
    ta.move_cursor(CursorMove::Jump(from.0 as u16, from.1));
    ta.start_selection();
    ta.move_cursor(CursorMove::Jump(to.0 as u16, to.1));
    ta.cut();
    if last && row > 0 {
        ta.move_cursor(CursorMove::Jump(row as u16 - 1, 0));
    }
    text
}

/// Applies a line-editing key to `input` at `cursor`, a byte offset.
fn edit_line(input: &mut String, cursor: &mut usize, k: &Input) {
    let prev = input[..*cursor].char_indices().next_back().map_or(0, |(i, _)| i);
    let next = input[*cursor..].chars().next().map_or(*cursor, |c| *cursor + c.len_utf8());
    match k.key {
        Key::Backspace => {
            input.replace_range(prev..*cursor, "");
            *cursor = prev;
        }
        Key::Delete => input.replace_range(*cursor..next, ""),
        Key::Left => *cursor = prev,
        Key::Right => *cursor = next,
        Key::Home | Key::Up => *cursor = 0,
        Key::End | Key::Down => *cursor = input.len(),
        Key::Char('a') if k.ctrl => *cursor = 0,
        Key::Char('e') if k.ctrl => *cursor = input.len(),
        Key::Char('u') if k.ctrl => {
            input.clear();
            *cursor = 0;
        }
        Key::Char(c) if !k.ctrl => {
            input.insert(*cursor, c);
            *cursor += c.len_utf8();
        }
        _ => {}
    }
}

/// Validates user input as a path relative to some dir in the repo.
fn clean_rel(input: &str) -> Result<PathBuf> {
    let trimmed = input.trim().trim_matches('/').trim_matches('\\');
    paths::normalize(Path::new(trimmed))
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| anyhow!("Invalid path: {input}"))
}

fn expand_user_path(input: &str) -> PathBuf {
    let s = input.trim().trim_matches(|c| c == '"' || c == '\'');
    let s = s.strip_prefix("file://").unwrap_or(s);
    if let Some(rest) = s.strip_prefix('~')
        && let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))
    {
        return PathBuf::from(home).join(rest.trim_start_matches(['/', '\\']));
    }
    PathBuf::from(s)
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
