//! gition in the browser: the same app and UI as the terminal build, drawn with
//! Ratzilla, with a GitHub repo as storage.

mod github;
mod workspace;

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use anyhow::{Result, anyhow};
use gition::app::App;
use gition::config::{Config, FILE_NAME};
use gition::images::Images;
use gition::keymap::{Action, KeySpec};
use gition::repo;
use gition::storage::Storage;
use ratatui_textarea::{Input, Key};
use ratzilla::DomBackend;
use ratzilla::ratatui::Frame;
use ratzilla::ratatui::Terminal;
use ratzilla::ratatui::backend::{Backend, ClearType, WindowSize};
use ratzilla::ratatui::buffer::Cell;
use ratzilla::ratatui::layout::Rect;
use ratzilla::ratatui::layout::{Position, Size};
use ratzilla::ratatui::style::{Color, Modifier, Style};
use ratzilla::ratatui::text::Line;
use ratzilla::ratatui::widgets::{Block, Clear, Paragraph, Wrap};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::spawn_local;
use web_sys::{BeforeUnloadEvent, ClipboardEvent, Event, HtmlElement, HtmlTextAreaElement, InputEvent, KeyboardEvent};

use crate::github::{Api, Session, status};
use crate::workspace::{Ctx, WebHost, get_item, set_item};

const LAST_REPO: &str = "gition:last";

enum Screen {
    Message(String),
    /// An error; Enter reloads.
    Error(String),
    Login,
    Repos {
        session: Session,
        repos: Vec<String>,
        sel: usize,
    },
    /// The repo has no INDEX.md yet.
    Init(Rc<Ctx>),
    /// The repo has no commits at all.
    Empty {
        session: Session,
        full_name: String,
    },
    App(Box<App>, Rc<Ctx>),
}

type Shared = Rc<RefCell<Screen>>;

fn set(screen: &Shared, s: Screen) {
    *screen.borrow_mut() = s;
}

fn window() -> web_sys::Window {
    web_sys::window().expect("no window")
}

fn main() {
    console_error_panic_hook::set_once();
    let screen: Shared = Rc::new(RefCell::new(Screen::Message("Loading…".into())));
    listen(&screen);
    spawn_local(start(screen.clone()));
    spawn_local(async move {
        // The grid is sized from a measured cell, so measure with the final font.
        if let Some(fonts) = window().document().map(|d| d.fonts())
            && let Ok(ready) = fonts.ready()
        {
            let _ = wasm_bindgen_futures::JsFuture::from(ready).await;
        }
        let grid = Grid { dom: DomBackend::new_by_id("term").expect("DOM backend"), grid: None };
        run(Terminal::new(grid).expect("terminal"), screen);
    });
}

/// Draws a frame on every animation frame, like Ratzilla's `draw_web`, but lets the DOM
/// backend rebuild its grid first and then redraws everything when it did.
fn run(mut terminal: Terminal<Grid>, screen: Shared) {
    let frame: FrameCallback = Rc::default();
    let next = frame.clone();
    *frame.borrow_mut() = Some(Closure::new(move || {
        if terminal.backend_mut().sync() {
            let _ = terminal.clear();
        }
        if let Err(e) = terminal.draw(|f| draw(f, &screen)) {
            web_sys::console::error_1(&e.to_string().into());
        }
        request_frame(&next);
    }));
    request_frame(&frame);
}

type FrameCallback = Rc<RefCell<Option<Closure<dyn FnMut()>>>>;

fn request_frame(f: &FrameCallback) {
    if let Some(cb) = &*f.borrow() {
        let _ = window().request_animation_frame(cb.as_ref().unchecked_ref());
    }
}

// ---- startup ----------------------------------------------------------------

async fn start(screen: Shared) {
    let result: Result<()> = async {
        let Some(session) = github::session().await? else {
            set(&screen, Screen::Login);
            return Ok(());
        };
        let hash = window().location().hash().unwrap_or_default();
        let hash = hash.trim_start_matches('#');
        let repo = if hash == "repos" {
            None
        } else if hash.contains('/') {
            Some(hash.to_string())
        } else {
            get_item(LAST_REPO)
        };
        match repo {
            Some(r) => open_repo(&screen, session, &r).await,
            None => show_repos(&screen, session).await,
        }
    }
    .await;
    if let Err(e) = result {
        set(&screen, Screen::Error(format!("{e:#}")));
    }
}

