//! Drives the app through key events and renders it with a test backend.

use std::fs;
use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use gition::app::App;
use gition::config::Config;
use gition::ui;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

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

fn key(app: &mut App, code: KeyCode) {
    app.on_key(KeyEvent::new(code, KeyModifiers::NONE));
}

fn typ(app: &mut App, s: &str) {
    for c in s.chars() {
        key(app, KeyCode::Char(c));
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
    let mut app = App::new(root, Config::default(), None);
    let t = render(&mut app);
    insta::assert_snapshot!(t.backend());
}

#[test]
fn rename_moves_page_and_rewrites_links() {
    let (_tmp, root) = workspace();
    let mut app = App::new(root.clone(), Config::default(), None);
    key(&mut app, KeyCode::Enter); // expand root -> opens INDEX
    select(&mut app, "Projects");
    key(&mut app, KeyCode::Enter);
    select(&mut app, "Projects/plan.md");
    key(&mut app, KeyCode::Char('r'));
    for _ in 0.."Projects/plan".len() {
        key(&mut app, KeyCode::Backspace);
    }
    typ(&mut app, "Archive/old plan");
    key(&mut app, KeyCode::Enter);

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
    let mut app = App::new(root.clone(), Config::default(), None);
    select(&mut app, "Projects");
    key(&mut app, KeyCode::Char('n'));
    typ(&mut app, "Roadmap");
    key(&mut app, KeyCode::Enter);
    assert!(app.editor.is_some());
    typ(&mut app, "q1 goals");
    app.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
    key(&mut app, KeyCode::Esc);
    assert!(app.editor.is_none());
    assert_eq!(fs::read_to_string(root.join("Projects/Roadmap.md")).unwrap(), "# Roadmap\nq1 goals");
}

#[test]
fn escape_with_unsaved_changes_asks() {
    let (_tmp, root) = workspace();
    let mut app = App::new(root.clone(), Config::default(), None);
    key(&mut app, KeyCode::Tab);
    key(&mut app, KeyCode::Char('e'));
    typ(&mut app, "x");
    key(&mut app, KeyCode::Esc);
    let t = render(&mut app);
    assert!(format!("{:?}", t.backend()).contains("Save changes?"));
    key(&mut app, KeyCode::Char('n'));
    assert!(app.editor.is_none());
    assert!(fs::read_to_string(root.join("INDEX.md")).unwrap().starts_with("# Notes"));
}

#[test]
fn delete_warns_about_incoming_links() {
    let (_tmp, root) = workspace();
    let mut app = App::new(root.clone(), Config::default(), None);
    select(&mut app, "ideas.md");
    key(&mut app, KeyCode::Char('d'));
    let t = render(&mut app);
    assert!(format!("{:?}", t.backend()).contains("2 pages link here"));
    key(&mut app, KeyCode::Char('y'));
    assert!(!root.join("ideas.md").exists());
}

#[test]
fn insert_image_copies_into_assets() {
    let (tmp, root) = workspace();
    let src = tmp.path().join("photo shot.png");
    fs::write(&src, "png bytes").unwrap();
    let mut app = App::new(root.clone(), Config::default(), None);
    select(&mut app, "Projects");
    key(&mut app, KeyCode::Enter); // opens Projects/INDEX.md
    key(&mut app, KeyCode::Char('i'));
    app.on_paste(format!("\"{}\"", src.display()));
    key(&mut app, KeyCode::Enter);
    assert_eq!(fs::read(root.join("assets/photo shot.png")).unwrap(), b"png bytes");
    let page = fs::read_to_string(root.join("Projects/INDEX.md")).unwrap();
    assert_eq!(page, "# Projects\n\n![photo shot](../assets/photo%20shot.png)\n");
}

#[test]
fn search_opens_page() {
    let (_tmp, root) = workspace();
    let mut app = App::new(root, Config::default(), None);
    key(&mut app, KeyCode::Char('/'));
    typ(&mut app, "plan");
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.page.as_ref().unwrap().path, Path::new("Projects/plan.md"));
    assert_eq!(app.visible[app.sel].path, Path::new("Projects/plan.md"));
}
