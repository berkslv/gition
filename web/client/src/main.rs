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
use gition::keymap::Action;
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
use web_sys::{
    BeforeUnloadEvent, ClipboardEvent, Event, HtmlElement, HtmlTextAreaElement, InputEvent, KeyboardEvent, MouseEvent,
    PointerEvent, WheelEvent,
};

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
    app.touch = touch();
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
        fit_keyboard(screen);
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

/// The pointer under a finger or mouse, from press to release.
#[derive(Default)]
struct Press {
    id: i32,
    touch: bool,
    x: f64,
    y: f64,
    /// Where the last whole row of scrolling was taken, in pixels.
    scrolled_y: f64,
    moved: bool,
    /// A long press already opened the menu.
    held: bool,
    timer: Option<i32>,
}

#[derive(Default)]
struct Gestures {
    press: Option<Press>,
    /// Time and cell of the last tap, to notice a double tap.
    last_tap: Option<(f64, u16, u16)>,
}

const MOVE_SLOP: f64 = 10.0;
const LONG_PRESS_MS: i32 = 500;
const DOUBLE_TAP_MS: f64 = 350.0;
const SWIPE_MIN: f64 = 60.0;

/// The grid cell under a point, and the height of a cell in pixels.
fn cell_at(x: f64, y: f64) -> Option<(u16, u16, f64)> {
    let grid = window().document()?.get_element_by_id(Grid::ID)?;
    let rows = grid.child_element_count();
    let cols = grid.first_element_child()?.child_element_count();
    let r = grid.get_bounding_client_rect();
    if rows == 0 || cols == 0 || r.width() <= 0.0 {
        return None;
    }
    let (cw, ch) = (r.width() / cols as f64, r.height() / rows as f64);
    let col = ((x - r.left()) / cw).floor().clamp(0.0, cols as f64 - 1.0) as u16;
    let row = ((y - r.top()) / ch).floor().clamp(0.0, rows as f64 - 1.0) as u16;
    Some((col, row, ch))
}

/// A tap on a screen before the app: sign in, pick a repo, or press a hint's key.
fn tap_screen(screen: &Shared, col: u16, row: u16) {
    let p = Position::new(col, row);
    let Some(tap) = TAPS.with(|t| t.borrow().iter().rev().find(|(r, _)| r.contains(p)).map(|(_, t)| t.clone())) else {
        return;
    };
    match tap {
        Tap::Key(k) => {
            handle(screen, k);
        }
        Tap::Repo(i) => {
            if let Screen::Repos { sel, .. } = &mut *screen.borrow_mut() {
                *sel = i;
            }
            handle(screen, Input { key: Key::Enter, ..Input::default() });
        }
    }
}

/// Runs `f` on the app if it's open; otherwise lets the screen take a tap.
fn with_app(screen: &Shared, f: impl FnOnce(&mut App)) -> bool {
    match &mut *screen.borrow_mut() {
        Screen::App(app, _) => {
            f(app);
            true
        }
        _ => false,
    }
}

/// Phones: shows the on-screen keyboard while there's something to type into, and hides it
/// otherwise. Runs inside the touch or key handler, as iOS only opens it from one.
fn fit_keyboard(screen: &Shared) {
    if !touch() {
        return;
    }
    let Some(kbd) = element::<HtmlTextAreaElement>("kbd") else { return };
    let wants = matches!(&*screen.borrow(), Screen::App(app, _) if app.wants_text());
    let focused = window().document().and_then(|d| d.active_element()).is_some_and(|f| f.is_same_node(Some(&kbd)));
    if wants && !focused {
        let _ = kbd.focus();
    } else if !wants && focused {
        let _ = kbd.blur();
    }
}

