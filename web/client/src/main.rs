//! gition in the browser: a notebook of pages you edit in place, with a GitHub repo as storage.
//! The pages, sync and file operations are the terminal app's; the interface is the web's own.

mod cache;
mod dom;
mod editor;
mod github;
mod notebook;
mod pictures;
mod workspace;

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::rc::Rc;

use anyhow::{Result, anyhow};
use gition::blocks::escape;
use gition::config::{Config, FILE_NAME};
use gition::repo;
use gition::storage::Storage;
use ratatui_textarea::{Input, Key};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::spawn_local;
use web_sys::{ClipboardEvent, Event, KeyboardEvent, MouseEvent};

use crate::dom::{closest, data, set_html, window};
use crate::github::{Api, Installed, Session, status};
use crate::notebook::Notebook;
use crate::workspace::{Ctx, Local, get_item, set_item};

const LAST_REPO: &str = "gition:last";
const THEME: &str = "gition:theme";

/// The chosen theme: `light`, `dark`, or empty to follow the system.
pub fn theme() -> String {
    get_item(THEME).filter(|t| t == "light" || t == "dark").unwrap_or_default()
}

/// Shows and remembers `theme`.
pub fn set_theme(theme: &str) {
    set_item(THEME, theme);
    apply_theme();
}

/// Ends the session with the worker, which clears its cookie and comes back to the sign-in screen.
/// What this browser keeps of each repo, unsynced changes included, stays for the next sign-in.
pub fn sign_out() {
    let _ = window().location().assign("/auth/logout");
}

fn apply_theme() {
    let doc = dom::document();
    let Some(root) = doc.document_element() else { return };
    let theme = theme();
    let _ = match theme.as_str() {
        "" => root.remove_attribute("data-theme"),
        t => root.set_attribute("data-theme", t),
    };
    // The browser's bar follows the page, not the system, once a theme is picked.
    let Ok(metas) = doc.query_selector_all("meta[name=theme-color]") else { return };
    for i in 0..metas.length() {
        let Some(meta) = metas.item(i).and_then(|m| m.dyn_into::<web_sys::Element>().ok()) else { continue };
        let own = if meta.get_attribute("media").is_some_and(|m| m.contains("dark")) { "dark" } else { "light" };
        let shown = if theme.is_empty() { own } else { &theme };
        let _ = meta.set_attribute("content", if shown == "dark" { "#0f1115" } else { "#ffffff" });
    }
}

enum Screen {
    Message(String),
    /// An error; it offers to reload.
    Error(String),
    Login,
    Repos {
        session: Session,
        repos: Vec<Installed>,
        /// The repo asking whether to remove it.
        confirm: Option<String>,
    },
    /// The repo has no commits at all.
    Empty {
        session: Session,
        full_name: String,
    },
    Notebook(Box<Notebook>),
}

thread_local! {
    static SCREEN: RefCell<Screen> = RefCell::new(Screen::Message("Loading…".into()));
    /// A pointer is down: a focus change now is the start of a click, not the end of editing.
    static PRESSING: Cell<bool> = const { Cell::new(false) };
    static TOAST: Cell<u32> = const { Cell::new(0) };
}

/// Runs `f` on the screen. `None` if it's busy, which only happens to events a DOM change
/// fires while the screen is being updated.
fn try_with<R>(f: impl FnOnce(&mut Screen) -> R) -> Option<R> {
    SCREEN.with(|s| s.try_borrow_mut().ok().map(|mut s| f(&mut s)))
}

pub fn with_notebook(f: impl FnOnce(&mut Notebook)) {
    try_with(|s| {
        if let Screen::Notebook(nb) = s {
            f(nb);
        }
    });
}

fn set(screen: Screen) {
    try_with(|s| {
        draw(&screen);
        *s = screen;
    });
}

/// Shows a message at the bottom for a few seconds.
pub fn toast(text: &str, error: bool) {
    let Some(el) = dom::by_id::<web_sys::Element>("toast") else { return };
    el.set_text_content(Some(text));
    el.set_class_name(if error { "show err" } else { "show" });
    let id = TOAST.with(|t| t.get().wrapping_add(1));
    TOAST.with(|t| t.set(id));
    dom::timeout(if error { 6000 } else { 3000 }, move || {
        if TOAST.with(Cell::get) == id {
            el.set_class_name("");
        }
    });
}

