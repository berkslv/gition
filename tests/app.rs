//! Drives the app through key events and renders it with a test backend.

use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::Sender;

use gition::app::App;
use gition::config::Config;
use gition::host::{Host, Msg, Open};
use gition::images::Images;
use gition::storage::{Blob, Files, MemStorage, Storage};
use gition::ui;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui_textarea::{Input, Key};

fn write(root: &Path, rel: &str, content: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, content).unwrap();
}

fn workspace() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("notes");
    write(&root, "INDEX.md", "# Notes\n\nSee [the plan](Projects/plan.md) and [ideas](ideas.md).\n\n- one\n- two\n");
    write(&root, "ideas.md", "# Ideas\n\nBack to [home](INDEX.md).\n");
    write(&root, "Projects/INDEX.md", "# Projects\n");
    write(&root, "Projects/plan.md", "# Plan\n\n![diagram](../assets/d.png)\n\n[ideas](../ideas.md)\n");
    write(&root, "assets/d.png", "not really a png");
    (tmp, root)
}

fn key(app: &mut App, key: Key) {
    app.on_key(Input { key, ..Input::default() });
}

fn typ(app: &mut App, s: &str) {
    for c in s.chars() {
        key(app, Key::Char(c));
    }
}

fn render(app: &mut App) -> Terminal<TestBackend> {
    let mut t = Terminal::new(TestBackend::new(90, 16)).unwrap();
    t.draw(|f| ui::draw(f, app)).unwrap();
    t
}

fn select(app: &mut App, path: &str) {
    let i = app.visible.iter().position(|v| v.path == Path::new(path)).unwrap_or_else(|| panic!("{path} not in tree"));
    app.sel = i;
}

#[test]
fn renders_tree_and_page() {
    let (_tmp, root) = workspace();
    let mut app = App::open_dir(root, Config::default(), None);
    let t = render(&mut app);
    insta::assert_snapshot!(t.backend());
}

#[test]
fn rename_moves_page_and_rewrites_links() {
    let (_tmp, root) = workspace();
    let mut app = App::open_dir(root.clone(), Config::default(), None);
    key(&mut app, Key::Enter); // expand root -> opens INDEX
    select(&mut app, "Projects");
    key(&mut app, Key::Enter);
    select(&mut app, "Projects/plan.md");
    key(&mut app, Key::Char('r'));
    for _ in 0.."Projects/plan".len() {
        key(&mut app, Key::Backspace);
    }
    typ(&mut app, "Archive/old plan");
    key(&mut app, Key::Enter);

    assert!(root.join("Archive/old plan.md").exists());
    assert!(!root.join("Projects/plan.md").exists());
    let index = fs::read_to_string(root.join("INDEX.md")).unwrap();
    assert!(index.contains("[the plan](Archive/old%20plan.md)"), "{index}");
    let moved = fs::read_to_string(root.join("Archive/old plan.md")).unwrap();
    assert!(moved.contains("![diagram](../assets/d.png)"), "{moved}");
    assert!(moved.contains("[ideas](../ideas.md)"), "{moved}");
}

#[test]
fn new_page_opens_editor_and_saves() {
    let (_tmp, root) = workspace();
    let mut app = App::open_dir(root.clone(), Config::default(), None);
    select(&mut app, "Projects");
    key(&mut app, Key::Char('n'));
    typ(&mut app, "Roadmap");
    key(&mut app, Key::Enter);
    assert!(app.editor.is_some());
    typ(&mut app, "q1 goals");
    app.on_key(Input { key: Key::Char('s'), ctrl: true, ..Input::default() });
    key(&mut app, Key::Esc);
    assert!(app.editor.is_none());
    assert_eq!(fs::read_to_string(root.join("Projects/Roadmap.md")).unwrap(), "# Roadmap\nq1 goals");
}

#[test]
fn escape_with_unsaved_changes_asks() {
    let (_tmp, root) = workspace();
    let mut app = App::open_dir(root.clone(), Config::default(), None);
    key(&mut app, Key::Tab);
    key(&mut app, Key::Char('e'));
    typ(&mut app, "x");
    key(&mut app, Key::Esc);
    let t = render(&mut app);
    assert!(format!("{:?}", t.backend()).contains("Save changes?"));
    key(&mut app, Key::Char('n'));
    assert!(app.editor.is_none());
    assert!(fs::read_to_string(root.join("INDEX.md")).unwrap().starts_with("# Notes"));
}

#[test]
fn delete_warns_about_incoming_links() {
    let (_tmp, root) = workspace();
    let mut app = App::open_dir(root.clone(), Config::default(), None);
    select(&mut app, "ideas.md");
    key(&mut app, Key::Char('d'));
    let t = render(&mut app);
    assert!(format!("{:?}", t.backend()).contains("2 pages link here"));
    key(&mut app, Key::Char('y'));
    assert!(!root.join("ideas.md").exists());
}

