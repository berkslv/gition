//! An open repo: the page tree on the left, the page on the right, sync at the top.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::{Receiver, Sender, channel};

use anyhow::{Result, bail};
use gition::blocks::escape;
use gition::config::Config;
use gition::host::{Host, Msg, Open};
use gition::keymap::{Action, Keymap};
use gition::repo::{self, INDEX};
use gition::storage::{self, Storage};
use gition::sync::{Remote, State};
use gition::tree::{self, Filter, Node};
use gition::{links, pages, paste, paths};
use wasm_bindgen::JsCast;
use web_sys::{ClipboardEvent, Element, Event, HtmlElement, HtmlInputElement, KeyboardEvent, MouseEvent};

use crate::dom::{self, by_id, closest, data, icons, set_html};
use crate::editor::Editor;
use crate::pictures::Pictures;
use crate::workspace::{Ctx, WebHost};
use crate::{to_input, toast};

const STILL_DOWNLOADING: &str = "Still downloading pages, try again in a moment";
/// Search shows at most this many results.
const RESULTS: usize = 50;

enum View {
    Editor(Editor),
    Image,
    Binary,
    /// A folder without INDEX.md.
    Folder,
    /// A file the host is still downloading.
    Loading,
}

struct Page {
    path: PathBuf,
    view: View,
}

enum Dialog {
    /// A new page in `dir`; `page` is the page that becomes the folder `dir` first.
    NewPage {
        dir: PathBuf,
        page: Option<PathBuf>,
    },
    Rename(PathBuf),
    Delete(PathBuf),
    Search(Box<Search>),
    Menu,
}

/// A search result: a page from the dialog's items, and for a match in its text, that line as HTML.
struct Hit {
    item: usize,
    line: Option<String>,
}

/// The search dialog. Titles are matched as you type; page text is searched in the background,
/// a few milliseconds at a time (see [`Notebook::search_text`]), so typing never waits for it.
struct Search {
    items: Vec<(String, PathBuf)>,
    results: Vec<Hit>,
    sel: usize,
    /// The query, lowercased, for the text search under way; empty when there's none.
    query: String,
    /// Bumped on every keystroke, so a text search for an older query stops.
    run: u64,
    /// The next item the text search looks at.
    next: usize,
    /// Each page's text, lowercased, once read. `Some(None)`: it couldn't be read (still downloading).
    texts: Vec<Option<Option<String>>>,
}

/// Wait this long after the last keystroke before searching page text.
const TEXT_SEARCH_DELAY_MS: i32 = 150;
/// How long one step of the text search may run before the browser gets a turn.
const TEXT_SEARCH_SLICE_MS: f64 = 8.0;

pub struct Notebook {
    ctx: Rc<Ctx>,
    host: WebHost,
    cfg: Config,
    keys: Keymap,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    tree: Node,
    expanded: HashSet<PathBuf>,
    show_all: bool,
    page: Option<Page>,
    git: Option<State>,
    /// A fetch or sync is running.
    busy: bool,
    syncing: bool,
    dialog: Option<Dialog>,
    /// Local changes, counted for (working copy version, base commit).
    changes: (u64, String, usize),
    /// The status last drawn.
    status: String,
}

/// The tree row of a page: a folder page is its folder.
fn node_of(page: &Path) -> PathBuf {
    let parent = paths::parent(page);
    if paths::is_index(page) && !parent.as_os_str().is_empty() { parent } else { page.to_path_buf() }
}

fn label(path: &Path) -> String {
    if path == Path::new(INDEX) { "Home".into() } else { pages::title(path) }
}

impl Notebook {
    pub fn new(ctx: Rc<Ctx>, cfg: Config, warning: Option<String>) -> Notebook {
        let (keys, warnings) = Keymap::from_config(&cfg);
        let (tx, rx) = channel();
        let mut nb = Notebook {
            host: WebHost(ctx.clone()),
            ctx,
            show_all: cfg.show_all_files,
            cfg,
            keys,
            tx,
            rx,
            tree: Node { path: PathBuf::new(), name: String::new(), is_dir: true, children: Vec::new() },
            expanded: HashSet::new(),
            page: None,
            git: None,
            busy: true,
            syncing: false,
            dialog: None,
            changes: (u64::MAX, String::new(), 0),
            status: String::new(),
        };
        nb.mount();
        nb.reload_tree();
        nb.open_home();
        nb.host.fetch(nb.tx.clone());
        nb.ctx.fill_in_background();
        let problems: Vec<String> =
            warning.into_iter().chain(warnings.into_iter().map(|w| format!("config: {w}"))).collect();
        if !problems.is_empty() {
            toast(&problems.join("; "), true);
        }
        nb.draw_status();
        nb
    }

    fn fs(&self) -> &dyn Storage {
        &*self.ctx.work
    }

