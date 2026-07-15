//! Render the editor frame to the terminal via ratatui.
//!
//! Layout:
//! ```text
//! ┌─────────────────────────────────────────────────────────────────┐
//! │  header: file path + dirty indicator                           │  1 line
//! ├─────────────────────────────────────────────────────────────────┤
//! │  1 │ line 1 of buffer                                          │
//! │  2 │ line 2 ...                                                │  N lines
//! │  ⋮ │ ⋮                                                          │  selected chars reverse-video
//! ├─────────────────────────────────────────────────────────────────┤
//! │  status: message + L{line}:{col} / L{total}                    │  1 line
//! └─────────────────────────────────────────────────────────────────┘
//! ```
//!
//! The terminal cursor is positioned over the buffer cursor at the end
//! of each render pass.

use core::selection_in_line;
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use crate::app::{App, CloseChoice};

pub fn render(f: &mut Frame, app: &mut App) {
    let area = f.area();

    // Vertical chunks: header, content, status, and a row each for the
    // find bar, replace bar, close-confirm prompt, open-file dialog,
    // and go-to-line dialog (whichever are open). Modals take priority:
    // we always reserve their row when their state is set so
    // opening/closing one doesn't make the rest of the layout jump.
    let mut constraints = vec![
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
    ];
    if app.search.bar_open {
        constraints.push(Constraint::Length(1));
    }
    if app.search.replace_bar_open {
        constraints.push(Constraint::Length(1));
    }
    if app.close_confirm.is_some() {
        constraints.push(Constraint::Length(1));
    }
    if app.go_to_line_dialog.is_some() {
        constraints.push(Constraint::Length(1));
    }
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    let header = render_header(app);
    f.render_widget(Paragraph::new(header), chunks[0]);

    // When the project tree is open, split the content area into a
    // left sidebar and the main editor area.
    let (tree_area, content_area) = if app.project_tree_open {
        let hchunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(25), Constraint::Percentage(75)])
            .split(chunks[1]);
        (Some(hchunks[0]), hchunks[1])
    } else {
        (None, chunks[1])
    };

    if let Some(area) = tree_area {
        app.tree_width = area.width;
        let tree_lines = render_project_tree(app, area.width);
        f.render_widget(Paragraph::new(tree_lines), area);
    } else {
        app.tree_width = 0;
    }

    app.adjust_viewport(content_area.height);
    let content = render_content(app, content_area.width);
    f.render_widget(Paragraph::new(content), content_area);

    let status = render_status(app, chunks[2].width);
    f.render_widget(Paragraph::new(status), chunks[2]);

    // Modal rows, in order. Index = 3 + (1 per preceding modal that's
    // open). We track it explicitly to avoid the off-by-one that
    // would come from chained `if` indexing.
    let mut idx = 3;
    if app.search.bar_open {
        let find_line = render_find_bar(app);
        f.render_widget(Paragraph::new(find_line), chunks[idx]);
        idx += 1;
    }
    if app.search.replace_bar_open {
        let replace_line = render_replace_bar(app);
        f.render_widget(Paragraph::new(replace_line), chunks[idx]);
        idx += 1;
    }
    if let Some(confirm) = &app.close_confirm {
        let line = render_close_confirm(confirm, &app.documents[confirm.doc_index]);
        f.render_widget(Paragraph::new(line), chunks[idx]);
        idx += 1;
    }
    if let Some(dialog) = &app.go_to_line_dialog {
        let line = render_go_to_line_dialog(dialog);
        f.render_widget(Paragraph::new(line), chunks[idx]);
        // No further modals to index after this.
    }

    // Position the terminal cursor. The find bar, replace bar, and
    // go-to-line dialog all have a text input — they want the cursor at
    // end of their query string. Close-confirm has no input; cursor
    // stays on the buffer if visible. Replace bar
    // takes priority over find bar (it's the more recently opened
    // modal); find bar takes priority over no modal.
    if app.command_palette.open {
        let overlay_rect = command_palette_rect(area);
        let block = Block::default().borders(Borders::ALL);
        let inner = block.inner(overlay_rect);
        let prefix_chars = "▸ ".chars().count() as u16;
        let cursor_x = inner.x + prefix_chars + app.command_palette.query.chars().count() as u16;
        f.set_cursor_position(Position::new(cursor_x, inner.y));
    } else if app.project_search.open {
        let overlay_rect = project_search_rect(area);
        let block = Block::default().borders(Borders::ALL);
        let inner = block.inner(overlay_rect);
        if app.project_search.replace_focused {
            let prefix_chars = "▸ Replace: ".chars().count() as u16;
            let cursor_x =
                inner.x + prefix_chars + app.project_search.replace_query.chars().count() as u16;
            f.set_cursor_position(Position::new(cursor_x, inner.y + 1));
        } else {
            let prefix_chars = "▸ Find: ".chars().count() as u16;
            let cursor_x = inner.x + prefix_chars + app.project_search.query.chars().count() as u16;
            f.set_cursor_position(Position::new(cursor_x, inner.y));
        }
    } else if app.search.replace_bar_open {
        let replace_idx = if app.search.bar_open { 4 } else { 3 };
        let prefix_chars = " Replace: ".chars().count() as u16;
        let cursor_x =
            chunks[replace_idx].x + prefix_chars + app.search.replace_query.chars().count() as u16;
        f.set_cursor_position(Position::new(cursor_x, chunks[replace_idx].y));
    } else if app.search.bar_open {
        let find_idx = 3;
        let query_prefix_chars = " Find: ".chars().count() as u16;
        let cursor_x =
            chunks[find_idx].x + query_prefix_chars + app.search.query.chars().count() as u16;
        f.set_cursor_position(Position::new(cursor_x, chunks[find_idx].y));
    } else if let Some(dialog) = app.go_to_line_dialog.as_ref() {
        // The dialog row sits at the last allocated chunk — chunks is
        // built in declaration order so the dialog row is always the
        // final entry. `unwrap` is safe: we just rendered into it.
        let last = chunks.last().unwrap();
        let prefix_chars = " Go to line: ".chars().count() as u16;
        let cursor_x = last.x + prefix_chars + dialog.query.chars().count() as u16;
        f.set_cursor_position(Position::new(cursor_x, last.y));
    } else if let Some(pos) = compute_cursor_screen_pos(app, chunks[1]) {
        f.set_cursor_position(pos);
    }

    // Highlight the focused choice in the close-confirm prompt by
    // painting a centered overlay. Done last so it draws on top of the
    // content. Only the focused cell gets a coloured background; the
    // others stay plain so the eye lands on the highlighted one.
    if let Some(confirm) = &app.close_confirm {
        render_close_confirm_overlay(f, area, confirm);
    }

    // Project-wide search draws as a centered overlay so the results
    // list has enough room.
    if app.project_search.open {
        render_project_search_overlay(f, area, app);
    }

    // Command palette draws on top of everything else.
    if app.command_palette.open {
        render_command_palette_overlay(f, area, app);
    }

    // Fuzzy file finder draws on top of everything else.
    if app.fuzzy_finder.open {
        render_fuzzy_finder_overlay(f, area, app);
    }

    // LSP completion popup draws on top of everything else.
    if app.lsp_completion.open {
        render_lsp_completion_overlay(f, area, app);
    }
}