async fn show_repos(screen: &Shared, session: Session) -> Result<()> {
    set(screen, Screen::Message("Loading your repositories…".into()));
    let repos = Api::new("", "", session.clone()).installed_repos().await?;
    set(screen, Screen::Repos { session, repos, sel: 0 });
    Ok(())
}

fn set_hash(hash: &str) {
    if let Ok(history) = window().history() {
        let _ = history.replace_state_with_url(&JsValue::NULL, "", Some(&format!("#{hash}")));
    }
}

async fn open_repo(screen: &Shared, session: Session, full_name: &str) -> Result<()> {
    let (owner, name) = full_name.split_once('/').ok_or_else(|| anyhow!("not a repository: {full_name}"))?;
    set(screen, Screen::Message(format!("Opening {full_name}…")));
    let api = Api::new(owner, name, session.clone());
    let branch = match api.default_branch().await {
        Err(e) if status(&e) == Some(404) => {
            set_item(LAST_REPO, "");
            return show_repos(screen, session).await;
        }
        other => other?,
    };
    set_hash(full_name);
    set_item(LAST_REPO, full_name);
    let Some(head) = api.branch_head(&branch).await? else {
        set(screen, Screen::Empty { session, full_name: full_name.into() });
        return Ok(());
    };
    let ctx = Ctx::open(api, branch, head).await?;
    if repo::dir_has_index(&*ctx.work, Path::new("")) {
        start_app(screen, ctx);
    } else {
        set(screen, Screen::Init(ctx));
    }
    Ok(())
}

fn start_app(screen: &Shared, ctx: Rc<Ctx>) {
    let (cfg, warning) = match ctx.work.read_to_string(Path::new(FILE_NAME)) {
        Ok(text) => match Config::parse(&text) {
            Ok(c) => (c, None),
            Err(e) => (Config::default(), Some(format!("{e:#}"))),
        },
        Err(_) => (Config::default(), None),
    };
    let mut app = App::new(Box::new(ctx.work.clone()), Box::new(WebHost(ctx.clone())), cfg, Images::default());
    if let Some(w) = warning {
        app.error(w);
    }
    app.start_fetch();
    set(screen, Screen::App(Box::new(app), ctx));
}

// ---- input --------------------------------------------------------------------

/// Converts a browser key event; `None` for lone modifiers and unknown keys.
fn to_input(e: &KeyboardEvent) -> Option<Input> {
    let key = match e.key().as_str() {
        "Enter" => Key::Enter,
        "Escape" => Key::Esc,
        "Backspace" => Key::Backspace,
        "Tab" => Key::Tab,
        "Delete" => Key::Delete,
        "ArrowUp" => Key::Up,
        "ArrowDown" => Key::Down,
        "ArrowLeft" => Key::Left,
        "ArrowRight" => Key::Right,
        "Home" => Key::Home,
        "End" => Key::End,
        "PageUp" => Key::PageUp,
        "PageDown" => Key::PageDown,
        f if f.len() > 1 && f.starts_with('F') && f[1..].parse::<u8>().is_ok() => Key::F(f[1..].parse().ok()?),
        s => {
            let mut chars = s.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => Key::Char(c),
                _ => return None,
            }
        }
    };
    let mut ctrl = e.ctrl_key() || e.meta_key();
    let mut alt = e.alt_key();
    // AltGr (Ctrl+Alt on Windows) and macOS Option produce characters; type them.
    if let Key::Char(c) = key
        && (e.get_modifier_state("AltGraph") || (alt && !c.is_ascii_alphanumeric()))
    {
        ctrl = false;
        alt = false;
    }
    Some(Input { key, ctrl, alt, shift: e.shift_key() })
}

enum Then {
    Nothing,
    Reload,
    Navigate(String),
    OpenTab(String),
    Open(Session, String),
    Repos(Session),
    Init(Rc<Ctx>),
    InitEmpty(Session, String),
}

fn on_key(screen: &Shared, e: &KeyboardEvent) {
    if e.is_composing() {
        return;
    }
    if let Some(input) = to_input(e)
        && handle(screen, input)
    {
        e.prevent_default();
    }
}