fn main() {
    console_error_panic_hook::set_once();
    apply_theme();
    listen();
    dom::every(300, || with_notebook(|nb| nb.tick()));
    spawn_local(start());
}

// ---- startup ----------------------------------------------------------------

async fn start() {
    let result: Result<()> = async {
        let hash = window().location().hash().unwrap_or_default();
        let hash = hash.trim_start_matches('#');
        #[cfg(debug_assertions)]
        if hash == "demo" {
            start_app(demo().await);
            return Ok(());
        }
        // The repo list with made-up repos; refreshing it fails, as there's no real session.
        #[cfg(debug_assertions)]
        if hash == "demo-repos" {
            let session = Session { token: String::new(), expires_at: None, app: None };
            let repo = |name: &str, all| Installed {
                full_name: name.into(),
                all,
                settings: "https://github.com/settings/installations".into(),
            };
            let repos = vec![repo("me/notes", false), repo("me/work", false), repo("team/wiki", true)];
            set(Screen::Repos { session, repos, confirm: None });
            return Ok(());
        }
        let repo = if hash == "repos" {
            None
        } else if hash.contains('/') {
            Some(hash.to_string())
        } else {
            get_item(LAST_REPO).filter(|r| !r.is_empty())
        };
        // Read what we kept of the repo while the worker looks up the token.
        let local = async {
            match &repo {
                Some(r) => Some(Local::load(r).await),
                None => None,
            }
        };
        let (session, local) = futures_util::future::join(github::session(), local).await;
        let Some(session) = session? else {
            set(Screen::Login);
            return Ok(());
        };
        match repo {
            Some(r) => open_repo(session, &r, local).await,
            None => show_repos(session).await,
        }
    }
    .await;
    if let Err(e) = result {
        set(Screen::Error(format!("{e:#}")));
    }
}

async fn show_repos(session: Session) -> Result<()> {
    set(Screen::Message("Loading your repositories…".into()));
    let repos = Api::new("", "", session.clone()).installed_repos().await?;
    set(Screen::Repos { session, repos, confirm: None });
    Ok(())
}

fn set_hash(hash: &str) {
    if let Ok(history) = window().history() {
        let _ = history.replace_state_with_url(&JsValue::NULL, "", Some(&format!("#{hash}")));
    }
}

/// A GitHub owner or repository name: letters, digits, `-`, `_` and `.`, but not `.` or `..`.
fn valid_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 100
        && s != "."
        && s != ".."
        && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// Opens a repo. `local` is what [`Local::load`] gave for it, if it's been read already.
async fn open_repo(session: Session, full_name: &str, local: Option<Local>) -> Result<()> {
    // The name comes from the URL, and goes into API paths: no `..` or other path tricks.
    let (owner, name) = full_name
        .split_once('/')
        .filter(|(o, n)| valid_name(o) && valid_name(n))
        .ok_or_else(|| anyhow!("not a repository: {full_name}"))?;
    set(Screen::Message(format!("Opening {full_name}…")));
    let mut local = match local {
        Some(l) => l,
        None => Local::load(full_name).await,
    };
    let api = Api::new(owner, name, session.clone());
    // Show what we had last time right away; the first fetch catches up with GitHub, and
    // reports errors like lost access. Only the first screen's files that the cache lacks are
    // downloaded first; if that fails, those pages say so when opened.
    if let Some((branch, base)) = local.cached.take() {
        set_hash(full_name);
        set_item(LAST_REPO, full_name);
        let ctx = Ctx::new(api, branch, base, local);
        let _ = ctx.load_first().await;
        start_app(ctx);
        return Ok(());
    }
    let (branch, head) = match api.default_branch().await {
        Err(e) if status(&e) == Some(404) => {
            set_item(LAST_REPO, "");
            return show_repos(session).await;
        }
        other => other?,
    };
    set_hash(full_name);
    set_item(LAST_REPO, full_name);
    let Some(head) = head else {
        set(Screen::Empty { session, full_name: full_name.into() });
        return Ok(());
    };
    let ctx = Ctx::open(api, branch, head, local).await?;
    start_app(ctx);
    Ok(())
}

