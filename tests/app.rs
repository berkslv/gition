//! Drives the app through key events and renders it with a test backend.

use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::Sender;

use gition::app::{App, Focus};
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
    select(&mut app, "Projects");
    key(&mut app, Key::Right);
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

#[cfg(unix)]
fn run_command(app: &mut App, cmd: &str) {
    key(app, Key::Char('!'));
    typ(app, cmd);
    key(app, Key::Enter);
    let start = std::time::Instant::now();
    while app.running {
        assert!(start.elapsed().as_secs() < 10, "command didn't finish");
        std::thread::sleep(std::time::Duration::from_millis(10));
        app.tick();
    }
}

#[cfg(unix)]
#[test]
fn shell_command_runs_in_repo() {
    let (_tmp, root) = workspace();
    let mut app = App::open_dir(root.clone(), Config::default(), None);
    key(&mut app, Key::Char('!'));
    typ(&mut app, "echo hi");
    let t = render(&mut app);
    assert!(format!("{:?}", t.backend()).contains(" ! echo hi"));
    key(&mut app, Key::Esc);

    run_command(&mut app, "echo '# New' > new.md && echo done");
    assert!(root.join("new.md").exists());
    assert_eq!(app.message.as_ref().map(|m| m.0.as_str()), Some("done"));
    assert!(app.visible.iter().any(|v| v.path == Path::new("new.md")), "tree reloads");

    run_command(&mut app, "printf 'one\\ntwo\\n'; exit 3");
    let t = render(&mut app);
    let screen = format!("{:?}", t.backend());
    assert!(screen.contains("(failed)") && screen.contains("one") && screen.contains("two"), "{screen}");
    key(&mut app, Key::Esc);
    assert!(matches!(app.modal, gition::app::Modal::None));
}

