use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event, KeyEvent,
    KeyEventKind, KeyModifiers, KeyboardEnhancementFlags, MouseButton, MouseEvent, MouseEventKind,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use ratatui::DefaultTerminal;
use ratatui_image::picker::Picker;
use ratatui_textarea::Input;

use gition::app::App;
use gition::config::Config;
use gition::keymap::{Action, Keymap};
use gition::repo::{self, Found};
use gition::storage::FsStorage;
use gition::{git, import, ui, update};

const USAGE: &str = "gition - a notebook for the terminal, stored as markdown in git

Usage: gition [PATH]
       gition [PATH] import notion [--database-as-table] EXPORT_DIR

Opens the gition repo containing PATH (default: current directory).
A gition repo is any directory with an INDEX.md; gition offers to create one.
Press ? inside gition for keys.

import notion copies an unzipped Notion export (Markdown & CSV) into PATH,
as gition pages, with its attachments in the assets folder. With
--database-as-table, a database becomes a page with a plain table, without a
page per row.";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("gition: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return Ok(());
    }
    if args.iter().any(|a| a == "-V" || a == "--version") {
        println!("gition {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if let Some(i) = args.iter().position(|a| a == "import").filter(|&i| i <= 1 && args.len() > i + 1) {
        return import(args[..i].first(), &args[i + 1..]);
    }
    if args.len() > 1 {
        bail!("expected at most one path\n\n{USAGE}");
    }
    let start = start_dir(args.first())?;
    let Some(root) = open_repo(&start)? else { return Ok(()) };
    let cfg = Config::load(&root)?;
    if !io::stdout().is_terminal() || !io::stdin().is_terminal() {
        bail!("gition needs an interactive terminal");
    }

    let mut terminal = ratatui::init();
    let _ = execute!(io::stdout(), EnableBracketedPaste, EnableMouseCapture);
    // Terminals with the kitty keyboard protocol can report Cmd, so Cmd+Z and Cmd+X work on macOS.
    let enhanced = crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false);
    if enhanced {
        let _ =
            execute!(io::stdout(), PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES));
    }
    // ratatui's panic hook restores the screen but not the mouse.
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_input(enhanced);
        hook(info);
    }));
    let (update_tx, update_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        if let Some(v) = update::check() {
            let _ = update_tx.send(v);
        }
    });
    let picker = Picker::from_query_stdio().ok();
    let mut app = App::open_dir(root.clone(), cfg, picker);
    app.start_fetch();
    let mut newer = None;
    let result = event_loop(&mut terminal, &mut app, &update_rx, &mut newer);
    restore_input(enhanced);
    ratatui::restore();
    result?;

    let changes = git::changes(&root);
    if changes > 0 {
        println!("gition: {changes} uncommitted change(s). Press {} in gition to sync.", app.keys.label(Action::Sync));
    }
    if let Some(v) = newer {
        println!("gition: Update available! {} → {v}. Run: {}", env!("CARGO_PKG_VERSION"), update::COMMAND);
    }
    Ok(())
}

/// `gition [PATH] import notion [--database-as-table] EXPORT_DIR`: imports into PATH.
/// Doesn't commit; that's for sync.
fn import(path: Option<&String>, args: &[String]) -> Result<()> {
    let mut opts = import::Options::default();
    let mut rest: Vec<&String> = Vec::new();
    for a in args {
        match a.as_str() {
            "--database-as-table" => opts.databases_as_tables = true,
            s if s.starts_with("--") => bail!("unknown option '{s}'\n\n{USAGE}"),
            _ => rest.push(a),
        }
    }
    let [kind, export] = rest[..] else { bail!("expected an importer and a directory\n\n{USAGE}") };
    if kind != "notion" {
        bail!("unknown importer '{kind}'; only 'notion' is supported");
    }
    let export = PathBuf::from(export);
    if !export.is_dir() {
        bail!("{} is not a directory; unzip the Notion export first", export.display());
    }
    let start = start_dir(path)?;
    let Some(root) = open_repo(&start)? else { return Ok(()) };
    let into = repo::dunce(start.canonicalize()?).strip_prefix(&root).map(Path::to_path_buf).unwrap_or_default();
    let cfg = Config::load(&root)?;
    let s = import::notion(&FsStorage::new(export), &FsStorage::new(root.clone()), &into, &cfg.assets_dir, opts)?;
    println!(
        "Imported {} pages, {} databases and {} files into {}. Press {} in gition to sync.",
        s.pages,
        s.databases,
        s.files,
        root.join(&into).components().as_path().display(),
        Keymap::from_config(&cfg).0.label(Action::Sync)
    );
    Ok(())
}