/// Debug builds: a notebook held in memory, to try the interface without signing in.
#[cfg(debug_assertions)]
async fn demo() -> Rc<Ctx> {
    use gition::storage::Blob;
    const FILES: &[(&str, &str)] = &[
        (
            "INDEX.md",
            "# Demo

Welcome to **gition**. Click any block to edit it.

- [ ] a task
- [x] a done task
- see [Plan](Projects/plan.md)

> A quote

```
code block
```
",
        ),
        (
            "Projects/INDEX.md",
            "# Projects
",
        ),
        (
            "Projects/plan.md",
            "# Plan

1. one
2. two

Back [home](../INDEX.md).
",
        ),
        (
            "Ideas.md",
            "# Ideas

A <b>raw</b> tag stays text.
",
        ),
    ];
    let files = FILES.iter().map(|(p, c)| (p.into(), Blob::Data(c.as_bytes().to_vec()))).collect();
    let base = workspace::Snapshot { commit: "demo".into(), tree: "demo".into(), files, modes: Default::default() };
    let session = Session { token: String::new(), expires_at: None, app: None };
    Ctx::new(Api::new("demo", "notebook", session), "main".into(), base, Local::load("demo/notebook").await)
}

fn start_app(ctx: Rc<Ctx>) {
    let (cfg, warning) = match ctx.work.read_to_string(Path::new(FILE_NAME)) {
        Ok(text) => match Config::parse(&text) {
            Ok(c) => (c, None),
            Err(e) => (Config::default(), Some(format!("{e:#}"))),
        },
        Err(_) => (Config::default(), None),
    };
    let nb = Notebook::new(ctx, cfg, warning);
    set(Screen::Notebook(Box::new(nb)));
}

// ---- screens before the notebook ----------------------------------------------------