#[test]
fn rename_to_existing_folder_moves_into_it() {
    let (_tmp, root) = workspace();
    let mut app = App::open_dir(root.clone(), Config::default(), None);
    select(&mut app, "ideas.md");
    key(&mut app, Key::Char('r'));
    key(&mut app, Key::Home);
    typ(&mut app, "Projects/");
    key(&mut app, Key::End);
    for _ in 0.."ideas".len() {
        key(&mut app, Key::Backspace);
    }
    key(&mut app, Key::Enter);

    assert!(root.join("Projects/ideas.md").exists());
    assert!(!root.join("ideas.md").exists());
    let index = fs::read_to_string(root.join("INDEX.md")).unwrap();
    assert!(index.contains("[ideas](Projects/ideas.md)"), "{index}");
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
fn delete_confirms_with_enter() {
    let (_tmp, root) = workspace();
    let mut app = App::open_dir(root.clone(), Config::default(), None);
    select(&mut app, "Projects");
    key(&mut app, Key::Char('d'));
    key(&mut app, Key::Enter);
    assert!(!root.join("Projects").exists());
}

#[test]
fn insert_image_copies_into_assets() {
    let (tmp, root) = workspace();
    let src = tmp.path().join("photo shot.png");
    fs::write(&src, "png bytes").unwrap();
    let mut app = App::open_dir(root.clone(), Config::default(), None);
    select(&mut app, "Projects");
    key(&mut app, Key::Right); // opens Projects/INDEX.md
    key(&mut app, Key::Char('i'));
    app.on_paste(format!("\"{}\"", src.display()));
    key(&mut app, Key::Enter);
    assert_eq!(fs::read(root.join("assets/photo shot.png")).unwrap(), b"png bytes");
    let page = fs::read_to_string(root.join("Projects/INDEX.md")).unwrap();
    assert_eq!(page, "# Projects\n\n![photo shot](../assets/photo%20shot.png)\n");
}

#[test]
fn tree_opens_pages_and_enter_on_a_page_edits_it() {
    let (_tmp, root) = workspace();
    let mut app = App::open_dir(root, Config::default(), None);
    select(&mut app, "ideas.md");
    key(&mut app, Key::Enter);
    assert_eq!(app.page.as_ref().unwrap().path, Path::new("ideas.md"));
    assert!(app.editor.is_none() && app.focus == Focus::Page);
    key(&mut app, Key::Enter);
    assert_eq!(app.editor.as_ref().unwrap().path, Path::new("ideas.md"));
    key(&mut app, Key::Esc);
    key(&mut app, Key::Tab);

    // The first press opens a closed folder and stays in the tree; the second goes into its page.
    select(&mut app, "Projects");
    key(&mut app, Key::Right);
    assert!(app.focus == Focus::Tree);
    assert!(app.visible.iter().any(|v| v.path == Path::new("Projects/plan.md")));
    key(&mut app, Key::Right);
    assert_eq!(app.page.as_ref().unwrap().path, Path::new("Projects/INDEX.md"));
    assert!(app.editor.is_none() && app.focus == Focus::Page);
}

#[test]
fn ctrl_c_quits_and_asks_to_save_from_the_editor() {
    let (_tmp, root) = workspace();
    let ctrl_c = Input { key: Key::Char('c'), ctrl: true, ..Input::default() };
    let mut app = App::open_dir(root.clone(), Config::default(), None);
    select(&mut app, "ideas.md");
    key(&mut app, Key::Enter);
    key(&mut app, Key::Enter);
    typ(&mut app, "q");
    assert!(!app.quit);
    app.on_key(ctrl_c.clone());
    assert!(matches!(app.modal, gition::app::Modal::SaveChanges(_)));
    key(&mut app, Key::Char('n'));
    assert!(app.quit);

    let mut app = App::open_dir(root, Config::default(), None);
    app.on_key(ctrl_c);
    assert!(app.quit);
}

#[test]
fn ctrl_a_selects_everything_in_the_editor() {
    let (_tmp, root) = workspace();
    let mut app = App::open_dir(root, Config::default(), None);
    select(&mut app, "ideas.md");
    key(&mut app, Key::Enter);
    key(&mut app, Key::Enter);
    app.on_key(Input { key: Key::Char('a'), ctrl: true, ..Input::default() });
    let text = app.editor.as_ref().unwrap().selected_text().unwrap();
    assert!(text.starts_with("# Ideas") && text.contains("Back to [home](INDEX.md)."), "{text}");

    // ←/→ collapse the selection to its start or end.
    key(&mut app, Key::Right);
    let e = app.editor.as_ref().unwrap();
    assert!(e.selected_text().is_none());
    let last = e.ta.lines().len() - 1;
    assert_eq!(e.ta.cursor(), (last, e.ta.lines()[last].chars().count()));
    app.on_key(Input { key: Key::Char('a'), ctrl: true, ..Input::default() });
    key(&mut app, Key::Left);
    let e = app.editor.as_ref().unwrap();
    assert!(e.selected_text().is_none());
    assert_eq!(e.ta.cursor(), (0, 0));
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
    select(&mut app, "Projects");
    key(&mut app, Key::Right);
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

/// An offline host that keeps what it was asked to copy.
struct Clipboard(Rc<RefCell<Vec<String>>>);

impl Host for Clipboard {
    fn fetch(&self, _tx: Sender<Msg>) {}
    fn sync(&self, _prefix: &str, _tx: Sender<Msg>) {}
    fn changes(&self) -> usize {
        0
    }
    fn open(&self, _target: Open) -> anyhow::Result<()> {
        Ok(())
    }
    fn copy(&self, text: &str) -> bool {
        self.0.borrow_mut().push(text.to_string());
        true
    }
}

/// The screen row and first column of the first viewer row containing `text`.
fn view_row_of(app: &App, text: &str) -> (u16, u16) {
    let i = app
        .view
        .rows
        .iter()
        .position(|r| matches!(r, gition::app::Row::Text { line, .. } if line.to_string().contains(text)))
        .unwrap();
    (app.view.area.x, app.view.area.y + (i - app.view.scroll) as u16)
}

#[test]
fn dragging_selects_and_copies() {
    let files: Files =
        [("INDEX.md", "# Notes\n\nalpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu\n\nnext\n")]
            .into_iter()
            .map(|(p, c)| (PathBuf::from(p), Blob::Data(c.as_bytes().to_vec())))
            .collect();
    let copied = Rc::new(RefCell::new(Vec::new()));
    let mut app = App::new(
        Box::new(MemStorage::new("notes", files)),
        Box::new(Clipboard(copied.clone())),
        Config::default(),
        Images::new(PathBuf::new(), None),
    );
    app.load_page(PathBuf::from("INDEX.md"));
    render(&mut app);

    // In the viewer: both end cells are included, and the selection is drawn.
    let (x, y) = view_row_of(&app, "alpha");
    app.drag((x + 6, y), (x + 8, y));
    app.drag((x + 6, y), (x + 9, y));
    app.drag_end();
    assert_eq!(copied.borrow().last().unwrap(), "beta");
    let t = render(&mut app);
    assert_eq!(t.backend().buffer()[(x + 7, y)].style().bg, Some(gition::config::Theme::default().selection));

    // Wrapped rows join back into one line; separate blocks keep their line breaks.
    let (_, below) = view_row_of(&app, "next");
    app.drag((x, y), (x + 3, below));
    app.drag_end();
    assert_eq!(
        copied.borrow().last().unwrap(),
        "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu\n\nnext"
    );

    // A click or key clears it.
    app.tap(x, y);
    assert!(app.selection.is_none());

    // In the editor: from the pressed cell up to the one under the pointer.
    key(&mut app, Key::Char('e'));
    let t = render(&mut app);
    let area = app
        .hits
        .iter()
        .find_map(|(_, t)| match t {
            gition::pointer::Target::Editor(a) => Some(*a),
            _ => None,
        })
        .unwrap();
    app.drag((area.x, area.y), (area.x + 2, area.y + 2));
    app.drag_end();
    assert_eq!(copied.borrow().last().unwrap(), "# Notes\n\nal");
    drop(t);
    // Typing still works and replaces the selection.
    typ(&mut app, "X");
    assert_eq!(
        app.editor.as_ref().unwrap().ta.lines()[0],
        "Xpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu"
    );
}

/// An offline host with canned page titles and a clipboard image. Remembers the URLs it fetched.
struct Paster {
    titles: Vec<(&'static str, &'static str)>,
    fetched: Rc<RefCell<Vec<String>>>,
    image: Option<Vec<u8>>,
}

impl Host for Paster {
    fn fetch(&self, _tx: Sender<Msg>) {}
    fn sync(&self, _prefix: &str, _tx: Sender<Msg>) {}
    fn changes(&self) -> usize {
        0
    }
    fn open(&self, _target: Open) -> anyhow::Result<()> {
        Ok(())
    }
    fn fetch_title(&self, url: &str, tx: Sender<Msg>) {
        self.fetched.borrow_mut().push(url.to_string());
        let title = self.titles.iter().find(|(u, _)| *u == url).map(|(_, t)| t.to_string());
        let _ = tx.send(Msg::Title(url.to_string(), title));
    }
    fn paste_image(&self) -> Option<Vec<u8>> {
        self.image.clone()
    }
}

/// An app editing `INDEX.md` (`# Notes`), with the cursor at the end of its first line.
fn paste_app(cfg: Config, image: Option<Vec<u8>>) -> (App, Rc<MemStorage>, Rc<RefCell<Vec<String>>>) {
    let files: Files = [(PathBuf::from("INDEX.md"), Blob::Data(b"# Notes\n".to_vec()))].into_iter().collect();
    let fs = Rc::new(MemStorage::new("notes", files));
    let fetched = Rc::new(RefCell::new(Vec::new()));
    let host = Paster { titles: vec![("https://medium.com", "Medium [blog]")], fetched: fetched.clone(), image };
    let mut app = App::new(Box::new(fs.clone()), Box::new(host), cfg, Images::new(PathBuf::new(), None));
    app.load_page(PathBuf::from("INDEX.md"));
    app.focus = Focus::Page;
    key(&mut app, Key::Char('e'));
    key(&mut app, Key::End);
    typ(&mut app, " ");
    (app, fs, fetched)
}

fn first_line(app: &App) -> String {
    app.editor.as_ref().unwrap().ta.lines()[0].clone()
}

#[test]
fn pasted_url_becomes_a_titled_link() {
    let (mut app, _, fetched) = paste_app(Config::default(), None);
    app.on_paste("https://medium.com\n".into());
    assert_eq!(first_line(&app), "# Notes https://medium.com");
    app.tick();
    assert_eq!(first_line(&app), r"# Notes [Medium \[blog\]](https://medium.com)");
    assert_eq!(*fetched.borrow(), ["https://medium.com"]);
    // The cursor stays after the link, and undo takes back just the swap.
    typ(&mut app, "!");
    assert_eq!(first_line(&app), r"# Notes [Medium \[blog\]](https://medium.com)!");
    app.on_key(Input { key: Key::Char('z'), ctrl: true, ..Input::default() });
    app.on_key(Input { key: Key::Char('z'), ctrl: true, ..Input::default() });
    assert_eq!(first_line(&app), "# Notes https://medium.com");
    app.on_key(Input { key: Key::Char('y'), ctrl: true, ..Input::default() });
    assert_eq!(first_line(&app), r"# Notes [Medium \[blog\]](https://medium.com)");
}

#[test]
fn pasted_url_stays_bare_when_it_should() {
    // No title.
    let (mut app, _, _) = paste_app(Config::default(), None);
    app.on_paste("https://example.com".into());
    app.tick();
    assert_eq!(first_line(&app), "# Notes https://example.com");

    // Already inside a link.
    let (mut app, _, fetched) = paste_app(Config::default(), None);
    typ(&mut app, "[x](");
    app.on_paste("https://medium.com".into());
    app.tick();
    assert_eq!(first_line(&app), "# Notes [x](https://medium.com");
    assert!(fetched.borrow().is_empty());

    // The text moved before the title came back.
    let (mut app, _, _) = paste_app(Config::default(), None);
    app.on_paste("https://medium.com".into());
    key(&mut app, Key::Home);
    typ(&mut app, ">");
    app.tick();
    assert_eq!(first_line(&app), "># Notes https://medium.com");

    // Turned off.
    let cfg = Config { link_titles: false, ..Config::default() };
    let (mut app, _, fetched) = paste_app(cfg, None);
    app.on_paste("https://medium.com".into());
    app.tick();
    assert_eq!(first_line(&app), "# Notes https://medium.com");
    assert!(fetched.borrow().is_empty());

    // More than a URL.
    let (mut app, _, _) = paste_app(Config::default(), None);
    app.on_paste("see https://medium.com".into());
    app.tick();
    assert_eq!(first_line(&app), "# Notes see https://medium.com");
}

#[test]
fn pasted_image_path_is_imported() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("Screen Shot.png");
    fs::write(&src, "png bytes").unwrap();
    let (mut app, fs, _) = paste_app(Config::default(), None);
    // A dropped file, as the terminal pastes it: quoted on Windows, backslash-escaped elsewhere.
    let pasted = if cfg!(windows) {
        format!("\"{}\"", src.display())
    } else {
        src.display().to_string().replace(' ', "\\ ")
    };
    app.on_paste(pasted);
    assert_eq!(first_line(&app), "# Notes ![Screen Shot](assets/Screen%20Shot.png)");
    assert_eq!(fs.read(Path::new("assets/Screen Shot.png")).unwrap(), b"png bytes");

    // A path that doesn't exist is just text.
    app.on_paste("/no/such/image.png".into());
    assert_eq!(first_line(&app), "# Notes ![Screen Shot](assets/Screen%20Shot.png)/no/such/image.png");
}

#[test]
fn clipboard_images_go_under_assets() {
    let (mut app, fs, _) = paste_app(Config::default(), Some(b"\x89PNG one".to_vec()));
    app.on_key(Input { key: Key::Char('v'), ctrl: true, ..Input::default() });
    let line = first_line(&app);
    let name = line.strip_prefix("# Notes ![").and_then(|l| l.split_once(']')).unwrap().0.to_string();
    assert!(name.starts_with("pasted-") && name.len() == "pasted-YYYYMMDD-HHMMSS".len(), "{line}");
    assert_eq!(line, format!("# Notes ![{name}](assets/{name}.png)"));
    assert_eq!(fs.read(&Path::new("assets").join(format!("{name}.png"))).unwrap(), b"\x89PNG one");

    // Several at once (the web), one per line.
    app.paste_images(vec![("png", b"a".to_vec()), ("jpg", b"b".to_vec())]);
    let lines = app.editor.as_ref().unwrap().ta.lines().to_vec();
    assert!(lines[0].ends_with(".png)") && lines[0].contains("](assets/pasted-"), "{lines:?}");
    assert!(lines[1].starts_with("![pasted-") && lines[1].ends_with(".jpg)"), "{lines:?}");

    // Nothing on the clipboard.
    let (mut app, _, _) = paste_app(Config::default(), None);
    app.on_key(Input { key: Key::Char('v'), ctrl: true, ..Input::default() });
    assert_eq!(first_line(&app), "# Notes ");
}

#[test]
fn ctrl_x_cuts_the_line_and_ctrl_z_brings_it_back() {
    let files: Files =
        [(PathBuf::from("INDEX.md"), Blob::Data(b"# Notes\nsecond\nthird".to_vec()))].into_iter().collect();
    let copied = Rc::new(RefCell::new(Vec::new()));
    let mut app = App::new(
        Box::new(MemStorage::new("notes", files)),
        Box::new(Clipboard(copied.clone())),
        Config::default(),
        Images::new(PathBuf::new(), None),
    );
    app.load_page(PathBuf::from("INDEX.md"));
    app.focus = Focus::Page;
    key(&mut app, Key::Char('e'));
    let ctrl = |c| Input { key: Key::Char(c), ctrl: true, ..Input::default() };
    let lines = |app: &App| app.editor.as_ref().unwrap().ta.lines().to_vec();

    // The cursor starts on the first line.
    app.on_key(ctrl('x'));
    assert_eq!(lines(&app), ["second", "third"]);
    // The last line takes the break before it.
    key(&mut app, Key::Down);
    app.on_key(ctrl('x'));
    assert_eq!(lines(&app), ["second"]);
    assert_eq!(copied.borrow().as_slice(), ["# Notes\n", "third\n"]);

    app.on_key(ctrl('z'));
    app.on_key(ctrl('z'));
    assert_eq!(lines(&app), ["# Notes", "second", "third"]);
}

#[test]
fn alt_arrows_move_by_word_in_the_editor() {
    let (mut app, _fs, _fetched) = paste_app(Config::default(), None);
    let alt = |key| Input { key, alt: true, ..Input::default() };
    let col = |app: &App| app.editor.as_ref().unwrap().ta.cursor().1;
    // `# Notes ` with the cursor at the end.
    app.on_key(alt(Key::Left));
    assert_eq!(col(&app), 2);
    app.on_key(alt(Key::Left));
    assert_eq!(col(&app), 0);
    app.on_key(alt(Key::Right));
    assert_eq!(col(&app), 2);
}

#[test]
fn ctrl_c_copies_the_editor_selection() {
    let files: Files = [(PathBuf::from("INDEX.md"), Blob::Data(b"# Notes".to_vec()))].into_iter().collect();
    let copied = Rc::new(RefCell::new(Vec::new()));
    let mut app = App::new(
        Box::new(MemStorage::new("notes", files)),
        Box::new(Clipboard(copied.clone())),
        Config::default(),
        Images::new(PathBuf::new(), None),
    );
    app.load_page(PathBuf::from("INDEX.md"));
    app.focus = Focus::Page;
    key(&mut app, Key::Char('e'));
    key(&mut app, Key::End);
    for _ in 0..5 {
        app.on_key(Input { key: Key::Left, shift: true, ..Input::default() });
    }
    let ctrl_c = Input { key: Key::Char('c'), ctrl: true, ..Input::default() };
    app.on_key(ctrl_c.clone());
    assert_eq!(copied.borrow().as_slice(), ["Notes"]);
    assert!(!app.quit && matches!(app.modal, gition::app::Modal::None));
}

#[test]
fn an_empty_paste_pastes_the_clipboard_image() {
    let (mut app, fs, _fetched) = paste_app(Config::default(), Some(b"\x89PNG fake".to_vec()));
    app.on_paste(String::new());
    let line = first_line(&app);
    assert!(line.starts_with("# Notes ![pasted-") && line.ends_with(".png)"), "{line}");
    let name = line.split_once("](").unwrap().1.trim_end_matches(')');
    assert_eq!(fs.read(Path::new(name)).unwrap(), b"\x89PNG fake");
}