/// Touch and mouse: tap, double tap, long press, drag to scroll and swipe for the tree,
/// plus the mouse wheel. The on-screen keyboard and the visual viewport on phones.
fn listen_touch(screen: &Shared) {
    let w = window();
    let Some(term) = element::<HtmlElement>("term") else { return };
    let gestures: Rc<RefCell<Gestures>> = Rc::default();

    let s = screen.clone();
    let g = gestures.clone();
    let down = Closure::<dyn FnMut(PointerEvent)>::new(move |e: PointerEvent| {
        if e.button() != 0 {
            return;
        }
        // Keeps the focus (and so the keyboard) where it is; taps are handled on release.
        e.prevent_default();
        let touch = e.pointer_type() != "mouse";
        let (x, y) = (e.client_x() as f64, e.client_y() as f64);
        let mut press = Press { id: e.pointer_id(), touch, x, y, scrolled_y: y, ..Press::default() };
        if touch {
            let s = s.clone();
            let g2 = g.clone();
            let hold = Closure::once_into_js(move || {
                let mut gs = g2.borrow_mut();
                let Some(p) = gs.press.as_mut().filter(|p| !p.moved) else { return };
                p.held = true;
                p.timer = None;
                let (x, y) = (p.x, p.y);
                drop(gs);
                if let Some((col, row, _)) = cell_at(x, y) {
                    with_app(&s, |app| app.long_press(col, row));
                    fit_keyboard(&s);
                }
            });
            press.timer = window()
                .set_timeout_with_callback_and_timeout_and_arguments_0(hold.unchecked_ref(), LONG_PRESS_MS)
                .ok();
        }
        g.borrow_mut().press = Some(press);
    });

    let s = screen.clone();
    let g = gestures.clone();
    let mv = Closure::<dyn FnMut(PointerEvent)>::new(move |e: PointerEvent| {
        let mut gs = g.borrow_mut();
        let Some(p) = gs.press.as_mut().filter(|p| p.id == e.pointer_id()) else { return };
        let (x, y) = (e.client_x() as f64, e.client_y() as f64);
        if !p.moved && (x - p.x).hypot(y - p.y) > MOVE_SLOP {
            p.moved = true;
            if let Some(t) = p.timer.take() {
                window().clear_timeout_with_handle(t);
            }
        }
        // A vertical drag scrolls what's under the finger, a row at a time.
        if !p.touch || !p.moved || p.held || (x - p.x).abs() > (y - p.y).abs() {
            return;
        }
        let Some((col, row, ch)) = cell_at(p.x, p.y) else { return };
        let rows = ((p.scrolled_y - y) / ch).trunc();
        if rows != 0.0 {
            p.scrolled_y -= rows * ch;
            drop(gs);
            with_app(&s, |app| app.scroll(col, row, rows as isize));
        }
    });

    let s = screen.clone();
    let g = gestures.clone();
    let up = Closure::<dyn FnMut(PointerEvent)>::new(move |e: PointerEvent| {
        let mut gs = g.borrow_mut();
        let Some(p) = gs.press.take().filter(|p| p.id == e.pointer_id()) else { return };
        if let Some(t) = p.timer {
            window().clear_timeout_with_handle(t);
        }
        if p.held {
            return;
        }
        let (x, y) = (e.client_x() as f64, e.client_y() as f64);
        let (dx, dy) = (x - p.x, y - p.y);
        if p.moved {
            drop(gs);
            if p.touch && dx.abs() > SWIPE_MIN && dx.abs() > 2.0 * dy.abs() {
                with_app(&s, |app| app.swipe(dx > 0.0));
            }
            return fit_keyboard(&s);
        }
        let Some((col, row, _)) = cell_at(p.x, p.y) else { return };
        let now = js_sys::Date::now();
        let double = gs
            .last_tap
            .is_some_and(|(t, c, r)| now - t < DOUBLE_TAP_MS && c.abs_diff(col) <= 1 && r.abs_diff(row) <= 1);
        gs.last_tap = if double { None } else { Some((now, col, row)) };
        drop(gs);
        let handled = with_app(&s, |app| if double { app.double_tap(col, row) } else { app.tap(col, row) });
        if !handled {
            tap_screen(&s, col, row);
        }
        fit_keyboard(&s);
    });

    let g = gestures.clone();
    let cancel = Closure::<dyn FnMut(PointerEvent)>::new(move |_: PointerEvent| {
        if let Some(t) = g.borrow_mut().press.take().and_then(|p| p.timer) {
            window().clear_timeout_with_handle(t);
        }
    });

    let s = screen.clone();
    let wheel = Closure::<dyn FnMut(WheelEvent)>::new(move |e: WheelEvent| {
        let Some((col, row, ch)) = cell_at(e.client_x() as f64, e.client_y() as f64) else { return };
        e.prevent_default();
        let px = match e.delta_mode() {
            WheelEvent::DOM_DELTA_LINE => e.delta_y() * ch,
            WheelEvent::DOM_DELTA_PAGE => e.delta_y() * ch * 20.0,
            _ => e.delta_y(),
        };
        let rows = (px / ch).round().clamp(-10.0, 10.0) as isize;
        let rows = if rows == 0 { px.signum() as isize } else { rows };
        with_app(&s, |app| app.scroll(col, row, rows));
    });

    // A right click is the long press of a mouse.
    let s = screen.clone();
    let context = Closure::<dyn FnMut(MouseEvent)>::new(move |e: MouseEvent| {
        let Some((col, row, _)) = cell_at(e.client_x() as f64, e.client_y() as f64) else { return };
        if with_app(&s, |app| app.long_press(col, row)) {
            e.prevent_default();
        }
    });

    let on = |name: &str, f: &JsValue| {
        let _ = term.add_event_listener_with_callback(name, f.unchecked_ref());
    };
    on("pointerdown", down.as_ref());
    on("pointermove", mv.as_ref());
    on("pointerup", up.as_ref());
    on("pointercancel", cancel.as_ref());
    on("contextmenu", context.as_ref());
    let opts = web_sys::AddEventListenerOptions::new();
    opts.set_passive(false);
    let _ = term.add_event_listener_with_callback_and_add_event_listener_options(
        "wheel",
        wheel.as_ref().unchecked_ref(),
        &opts,
    );
    down.forget();
    mv.forget();
    up.forget();
    cancel.forget();
    wheel.forget();
    context.forget();

    // Typing on the on-screen keyboard. Keys with a real `key` already arrived as keydown and
    // were cancelled there; this catches the rest (predictive text, autocorrect, dictation).
    if let Some(kbd) = element::<HtmlTextAreaElement>("kbd") {
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
            fit_keyboard(&s);
        });
        let _ = kbd.add_event_listener_with_callback("beforeinput", input.as_ref().unchecked_ref());
        input.forget();
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
    TAPS.with(|t| t.borrow_mut().clear());
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
        Screen::Message(m) => {
            dialog(f, "gition", vec![Line::raw(m.clone())]);
        }
        Screen::Error(m) => {
            const HINT: &str = "Enter reload · p pick another repository";
            let rows = dialog(
                f,
                "gition",
                vec![Line::styled(m.clone(), Style::new().fg(Color::Red)), Line::raw(""), hint(HINT)],
            );
            hint_taps(rows.last(), HINT);
        }
        Screen::Login => {
            add_tap(f.area(), Tap::Key(Input { key: Key::Enter, ..Input::default() }));
            dialog(
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
            );
        }
        Screen::Repos { repos, sel, .. } => {
            let mut lines = vec![Line::raw("Open a repository:"), Line::raw("")];
            if repos.is_empty() {
                lines.push(Line::raw("No repositories yet. Install the gition app on one."));
            }
            let max = f.area().height.saturating_sub(10) as usize;
            let start = sel.saturating_sub(max.saturating_sub(1));
            let mut shown = Vec::new();
            for (i, r) in repos.iter().enumerate().skip(start).take(max.max(1)) {
                let style = if i == *sel { Style::new().add_modifier(Modifier::REVERSED) } else { Style::new() };
                shown.push((lines.len(), i));
                lines.push(Line::styled(format!(" {r} "), style));
            }
            lines.push(Line::raw(""));
            const HINT: &str = "j/k move · Enter open · i add repositories · r refresh";
            lines.push(hint(HINT));
            let rows = dialog(f, "gition", lines);
            for (line, i) in shown {
                if let Some(&r) = rows.get(line) {
                    add_tap(r, Tap::Repo(i));
                }
            }
            hint_taps(rows.last(), HINT);
        }
        Screen::Init(ctx) => {
            const HINT: &str = "y create · n pick another repository";
            let rows = dialog(
                f,
                "gition",
                vec![
                    Line::raw(format!("{} has no INDEX.md.", ctx.api.full_name())),
                    Line::raw("Create a gition workspace? Nothing is committed until you sync."),
                    Line::raw(""),
                    hint(HINT),
                ],
            );
            hint_taps(rows.last(), HINT);
        }
        Screen::Empty { full_name, .. } => {
            const HINT: &str = "y commit · n pick another repository";
            let rows = dialog(
                f,
                "gition",
                vec![
                    Line::raw(format!("{full_name} is empty.")),
                    Line::raw("Commit an INDEX.md to start a gition workspace?"),
                    Line::raw(""),
                    hint(HINT),
                ],
            );
            hint_taps(rows.last(), HINT);
        }
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

