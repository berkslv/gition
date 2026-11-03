use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Result, bail};
use crossterm::event::{self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyEventKind};
use crossterm::execute;
use ratatui::DefaultTerminal;
use ratatui_image::picker::Picker;
use ratatui_textarea::Input;

use gition::app::App;
use gition::config::Config;
use gition::keymap::{Action, Keymap};
use gition::repo::{self, Found};
use gition::storage::FsStorage;
use gition::{git, import, ui};

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
    let _ = execute!(io::stdout(), EnableBracketedPaste);
    let picker = Picker::from_query_stdio().ok();
    let mut app = App::open_dir(root.clone(), cfg, picker);
    app.start_fetch();
    let result = event_loop(&mut terminal, &mut app);
    let _ = execute!(io::stdout(), DisableBracketedPaste);
    ratatui::restore();
    result?;

    let changes = git::changes(&root);
    if changes > 0 {
        println!("gition: {changes} uncommitted change(s). Press {} in gition to sync.", app.keys.label(Action::Sync));
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

fn event_loop(terminal: &mut DefaultTerminal, app: &mut App) -> Result<()> {
    while !app.quit {
        terminal.draw(|f| ui::draw(f, app))?;
        if event::poll(Duration::from_millis(100))? {
            match event::read()? {
                // Windows also reports key releases; only act on presses.
                Event::Key(k) if k.kind != KeyEventKind::Release => app.on_key(Input::from(k)),
                Event::Paste(s) => app.on_paste(s),
                _ => {}
            }
        }
        app.tick();
    }
    Ok(())
}