#[test]
fn insert_image_copies_into_assets() {
    let (tmp, root) = workspace();
    let src = tmp.path().join("photo shot.png");
    fs::write(&src, "png bytes").unwrap();
    let mut app = App::open_dir(root.clone(), Config::default(), None);
    select(&mut app, "Projects");
    key(&mut app, Key::Enter); // opens Projects/INDEX.md
    key(&mut app, Key::Char('i'));
    app.on_paste(format!("\"{}\"", src.display()));
    key(&mut app, Key::Enter);
    assert_eq!(fs::read(root.join("assets/photo shot.png")).unwrap(), b"png bytes");
    let page = fs::read_to_string(root.join("Projects/INDEX.md")).unwrap();
    assert_eq!(page, "# Projects\n\n![photo shot](../assets/photo%20shot.png)\n");
}

#[test]
fn search_opens_page() {
    let (_tmp, root) = workspace();
    let mut app = App::open_dir(root, Config::default(), None);
    key(&mut app, Key::Char('/'));
    typ(&mut app, "plan");
    key(&mut app, Key::Enter);
    assert_eq!(app.page.as_ref().unwrap().path, Path::new("Projects/plan.md"));
    assert_eq!(app.visible[app.sel].path, Path::new("Projects/plan.md"));
}

/// A host that never syncs, for running the app on an in-memory working copy like the web build.
struct Offline;

impl Host for Offline {
    fn fetch(&self, _tx: Sender<Msg>) {}
    fn sync(&self, _prefix: &str, _tx: Sender<Msg>) {}
    fn changes(&self) -> usize {
        0
    }
    fn open(&self, _target: Open) -> anyhow::Result<()> {
        Ok(())
    }
}

#[test]
fn works_on_in_memory_storage() {
    let files: Files = [
        ("INDEX.md", "# Notes\n\n[the plan](Projects/plan.md)\n"),
        ("Projects/INDEX.md", "# Projects\n"),
        ("Projects/plan.md", "# Plan\n\n[home](../INDEX.md)\n"),
    ]
    .into_iter()
    .map(|(p, c)| (PathBuf::from(p), Blob::Data(c.as_bytes().to_vec())))
    .collect();
    let fs = Rc::new(MemStorage::new("notes", files));
    let mut app =
        App::new(Box::new(fs.clone()), Box::new(Offline), Config::default(), Images::new(PathBuf::new(), None));
    key(&mut app, Key::Enter);
    select(&mut app, "Projects");
    key(&mut app, Key::Enter);
    select(&mut app, "Projects/plan.md");
    key(&mut app, Key::Char('r'));
    for _ in 0.."Projects/plan".len() {
        key(&mut app, Key::Backspace);
    }
    typ(&mut app, "plan");
    key(&mut app, Key::Enter);

    let read = |p: &str| fs.read_to_string(Path::new(p)).unwrap();
    assert_eq!(read("INDEX.md"), "# Notes\n\n[the plan](plan.md)\n");
    assert_eq!(read("plan.md"), "# Plan\n\n[home](INDEX.md)\n");
    assert!(!fs.exists(Path::new("Projects/plan.md")));

    select(&mut app, "Projects");
    key(&mut app, Key::Char('d'));
    key(&mut app, Key::Char('y'));
    assert!(!fs.exists(Path::new("Projects")));
}

/// Where a tree row was drawn in the last frame.
fn row_of(app: &App, path: &str) -> (u16, u16) {
    let i = app.visible.iter().position(|v| v.path == Path::new(path)).unwrap();
    let (r, _) = app.hits.iter().find(|(_, t)| matches!(t, gition::pointer::Target::TreeRow(n) if *n == i)).unwrap();
    (r.x, r.y)
}

#[test]
fn touch_opens_edits_and_uses_the_menu() {
    let (_tmp, root) = workspace();
    let mut app = App::open_dir(root.clone(), Config::default(), None);
    app.touch = true;
    key(&mut app, Key::Enter); // expand the root
    render(&mut app);

    // Tap a page in the tree to open it; a tap on a link line follows it.
    let (x, y) = row_of(&app, "ideas.md");
    app.tap(x, y);
    assert_eq!(app.page.as_ref().unwrap().path, Path::new("ideas.md"));
    let t = render(&mut app);
    assert!(format!("{:?}", t.backend()).contains("☰ Menu"));

    // Double tap the page to edit it, then save and leave through the menu.
    app.double_tap(60, 3);
    assert!(app.editor.is_some() && app.wants_text());
    typ(&mut app, "x");
    render(&mut app);
    app.tap(1, 15); // the status bar
    let t = render(&mut app);
    let screen = format!("{:?}", t.backend());
    assert!(screen.contains("Done editing"), "{screen}");
    let save = app.hits.iter().find(|(_, t)| matches!(t, gition::pointer::Target::MenuItem(0))).unwrap().0;
    app.tap(save.x, save.y);
    assert!(fs::read_to_string(root.join("ideas.md")).unwrap().starts_with('x'));

    // Long press a tree row for its menu; tapping outside closes it.
    render(&mut app);
    let (x, y) = row_of(&app, "Projects");
    app.long_press(x, y);
    assert!(matches!(app.modal, gition::app::Modal::Menu { .. }));
    assert_eq!(app.visible[app.sel].path, Path::new("Projects"));
    render(&mut app);
    app.tap(0, 0);
    assert!(matches!(app.modal, gition::app::Modal::None));
}