/// Render the close-on-dirty prompt as a single line at the bottom.
/// The line shows the three choices with the focused one highlighted
/// by reverse video. Also includes a hint about the key bindings.
fn render_close_confirm(confirm: &crate::app::CloseConfirm, doc: &core::Document) -> Line<'static> {
    let doc_name = doc.display_name();
    let dirty_msg = format!("'{doc_name}' has unsaved changes.");
    let choice_label = |c: CloseChoice, label: &str| -> Span<'static> {
        let focused = confirm.choice == c;
        let mut style = Style::default();
        if focused {
            style = style
                .bg(Color::Rgb(60, 80, 140))
                .fg(Color::White)
                .add_modifier(Modifier::BOLD);
        } else {
            style = style.fg(Color::DarkGray);
        }
        Span::styled(format!(" {label} "), style)
    };
    Line::from(vec![
        Span::raw(format!(" {dirty_msg} ")),
        choice_label(CloseChoice::Save, "Save (Enter)"),
        Span::raw(" "),
        choice_label(CloseChoice::Discard, "Discard (y)"),
        Span::raw(" "),
        choice_label(CloseChoice::Cancel, "Cancel (Esc)"),
    ])
}

/// Draw a centred overlay line over the content area showing the three
/// choices in reverse video on the focused one. We render this AFTER
/// the content so it visually sits on top of the buffer text — makes
/// the prompt unmissable.
fn render_close_confirm_overlay(f: &mut Frame, area: Rect, confirm: &crate::app::CloseConfirm) {
    // Centre horizontally on the available area, with one row of padding
    // above and below.
    let label_w = 60usize.min(area.width as usize);
    let x = area.x + (area.width.saturating_sub(label_w as u16)) / 2;
    let y = area.y + area.height / 2;
    let overlay_rect = Rect::new(x, y, label_w as u16, 3);
    f.render_widget(Clear, overlay_rect);

    let focused = confirm.choice;
    let save_style = if focused == CloseChoice::Save {
        Style::default()
            .bg(Color::Rgb(60, 80, 140))
            .fg(Color::White)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let discard_style = if focused == CloseChoice::Discard {
        Style::default()
            .bg(Color::Rgb(60, 80, 140))
            .fg(Color::White)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let cancel_style = if focused == CloseChoice::Cancel {
        Style::default()
            .bg(Color::Rgb(60, 80, 140))
            .fg(Color::White)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let prompt = Line::from(vec![
        Span::raw(" Save?  "),
        Span::styled(" Save ", save_style),
        Span::raw("  "),
        Span::styled(" Discard ", discard_style),
        Span::raw("  "),
        Span::styled(" Cancel ", cancel_style),
    ]);
    f.render_widget(Paragraph::new(prompt), overlay_rect);
}

/// Render the project-wide search / replace overlay as a centred popup.
/// Shows find and replace inputs, live results or replace preview, and
/// a hint row.
fn render_project_search_overlay(f: &mut Frame, area: Rect, app: &App) {
    let overlay_rect = project_search_rect(area);

    f.render_widget(Clear, overlay_rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(" Project Search & Replace ");
    let inner = block.inner(overlay_rect);
    f.render_widget(block, overlay_rect);

    let showing_replace = !app.project_search.replace_query.is_empty();
    let find_focus_marker = if app.project_search.replace_focused {
        " "
    } else {
        "▸"
    };
    let replace_focus_marker = if app.project_search.replace_focused {
        "▸"
    } else {
        " "
    };
    let find_line = Line::from(format!(
        "{} Find: {}█  ({} results)",
        find_focus_marker,
        app.project_search.query,
        app.project_search.results.len()
    ));
    let replace_line = Line::from(format!(
        "{} Replace: {}█",
        replace_focus_marker, app.project_search.replace_query
    ));
    f.render_widget(Paragraph::new(find_line), inner);
    let replace_area = Rect {
        x: inner.x,
        y: inner.y + 1,
        width: inner.width,
        height: 1,
    };
    f.render_widget(Paragraph::new(replace_line), replace_area);

    // Reserve top two rows for inputs and bottom row for hint.
    let results_area = Rect {
        x: inner.x,
        y: inner.y + 2,
        width: inner.width,
        height: inner.height.saturating_sub(3),
    };

    let selected = app.project_search.selected;
    let selected_style = Style::default()
        .bg(Color::Rgb(60, 80, 140))
        .fg(Color::White)
        .add_modifier(Modifier::BOLD);

    let mut lines: Vec<Line<'static>> = Vec::new();
    if showing_replace {
        for (i, preview) in app.project_search.replace_previews.iter().enumerate() {
            if lines.len() >= results_area.height as usize {
                break;
            }
            let label = format!(
                " {}:{}  {} → {}",
                preview.rel_path.to_string_lossy(),
                preview.line,
                preview.before,
                preview.after
            );
            let truncated = label
                .chars()
                .take(results_area.width as usize)
                .collect::<String>();
            let line = if i == selected {
                Line::from(Span::styled(truncated, selected_style))
            } else {
                Line::from(truncated)
            };
            lines.push(line);
        }
    } else {
        for (i, result) in app.project_search.results.iter().enumerate() {
            if lines.len() >= results_area.height as usize {
                break;
            }
            let label = format!(
                " {}:{} {}",
                result.rel_path.to_string_lossy(),
                result.line,
                result.text
            );
            let truncated = label
                .chars()
                .take(results_area.width as usize)
                .collect::<String>();
            let line = if i == selected {
                Line::from(Span::styled(truncated, selected_style))
            } else {
                Line::from(truncated)
            };
            lines.push(line);
        }
    }

    f.render_widget(Paragraph::new(lines), results_area);

    let hint_area = Rect {
        x: inner.x,
        y: inner.y + inner.height - 1,
        width: inner.width,
        height: 1,
    };
    let hint_text = if app.project_search.confirm_replace {
        let occurrence_count: usize = app
            .project_search
            .replace_previews
            .iter()
            .map(|p| p.occurrence_count)
            .sum();
        let mut files: Vec<_> = app
            .project_search
            .replace_previews
            .iter()
            .map(|p| p.rel_path.clone())
            .collect();
        files.sort();
        files.dedup();
        format!(
            " Replace {} occurrences in {} files? Enter = yes, Esc = no ",
            occurrence_count,
            files.len()
        )
    } else if showing_replace {
        let line_count = app.project_search.replace_previews.len();
        let occurrence_count: usize = app
            .project_search
            .replace_previews
            .iter()
            .map(|p| p.occurrence_count)
            .sum();
        format!(
            " {} lines · {} occurrences · Tab focus · Ctrl+Enter confirm · Esc close ",
            line_count, occurrence_count
        )
    } else {
        format!(
            " {} results · Enter open · Up/Down · Tab focus · Esc close ",
            app.project_search.results.len()
        )
    };
    let hint = Line::from(hint_text);
    f.render_widget(Paragraph::new(hint), hint_area);
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    Rect {
        x,
        y,
        width: width.min(area.width),
        height: height.min(area.height),
    }
}

fn project_search_rect(area: Rect) -> Rect {
    let width = (area.width as f32 * 0.75).clamp(40.0, 80.0) as u16;
    let height = (area.height as f32 * 0.75).clamp(10.0, 30.0) as u16;
    centered_rect(width, height, area)
}

fn command_palette_rect(area: Rect) -> Rect {
    let width = (area.width as f32 * 0.6).clamp(40.0, 70.0) as u16;
    let height = (area.height as f32 * 0.6).clamp(10.0, 25.0) as u16;
    centered_rect(width, height, area)
}

fn lsp_completion_rect(area: Rect) -> Rect {
    let width = (area.width as f32 * 0.5).clamp(30.0, 60.0) as u16;
    let height = (area.height as f32 * 0.5).clamp(8.0, 20.0) as u16;
    centered_rect(width, height, area)
}

/// Render the command palette as a centred popup.
fn render_command_palette_overlay(f: &mut Frame, area: Rect, app: &App) {
    let overlay_rect = command_palette_rect(area);

    f.render_widget(Clear, overlay_rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Magenta))
        .title(" Command Palette ");
    let inner = block.inner(overlay_rect);
    f.render_widget(block, overlay_rect);

    let query_line = Line::from(format!(
        "▸ {}█  ({} commands)",
        app.command_palette.query,
        app.command_palette.items.len()
    ));
    f.render_widget(Paragraph::new(query_line), inner);

    // Reserve top row for query and bottom row for hint.
    let items_area = Rect {
        x: inner.x,
        y: inner.y + 1,
        width: inner.width,
        height: inner.height.saturating_sub(2),
    };

    let selected = app.command_palette.selected;
    let selected_style = Style::default()
        .bg(Color::Rgb(100, 60, 140))
        .fg(Color::White)
        .add_modifier(Modifier::BOLD);

    let mut lines: Vec<Line<'static>> = Vec::new();
    for (i, command) in app.command_palette.items.iter().enumerate() {
        if lines.len() >= items_area.height as usize {
            break;
        }
        let label = if command.keybinding.is_empty() {
            format!(" {}", command.label)
        } else {
            format!(" {}  ({})", command.label, command.keybinding)
        };
        let truncated = label
            .chars()
            .take(items_area.width as usize)
            .collect::<String>();
        let line = if i == selected {
            Line::from(Span::styled(truncated, selected_style))
        } else {
            Line::from(truncated)
        };
        lines.push(line);
    }

    f.render_widget(Paragraph::new(lines), items_area);

    let hint_area = Rect {
        x: inner.x,
        y: inner.y + inner.height - 1,
        width: inner.width,
        height: 1,
    };
    let hint = Line::from(" Enter to run · Up/Down · Esc to close ".to_string());
    f.render_widget(Paragraph::new(hint), hint_area);
}

/// Render the LSP completion popup as a centred popup.
fn render_lsp_completion_overlay(f: &mut Frame, area: Rect, app: &App) {
    let overlay_rect = lsp_completion_rect(area);

    f.render_widget(Clear, overlay_rect);
    let title = format!(" Completions ({}) ", app.lsp_completion.items.len());
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Green))
        .title(title);
    let inner = block.inner(overlay_rect);
    f.render_widget(block, overlay_rect);

    if app.lsp_completion.items.is_empty() {
        let msg = if app.lsp_completion.pending {
            "Loading..."
        } else {
            "No completions"
        };
        f.render_widget(
            Paragraph::new(Line::from(msg)).alignment(Alignment::Center),
            inner,
        );
        return;
    }

    // Reserve top row for title context and bottom row for hint.
    let items_area = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: inner.height.saturating_sub(1),
    };

    let selected = app.lsp_completion.selected;
    let selected_style = Style::default()
        .bg(Color::Rgb(60, 120, 80))
        .fg(Color::White)
        .add_modifier(Modifier::BOLD);

    let mut lines: Vec<Line<'static>> = Vec::new();
    for (i, item) in app.lsp_completion.items.iter().enumerate() {
        if lines.len() >= items_area.height as usize {
            break;
        }
        let label = item.label.clone();
        let truncated = label
            .chars()
            .take(items_area.width as usize)
            .collect::<String>();
        let line = if i == selected {
            Line::from(Span::styled(truncated, selected_style))
        } else {
            Line::from(truncated)
        };
        lines.push(line);
    }

    f.render_widget(Paragraph::new(lines), items_area);

    let hint_area = Rect {
        x: inner.x,
        y: inner.y + inner.height - 1,
        width: inner.width,
        height: 1,
    };
    let hint = Line::from(" Enter/Tab insert · Up/Down · Esc close ".to_string());
    f.render_widget(Paragraph::new(hint), hint_area);
}