/// Draws a centered box; returns where each line went.
fn dialog(f: &mut Frame, title: &str, lines: Vec<Line>) -> Vec<Rect> {
    let area = f.area();
    let w = 72.min(area.width);
    // Count wrapped rows so narrow screens don't cut the last lines off.
    let inner = w.saturating_sub(2).max(1) as usize;
    let heights: Vec<u16> = lines.iter().map(|l| l.width().div_ceil(inner).max(1) as u16).collect();
    let h = (heights.iter().sum::<u16>() + 2).min(area.height);
    let r = Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 3, w, h);
    f.render_widget(Clear, r);
    let block = Block::bordered().title(format!(" {title} ")).border_style(Style::new().fg(Color::Cyan));
    f.render_widget(Paragraph::new(lines).block(block).wrap(Wrap { trim: false }), r);
    let mut y = r.y + 1;
    heights
        .iter()
        .map(|&h| {
            let line = Rect::new(r.x + 1, y, inner as u16, h);
            y += h;
            line
        })
        .collect()
}

/// What a tap on the screens before the app does.
#[derive(Clone)]
enum Tap {
    Key(Input),
    Repo(usize),
}

thread_local! {
    /// Filled while drawing those screens.
    static TAPS: RefCell<Vec<(Rect, Tap)>> = const { RefCell::new(Vec::new()) };
}

fn add_tap(r: Rect, tap: Tap) {
    TAPS.with(|t| t.borrow_mut().push((r, tap)));
}

/// Makes each segment of a hint line like "y create · n pick" press its key.
fn hint_taps(line: Option<&Rect>, text: &str) {
    let Some(line) = line else { return };
    let mut x = line.x;
    for seg in text.split(" · ") {
        let w = Line::raw(seg).width() as u16;
        if let Some(k) = gition::pointer::hint_key(seg) {
            add_tap(Rect::new(x, line.y, w, 1), Tap::Key(k));
        }
        x += w + 3;
    }
}