fn card(body: &str) {
    set_html("app", &format!(r#"<div class="center"><div class="card"><h1>gition</h1>{body}</div></div>"#));
}

fn draw(screen: &Screen) {
    match screen {
        Screen::Message(m) => card(&format!(r#"<p class="muted">{}</p>"#, escape(m))),
        Screen::Error(m) => card(&format!(
            r#"<p class="error">{}</p><div class="btns"><button data-act="pick">Pick another repository</button><button class="primary" data-act="reload">Reload</button></div>"#,
            escape(m)
        )),
        Screen::Login => card(
            r#"<p>A notebook stored as markdown in a GitHub repo.</p><button class="primary wide" data-act="login">Sign in with GitHub</button><p class="hint">Notes go straight from your browser to GitHub; this server stores nothing. gition only sees the repos you install the app on.</p>"#,
        ),
        Screen::Repos { repos, confirm, .. } => {
            let mut list: String =
                repos.iter().map(|r| repo_row(r, confirm.as_deref() == Some(r.full_name.as_str()))).collect();
            if repos.is_empty() {
                list = r#"<p class="muted">No repositories yet. Add the gition app to one.</p>"#.into();
            }
            card(&format!(
                r#"<p>Open a repository</p><div class="list">{list}</div><div class="btns"><button class="push-left" data-act="logout">Sign out</button><button data-act="refresh">Refresh</button><button data-act="install">Add repositories</button></div>"#
            ))
        }
        Screen::Empty { full_name, .. } => card(&format!(
            r#"<p><b>{}</b> is empty. Commit a first page to start a notebook?</p><div class="btns"><button data-act="pick">Pick another repository</button><button class="primary" data-act="init">Create</button></div>"#,
            escape(full_name)
        )),
        // The notebook draws itself.
        Screen::Notebook(_) => {}
    }
}

/// A row in the repo list, or how to remove it. Only GitHub can take a repo away from the app:
/// its API refuses that to the app's own sign-in, so the row links to the installation's settings.
fn repo_row(r: &Installed, confirm: bool) -> String {
    let name = escape(&r.full_name);
    if !confirm {
        return format!(
            r#"<div class="repo-row"><button class="result" data-act="repo" data-name="{name}">{name}</button><button class="ib" data-act="remove-ask" data-name="{name}" title="Remove from gition">{}</button></div>"#,
            dom::icons::TRASH
        );
    }
    let how = if r.all {
        let owner = escape(r.full_name.split('/').next().unwrap_or_default());
        format!(
            "gition can see all of <b>{owner}</b>'s repositories. On GitHub, choose <b>Only select \
             repositories</b> and leave <b>{name}</b> out."
        )
    } else {
        format!("On GitHub, remove <b>{name}</b> under <b>Repository access</b> and save.")
    };
    format!(
        r#"<div class="repo-row ask"><p>{how} The repository itself stays. This list refreshes when you come back.</p><div class="btns"><button data-act="remove-cancel">Cancel</button><button class="primary" data-act="settings" data-url="{}">Open GitHub settings</button></div></div>"#,
        escape(&r.settings)
    )
}

enum Then {
    Nothing,
    Navigate(String),
    OpenTab(String),
    Open(Session, String),
    Repos(Session),
    InitEmpty(Session, String),
}

/// A click on a screen before the notebook.
fn screen_click(screen: &mut Screen, act: &str, el: &web_sys::Element) -> Then {
    if let Screen::Repos { confirm, .. } = screen
        && matches!(act, "remove-ask" | "remove-cancel")
    {
        *confirm = data(el, "name").filter(|_| act == "remove-ask");
        draw(screen);
        return Then::Nothing;
    }
    match (&*screen, act) {
        (_, "reload") => {
            let _ = window().location().reload();
            Then::Nothing
        }
        (_, "pick") => Then::Navigate("/#repos".into()),
        (_, "logout") => {
            sign_out();
            Then::Nothing
        }
        (Screen::Login, "login") => Then::Navigate("/auth/login".into()),
        (Screen::Repos { session, .. }, "repo") => match data(el, "name") {
            Some(name) => Then::Open(session.clone(), name),
            None => Then::Nothing,
        },
        (Screen::Repos { session, .. }, "refresh") => Then::Repos(session.clone()),
        (_, "settings") => match data(el, "url") {
            Some(url) => Then::OpenTab(url),
            None => Then::Nothing,
        },
        (Screen::Repos { session, .. }, "install") => {
            let app = session.app.clone().unwrap_or_default();
            Then::OpenTab(format!("https://github.com/apps/{app}/installations/new"))
        }
        (Screen::Empty { session, full_name }, "init") => Then::InitEmpty(session.clone(), full_name.clone()),
        _ => Then::Nothing,
    }
}

fn run(then: Then) {
    let fail = |e: anyhow::Error| set(Screen::Error(format!("{e:#}")));
    match then {
        Then::Nothing => {}
        Then::Navigate(url) => {
            let _ = window().location().assign(&url);
            if url.starts_with("/#") {
                let _ = window().location().reload();
            }
        }
        Then::OpenTab(url) => {
            let _ = window().open_with_url_and_target_and_features(&url, "_blank", "noopener,noreferrer");
        }
        Then::Open(session, name) => {
            spawn_local(async move { open_repo(session, &name, None).await.unwrap_or_else(fail) })
        }
        Then::Repos(session) => spawn_local(async move { show_repos(session).await.unwrap_or_else(fail) }),
        Then::InitEmpty(session, name) => spawn_local(async move {
            let res = async {
                let (owner, repo_name) = name.split_once('/').unwrap_or_default();
                let api = Api::new(owner, repo_name, session.clone());
                let (path, content) = repo::starter_files(repo_name, &Config::default().assets_dir).remove(0);
                api.create_file(&path, &content, "gition: init").await?;
                open_repo(session, &name, None).await
            }
            .await;
            res.unwrap_or_else(fail)
        }),
    }
}

// ---- input --------------------------------------------------------------------

/// Converts a browser key event for the keymap; `None` for lone modifiers and unknown keys.
pub fn to_input(e: &KeyboardEvent) -> Option<Input> {
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

fn listen() {
    let doc = dom::document();
    let target: &web_sys::EventTarget = doc.as_ref();

    dom::on(target, "click", |e: MouseEvent| {
        if e.button() != 0 {
            return;
        }
        let then = try_with(|s| match s {
            Screen::Notebook(nb) => {
                nb.click(&e);
                Then::Nothing
            }
            other => match closest(e.target(), "[data-act]") {
                Some(el) => screen_click(other, &data(&el, "act").unwrap_or_default(), &el),
                None => Then::Nothing,
            },
        });
        run(then.unwrap_or(Then::Nothing));
    });

    dom::on(target, "keydown", |e: KeyboardEvent| {
        let used = try_with(|s| match s {
            Screen::Notebook(nb) => nb.keydown(&e),
            Screen::Login if e.key() == "Enter" => {
                let _ = window().location().assign("/auth/login");
                true
            }
            _ => false,
        });
        if used == Some(true) {
            e.prevent_default();
        }
    });

    dom::on(target, "input", |e: Event| with_notebook(|nb| nb.input(&e)));
    dom::on(target, "paste", |e: ClipboardEvent| with_notebook(|nb| nb.paste(&e)));

    // Editing ends when the focus leaves the block, but not halfway through a click: that
    // click may land on another block, which is found before this one is redrawn.
    dom::on(target, "focusout", |_: Event| {
        dom::timeout(0, || {
            if !PRESSING.with(Cell::get) {
                with_notebook(|nb| nb.focus_left());
            }
        });
    });
    dom::on(target, "pointerdown", |_: Event| PRESSING.with(|p| p.set(true)));
    for name in ["pointerup", "pointercancel"] {
        dom::on(target, name, |_: Event| {
            PRESSING.with(|p| p.set(false));
            dom::timeout(0, || with_notebook(|nb| nb.focus_left()));
        });
    }

    let w = window();
    // Saved edits are in the working copy; keep it when the tab goes away.
    let keep = || {
        try_with(|s| {
            if let Screen::Notebook(nb) = s {
                nb.persist();
            }
        });
    };
    dom::on(w.as_ref(), "beforeunload", move |_: Event| keep());
    dom::on(target, "visibilitychange", move |_: Event| {
        keep();
        // Back from GitHub's settings after removing a repo there: show the list as it is now.
        if !dom::document().hidden() {
            let then = try_with(|s| match s {
                Screen::Repos { session, confirm: Some(_), .. } => Then::Repos(session.clone()),
                _ => Then::Nothing,
            });
            run(then.unwrap_or(Then::Nothing));
        }
    });
    dom::on(w.as_ref(), "hashchange", |_: Event| {
        let _ = window().location().reload();
    });
    // The on-screen keyboard shrinks only the visual viewport, and iOS also pans it down the page to
    // keep the focused field in sight. Keep the app on the part of the screen that's showing.
    if let Some(vv) = w.visual_viewport() {
        dom::on(vv.as_ref(), "resize", |_: Event| fit_viewport(true));
        dom::on(vv.as_ref(), "scroll", |_: Event| fit_viewport(false));
    }
}

/// Sizes the (fixed) body to the visual viewport and moves it down by however far iOS panned that
/// viewport, so the bar stays at the top and nothing is left under the keyboard. On a resize, also
/// scrolls the page so the block being edited isn't behind the keyboard.
fn fit_viewport(resized: bool) {
    let w = window();
    let (Some(vv), Some(body)) = (w.visual_viewport(), w.document().and_then(|d| d.body())) else { return };
    // While zoomed in, the visual viewport is the zoomed area, not the keyboard's room.
    if (vv.scale() - 1.0).abs() > 0.01 {
        return;
    }
    let style = body.style();
    let _ = style.set_property("height", &format!("{}px", vv.height()));
    let _ = match vv.offset_top() {
        top if top > 0.5 => style.set_property("transform", &format!("translateY({top}px)")),
        _ => style.remove_property("transform").map(|_| ()),
    };
    if resized {
        reveal_editing();
    }
}

/// Scrolls the page so the textarea being edited starts in its upper part, if it's out of sight.
fn reveal_editing() {
    let (Some(scroll), Some(ta)) = (
        dom::by_id::<web_sys::Element>("scroll"),
        dom::document().active_element().filter(|e| e.tag_name() == "TEXTAREA"),
    ) else {
        return;
    };
    let (s, t) = (scroll.get_bounding_client_rect(), ta.get_bounding_client_rect());
    if t.top() > s.bottom() - 48.0 || t.bottom() < s.top() {
        scroll.set_scroll_top(scroll.scroll_top() + (t.top() - s.top() - s.height() / 3.0) as i32);
    }
}