/// Render the fuzzy file finder as a centred popup.
fn render_fuzzy_finder_overlay(f: &mut Frame, area: Rect, app: &App) {
    let overlay_rect = command_palette_rect(area);

    f.render_widget(Clear, overlay_rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(" Fuzzy Finder ");
    let inner = block.inner(overlay_rect);
    f.render_widget(block, overlay_rect);

    let count = app.fuzzy_finder.filtered.len();
    let query_line = Line::from(format!("▸ {}█  ({} files)", app.fuzzy_finder.query, count));
    f.render_widget(Paragraph::new(query_line), inner);

    // Reserve top row for query and bottom row for hint.
    let items_area = Rect {
        x: inner.x,
        y: inner.y + 1,
        width: inner.width,
        height: inner.height.saturating_sub(2),
    };

    let selected = app.fuzzy_finder.selected;
    let selected_style = Style::default()
        .bg(Color::Rgb(60, 100, 140))
        .fg(Color::White)
        .add_modifier(Modifier::BOLD);

    let mut lines: Vec<Line<'static>> = Vec::new();
    for (row, (idx, _)) in app.fuzzy_finder.filtered.iter().enumerate() {
        if lines.len() >= items_area.height as usize {
            break;
        }
        let candidate = &app.fuzzy_finder.items[*idx];
        let label = format!(" {}", candidate.display);
        let truncated = label
            .chars()
            .take(items_area.width as usize)
            .collect::<String>();
        let line = if row == selected {
            Line::from(Span::styled(truncated, selected_style))
        } else {
            Line::from(truncated)
        };
        lines.push(line);
    }

    f.render_widget(Paragraph::new(lines), items_area);

    let hint_area = Rect {
        x: inner.x,
        y: inner.y + inner.height - 1,
        width: inner.width,
        height: 1,
    };
    let hint = Line::from(" Enter to open · Up/Down · Esc to close ".to_string());
    f.render_widget(Paragraph::new(hint), hint_area);
}

fn render_go_to_line_dialog(dialog: &crate::app::GoToLineDialog) -> Line<'static> {
    Line::from(format!(" Go to line: {}█", dialog.query))
}