    fn mount(&self) {
        let name = escape(&self.ctx.api.full_name());
        set_html(
            "app",
            &format!(
                r#"<div class="nb" id="nb">
  <aside id="side">
    <div class="side-top">
      <button class="repo" data-act="repos" title="Switch repository">{name}</button>
      <button class="ib" data-act="sidebar" title="Hide the sidebar">«</button>
    </div>
    <button class="side-btn" data-act="search">Search</button>
    <nav id="tree"></nav>
    <button class="side-btn" data-act="new">+ New page</button>
  </aside>
  <div id="scrim" data-act="sidebar"></div>
  <main>
    <header class="bar">
      <button class="ib show-side" data-act="sidebar" title="Pages">☰</button>
      <div id="crumbs"></div>
      <span id="note"></span>
      <button id="sync" data-act="sync"></button>
      <button class="ib" data-act="page-menu" title="More">⋯</button>
    </header>
    <div id="scroll"><article id="page"></article></div>
  </main>
</div>
<div id="overlay"></div>"#
            ),
        );
    }

    // ---- tree ---------------------------------------------------------------------

    fn reload_tree(&mut self) {
        self.tree = tree::build(self.fs(), &Filter { show_all: self.show_all, assets_dir: &self.cfg.assets_dir });
        self.draw_tree();
    }