/// Runs one key through the current screen; true when gition used it.
fn handle(screen: &Shared, input: Input) -> bool {
    let plain = !input.ctrl && !input.alt && !matches!(input.key, Key::F(_));
    let mut then = Then::Nothing;
    let handled = {
        let mut s = screen.borrow_mut();
        match &mut *s {
            Screen::App(app, _) => {
                // Leave browser shortcuts (copy, paste, reload, …) alone unless gition binds them.
                let bound = Action::ALL.iter().any(|&a| app.keys.is(a, &input));
                let take = plain
                    || bound
                    || app.editor.is_some() && input.ctrl && !matches!(input.key, Key::Char('c' | 'v' | 'x'));
                if take {
                    app.on_key(input.clone());
                }
                take
            }
            Screen::Login if input.key == Key::Enter => {
                then = Then::Navigate("/auth/login".into());
                true
            }
            Screen::Error(_) if input.key == Key::Enter => {
                then = Then::Reload;
                true
            }
            Screen::Error(_) if input.key == Key::Char('p') => {
                then = Then::Navigate("/#repos".into());
                true
            }
            Screen::Repos { session, repos, sel } if plain => {
                match input.key {
                    Key::Up | Key::Char('k') => *sel = sel.saturating_sub(1),
                    Key::Down | Key::Char('j') => *sel = (*sel + 1).min(repos.len().saturating_sub(1)),
                    Key::Enter => {
                        if let Some(r) = repos.get(*sel) {
                            then = Then::Open(session.clone(), r.clone());
                        }
                    }
                    Key::Char('i') => {
                        let app = session.app.clone().unwrap_or_default();
                        then = Then::OpenTab(format!("https://github.com/apps/{app}/installations/new"));
                    }
                    Key::Char('r') => then = Then::Repos(session.clone()),
                    _ => {}
                }
                true
            }
            Screen::Init(ctx) if plain => {
                match input.key {
                    Key::Char('y') => then = Then::Init(ctx.clone()),
                    Key::Char('n') | Key::Esc => then = Then::Navigate("/#repos".into()),
                    _ => {}
                }
                true
            }
            Screen::Empty { session, full_name } if plain => {
                match input.key {
                    Key::Char('y') => then = Then::InitEmpty(session.clone(), full_name.clone()),
                    Key::Char('n') | Key::Esc => then = Then::Navigate("/#repos".into()),
                    _ => {}
                }
                true
            }
            _ => false,
        }
    };
    let screen = screen.clone();
    match then {
        Then::Nothing => {}
        Then::Reload => {
            let _ = window().location().reload();
        }
        Then::Navigate(url) => {
            let _ = window().location().assign(&url);
            if url.starts_with("/#") {
                let _ = window().location().reload();
            }
        }
        Then::OpenTab(url) => {
            let _ = window().open_with_url_and_target(&url, "_blank");
        }
        Then::Open(session, name) => spawn_local(async move {
            if let Err(e) = open_repo(&screen, session, &name).await {
                set(&screen, Screen::Error(format!("{e:#}")));
            }
        }),
        Then::Repos(session) => spawn_local(async move {
            if let Err(e) = show_repos(&screen, session).await {
                set(&screen, Screen::Error(format!("{e:#}")));
            }
        }),
        Then::Init(ctx) => {
            let title = ctx.api.repo.clone();
            match workspace::init_files(&ctx.work, &title, &Config::default().assets_dir) {
                Ok(()) => start_app(&screen, ctx),
                Err(e) => set(&screen, Screen::Error(e.to_string())),
            }
        }
        Then::InitEmpty(session, name) => spawn_local(async move {
            let res = async {
                let (owner, repo_name) = name.split_once('/').unwrap_or_default();
                let api = Api::new(owner, repo_name, session.clone());
                let (path, content) = repo::starter_files(repo_name, &Config::default().assets_dir).remove(0);
                api.create_file(&path, &content, "gition: init").await?;
                open_repo(&screen, session, &name).await
            }
            .await;
            if let Err(e) = res {
                set(&screen, Screen::Error(format!("{e:#}")));
            }
        }),
    }
    handled
}