fn render_find_bar(app: &App) -> Line<'static> {
    let total = app.search.matches.len();
    let current = app.search.current.map(|i| i + 1).unwrap_or(0);
    let mode = if app.search.regex_mode {
        " [regex]"
    } else {
        ""
    };
    let count = if let Some(ref err) = app.search.regex_error {
        format!(" (invalid regex: {err})")
    } else if total == 0 && !app.search.query.is_empty() {
        " (no matches)".to_string()
    } else {
        format!(" {current}/{total}")
    };
    let line = format!("/{}", app.search.query);
    Line::from(format!(" Find: {line}{count}{mode} "))
}

/// Render the replace bar (single-line text input for the replacement
/// string). Cursor is positioned at the end of the query by the caller.
fn render_replace_bar(app: &App) -> Line<'static> {
    // Show the bar with the current replacement text. The replace bar
    // appears below the find bar in the layout; visual order matches
    // modal hierarchy (find = older, replace = newer focus).
    let hint = if app.search.replace_query.is_empty() {
        " (empty)"
    } else {
        ""
    };
    Line::from(format!(" Replace: {}{hint} ", app.search.replace_query))
}

/// Render the project-tree sidebar as a collapsible tree. The selected
/// row is highlighted and the list is truncated to the available height.
fn render_project_tree(app: &App, width: u16) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let title = app
        .active_doc()
        .project
        .as_ref()
        .and_then(|p| p.root.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("Project");
    lines.push(Line::from(format!(" 📁 {title}")));
    lines.push(Line::from("─".repeat(width as usize)));

    if app.active_doc().project.is_none() {
        lines.push(Line::from(" No project detected."));
        return lines;
    }

    let rows = app.project_tree_rows();
    if rows.is_empty() {
        lines.push(Line::from(" No files found."));
        return lines;
    }

    let selected_style = Style::default()
        .bg(Color::Rgb(60, 80, 140))
        .fg(Color::White)
        .add_modifier(Modifier::BOLD);

    for (i, (depth, node)) in rows.iter().enumerate() {
        let is_dir = node.is_dir();
        let expanded = app
            .project_tree
            .as_ref()
            .map(|t| t.expanded.contains(node.rel_path()))
            .unwrap_or(false);
        let indent = "  ".repeat(*depth);
        let icon = if is_dir {
            if expanded {
                "▾"
            } else {
                "▸"
            }
        } else {
            " "
        };
        let label = format!(" {indent}{icon} {}", node.name());
        let truncated = label.chars().take(width as usize).collect::<String>();
        let line = if i == app.project_tree_selected {
            Line::from(Span::styled(truncated, selected_style))
        } else {
            Line::from(truncated)
        };
        lines.push(line);
    }

    lines
}

fn render_header(app: &App) -> Line<'static> {
    // Single-doc header: just filename + dirty marker. We deliberately
    // don't render the tab strip with one entry — the label would just
    // say "[No Name]" or whatever, which is noise. Match the browser
    // convention of hiding the tab bar when there's only one tab.
    if app.doc_count() == 1 {
        let path = app
            .active_buffer()
            .source_path()
            .and_then(|p| p.to_str())
            .unwrap_or("[No Name]");
        let dirty = if app.is_dirty() { " [+]" } else { "" };
        let stale = if app.active_doc().external_change {
            " [!]"
        } else {
            ""
        };
        return Line::from(format!(" {path}{dirty}{stale}"));
    }

    // Multi-doc tab strip: one labelled cell per open doc. The active
    // doc gets a coloured background; inactive ones are dimmed. Thin
    // `│` separators between cells. No "(N/M)" counter — the tabs
    // themselves communicate position now.
    let active_style = Style::default()
        .bg(Color::Rgb(60, 80, 140))
        .fg(Color::White)
        .add_modifier(Modifier::BOLD);
    let inactive_style = Style::default().fg(Color::DarkGray);
    let separator_style = Style::default().fg(Color::Rgb(80, 80, 80));

    let mut spans: Vec<Span<'static>> = vec![Span::raw(" ")];
    for (i, doc) in app.documents.iter().enumerate() {
        let name = doc.display_name();
        let dirty = if doc.is_dirty() { "*" } else { "" };
        let stale = if doc.external_change { "!" } else { "" };
        let label = format!(" {}{}{} ", name, dirty, stale);
        let style = if i == app.active {
            active_style
        } else {
            inactive_style
        };
        spans.push(Span::styled(label, style));
        if i + 1 < app.documents.len() {
            spans.push(Span::styled(" │ ", separator_style));
        }
    }
    Line::from(spans)
}