    fn draw_tree(&self) {
        let current = self.page.as_ref().map(|p| node_of(&p.path));
        let mut html = String::new();
        for v in tree::flatten(&self.tree, &self.expanded) {
            let path = escape(&paths::to_slash(&v.path));
            let twist = match (v.is_dir, v.expanded) {
                (true, true) => format!(r#"<span class="tw" data-act="toggle" data-path="{path}">▾</span>"#),
                (true, false) => format!(r#"<span class="tw" data-act="toggle" data-path="{path}">▸</span>"#),
                _ => r#"<span class="tw"></span>"#.into(),
            };
            let cur = if current.as_ref() == Some(&v.path) { " cur" } else { "" };
            let name = if v.path == Path::new(INDEX) { "Home".into() } else { v.name };
            html.push_str(&format!(
                r#"<div class="row{cur}" data-act="open" data-path="{path}" style="--depth:{depth}">{twist}<span class="nm">{name}</span><span class="tools"><span class="ib" data-act="row-menu" data-path="{path}" title="More">⋯</span><span class="ib" data-act="add" data-path="{path}" title="Add a page inside">+</span></span></div>"#,
                depth = v.depth,
                name = escape(&name),
            ));
        }
        if html.is_empty() {
            html.push_str(r#"<p class="muted pad">No pages yet.</p>"#);
        }
        set_html("tree", &html);
    }

    /// Expands the folders above `path`.
    fn reveal(&mut self, path: &Path) {
        for a in node_of(path).ancestors().skip(1) {
            if !a.as_os_str().is_empty() {
                self.expanded.insert(a.to_path_buf());
            }
        }
    }

    fn toggle_sidebar(&self) {
        let Some(nb) = by_id::<Element>("nb") else { return };
        let narrow = dom::window().inner_width().ok().and_then(|w| w.as_f64()).is_some_and(|w| w < 760.0);
        let class = if narrow { "side-open" } else { "side-closed" };
        let _ = nb.class_list().toggle(class);
    }

    fn close_drawer(&self) {
        if let Some(nb) = by_id::<Element>("nb") {
            dom::toggle_class(&nb, "side-open", false);
        }
    }

    // ---- pages --------------------------------------------------------------------

    fn open_home(&mut self) {
        let fs = self.fs();
        let home = if repo::dir_has_index(fs, Path::new("")) {
            Some(PathBuf::from(INDEX))
        } else {
            tree::flatten(&self.tree, &self.expanded).into_iter().find(|v| !v.is_dir).map(|v| v.path)
        };
        match home {
            Some(p) => self.load(p),
            None => {
                self.page = None;
                self.draw_page();
            }
        }
    }

    /// Opens `path` after saving the page being edited.
    fn open(&mut self, path: PathBuf) {
        self.flush();
        self.load(path);
    }

    /// Shows `path` (a file or a folder) as it is in the working copy.
    fn load(&mut self, path: PathBuf) {
        let fs = self.fs();
        let path = if fs.is_dir(&path) && repo::dir_has_index(fs, &path) { path.join(INDEX) } else { path };
        let view = if fs.is_dir(&path) {
            View::Folder
        } else if paths::is_image(&path) {
            View::Image
        } else {
            match fs.read(&path) {
                Ok(bytes) if !bytes.contains(&0) => match String::from_utf8(bytes) {
                    Ok(s) => View::Editor(Editor::new(path.clone(), &s, !paths::is_markdown(&path))),
                    Err(_) => View::Binary,
                },
                Ok(_) => View::Binary,
                Err(e) if storage::not_downloaded(&e) => {
                    if self.host.want(&path, self.tx.clone()) {
                        View::Loading
                    } else {
                        View::Binary
                    }
                }
                Err(e) => return toast(&format!("{}: {e}", paths::to_slash(&path)), true),
            }
        };
        let same = self.page.as_ref().is_some_and(|p| p.path == path);
        self.reveal(&path);
        self.page = Some(Page { path, view });
        self.draw_page();
        self.draw_tree();
        self.close_drawer();
        if !same && let Some(scroll) = by_id::<Element>("scroll") {
            scroll.set_scroll_top(0);
        }
    }

    fn draw_page(&mut self) {
        let crumbs = match &self.page {
            Some(p) => {
                let node = node_of(&p.path);
                let mut parts: Vec<String> = node
                    .ancestors()
                    .skip(1)
                    .filter(|a| !a.as_os_str().is_empty())
                    .map(|a| {
                        format!(
                            r#"<button class="crumb" data-act="open" data-path="{}">{}</button>"#,
                            escape(&paths::to_slash(a)),
                            escape(&label(a))
                        )
                    })
                    .collect();
                parts.reverse();
                parts.push(format!(r#"<span class="crumb here">{}</span>"#, escape(&label(&node))));
                dom::document().set_title(&format!("{} · gition", label(&node)));
                parts.join(r#"<span class="sep">/</span>"#)
            }
            None => String::new(),
        };
        set_html("crumbs", &crumbs);
        let Some(page) = &mut self.page else {
            return set_html(
                "page",
                r#"<div class="file"><p class="muted">No pages yet.</p><button class="primary" data-act="new">New page</button></div>"#,
            );
        };
        let path = escape(&paths::to_slash(&page.path));
        let name = escape(&label(&page.path));
        match &mut page.view {
            View::Editor(ed) => {
                set_html("page", r#"<div id="blocks"></div><div class="tail" data-act="append"></div>"#);
                ed.reset();
                if let Some(root) = by_id::<Element>("blocks") {
                    ed.sync(&root);
                }
            }
            View::Image => {
                set_html("page", &format!(r#"<div class="file"><img data-src="{path}" alt="{name}"></div>"#))
            }
            View::Binary => set_html(
                "page",
                &format!(
                    r#"<div class="file"><p>{name} can't be shown here.</p><button data-act="github">Open on GitHub</button></div>"#
                ),
            ),
            View::Loading => set_html("page", r#"<div class="file"><p class="muted">Downloading…</p></div>"#),
            View::Folder => {
                let mut html = format!("<h1>{name}</h1><ul class=\"children\">");
                if let Some(node) = find(&self.tree, &page.path) {
                    for c in &node.children {
                        html.push_str(&format!(
                            r#"<li><a data-act="open" data-path="{}">{}</a></li>"#,
                            escape(&paths::to_slash(&c.path)),
                            escape(&c.name)
                        ));
                    }
                }
                html.push_str(&format!(r#"</ul><button data-act="add" data-path="{path}">+ Add a page</button>"#));
                set_html("page", &html);
            }
        }
        if let Some(root) = by_id::<Element>("page") {
            Pictures::fill(&self.ctx, &root);
        }
    }

    fn editor(&mut self) -> Option<&mut Editor> {
        match &mut self.page {
            Some(Page { view: View::Editor(ed), .. }) => Some(ed),
            _ => None,
        }
    }

    /// Runs `f` on the editor and the element holding its blocks, then saves the page.
    fn with_editor(&mut self, f: impl FnOnce(&mut Editor, &Element)) {
        let Some(root) = by_id::<Element>("blocks") else { return };
        let ctx = self.ctx.clone();
        let Some(ed) = self.editor() else { return };
        f(ed, &root);
        Pictures::fill(&ctx, &root);
        if let Err(e) = ed.save(&*ctx.work) {
            toast(&format!("Save failed: {e}"), true);
        }
    }

    /// Stops editing and saves, before anything that reads or moves files.
    fn flush(&mut self) {
        self.with_editor(|ed, root| ed.leave(root));
    }

    /// Stops editing once the focus has left the block being edited.
    pub fn focus_left(&mut self) {
        let Some(ed) = self.editor() else { return };
        if ed.editing.is_none() || dom::document().active_element().is_some_and(|e| e.class_list().contains("src")) {
            return;
        }
        self.flush();
    }

    /// Picks up files that changed under the page, after a pull or a sync.
    fn refresh(&mut self) {
        self.reload_tree();
        let Some(page) = &self.page else { return };
        let path = page.path.clone();
        if !self.fs().exists(&path) {
            return self.open_home();
        }
        if let View::Editor(ed) = &page.view
            && self.fs().read_to_string(&path).is_ok_and(|s| ed.is_current(&s))
        {
            return;
        }
        self.load(path);
    }

    // ---- input --------------------------------------------------------------------

    pub fn click(&mut self, e: &MouseEvent) {
        let target = e.target();
        if let Some(a) = closest(target.clone(), "a[data-page]") {
            e.prevent_default();
            let path = PathBuf::from(data(&a, "page").unwrap_or_default());
            if self.fs().exists(&path) {
                self.open(path);
            } else {
                toast(&format!("No page at {}", paths::to_slash(&path)), true);
            }
            return;
        }
        if closest(target.clone(), "a[href]").is_some() {
            return;
        }
        if let Some(cb) = closest(target.clone(), "input[data-at]") {
            let at = data(&cb, "at").and_then(|a| a.parse().ok());
            let i = closest(target, ".blk").and_then(|b| data(&b, "i")).and_then(|i| i.parse().ok());
            if let (Some(i), Some(at)) = (i, at) {
                self.with_editor(|ed, root| ed.toggle_task(root, i, at));
            }
            return;
        }
        if let Some(el) = closest(target.clone(), "[data-act]") {
            let act = data(&el, "act").unwrap_or_default();
            return self.act(&act, &el);
        }
        if let Some(blk) = closest(target, "#blocks > .blk:not(.editing)") {
            // Selecting text to copy isn't a click to edit.
            let selecting = dom::window().get_selection().ok().flatten().is_some_and(|s| !s.is_collapsed());
            let Some(i) = data(&blk, "i").and_then(|i| i.parse().ok()) else { return };
            if !selecting {
                let (x, y) = (e.client_x() as f64, e.client_y() as f64);
                self.with_editor(|ed, root| ed.click(root, i, &blk, x, y));
            }
        }
    }

    fn act(&mut self, act: &str, el: &Element) {
        let path = data(el, "path").map(PathBuf::from);
        match act {
            "open" | "search-open" => {
                let Some(path) = path else { return };
                self.close_dialog();
                if self.fs().is_dir(&path) {
                    self.expanded.insert(path.clone());
                }
                self.open(path);
            }
            "toggle" => {
                let Some(path) = path else { return };
                if !self.expanded.remove(&path) {
                    self.expanded.insert(path);
                }
                self.draw_tree();
            }
            "add" => self.ask_new_page(path.unwrap_or_default()),
            "new" => self.ask_new_page(PathBuf::new()),
            "row-menu" => self.menu(el, path.unwrap_or_default(), false),
            "page-menu" => {
                if let Some(p) = self.page.as_ref().map(|p| node_of(&p.path)) {
                    self.menu(el, p, true);
                }
            }
            "menu-item" => {
                self.close_dialog();
                let path = path.unwrap_or_default();
                match data(el, "do").as_deref() {
                    Some("add") => self.ask_new_page(path),
                    Some("rename") => self.ask_rename(path),
                    Some("delete") => self.ask_delete(path),
                    Some("github") => self.open_external(&path),
                    Some("logout") => {
                        self.flush();
                        self.ctx.persist();
                        crate::sign_out();
                    }
                    _ => {}
                }
            }
            // Leaves the menu open, so the switch shows the theme it moved to.
            "theme" => {
                let theme = data(el, "theme").unwrap_or_default();
                crate::set_theme(&theme);
                if let Some(seg) = el.parent_element() {
                    let buttons = seg.children();
                    for i in 0..buttons.length() {
                        let Some(b) = buttons.item(i) else { continue };
                        dom::toggle_class(&b, "on", data(&b, "theme").as_deref() == Some(theme.as_str()));
                    }
                }
            }
            "github" => {
                if let Some(p) = self.page.as_ref().map(|p| p.path.clone()) {
                    self.open_external(&p);
                }
            }
            "sync" => self.sync(),
            "search" => self.open_search(),
            "sidebar" => self.toggle_sidebar(),
            "repos" => {
                self.flush();
                self.ctx.persist();
                let _ = dom::window().location().assign("/#repos");
                let _ = dom::window().location().reload();
            }
            "append" => self.with_editor(|ed, root| ed.append(root)),
            "dlg-ok" => self.submit(),
            "dlg-cancel" => self.close_dialog(),
            _ => {}
        }
    }

    /// Handles a key; true when it was used.
    pub fn keydown(&mut self, e: &KeyboardEvent) -> bool {
        if self.dialog.is_some() {
            return match e.key().as_str() {
                "Escape" => {
                    self.close_dialog();
                    true
                }
                "Enter" if !e.is_composing() => {
                    self.submit();
                    true
                }
                "ArrowDown" | "ArrowUp" => {
                    if let Some(Dialog::Search(s)) = &mut self.dialog {
                        let down = e.key() == "ArrowDown";
                        s.sel = if down {
                            (s.sel + 1).min(s.results.len().saturating_sub(1))
                        } else {
                            s.sel.saturating_sub(1)
                        };
                        self.draw_results();
                    }
                    true
                }
                _ => false,
            };
        }
        let in_editor = closest(e.target(), "textarea.src").is_some();
        if let Some(input) = to_input(e) {
            if self.keys.is(Action::Sync, &input) {
                self.sync();
                return true;
            }
            if self.keys.is(Action::ToggleTree, &input) {
                self.toggle_sidebar();
                return true;
            }
            if !dom::typing() && self.keys.is(Action::Search, &input) {
                self.open_search();
                return true;
            }
            if in_editor && self.keys.is(Action::Save, &input) {
                self.with_editor(|_, _| {});
                toast("Saved", false);
                return true;
            }
            if in_editor && self.keys.is(Action::ExitEdit, &input) {
                self.flush();
                return true;
            }
        }
        // Tab moves list items, so it never moves the focus to the next link instead.
        let tab = e.key() == "Tab";
        if !in_editor {
            return tab;
        }
        let mut used = false;
        self.with_editor(|ed, root| used = ed.key(root, e));
        used || tab
    }

    pub fn input(&mut self, e: &Event) {
        if closest(e.target(), "textarea.src").is_some() {
            self.with_editor(|ed, _| ed.input());
        } else if closest(e.target(), "#dlg-input").is_some() && matches!(self.dialog, Some(Dialog::Search { .. })) {
            let query = by_id::<HtmlInputElement>("dlg-input").map(|i| i.value()).unwrap_or_default();
            self.search(&query);
        }
    }

    pub fn paste(&mut self, e: &ClipboardEvent) {
        let Some(ta) = closest(e.target(), "textarea.src") else { return };
        let Some(data) = e.clipboard_data() else { return };
        // Images win over the text that often comes with them.
        let files = data.files();
        let images: Vec<_> = (0..files.as_ref().map_or(0, |f| f.length()))
            .filter_map(|i| files.as_ref()?.get(i))
            .filter_map(|f| Some((paste::image_ext(&f.type_())?, f)))
            .collect();
        if !images.is_empty() {
            e.prevent_default();
            let ctx = self.ctx.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let mut batch = Vec::new();
                for (ext, file) in images {
                    if let Ok(buf) = wasm_bindgen_futures::JsFuture::from(file.array_buffer()).await {
                        batch.push((ext, js_sys::Uint8Array::new(&buf).to_vec()));
                    }
                }
                ctx.pasted.borrow_mut().push(batch);
            });
            return;
        }
        // A bare URL is pasted as it is, and becomes a link once its page title arrives.
        let Ok(text) = data.get_data("text") else { return };
        let ta: &web_sys::HtmlTextAreaElement = ta.unchecked_ref();
        let caret = ta.selection_start().ok().flatten().unwrap_or(0);
        let value = ta.value();
        let before = &value[..dom::byte_at(&value, caret)];
        if let Some(url) = paste::bare_url(&text).filter(|_| self.cfg.link_titles && !paste::in_link(before)) {
            self.host.fetch_title(url, self.tx.clone());
        }
    }

    // ---- background work ------------------------------------------------------------

    /// Applies what arrived since the last tick, saves the working copy, and redraws the status.
    pub fn tick(&mut self) {
        let pasted: Vec<_> = self.ctx.pasted.borrow_mut().drain(..).collect();
        for batch in pasted {
            let Some(page) = self.editor().map(|ed| ed.path.clone()) else { continue };
            let secs = (js_sys::Date::now() / 1000.0) as u64;
            let images = batch.into_iter().map(|(ext, bytes)| (paste::image_name(ext, secs), bytes)).collect();
            self.add_images(page, images);
        }
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Fetched(state, pulled) => {
                    self.busy = false;
                    self.git = Some(state);
                    if pulled > 0 {
                        self.refresh();
                        toast(&format!("Pulled {pulled} commit(s)"), false);
                    }
                }
                Msg::Synced(res, state) => {
                    self.busy = false;
                    self.syncing = false;
                    self.git = Some(state);
                    self.refresh();
                    match res {
                        Ok(m) => toast(&m, false),
                        Err(m) => toast(&m, true),
                    }
                }
                // A page that won't give its title still becomes a link, named after its address.
                Msg::Title(url, title) => self.with_editor(|ed, root| {
                    ed.link_title(root, &url, &title.unwrap_or_else(|| paste::address_title(&url)));
                }),
                Msg::Images(page, images) => self.place_images(&page, images),
                Msg::Downloaded(Some(e)) => toast(&format!("Download failed: {e}"), true),
                Msg::Downloaded(None) => {
                    if let Some(Page { path, view: View::Loading }) = &self.page {
                        let path = path.clone();
                        self.load(path);
                    }
                }
                Msg::Ran(..) => {}
            }
        }
        self.ctx.persist();
        self.draw_status();
    }

    fn count_changes(&mut self) -> usize {
        let key = (self.ctx.work.version(), self.ctx.base.borrow().commit.clone());
        if (self.changes.0, &self.changes.1) != (key.0, &key.1) {
            self.changes = (key.0, key.1, self.host.changes());
        }
        self.changes.2
    }

    fn draw_status(&mut self) {
        let changes = self.count_changes();
        let (text, class, title) = match &self.git {
            _ if self.syncing => ("Syncing…".to_string(), "busy", String::new()),
            Some(State { remote: Remote::Offline(e), .. }) if changes > 0 => {
                (format!("Offline · {changes}"), "warn", e.clone())
            }
            Some(State { remote: Remote::Offline(e), .. }) => ("Offline".into(), "warn", e.clone()),
            _ if changes > 0 => {
                (format!("Sync · {changes}"), "pending", format!("{changes} changed file(s) to commit and push"))
            }
            Some(State { remote: Remote::Tracking { behind, .. }, .. }) if *behind > 0 => {
                ("Sync ↓".into(), "pending", "GitHub has newer commits".into())
            }
            _ => ("Synced".into(), "ok", "Everything is on GitHub".into()),
        };
        let note = match self.host.downloading() {
            Some((done, total)) => format!("Downloading {done}/{total}"),
            None => String::new(),
        };
        let status = format!("{text}\0{class}\0{title}\0{note}");
        if status == self.status {
            return;
        }
        self.status = status;
        if let Some(b) = by_id::<HtmlElement>("sync") {
            b.set_text_content(Some(&text));
            b.set_class_name(class);
            b.set_title(&title);
        }
        dom::set_text("note", &note);
    }

    fn sync(&mut self) {
        if self.syncing {
            return toast("Sync already running…", false);
        }
        self.flush();
        self.busy = true;
        self.syncing = true;
        self.host.sync(&self.cfg.commit_prefix, self.tx.clone());
        self.draw_status();
    }

    fn open_external(&self, path: &Path) {
        if let Err(e) = self.host.open(Open::File(path)) {
            toast(&e.to_string(), true);
        }
    }

    // ---- images -------------------------------------------------------------------

    /// Adds images to `page`, compressed first when the config asks for it.
    fn add_images(&mut self, page: PathBuf, images: Vec<(String, Vec<u8>)>) {
        if !self.cfg.compress_images || !images.iter().any(|(_, b)| self.host.compressible(b)) {
            return self.place_images(&page, images);
        }
        toast(if images.len() == 1 { "Compressing image…" } else { "Compressing images…" }, false);
        self.host.compress(page, images, self.tx.clone());
    }

    /// Saves images under the assets dir and links them from `page`: at the caret, or at the end.
    fn place_images(&mut self, page: &Path, images: Vec<(String, Vec<u8>)>) {
        let mut md = Vec::new();
        for (name, bytes) in images {
            match pages::save_image(self.fs(), &self.cfg.assets_dir, page, &name, &bytes) {
                Ok(m) => md.push(m),
                Err(e) => return toast(&e.to_string(), true),
            }
        }
        let md = md.join("\n");
        if self.editor().is_some_and(|ed| ed.path == page) {
            self.with_editor(|ed, root| ed.insert(root, &md));
        } else {
            let res = self.fs().read_to_string(page).and_then(|mut content| {
                if !content.is_empty() && !content.ends_with('\n') {
                    content.push('\n');
                }
                content.push_str(&format!("\n{md}\n"));
                self.fs().write(page, content.as_bytes())
            });
            if let Err(e) = res {
                return toast(&e.to_string(), true);
            }
        }
        self.reload_tree();
        toast("Image inserted", false);
    }

    // ---- dialogs ------------------------------------------------------------------

    fn show_dialog(&mut self, dialog: Dialog, html: &str) {
        self.dialog = Some(dialog);
        set_html("overlay", html);
        if let Some(input) = by_id::<HtmlInputElement>("dlg-input") {
            let _ = input.focus();
            input.select();
        }
    }

    fn close_dialog(&mut self) {
        if self.dialog.take().is_some() {
            set_html("overlay", "");
        }
    }

    fn prompt(&mut self, dialog: Dialog, title: &str, value: &str, hint: &str, ok: &str) {
        let html = format!(
            r#"<div class="backdrop" data-act="dlg-cancel"></div><div class="dlg" role="dialog"><label for="dlg-input">{}</label><input id="dlg-input" value="{}" autocomplete="off" autocapitalize="off" spellcheck="false"><p class="hint">{}</p><div class="btns"><button data-act="dlg-cancel">Cancel</button><button class="primary" data-act="dlg-ok">{}</button></div></div>"#,
            escape(title),
            escape(value),
            escape(hint),
            escape(ok)
        );
        self.show_dialog(dialog, &html);
    }

    fn menu(&mut self, anchor: &Element, path: PathBuf, page: bool) {
        let p = escape(&paths::to_slash(&path));
        let item = |what: &str, icon: &str, text: &str| {
            format!(
                r#"<button data-act="menu-item" data-do="{what}" data-path="{p}">{icon}<span>{}</span></button>"#,
                escape(text)
            )
        };
        let mut items = Vec::new();
        if !page {
            items.push(item("add", icons::PLUS, "Add a page inside"));
        }
        items.push(item("rename", icons::PENCIL, "Rename or move"));
        items.push(item("delete", icons::TRASH, "Delete"));
        if page {
            items.push(item("github", icons::EXTERNAL, "Open on GitHub"));
            let current = crate::theme();
            let choice = |theme: &str, icon: &str, title: &str| {
                let on = if current == theme { " on" } else { "" };
                format!(
                    r#"<button class="seg-btn{on}" data-act="theme" data-theme="{theme}" title="{title}">{icon}</button>"#
                )
            };
            items.push(format!(
                r#"<div class="menu-row">{}<span>Theme</span><span class="seg">{}{}{}</span></div>"#,
                icons::PALETTE,
                choice("light", icons::SUN, "Light"),
                choice("dark", icons::MOON, "Dark"),
                choice("", icons::MONITOR, "System"),
            ));
            items.push(r#"<div class="menu-sep"></div>"#.into());
            items.push(item("logout", icons::LOGOUT, "Sign out"));
        }
        let r = anchor.get_bounding_client_rect();
        let width = dom::window().inner_width().ok().and_then(|w| w.as_f64()).unwrap_or(1000.0);
        let left = r.left().min(width - 250.0).max(8.0);
        let html = format!(
            r#"<div class="backdrop clear" data-act="dlg-cancel"></div><div class="menu" style="top:{}px;left:{left}px">{}</div>"#,
            r.bottom() + 4.0,
            items.join("")
        );
        self.show_dialog(Dialog::Menu, &html);
    }

    fn ask_new_page(&mut self, under: PathBuf) {
        let fs = self.fs();
        let (dir, page) = if under.as_os_str().is_empty() || fs.is_dir(&under) {
            (under, None)
        } else if paths::is_index(&under) {
            (paths::parent(&under), None)
        } else {
            // A page holds subpages once it's a folder; moving it rewrites links in every page.
            if self.host.downloading().is_some() {
                return toast(STILL_DOWNLOADING, false);
            }
            (under.with_extension(""), Some(under))
        };
        let hint = if dir.as_os_str().is_empty() {
            "At the top level".to_string()
        } else {
            format!("Inside {}", paths::to_slash(&dir))
        };
        self.prompt(Dialog::NewPage { dir, page }, "New page", "", &hint, "Create");
    }

    fn ask_rename(&mut self, path: PathBuf) {
        // Moving rewrites links in every page, so it needs them all.
        if self.host.downloading().is_some() {
            return toast(STILL_DOWNLOADING, false);
        }
        let mut current = paths::to_slash(&path);
        if !self.fs().is_dir(&path) && !self.show_all && paths::is_markdown(&path) {
            current.truncate(current.len() - 3);
        }
        self.prompt(Dialog::Rename(path), "Rename or move", &current, "A path from the top of the repo", "Move");
    }

    fn ask_delete(&mut self, path: PathBuf) {
        // The warning counts links in every page.
        if self.host.downloading().is_some() {
            return toast(STILL_DOWNLOADING, false);
        }
        self.flush();
        let n = links::count_incoming(self.fs(), &path, &tree::markdown_files(self.fs()));
        let what = if self.fs().is_dir(&path) { "this folder and everything in it" } else { "this page" };
        let refs = match n {
            0 => String::new(),
            1 => " 1 page links here.".into(),
            n => format!(" {n} pages link here."),
        };
        let html = format!(
            r#"<div class="backdrop" data-act="dlg-cancel"></div><div class="dlg" role="dialog"><p>Delete {what}, <b>{}</b>?{refs}</p><p class="hint">It goes from GitHub on the next sync.</p><div class="btns"><button data-act="dlg-cancel">Cancel</button><button class="danger" data-act="dlg-ok">Delete</button></div></div>"#,
            escape(&paths::to_slash(&path))
        );
        self.show_dialog(Dialog::Delete(path), &html);
    }

    fn open_search(&mut self) {
        let items = tree::search_items(self.fs());
        let results = (0..items.len()).map(|item| Hit { item, line: None }).collect();
        let texts = vec![None; items.len()];
        let search = Search { items, results, sel: 0, query: String::new(), run: 0, next: 0, texts };
        let html = r#"<div class="backdrop" data-act="dlg-cancel"></div><div class="dlg search" role="dialog"><input id="dlg-input" placeholder="Search pages" autocomplete="off" autocapitalize="off" spellcheck="false"><div id="dlg-results"></div></div>"#;
        self.show_dialog(Dialog::Search(Box::new(search)), html);
        self.draw_results();
    }

    /// Shows the pages whose title matches `query` now, and starts looking for it in page text
    /// once typing pauses.
    fn search(&mut self, query: &str) {
        let Some(Dialog::Search(s)) = &mut self.dialog else { return };
        s.run += 1;
        s.sel = 0;
        s.next = 0;
        s.results = if query.trim().is_empty() {
            (0..s.items.len()).map(|item| Hit { item, line: None }).collect()
        } else {
            tree::search(&s.items, query).into_iter().map(|item| Hit { item, line: None }).collect()
        };
        s.query = if query.trim().chars().count() >= 2 { query.trim().to_lowercase() } else { String::new() };
        let (run, text) = (s.run, !s.query.is_empty());
        self.draw_results();
        if text {
            dom::timeout(TEXT_SEARCH_DELAY_MS, move || crate::with_notebook(|nb| nb.search_text(run)));
        }
    }

    /// One step of the text search for run `run`: looks through pages until its time slice is up,
    /// shows what it found, and schedules the next step.
    fn search_text(&mut self, run: u64) {
        let fs = &*self.ctx.work;
        let Some(Dialog::Search(s)) = &mut self.dialog else { return };
        if s.run != run || s.query.is_empty() {
            return;
        }
        let began = js_sys::Date::now();
        let found = s.results.len();
        while s.next < s.items.len() && s.results.len() < RESULTS && js_sys::Date::now() - began < TEXT_SEARCH_SLICE_MS
        {
            let item = s.next;
            s.next += 1;
            if s.results.iter().any(|h| h.item == item) {
                continue;
            }
            let path = &s.items[item].1;
            let text = s.texts[item].get_or_insert_with(|| fs.read_to_string(path).ok().map(|t| t.to_lowercase()));
            let Some(at) = text.as_deref().and_then(|t| t.find(&s.query)) else { continue };
            // Lowercasing keeps line breaks, so the match is on the same line of the original.
            let n = text.as_deref().map_or(0, |t| t[..at].matches('\n').count());
            let Ok(original) = fs.read_to_string(path) else { continue };
            let line = original.lines().nth(n).unwrap_or_default();
            let html = match tree::find_text(line, &s.query) {
                Some((line, start, end)) => snippet(line, start, end),
                None => snippet(line, 0, 0),
            };
            s.results.push(Hit { item, line: Some(html) });
        }
        let more = s.next < s.items.len() && s.results.len() < RESULTS;
        if s.results.len() > found {
            self.draw_results();
        }
        if more {
            dom::timeout(0, move || crate::with_notebook(|nb| nb.search_text(run)));
        }
    }

    fn draw_results(&self) {
        let Some(Dialog::Search(s)) = &self.dialog else { return };
        let (items, results, sel) = (&s.items, &s.results, &s.sel);
        let mut html = String::new();
        for (n, hit) in results.iter().take(RESULTS).enumerate() {
            let (label, path) = &items[hit.item];
            let cur = if n == *sel { " sel" } else { "" };
            let title = pages::title(path);
            let place = label.trim_end_matches('/').strip_suffix(title.as_str()).unwrap_or_default();
            let place = place.trim_end_matches('/');
            let place = if place.is_empty() {
                String::new()
            } else {
                format!(r#"<span class="where">{}</span>"#, escape(place))
            };
            let line = hit.line.as_deref().map(|l| format!(r#"<span class="line">{l}</span>"#)).unwrap_or_default();
            html.push_str(&format!(
                r#"<button class="result{cur}" data-act="search-open" data-path="{}"><span class="what">{}</span>{place}{line}</button>"#,
                escape(&paths::to_slash(path)),
                escape(&title)
            ));
        }
        if html.is_empty() {
            html.push_str(r#"<p class="muted pad">No pages match.</p>"#);
        }
        if self.host.downloading().is_some() {
            html.push_str(
                r#"<p class="hint pad">Some pages are still downloading, so their text isn't searched yet.</p>"#,
            );
        }
        set_html("dlg-results", &html);
    }

    fn submit(&mut self) {
        let input = by_id::<HtmlInputElement>("dlg-input").map(|i| i.value()).unwrap_or_default();
        let Some(dialog) = self.dialog.take() else { return };
        set_html("overlay", "");
        let res = match dialog {
            Dialog::NewPage { dir, page } if !input.trim().is_empty() => self.new_page(dir, page, input.trim()),
            Dialog::Rename(old) if !input.trim().is_empty() => self.rename(&old, input.trim()),
            Dialog::Delete(path) => self.delete(&path),
            Dialog::Search(s) => {
                if let Some(hit) = s.results.get(s.sel) {
                    let path = s.items[hit.item].1.clone();
                    self.open(path);
                }
                Ok(())
            }
            _ => Ok(()),
        };
        if let Err(e) = res {
            toast(&e.to_string(), true);
        }
    }

    // ---- file operations ----------------------------------------------------------

    fn new_page(&mut self, mut dir: PathBuf, page: Option<PathBuf>, name: &str) -> Result<()> {
        self.flush();
        let rel = pages::new_page_path(&dir, name)?;
        if let Some(page) = page {
            if self.host.downloading().is_some() {
                bail!(STILL_DOWNLOADING);
            }
            if self.fs().exists(&rel) {
                bail!("'{}' already exists", paths::to_slash(&rel));
            }
            dir = pages::make_folder(self.fs(), &page)?;
            self.moved(&page, &dir.join(INDEX));
        }
        pages::create(self.fs(), &rel, &pages::title(&rel))?;
        if !dir.as_os_str().is_empty() {
            self.expanded.insert(dir);
        }
        self.reload_tree();
        self.load(rel);
        // Straight to writing, under the title.
        self.with_editor(|ed, root| ed.append(root));
        Ok(())
    }

    fn rename(&mut self, old: &Path, input: &str) -> Result<()> {
        if self.host.downloading().is_some() {
            bail!(STILL_DOWNLOADING);
        }
        self.flush();
        let new = pages::rename_target(self.fs(), old, input)?;
        if new == old {
            return Ok(());
        }
        let n = pages::move_path(self.fs(), old, &new)?;
        self.moved(old, &new);
        match n {
            0 => toast(&format!("Moved to {}", paths::to_slash(&new)), false),
            n => toast(&format!("Moved to {}, updated links in {n} page(s)", paths::to_slash(&new)), false),
        }
        Ok(())
    }

    /// Follows a move of `old` to `new`: the open folders, the tree and the page.
    fn moved(&mut self, old: &Path, new: &Path) {
        self.expanded = self.expanded.iter().map(|p| links::map_moved(p, old, new)).collect();
        self.reload_tree();
        // Links in the open page may have been rewritten too.
        if let Some(path) = self.page.as_ref().map(|p| links::map_moved(&p.path, old, new)) {
            self.load(path);
        }
    }

    fn delete(&mut self, path: &Path) -> Result<()> {
        self.fs().remove(path)?;
        self.expanded.retain(|p| !p.starts_with(path));
        self.reload_tree();
        if self.page.as_ref().is_some_and(|p| p.path.starts_with(path)) {
            self.open_home();
        }
        toast(&format!("Deleted {}", paths::to_slash(path)), false);
        Ok(())
    }

    pub fn persist(&self) {
        self.ctx.persist();
    }
}

fn find<'a>(node: &'a Node, path: &Path) -> Option<&'a Node> {
    if node.path == path {
        return Some(node);
    }
    node.children.iter().filter(|c| path.starts_with(&c.path)).find_map(|c| find(c, path))
}

/// The part of `line` around bytes `start..end`, as HTML with that part marked.
fn snippet(line: &str, start: usize, end: usize) -> String {
    const AROUND: usize = 40;
    let from = line[..start].char_indices().rev().nth(AROUND).map_or(0, |(i, _)| i);
    let to = line[end..].char_indices().nth(AROUND).map_or(line.len(), |(i, _)| end + i);
    format!(
        "{}{}<mark>{}</mark>{}{}",
        if from > 0 { "…" } else { "" },
        escape(line[from..start].trim_start()),
        escape(&line[start..end]),
        escape(&line[end..to]),
        if to < line.len() { "…" } else { "" }
    )
}