fn listen(screen: &Shared) {
    let w = window();
    let s = screen.clone();
    let keydown = Closure::<dyn FnMut(KeyboardEvent)>::new(move |e: KeyboardEvent| on_key(&s, &e));
    let s = screen.clone();
    let paste = Closure::<dyn FnMut(ClipboardEvent)>::new(move |e: ClipboardEvent| {
        if let Screen::App(app, _) = &mut *s.borrow_mut()
            && let Some(text) = e.clipboard_data().and_then(|d| d.get_data("text").ok())
        {
            app.on_paste(text);
            e.prevent_default();
        }
    });
    let s = screen.clone();
    let unload = Closure::<dyn FnMut(BeforeUnloadEvent)>::new(move |e: BeforeUnloadEvent| {
        // Saved edits survive a reload; an open editor with unsaved text doesn't.
        if let Ok(screen) = s.try_borrow()
            && let Screen::App(app, _) = &*screen
            && app.editor.as_ref().is_some_and(|ed| ed.dirty())
        {
            e.prevent_default();
            e.set_return_value("");
        }
    });
    let hashchange = Closure::<dyn FnMut()>::new(|| {
        let _ = window().location().reload();
    });
    let _ = w.add_event_listener_with_callback("keydown", keydown.as_ref().unchecked_ref());
    let _ = w.add_event_listener_with_callback("paste", paste.as_ref().unchecked_ref());
    let _ = w.add_event_listener_with_callback("beforeunload", unload.as_ref().unchecked_ref());
    let _ = w.add_event_listener_with_callback("hashchange", hashchange.as_ref().unchecked_ref());
    keydown.forget();
    paste.forget();
    unload.forget();
    hashchange.forget();
    listen_touch(screen);
}

/// A phone or tablet: no physical keyboard to expect.
fn touch() -> bool {
    window().match_media("(hover: none) and (pointer: coarse)").ok().flatten().is_some_and(|m| m.matches())
}

fn element<T: JsCast>(id: &str) -> Option<T> {
    window().document()?.get_element_by_id(id)?.dyn_into().ok()
}

/// Phones: the key bar, the on-screen keyboard and the visual viewport.
fn listen_touch(screen: &Shared) {
    let w = window();
    let Some(kbd) = element::<HtmlTextAreaElement>("kbd") else { return };

    // The bar sends keys a phone keyboard doesn't have; ⌨ shows or hides the keyboard.
    // Cancelling mousedown keeps the focus, and so the keyboard, where it is; the click still comes.
    if let Some(bar) = element::<HtmlElement>("bar") {
        let keep_focus = Closure::<dyn FnMut(Event)>::new(|e: Event| e.prevent_default());
        let _ = bar.add_event_listener_with_callback("mousedown", keep_focus.as_ref().unchecked_ref());
        keep_focus.forget();
        let s = screen.clone();
        let k = kbd.clone();
        let click = Closure::<dyn FnMut(Event)>::new(move |e: Event| {
            let Some(key) = e
                .target()
                .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
                .and_then(|t| t.closest("[data-key]").ok().flatten())
                .and_then(|t| t.get_attribute("data-key"))
            else {
                return;
            };
            if key == "kbd" {
                let focused = window().document().and_then(|d| d.active_element());
                if focused.is_some_and(|f| f.is_same_node(Some(&k))) {
                    let _ = k.blur();
                } else {
                    let _ = k.focus();
                }
            } else if let Ok(spec) = KeySpec::parse(&key) {
                handle(&s, Input { key: spec.key, ctrl: spec.ctrl, alt: spec.alt, shift: spec.shift });
            }
        });
        let _ = bar.add_event_listener_with_callback("click", click.as_ref().unchecked_ref());
        click.forget();
    }

    // Typing on the on-screen keyboard. Keys with a real `key` already arrived as keydown and
    // were cancelled there; this catches the rest (predictive text, autocorrect, dictation).
    let s = screen.clone();
    let k = kbd.clone();
    let input = Closure::<dyn FnMut(InputEvent)>::new(move |e: InputEvent| {
        if e.is_composing() {
            return;
        }
        e.prevent_default();
        let keys: Vec<Key> = match e.input_type().as_str() {
            "insertText" | "insertReplacementText" => e.data().unwrap_or_default().chars().map(Key::Char).collect(),
            "insertLineBreak" | "insertParagraph" => vec![Key::Enter],
            "deleteContentBackward" => vec![Key::Backspace],
            "deleteContentForward" => vec![Key::Delete],
            _ => vec![],
        };
        for key in keys {
            handle(&s, Input { key, ..Input::default() });
        }
        k.set_value("");
    });
    let _ = kbd.add_event_listener_with_callback("beforeinput", input.as_ref().unchecked_ref());
    input.forget();

    // Tapping the sign-in screen signs in, since there's no Enter key to press.
    if let Some(term) = element::<HtmlElement>("term") {
        let s = screen.clone();
        let tap = Closure::<dyn FnMut()>::new(move || {
            let login = matches!(&*s.borrow(), Screen::Login);
            if login {
                handle(&s, Input { key: Key::Enter, ..Input::default() });
            }
        });
        let _ = term.add_event_listener_with_callback("click", tap.as_ref().unchecked_ref());
        tap.forget();
    }

    // The on-screen keyboard shrinks the visual viewport only: fit the page to it, and tell
    // the DOM backend to rebuild its grid (it listens for window resizes).
    if let Some(vv) = w.visual_viewport() {
        let fit = Closure::<dyn FnMut()>::new(move || {
            let w = window();
            let (Some(vv), Some(body)) = (w.visual_viewport(), w.document().and_then(|d| d.body())) else {
                return;
            };
            let height = format!("{}px", vv.height());
            if body.style().get_property_value("height").ok().as_deref() != Some(height.as_str()) {
                let _ = body.style().set_property("height", &height);
                w.scroll_to_with_x_and_y(0.0, 0.0);
                if let Ok(e) = Event::new("resize") {
                    let _ = w.dispatch_event(&e);
                }
            }
        });
        let _ = vv.add_event_listener_with_callback("resize", fit.as_ref().unchecked_ref());
        fit.forget();
    }
}