fn render_status(app: &App, area_width: u16) -> Line<'static> {
    let message = app.status_message.as_deref().unwrap_or("");
    let cursor_pos = app.active_buffer().cursor();
    let (line, col) = app
        .active_buffer()
        .pos_to_linecol(cursor_pos)
        .unwrap_or((0, 0));
    let pos = core::format_position(line, col, app.active_buffer().line_count());
    // Soft-wrap indicator: shows "wrap" when on so the user has a
    // persistent visual confirmation of the toggle (the status
    // message itself disappears after a few seconds). Empty when
    // off so the status bar stays clean in the common case.
    let wrap_indicator = if app.active_doc().view.soft_wrap {
        "  |  wrap"
    } else {
        ""
    };
    let git_indicator =
        if app.active_doc().view.git_gutter_enabled && app.active_doc().git_gutter.enabled() {
            let (added, modified, removed) = app.active_doc().git_gutter.summary();
            if added != 0 || modified != 0 || removed != 0 {
                format!("  |  +{added} ~{modified} -{removed}")
            } else {
                String::new()
            }
        } else {
            String::new()
        };
    let lsp_status = app
        .active_doc()
        .uri()
        .and_then(|uri| app.lsp_manager.document_server_status(&uri));
    let lsp_indicator = if let Some(status) = lsp_status {
        let name = core::lsp::LspManager::server_display_name(&status.language_id)
            .unwrap_or(&status.language_id);
        let symbol = if status.running { '●' } else { '○' };
        format!("  |  {symbol} {name}")
    } else if let Some(name) = app
        .active_doc()
        .language_id()
        .filter(|lang| core::lsp::LspManager::is_language_supported(lang))
        .and_then(|lang| core::lsp::LspManager::server_display_name(lang))
    {
        format!("  |  ○ {name}")
    } else {
        String::new()
    };
    let left = format!(" {message}  |  {pos}{wrap_indicator}{git_indicator}{lsp_indicator}");
    let theme = format!(" {theme} ", theme = app.syntax.theme_name());
    let tree_label = " Tree ";
    let width = area_width as usize;
    let padding = width.saturating_sub(left.len() + tree_label.len() + theme.len());

    let tree_style = if app.project_tree_open {
        Style::default()
            .bg(Color::Rgb(60, 80, 140))
            .fg(Color::White)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    Line::from(vec![
        Span::raw(left),
        Span::raw(" ".repeat(padding)),
        Span::styled(tree_label.to_string(), tree_style),
        Span::raw(theme),
    ])
}