fn start_dir(path: Option<&String>) -> Result<PathBuf> {
    let start = match path {
        Some(p) => PathBuf::from(p),
        None => std::env::current_dir()?,
    };
    if !start.is_dir() {
        bail!("{} is not a directory", start.display());
    }
    Ok(start)
}

/// The gition repo containing `start`, offering to create one. `None` if the user declines.
fn open_repo(start: &Path) -> Result<Option<PathBuf>> {
    if !git::available() {
        bail!("git was not found on PATH; install git and try again");
    }
    Ok(Some(match repo::discover(start)? {
        Found::Repo(root) => root,
        Found::NeedsInit(dir) => {
            print!("No INDEX.md found. Initialize gition in {}? [y/N] ", dir.display());
            io::stdout().flush()?;
            let mut answer = String::new();
            io::stdin().lock().read_line(&mut answer)?;
            if !answer.trim().eq_ignore_ascii_case("y") {
                return Ok(None);
            }
            repo::init(&dir, &Config::load(&dir)?.assets_dir)?;
            dir
        }
    }))
}

/// Undoes the input modes set up at start.
fn restore_input(enhanced: bool) {
    if enhanced {
        let _ = execute!(io::stdout(), PopKeyboardEnhancementFlags);
    }
    let _ = execute!(io::stdout(), DisableMouseCapture, DisableBracketedPaste);
}

/// Cmd (super) counts as Ctrl, like the web app's Meta.
fn key_input(k: KeyEvent) -> Input {
    let mut input = Input::from(k);
    input.ctrl |= k.modifiers.contains(KeyModifiers::SUPER);
    input
}

/// `newer` gets the version from the update check, if it finds one while gition runs.
fn event_loop(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    updates: &Receiver<String>,
    newer: &mut Option<String>,
) -> Result<()> {
    let mut mouse = Mouse::default();
    while !app.quit {
        if let Ok(v) = updates.try_recv() {
            app.info(format!("Update available! gition {v}. The update command is shown when you quit."));
            *newer = Some(v);
        }
        terminal.draw(|f| ui::draw(f, app))?;
        if event::poll(Duration::from_millis(100))? {
            match event::read()? {
                // Windows also reports key releases; only act on presses.
                Event::Key(k) if k.kind != KeyEventKind::Release => app.on_key(key_input(k)),
                Event::Paste(s) => app.on_paste(s),
                Event::Mouse(m) => mouse.on(app, m),
                _ => {}
            }
        }
        app.tick();
    }
    Ok(())
}

/// Two clicks on the same cell within this long make a double click.
const DOUBLE_CLICK: Duration = Duration::from_millis(400);
/// Rows a wheel notch scrolls.
const WHEEL_ROWS: isize = 3;

/// The left button from press to release: a click, a double click, or a drag that selects.
#[derive(Default)]
struct Mouse {
    down: Option<(u16, u16)>,
    dragged: bool,
    last_click: Option<(Instant, u16, u16)>,
}

impl Mouse {
    fn on(&mut self, app: &mut App, m: MouseEvent) {
        let at = (m.column, m.row);
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.down = Some(at);
                self.dragged = false;
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some(from) = self.down {
                    self.dragged = true;
                    app.drag(from, at);
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                if self.down.take().is_none() {
                    return;
                }
                if self.dragged {
                    self.last_click = None;
                    return app.drag_end();
                }
                let double = self.last_click.is_some_and(|(t, x, y)| t.elapsed() < DOUBLE_CLICK && (x, y) == at);
                self.last_click = if double { None } else { Some((Instant::now(), at.0, at.1)) };
                if double {
                    app.double_tap(at.0, at.1);
                } else {
                    app.tap(at.0, at.1);
                }
            }
            // A right click is the long press of a mouse.
            MouseEventKind::Down(MouseButton::Right) => app.long_press(at.0, at.1),
            MouseEventKind::ScrollDown => app.scroll(at.0, at.1, WHEEL_ROWS),
            MouseEventKind::ScrollUp => app.scroll(at.0, at.1, -WHEEL_ROWS),
            _ => {}
        }
    }
}