// ---- drawing ------------------------------------------------------------------

/// `DomBackend`, but reporting the size of the grid it actually built. Ratzilla 0.3's
/// `size()` guesses from the window (or, on phones, the whole screen), which is bigger than
/// the grid and made the first frame write past its end.
struct Grid {
    dom: DomBackend,
    /// The grid element of the last frame, to notice when the backend rebuilt it.
    grid: Option<web_sys::Element>,
}

impl Grid {
    const ID: &str = "term_ratzilla_grid";

    /// Lets the backend (re)build its grid, which it does lazily on the first draw and after
    /// a resize. True when it did, so the whole screen needs drawing again.
    fn sync(&mut self) -> bool {
        let _ = self.dom.draw(std::iter::empty());
        let grid = window().document().and_then(|d| d.get_element_by_id(Self::ID));
        let rebuilt = grid != self.grid;
        self.grid = grid;
        rebuilt
    }

    fn drawn_size(&self) -> Option<Size> {
        let grid = self.grid.as_ref()?;
        let cols = grid.first_element_child()?.child_element_count() as u16;
        Some(Size::new(cols, grid.child_element_count() as u16)).filter(|s| s.width > 0)
    }
}

impl Backend for Grid {
    type Error = std::io::Error;

    fn draw<'a, I: Iterator<Item = (u16, u16, &'a Cell)>>(&mut self, content: I) -> std::io::Result<()> {
        // Cells outside the grid have nowhere to go.
        let size = self.size()?;
        self.dom.draw(content.filter(|(x, y, _)| *x < size.width && *y < size.height))
    }
    fn hide_cursor(&mut self) -> std::io::Result<()> {
        self.dom.hide_cursor()
    }
    fn show_cursor(&mut self) -> std::io::Result<()> {
        self.dom.show_cursor()
    }
    fn get_cursor_position(&mut self) -> std::io::Result<Position> {
        self.dom.get_cursor_position()
    }
    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> std::io::Result<()> {
        self.dom.set_cursor_position(position)
    }
    fn clear(&mut self) -> std::io::Result<()> {
        self.dom.clear()
    }
    fn clear_region(&mut self, clear_type: ClearType) -> std::io::Result<()> {
        self.dom.clear_region(clear_type)
    }
    fn size(&self) -> std::io::Result<Size> {
        Ok(self.drawn_size().unwrap_or_default())
    }
    fn window_size(&mut self) -> std::io::Result<WindowSize> {
        self.dom.window_size()
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.dom.flush()
    }
}

fn draw(f: &mut Frame, screen: &Shared) {
    draw_screen(f, screen);
    for cell in f.buffer_mut().content.iter_mut() {
        cell.fg = theme(cell.fg, false);
        cell.bg = theme(cell.bg, true);
    }
}