fn render_content(app: &mut App, viewport_width: u16) -> Vec<Line<'static>> {
    let total_lines = app.active_buffer().line_count();
    // Gutter: enough digits to fit the largest line number, minimum 2.
    let gutter_width = total_lines.to_string().len().max(2);
    let diagnostics: Vec<lsp_types::Diagnostic> = app
        .active_doc()
        .uri()
        .map(|uri| app.lsp_manager.diagnostics(&uri).to_vec())
        .unwrap_or_default();

    let selection_style = Style::default()
        .bg(Color::Rgb(60, 80, 140))
        .fg(Color::White)
        .add_modifier(Modifier::BOLD);
    // Find-match highlight styles. Two intensities:
    // - current_match_style: bright amber for the match the user
    //   most recently navigated to (cursor sits at its start).
    // - other_match_style: dimmer amber for every other match.
    //
    // Pattern matches VSCode / Sublime — visual scan finds the
    // cluster fast, and the "where am I now" pointer stands out.
    let current_match_style = Style::default()
        .bg(Color::Rgb(200, 160, 40))
        .fg(Color::Black)
        .add_modifier(Modifier::BOLD);
    let other_match_style = Style::default()
        .bg(Color::Rgb(120, 100, 40))
        .fg(Color::White);
    let gutter_style = Style::default().fg(Color::DarkGray);

    // All non-collapsed selections for rendering selection styles.
    let sel_ranges: Vec<std::ops::Range<usize>> = app
        .active_buffer()
        .selections()
        .iter()
        .filter(|s| !s.is_collapsed())
        .map(|s| s.range())
        .collect();

    // Snapshot the search state once per frame so the per-line loop
    // doesn't have to look it up. `current_match_start` is the byte
    // position of the active match (or None when there's no current
    // match — empty query, no matches, or before the first FindNext).
    let query_nonempty = !app.search.query.is_empty();
    let current_match_start = app.search.current_match();

    let mut lines: Vec<Line<'static>> = Vec::new();
    let vh = app.viewport_height as usize;
    let top_line = app.active_doc().view.scroll_top_line;
    let end_line = (top_line + vh).min(total_lines);

    let syntax_engine = &app.syntax;

    for line_idx in top_line..end_line {
        let line_text = app
            .active_buffer()
            .line_text(line_idx)
            .map(|cow| cow.into_owned())
            .unwrap_or_default();

        let git_enabled =
            app.active_doc().view.git_gutter_enabled && app.active_doc().git_gutter.enabled();
        let status = app.active_doc().git_gutter.status(line_idx);
        let removed = app.active_doc().git_gutter.removed_blocks_before(line_idx);
        let marker = if git_enabled && !removed.is_empty() {
            '▲'
        } else if git_enabled {
            match status {
                core::LineStatus::Added => '+',
                core::LineStatus::Modified => '~',
                core::LineStatus::Unchanged => ' ',
            }
        } else {
            ' '
        };
        let marker_style = match marker {
            '+' => Style::default().fg(Color::Green),
            '~' => Style::default().fg(Color::Yellow),
            '▲' => Style::default().fg(Color::Red),
            _ => gutter_style,
        };

        // LSP diagnostic severity marker in the gutter.
        let diag_on_line = diagnostics
            .iter()
            .filter(|d| {
                let start = d.range.start.line as usize;
                let end = d.range.end.line as usize;
                start <= line_idx && end >= line_idx
            })
            .fold(None, |max_sev, d| match (max_sev, d.severity) {
                (Some(lsp_types::DiagnosticSeverity::ERROR), _) => max_sev,
                (_, Some(lsp_types::DiagnosticSeverity::ERROR)) => {
                    Some(lsp_types::DiagnosticSeverity::ERROR)
                }
                (Some(lsp_types::DiagnosticSeverity::WARNING), _) => max_sev,
                (_, Some(lsp_types::DiagnosticSeverity::WARNING)) => {
                    Some(lsp_types::DiagnosticSeverity::WARNING)
                }
                (Some(lsp_types::DiagnosticSeverity::INFORMATION), _) => max_sev,
                (_, Some(lsp_types::DiagnosticSeverity::INFORMATION)) => {
                    Some(lsp_types::DiagnosticSeverity::INFORMATION)
                }
                _ => Some(lsp_types::DiagnosticSeverity::HINT),
            });
        let (diag_marker, diag_style) = match diag_on_line {
            Some(lsp_types::DiagnosticSeverity::ERROR) => ('E', Style::default().fg(Color::Red)),
            Some(lsp_types::DiagnosticSeverity::WARNING) => {
                ('W', Style::default().fg(Color::Yellow))
            }
            Some(lsp_types::DiagnosticSeverity::INFORMATION) => {
                ('I', Style::default().fg(Color::Blue))
            }
            Some(lsp_types::DiagnosticSeverity::HINT) => ('H', Style::default().fg(Color::Gray)),
            _ => (' ', gutter_style),
        };

        let number_prefix = format!("{:>width$} │ ", line_idx + 1, width = gutter_width);
        let blame_on = app.active_doc().view.git_blame_enabled
            && app.active_doc().git_blame.enabled();
        let blame_span = if blame_on {
            if let Some(entry) = app.active_doc().git_blame.entry(line_idx) {
                format!("{:<8}", &entry.short_hash)
            } else {
                "        ".to_string()
            }
        } else {
            String::new()
        };
        let prefix_chars = 2 + blame_span.chars().count() + number_prefix.chars().count();
        let avail = (viewport_width as usize).saturating_sub(prefix_chars);

        // Compute the selected sub-range within this line, if any.
        let line_byte_range = app
            .active_buffer()
            .line_byte_range(line_idx)
            .unwrap_or(0..0);
        // Check all selections for intersection with this line.
        let selected_in_line: Vec<std::ops::Range<usize>> = sel_ranges
            .iter()
            .filter_map(|sr| selection_in_line(line_byte_range.clone(), sr.clone()))
            .collect();
        let has_sel = !selected_in_line.is_empty();
        let mut match_highlights: Vec<(std::ops::Range<usize>, Style)> = Vec::new();
        if !has_sel && query_nonempty {
            let mut idx = app
                .search
                .matches
                .partition_point(|&(s, _)| s < line_byte_range.start);
            if idx > 0 {
                // A match may start on the previous line but extend
                // into this one; include it if it overlaps.
                idx = idx.saturating_sub(1);
            }
            for &(m_start, m_end) in &app.search.matches[idx..] {
                if m_start >= line_byte_range.end {
                    break;
                }
                if m_end <= line_byte_range.start {
                    continue;
                }
                let hl_start = m_start.max(line_byte_range.start);
                let hl_end = m_end.min(line_byte_range.end);
                let style = if Some(m_start) == current_match_start {
                    current_match_style
                } else {
                    other_match_style
                };
                match_highlights.push((hl_start..hl_end, style));
            }
        }

        // Apply horizontal scroll: skip `scroll_x` chars from the start of
        // each line, then truncate to the available width.
        let scroll_x = app.active_doc().view.scroll_x_cols;
        let truncated: String = line_text.chars().skip(scroll_x).take(avail).collect();
        // Convert scroll_x (chars) into a byte offset within this
        // line so selection math can work in bytes.
        let scroll_bytes = core::char_col_to_byte_col(&line_text, scroll_x);

        let mut spans: Vec<Span<'static>> = vec![
            Span::styled(marker.to_string(), marker_style),
            Span::styled(diag_marker.to_string(), diag_style),
        ];
        if blame_on {
            spans.push(Span::styled(blame_span, Style::default().fg(Color::DarkGray)));
        }
        spans.push(Span::styled(number_prefix, gutter_style));

        // Get syntax segments for this line. Cache lookup first; on a
        // miss, highlight via tree-sitter (immutable doc borrow) then
        // insert into the cache (mutable). The split avoids borrowing
        // the doc both ways at once.
        let cached = if !app.documents[app.active].syntax.dirty {
            app.documents[app.active].syntax.lines.get(&line_idx).cloned()
        } else {
            None
        };
        let syntax_segments: Vec<core::ColorSegment> = match cached {
            Some(s) => s,
            None => {
                let syntax_theme = syntax_engine.ts_theme();
                let per_line = app.documents[app.active]
                    .highlight_lines_ts(line_idx, line_idx + 1, syntax_theme);
                let segs = per_line.into_iter().next().unwrap_or_default();
                let doc = &mut app.documents[app.active];
                if doc.syntax.dirty {
                    doc.syntax.lines.clear();
                    doc.syntax.dirty = false;
                }
                doc.syntax.lines.insert(line_idx, segs.clone());
                segs
            }
        };

        push_line_spans(
            &mut spans,
            &truncated,
            line_byte_range,
            &line_text,
            &selected_in_line,
            selection_style,
            &match_highlights,
            scroll_bytes,
            &syntax_segments,
        );
        lines.push(Line::from(spans));
    }

    // Pad to viewport height so the status line stays at the bottom.
    while lines.len() < vh {
        lines.push(Line::from(""));
    }

    lines
}

/// Push spans for one line's text content, applying selection styling
/// to the selected byte range and match styling to the matched byte
/// ranges (mapped back to char positions for the truncated visible
/// text).
///
/// Precedence: selection > matches. When the user has an active drag
/// selection, that wins visually and the match highlights are hidden.
/// When the selection is collapsed (the post-FindNext state), every
/// match on this line highlights — bright amber for the current
/// match, dimmer amber for the rest.
///
/// `scroll_bytes` is the byte offset within the FULL line where the
/// visible (truncated) window starts. Selection/match byte offsets
/// are relative to the full line; we shift them by `scroll_bytes` to
/// get offsets within the truncated string.
#[allow(clippy::too_many_arguments)]
fn push_line_spans(
    spans: &mut Vec<Span<'static>>,
    truncated: &str,
    line_byte_range: std::ops::Range<usize>,
    full_line_text: &str,
    selected_in_line: &[std::ops::Range<usize>],
    selection_style: Style,
    match_highlights: &[(std::ops::Range<usize>, Style)],
    scroll_bytes: usize,
    syntax_segments: &[core::ColorSegment],
) {
    let line_byte_start = line_byte_range.start;

    // Selection path: matches are hidden behind selections. For
    // multi-cursor, apply each selection range. Single selection is the
    // common case (one entry in the slice).
    if !selected_in_line.is_empty() {
        for sel in selected_in_line {
            push_selection_spans(
                spans,
                truncated,
                line_byte_start,
                sel.clone(),
                selection_style,
                scroll_bytes,
                full_line_text,
            );
        }
        return;
    }

    // No-selection path: render the match highlights, or fall back
    // to syntax-colored text. `match_highlights` is already
    // pre-filtered to matches that start on this line.
    if match_highlights.is_empty() {
        if syntax_segments.is_empty() {
            spans.push(Span::raw(truncated.to_string()));
        } else {
            push_syntax_spans(spans, truncated, syntax_segments, scroll_bytes);
        }
        return;
    }
    push_highlight_spans(
        spans,
        truncated,
        line_byte_start,
        match_highlights,
        scroll_bytes,
    );
}

