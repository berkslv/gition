//! Drawing.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block as Frame_, Clear, List, ListItem, Paragraph, Wrap};
use unicode_width::UnicodeWidthStr;

use crate::app::{App, Focus, Modal, PageKind, Row};
use crate::keymap::Action;
use crate::links;
use crate::markdown::Block;
use crate::paths;
use crate::sync::Remote;

const ACCENT: Color = Color::Cyan;
const DIM: Style = Style::new().fg(Color::DarkGray);
const CURSOR_BG: Color = Color::Indexed(236);

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    app.width = area.width;
    let [main, status] = Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(area);
    let tree_w = if app.tree_visible() { app.cfg.tree_width.min(main.width / 2) } else { 0 };
    let [tree_area, page_area] = Layout::horizontal([Constraint::Length(tree_w), Constraint::Fill(1)]).areas(main);
    if tree_w > 0 {
        draw_tree(f, app, tree_area);
    }
    draw_page(f, app, page_area);
    draw_status(f, app, status);
    draw_modal(f, app, area);
}

fn pane(title: String, focused: bool) -> Frame_<'static> {
    Frame_::bordered().title(Span::styled(title, Style::new().add_modifier(Modifier::BOLD))).border_style(if focused {
        Style::new().fg(ACCENT)
    } else {
        DIM
    })
}

fn draw_tree(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Tree;
    let current = app.page.as_ref().map(|p| p.path.clone());
    let items: Vec<ListItem> = app
        .visible
        .iter()
        .map(|v| {
            let icon = if v.is_dir { if v.expanded { "▾ " } else { "▸ " } } else { "  " };
            let is_current = current
                .as_ref()
                .is_some_and(|c| *c == v.path || (v.is_dir && paths::is_index(c) && paths::parent(c) == v.path));
            let mut style = if v.is_dir { Style::new().fg(Color::Blue) } else { Style::new() };
            if is_current {
                style = style.fg(Color::Green).add_modifier(Modifier::BOLD);
            }
            let indent = "  ".repeat(v.depth.saturating_sub(1));
            ListItem::new(Line::from(vec![
                Span::raw(indent),
                Span::styled(icon, DIM),
                Span::styled(v.name.clone(), style),
            ]))
        })
        .collect();
    let highlight = if focused { Style::new().add_modifier(Modifier::REVERSED) } else { Style::new().bg(CURSOR_BG) };
    let list = List::new(items).block(pane(" Pages ".into(), focused)).highlight_style(highlight);
    app.tree_state.select(Some(app.sel));
    f.render_stateful_widget(list, area, &mut app.tree_state);
}

fn draw_page(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Page;
    let (title, editing) = match (&app.editor, &app.page) {
        (Some(e), _) => {
            let dirty = if e.dirty() { " ●" } else { "" };
            (format!(" {} [edit]{dirty} ", paths::to_slash(&e.path)), true)
        }
        (None, Some(p)) => (format!(" {} ", paths::to_slash(&p.path)), false),
        (None, None) => (" gition ".into(), false),
    };
    let block = pane(title, focused);
    let inner = block.inner(area);
    f.render_widget(block, area);

    if editing {
        if let Some(e) = &app.editor {
            f.render_widget(&e.ta, inner);
        }
        return;
    }
    let notice = |f: &mut Frame, text: String| {
        f.render_widget(Paragraph::new(text).style(DIM).wrap(Wrap { trim: false }), inner);
    };
    let Some(page) = &app.page else {
        return notice(
            f,
            format!("No page open. Select one in the tree, or press {} to search.", app.keys.label(Action::Search)),
        );
    };
    match &page.kind {
        PageKind::Folder => notice(
            f,
            format!(
                "Folder without {}. Press {} to add a page here.",
                crate::repo::INDEX,
                app.keys.label(Action::NewPage)
            ),
        ),
        PageKind::Binary => {
            notice(f, format!("Binary file. Press {} to open it externally.", app.keys.label(Action::OpenExternal)))
        }
        PageKind::Image => {
            let path = page.path.clone();
            if !draw_image(f, app, &path, inner) {
                let label = app.keys.label(Action::OpenExternal);
                notice(f, format!("🖼 {} (press {label} to open)", paths::to_slash(&path)));
            }
        }
        PageKind::Markdown(_) | PageKind::Text(_) => draw_rows(f, app, inner, focused),
    }
}

