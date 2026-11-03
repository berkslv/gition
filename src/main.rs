use std::io::{self, BufRead, IsTerminal, Write};
use std::path::PathBuf;
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
use gition::keymap::Action;
use gition::repo::{self, Found};
use gition::{git, ui};

const USAGE: &str = "gition - a notebook for the terminal, stored as markdown in git

Usage: gition [PATH]

Opens the gition repo containing PATH (default: current directory).
A gition repo is any directory with an INDEX.md; gition offers to create one.
Press ? inside gition for keys.";

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
    if args.len() > 1 {
        bail!("expected at most one path\n\n{USAGE}");
    }
    if !git::available() {
        bail!("git was not found on PATH; install git and try again");
    }
    let start = match args.first() {
        Some(p) => PathBuf::from(p),
        None => std::env::current_dir()?,
    };
    if !start.is_dir() {
        bail!("{} is not a directory", start.display());
    }

    let root = match repo::discover(&start)? {
        Found::Repo(root) => root,
        Found::NeedsInit(dir) => {
            print!("No INDEX.md found. Initialize gition in {}? [y/N] ", dir.display());
            io::stdout().flush()?;
            let mut answer = String::new();
            io::stdin().lock().read_line(&mut answer)?;
            if !answer.trim().eq_ignore_ascii_case("y") {
                return Ok(());
            }
            repo::init(&dir, &Config::load(&dir)?.assets_dir)?;
            dir
        }
    };
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