/// Push spans for the selection case (one highlight, single style).
/// Identical to the v1 selection-only behaviour — kept as a separate
/// function so the match-highlights path stays readable.
fn push_selection_spans(
    spans: &mut Vec<Span<'static>>,
    truncated: &str,
    line_byte_start: usize,
    sel: std::ops::Range<usize>,
    selection_style: Style,
    scroll_bytes: usize,
    full_line_text: &str,
) {
    let trunc_byte_len = truncated.len();
    let sel_byte_lo_full = sel.start - line_byte_start;
    let sel_byte_hi_full = sel.end - line_byte_start;

    if sel_byte_lo_full < scroll_bytes {
        // Selection extends into the off-screen left part. The visible
        // portion of the selection runs from byte 0 of truncated up to
        // sel_byte_hi_full - scroll_bytes.
        let sel_byte_hi_in_trunc = sel_byte_hi_full.saturating_sub(scroll_bytes);
        if sel_byte_hi_in_trunc == 0 {
            // Selection is entirely off-screen left.
            spans.push(Span::raw(truncated.to_string()));
            return;
        }
        let char_count = truncated.chars().count();
        let char_sel_hi =
            core::byte_to_char_col(truncated, sel_byte_hi_in_trunc.min(trunc_byte_len));
        let char_sel_hi_clamped = char_sel_hi.min(char_count);
        let selected = truncated
            .chars()
            .take(char_sel_hi_clamped)
            .collect::<String>();
        if !selected.is_empty() {
            spans.push(Span::styled(selected, selection_style));
        }
        let after = truncated
            .chars()
            .skip(char_sel_hi_clamped)
            .collect::<String>();
        if !after.is_empty() {
            spans.push(Span::raw(after));
        }
        return;
    }

    let sel_byte_lo_in_trunc = sel_byte_lo_full - scroll_bytes;
    if sel_byte_lo_in_trunc >= trunc_byte_len {
        // Selection starts past what's visible.
        spans.push(Span::raw(truncated.to_string()));
        return;
    }
    let sel_byte_hi_clamped = sel_byte_hi_full
        .saturating_sub(scroll_bytes)
        .min(trunc_byte_len);

    let char_count = truncated.chars().count();
    let char_sel_lo = core::byte_to_char_col(truncated, sel_byte_lo_in_trunc);
    let char_sel_hi = core::byte_to_char_col(truncated, sel_byte_hi_clamped);
    let _ = full_line_text; // reserved for future byte-accurate sel

    if char_sel_lo >= char_count {
        spans.push(Span::raw(truncated.to_string()));
        return;
    }
    let char_sel_hi_clamped = char_sel_hi.min(char_count);

    let before = truncated.chars().take(char_sel_lo).collect::<String>();
    let selected = truncated
        .chars()
        .skip(char_sel_lo)
        .take(char_sel_hi_clamped.saturating_sub(char_sel_lo))
        .collect::<String>();
    let after = truncated
        .chars()
        .skip(char_sel_hi_clamped)
        .collect::<String>();

    if !before.is_empty() {
        spans.push(Span::raw(before));
    }
    if !selected.is_empty() {
        spans.push(Span::styled(selected, selection_style));
    }
    if !after.is_empty() {
        spans.push(Span::raw(after));
    }
}

/// Push spans for the no-selection case where `match_highlights` lists
/// every match that starts on this line (with the current match styled
/// brighter than the others). Translates each highlight's full-line
/// byte range into a char range within `truncated`, sorts by start,
/// and walks the truncated text emitting plain/styled/plain segments.
///
/// Highlights are byte ranges relative to the full line; the function
/// handles `scroll_bytes` shifting + truncation clipping. Highlights
/// entirely off-screen are dropped. Adjacent highlights with the same
/// style are emitted as one segment (small win, mostly relevant when
/// the query is 1 byte).
fn push_highlight_spans(
    spans: &mut Vec<Span<'static>>,
    truncated: &str,
    line_byte_start: usize,
    match_highlights: &[(std::ops::Range<usize>, Style)],
    scroll_bytes: usize,
) {
    let trunc_byte_len = truncated.len();
    let char_count = truncated.chars().count();
    let mut segments: Vec<(usize, usize, Style)> = Vec::with_capacity(match_highlights.len());
    for (range, style) in match_highlights {
        let lo_full = range.start - line_byte_start;
        let hi_full = range.end - line_byte_start;
        // Off-screen left or empty/inverted: skip.
        if hi_full <= scroll_bytes {
            continue;
        }
        let lo_trunc = lo_full.saturating_sub(scroll_bytes);
        let hi_trunc = hi_full.saturating_sub(scroll_bytes).min(trunc_byte_len);
        if hi_trunc <= lo_trunc {
            continue;
        }
        let char_lo = core::byte_to_char_col(truncated, lo_trunc).min(char_count);
        let char_hi = core::byte_to_char_col(truncated, hi_trunc).min(char_count);
        if char_lo >= char_hi {
            continue;
        }
        segments.push((char_lo, char_hi, *style));
    }
    // Sort by start so the walk below emits segments in left-to-right
    // order. Within the same start, keep insertion order (stable sort).
    segments.sort_by_key(|s| s.0);

    // Merge adjacent segments with the same style. memchr::memmem
    // returns non-overlapping matches so we won't see two ranges
    // overlap here in practice — adjacent-with-same-style is the only
    // merge case that matters.
    let merged = merge_adjacent_same_style(segments);

    let mut cursor = 0usize;
    for (lo, hi, style) in merged {
        if lo > cursor {
            let plain: String = truncated.chars().skip(cursor).take(lo - cursor).collect();
            if !plain.is_empty() {
                spans.push(Span::raw(plain));
            }
        }
        let inside: String = truncated.chars().skip(lo).take(hi - lo).collect();
        if !inside.is_empty() {
            spans.push(Span::styled(inside, style));
        }
        cursor = hi;
    }
    if cursor < char_count {
        let tail: String = truncated.chars().skip(cursor).collect();
        if !tail.is_empty() {
            spans.push(Span::raw(tail));
        }
    }
}