/// Draws an image fitted into `area`; false if it can't be shown.
#[cfg(feature = "native")]
fn draw_image(f: &mut Frame, app: &mut App, path: &std::path::Path, area: Rect) -> bool {
    let size = app.images.fit(path, ratatui::layout::Size::new(area.width, area.height));
    match size.and_then(|s| app.images.protocol(path, s).map(|p| (s, p))) {
        Some((s, proto)) => {
            f.render_widget(ratatui_image::Image::new(proto), Rect::new(area.x, area.y, s.width, s.height));
            true
        }
        None => false,
    }
}

#[cfg(not(feature = "native"))]
fn draw_image(_f: &mut Frame, _app: &mut App, _path: &std::path::Path, _area: Rect) -> bool {
    false
}

fn draw_rows(f: &mut Frame, app: &mut App, inner: Rect, focused: bool) {
    app.layout(inner.width, inner.height);
    #[cfg_attr(not(feature = "native"), allow(unused_variables))]
    let App { view, images, page, .. } = app;
    let Some(page) = page else { return };
    let blocks: &[Block] = match &page.kind {
        PageKind::Markdown(b) => b,
        _ => &[],
    };
    let end = (view.scroll + inner.height as usize).min(view.rows.len());
    for (i, y) in (view.scroll..end).zip(inner.y..) {
        let rect = Rect::new(inner.x, y, inner.width, 1);
        let is_cursor = focused && i == view.cursor;
        let highlight = if is_cursor { Style::new().bg(CURSOR_BG) } else { Style::new() };
        let placeholder = |block: usize| {
            let (dest, alt) = match blocks.get(block) {
                Some(Block::Image { dest, alt }) => (dest.as_str(), alt.as_str()),
                _ => ("", ""),
            };
            let label = if alt.is_empty() { dest } else { alt };
            Paragraph::new(Line::from(vec![
                Span::raw("🖼 "),
                Span::styled(label.to_string(), Style::new().fg(Color::Magenta)),
            ]))
            .style(highlight)
        };
        match &view.rows[i] {
            Row::Text { line, .. } => f.render_widget(Paragraph::new(line.clone()).style(highlight), rect),
            Row::ImagePlaceholder { block } => f.render_widget(placeholder(*block), rect),
            Row::Image { block, part, size } => {
                let fits = y + size.height <= inner.bottom();
                if *part == 0 && fits {
                    let dest = match blocks.get(*block) {
                        Some(Block::Image { dest, .. }) => dest,
                        _ => continue,
                    };
                    let Some((target, _)) = links::resolve(&page.path, dest) else { continue };
                    #[cfg(feature = "native")]
                    if let Some(proto) = images.protocol(&target, *size) {
                        let r = Rect::new(inner.x, y, size.width, size.height);
                        f.render_widget(ratatui_image::Image::new(proto), r);
                    }
                    #[cfg(not(feature = "native"))]
                    let _ = target;
                } else if *part == 0 || i == view.scroll {
                    // Partly scrolled out: graphics protocols can't clip, show a label instead.
                    f.render_widget(placeholder(*block), rect);
                }
            }
        }
    }
}

fn status_text(app: &App) -> (String, Style) {
    if app.busy {
        return ("syncing…".into(), Style::new().fg(Color::Yellow));
    }
    let Some(st) = &app.git else { return (String::new(), Style::new()) };
    let (text, color) = match &st.remote {
        Remote::None => ("no remote".to_string(), Color::DarkGray),
        Remote::NoUpstream => ("not pushed yet".to_string(), Color::Yellow),
        Remote::Offline(_) => ("offline".to_string(), Color::Red),
        Remote::Tracking { ahead: 0, behind: 0 } => ("✓ up to date".to_string(), Color::Green),
        Remote::Tracking { ahead, behind: 0 } => (format!("↑{ahead} ahead"), Color::Yellow),
        Remote::Tracking { ahead: 0, behind } => {
            (format!("↓{behind} behind, {} to sync", app.keys.label(Action::Sync)), Color::Yellow)
        }
        Remote::Tracking { ahead, behind } => {
            (format!("↑{ahead} ↓{behind} diverged, {} to sync", app.keys.label(Action::Sync)), Color::Red)
        }
    };
    let text = if st.changes > 0 { format!("● {} changed · {text}", st.changes) } else { text };
    (text, Style::new().fg(color))
}

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let left = match &app.message {
        Some((m, true, _)) => Span::styled(format!(" {m}"), Style::new().fg(Color::Red)),
        Some((m, false, _)) => Span::styled(format!(" {m}"), Style::new().fg(Color::Green)),
        None => Span::styled(format!(" {} for help", app.keys.label(Action::Help)), DIM),
    };
    let (right, style) = status_text(app);
    let right = format!("{right} ");
    let pad = (area.width as usize).saturating_sub(left.content.width() + right.width());
    let line = Line::from(vec![left, Span::raw(" ".repeat(pad)), Span::styled(right, style)]);
    f.render_widget(Paragraph::new(line).style(Style::new().bg(Color::Indexed(235))), area);
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(area.width.saturating_sub(2));
    let h = height.min(area.height.saturating_sub(2));
    Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 3, w, h)
}