fn draw_screen(f: &mut Frame, screen: &Shared) {
    let mut s = screen.borrow_mut();
    let hint = |t: &str| Line::styled(t.to_string(), Style::new().fg(Color::DarkGray));
    match &mut *s {
        Screen::App(app, ctx) => {
            let picked: Vec<_> = ctx.picked.borrow_mut().drain(..).collect();
            for (name, bytes) in picked {
                app.insert_image_data(&name, &bytes);
            }
            app.tick();
            if app.quit {
                app.quit = false;
                app.info("Close the tab to leave gition");
            }
            ctx.persist();
            gition::ui::draw(f, app);
        }
        Screen::Message(m) => dialog(f, "gition", vec![Line::raw(m.clone())]),
        Screen::Error(m) => dialog(
            f,
            "gition",
            vec![
                Line::styled(m.clone(), Style::new().fg(Color::Red)),
                Line::raw(""),
                hint("Enter reload · p pick another repository"),
            ],
        ),
        Screen::Login => dialog(
            f,
            "gition",
            vec![
                Line::raw("A notebook stored as markdown in a GitHub repo."),
                Line::raw(""),
                Line::styled(
                    if touch() { "Tap to sign in with GitHub" } else { "Press Enter to sign in with GitHub" },
                    Style::new().add_modifier(Modifier::BOLD),
                ),
                Line::raw(""),
                hint("Notes go straight from your browser to GitHub; this server stores nothing."),
                hint("gition only sees the repos you install the app on."),
            ],
        ),
        Screen::Repos { repos, sel, .. } => {
            let mut lines = vec![Line::raw("Open a repository:"), Line::raw("")];
            if repos.is_empty() {
                lines.push(Line::raw("No repositories yet. Install the gition app on one."));
            }
            let max = f.area().height.saturating_sub(10) as usize;
            let start = sel.saturating_sub(max.saturating_sub(1));
            for (i, r) in repos.iter().enumerate().skip(start).take(max.max(1)) {
                let style = if i == *sel { Style::new().add_modifier(Modifier::REVERSED) } else { Style::new() };
                lines.push(Line::styled(format!(" {r} "), style));
            }
            lines.push(Line::raw(""));
            lines.push(hint("j/k move · Enter open · i add repositories · r refresh"));
            dialog(f, "gition", lines);
        }
        Screen::Init(ctx) => dialog(
            f,
            "gition",
            vec![
                Line::raw(format!("{} has no INDEX.md.", ctx.api.full_name())),
                Line::raw("Create a gition workspace? Nothing is committed until you sync."),
                Line::raw(""),
                hint("y create · n pick another repository"),
            ],
        ),
        Screen::Empty { full_name, .. } => dialog(
            f,
            "gition",
            vec![
                Line::raw(format!("{full_name} is empty.")),
                Line::raw("Commit an INDEX.md to start a gition workspace?"),
                Line::raw(""),
                hint("y commit · n pick another repository"),
            ],
        ),
    }
}

/// Terminals map the 16 named colors through their theme; the browser has none, so pick one
/// that reads well on the dark page background.
fn theme(c: Color, bg: bool) -> Color {
    let rgb = |r, g, b| Color::Rgb(r, g, b);
    match c {
        Color::Reset if bg => rgb(15, 17, 21),
        Color::Reset => rgb(216, 222, 233),
        Color::Black => rgb(46, 52, 64),
        Color::Red => rgb(191, 97, 106),
        Color::Green => rgb(163, 190, 140),
        Color::Yellow => rgb(235, 203, 139),
        Color::Blue => rgb(129, 161, 193),
        Color::Magenta => rgb(180, 142, 173),
        Color::Cyan => rgb(136, 192, 208),
        Color::Gray => rgb(216, 222, 233),
        Color::DarkGray => rgb(118, 126, 142),
        Color::LightRed => rgb(208, 135, 112),
        Color::LightGreen => rgb(183, 210, 160),
        Color::LightYellow => rgb(245, 220, 160),
        Color::LightBlue => rgb(150, 185, 215),
        Color::LightMagenta => rgb(200, 162, 193),
        Color::LightCyan => rgb(160, 215, 230),
        Color::White => rgb(236, 239, 244),
        other => other,
    }
}

fn dialog(f: &mut Frame, title: &str, lines: Vec<Line>) {
    let area = f.area();
    let w = 72.min(area.width);
    // Count wrapped rows so narrow screens don't cut the last lines off.
    let inner = w.saturating_sub(2).max(1) as usize;
    let rows: usize = lines.iter().map(|l| l.width().div_ceil(inner).max(1)).sum();
    let h = (rows as u16 + 2).min(area.height);
    let r = Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 3, w, h);
    f.render_widget(Clear, r);
    let block = Block::bordered().title(format!(" {title} ")).border_style(Style::new().fg(Color::Cyan));
    f.render_widget(Paragraph::new(lines).block(block).wrap(Wrap { trim: false }), r);
}