/// Merge adjacent segments with the same style. Assumes segments are
/// sorted by start. Returns a new Vec; does not mutate the input.
fn merge_adjacent_same_style(segments: Vec<(usize, usize, Style)>) -> Vec<(usize, usize, Style)> {
    let mut out: Vec<(usize, usize, Style)> = Vec::with_capacity(segments.len());
    for seg in segments {
        if let Some(last) = out.last_mut() {
            if last.1 == seg.0 && last.2 == seg.2 {
                last.1 = seg.1;
                continue;
            }
        }
        out.push(seg);
    }
    out
}

/// Push spans for a single highlighted byte range (selection OR match
/// — same code path). Translates the range's full-line byte offsets
/// into within-truncated offsets, then emits at most three spans:
/// before (plain) / inside (styled) / after (plain). Out-of-view
/// portions of the range are clipped silently.
#[allow(dead_code)] // kept for now; replaced by push_highlight_spans
fn push_single_range(
    spans: &mut Vec<Span<'static>>,
    truncated: &str,
    line_byte_start: usize,
    range: std::ops::Range<usize>,
    style: Style,
    scroll_bytes: usize,
) {
    let trunc_byte_len = truncated.len();
    let range_lo_full = range.start - line_byte_start;
    let range_hi_full = range.end - line_byte_start;

    if range_lo_full >= scroll_bytes.saturating_add(trunc_byte_len) {
        // Range starts past what's visible.
        spans.push(Span::raw(truncated.to_string()));
        return;
    }
    let range_lo_in_trunc = range_lo_full.saturating_sub(scroll_bytes);
    let range_hi_in_trunc = range_hi_full
        .saturating_sub(scroll_bytes)
        .min(trunc_byte_len);

    if range_hi_in_trunc == 0 {
        // Range is entirely to the left of the visible window.
        spans.push(Span::raw(truncated.to_string()));
        return;
    }

    let char_count = truncated.chars().count();
    let char_lo = core::byte_to_char_col(truncated, range_lo_in_trunc);
    let char_hi = core::byte_to_char_col(truncated, range_hi_in_trunc);

    if char_lo >= char_count {
        spans.push(Span::raw(truncated.to_string()));
        return;
    }
    let char_hi_clamped = char_hi.min(char_count);

    let before = truncated.chars().take(char_lo).collect::<String>();
    let inside = truncated
        .chars()
        .skip(char_lo)
        .take(char_hi_clamped.saturating_sub(char_lo))
        .collect::<String>();
    let after = truncated.chars().skip(char_hi_clamped).collect::<String>();

    if !before.is_empty() {
        spans.push(Span::raw(before));
    }
    if !inside.is_empty() {
        spans.push(Span::styled(inside, style));
    }
    if !after.is_empty() {
        spans.push(Span::raw(after));
    }
}

/// Push spans for syntax-highlighted text. Walks the token list
/// left-to-right, emitting each token as a `Span::styled` with the
/// token's color. Gaps between tokens (whitespace not covered by any
/// token) are emitted as `Span::raw`.
///
/// Token byte ranges are relative to the full line text; this function
/// maps them into the truncated (horizontally-scrolled) view, clipping
/// tokens that fall off-screen.
fn push_syntax_spans(
    spans: &mut Vec<Span<'static>>,
    truncated: &str,
    segments: &[core::ColorSegment],
    scroll_bytes: usize,
) {
    let trunc_len = truncated.len();
    let mut char_cursor = 0usize;

    for seg in segments {
        let vis_start = seg.range.start.saturating_sub(scroll_bytes);
        let vis_end = seg.range.end.saturating_sub(scroll_bytes);
        if vis_end == 0 || vis_start >= trunc_len {
            continue;
        }
        let vis_start = vis_start.min(trunc_len);
        let vis_end = vis_end.min(trunc_len);

        let char_lo = core::byte_to_char_col(truncated, vis_start);
        let char_hi = core::byte_to_char_col(truncated, vis_end);

        if char_lo > char_cursor {
            let gap: String = truncated
                .chars()
                .skip(char_cursor)
                .take(char_lo - char_cursor)
                .collect();
            if !gap.is_empty() {
                spans.push(Span::raw(gap));
            }
        }

        let text: String = truncated
            .chars()
            .skip(char_lo)
            .take(char_hi.saturating_sub(char_lo))
            .collect();
        if !text.is_empty() {
            let c = seg.color;
            spans.push(Span::styled(
                text,
                Style::default().fg(Color::Rgb(c.r, c.g, c.b)),
            ));
        }
        char_cursor = char_hi;
    }

    let total_chars = truncated.chars().count();
    if char_cursor < total_chars {
        let tail: String = truncated.chars().skip(char_cursor).collect();
        if !tail.is_empty() {
            spans.push(Span::raw(tail));
        }
    }
}

fn compute_cursor_screen_pos(app: &App, area: Rect) -> Option<Position> {
    let cursor_pos = app.active_buffer().cursor();
    let (cursor_line, cursor_byte_col) = app.active_buffer().pos_to_linecol(cursor_pos)?;
    let top_line = app.active_doc().view.scroll_top_line;
    if cursor_line < top_line {
        return None;
    }
    let row_in_view = cursor_line - top_line;
    if row_in_view >= app.viewport_height as usize {
        return None;
    }

    let total_lines = app.active_buffer().line_count();
    let gutter_width = total_lines.to_string().len().max(2);
    let prefix = format!("{:>gutter_width$} │ ", cursor_line + 1);
    // Account for the git-gutter marker and LSP diagnostic marker columns.
    let prefix_chars = 2 + prefix.chars().count();

    // Convert byte column to char column for the cursor's line.
    let line_text = app
        .active_buffer()
        .line_text(cursor_line)
        .map(|c| c.into_owned())
        .unwrap_or_default();
    let char_col = core::byte_to_char_col(&line_text, cursor_byte_col);
    let scroll_x = app.active_doc().view.scroll_x_cols;
    // If the cursor is scrolled off the left edge, hide it.
    let visible_char_col = char_col.saturating_sub(scroll_x);
    // Char width is 1 cell in a monospace terminal — caller is
    // ratatui which draws one char per cell.
    let cursor_x = if visible_char_col == char_col - scroll_x && char_col >= scroll_x {
        prefix_chars + visible_char_col
    } else {
        // Off-screen left.
        prefix_chars
    };

    Some(Position::new(
        area.x + cursor_x as u16,
        area.y + row_in_view as u16,
    ))
}