fn draw_modal(f: &mut Frame, app: &App, area: Rect) {
    let hint = |s: &str| Line::styled(s.to_string(), DIM);
    match &app.modal {
        Modal::None => {}
        Modal::Prompt { title, input, .. } => {
            let r = centered(area, 64, 5);
            let lines = vec![
                Line::from(vec![
                    Span::styled("> ", Style::new().fg(ACCENT)),
                    Span::raw(input.clone()),
                    Span::styled("█", Style::new().fg(ACCENT)),
                ]),
                Line::raw(""),
                hint("Enter confirm · Esc cancel"),
            ];
            popup(f, r, title, lines);
        }
        Modal::ConfirmDelete { message, .. } => {
            let r = centered(area, 64, 5);
            popup(
                f,
                r,
                "Delete",
                vec![Line::raw(message.clone()), Line::raw(""), hint("y delete · any other key cancel")],
            );
        }
        Modal::SaveChanges(_) => {
            let r = centered(area, 48, 5);
            popup(
                f,
                r,
                "Unsaved changes",
                vec![Line::raw("Save changes?"), Line::raw(""), hint("y save · n discard · Esc cancel")],
            );
        }
        Modal::Search { query, items, results, sel } => {
            let r = centered(area, 70, 20);
            let max = r.height.saturating_sub(4) as usize;
            let start = sel.saturating_sub(max.saturating_sub(1));
            let mut lines = vec![Line::from(vec![
                Span::styled("/ ", Style::new().fg(ACCENT)),
                Span::raw(query.clone()),
                Span::styled("█", Style::new().fg(ACCENT)),
            ])];
            lines.push(hint(&format!("{} of {}", results.len(), items.len())));
            for (n, &i) in results.iter().enumerate().skip(start).take(max) {
                let style = if n == *sel { Style::new().add_modifier(Modifier::REVERSED) } else { Style::new() };
                lines.push(Line::styled(format!(" {} ", items[i].0), style));
            }
            popup(f, r, "Find page", lines);
        }
        Modal::Help => {
            let sections: [(&str, &[Action]); 4] = [
                (
                    "Global",
                    &[
                        Action::Search,
                        Action::Sync,
                        Action::ToggleTree,
                        Action::SwitchFocus,
                        Action::Help,
                        Action::Quit,
                    ],
                ),
                (
                    "Tree",
                    &[
                        Action::Up,
                        Action::Down,
                        Action::Open,
                        Action::Collapse,
                        Action::NewPage,
                        Action::NewFolder,
                        Action::Rename,
                        Action::Delete,
                        Action::InsertImage,
                        Action::ToggleAllFiles,
                    ],
                ),
                ("Page", &[Action::Edit, Action::Follow, Action::OpenExternal, Action::PageUp, Action::PageDown]),
                ("Editor", &[Action::Save, Action::Undo, Action::Redo, Action::ExitEdit]),
            ];
            let mut lines = Vec::new();
            for (name, actions) in sections {
                lines.push(Line::styled(name, Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)));
                for &a in actions {
                    lines.push(Line::from(vec![
                        Span::styled(format!("  {:<18}", app.keys.label(a)), Style::new().add_modifier(Modifier::BOLD)),
                        Span::raw(a.description()),
                    ]));
                }
            }
            lines.push(hint("Keys are configurable in .gition.toml · any key closes"));
            let r = centered(area, 66, lines.len() as u16 + 2);
            popup(f, r, "Keys", lines);
        }
    }
}

fn popup(f: &mut Frame, r: Rect, title: &str, lines: Vec<Line>) {
    f.render_widget(Clear, r);
    let block = Frame_::bordered().title(format!(" {title} ")).border_style(Style::new().fg(ACCENT));
    f.render_widget(Paragraph::new(lines).block(block), r);
}
