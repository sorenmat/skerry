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
    layout::{Constraint, Direction, Layout, Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::app::App;

pub fn render(f: &mut Frame, app: &mut App) {
    let area = f.area();

    // Four vertical chunks: header, content, status, optional find bar.
    let mut constraints = vec![
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
    ];
    if app.search.bar_open {
        constraints.push(Constraint::Length(1));
    }
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    let header = render_header(app);
    f.render_widget(Paragraph::new(header), chunks[0]);

    app.adjust_viewport(chunks[1].height);
    let content = render_content(app, chunks[1].width);
    f.render_widget(Paragraph::new(content), chunks[1]);

    let status = render_status(app);
    f.render_widget(Paragraph::new(status), chunks[2]);

    if app.search.bar_open {
        let find_line = render_find_bar(app);
        f.render_widget(Paragraph::new(find_line), chunks[3]);
    }

    // Position the terminal cursor over the buffer cursor OR the
    // find bar's text input.
    if app.search.bar_open {
        // Cursor at end of query in the find bar.
        let query_prefix_chars = " Find: ".chars().count() as u16;
        let cursor_x = chunks[3].x + query_prefix_chars + app.search.query.chars().count() as u16;
        f.set_cursor_position(Position::new(cursor_x, chunks[3].y));
    } else if let Some(pos) = compute_cursor_screen_pos(app, chunks[1]) {
        f.set_cursor_position(pos);
    }
}

fn render_find_bar(app: &App) -> Line<'static> {
    let total = app.search.matches.len();
    let current = app.search.current.map(|i| i + 1).unwrap_or(0);
    let count = if total == 0 && !app.search.query.is_empty() {
        " (no matches)".to_string()
    } else {
        format!(" {current}/{total}")
    };
    let line = format!("/{}", app.search.query);
    Line::from(format!(" Find: {line}{count} "))
}

fn render_header(app: &App) -> Line<'static> {
    let path = app
        .buffer
        .source_path()
        .and_then(|p| p.to_str())
        .unwrap_or("[No Name]");
    let dirty = if app.is_dirty() { " [+]" } else { "" };
    Line::from(format!(" {path}{dirty}"))
}

fn render_status(app: &App) -> Line<'static> {
    let message = app.status_message.as_deref().unwrap_or("");
    let cursor_pos = app.buffer.cursor();
    let (line, col) = app
        .buffer
        .pos_to_linecol(cursor_pos)
        .unwrap_or((0, 0));
    let pos = core::format_position(line, col, app.buffer.line_count());
    Line::from(format!(" {message}  |  {pos}"))
}

fn render_content(app: &App, viewport_width: u16) -> Vec<Line<'static>> {
    let total_lines = app.buffer.line_count();
    // Gutter: enough digits to fit the largest line number, minimum 2.
    let gutter_width = total_lines.to_string().len().max(2);

    let selection_style = Style::default()
        .bg(Color::Rgb(60, 80, 140))
        .fg(Color::White)
        .add_modifier(Modifier::BOLD);
    let gutter_style = Style::default().fg(Color::DarkGray);

    let selection = app.buffer.selection();
    let sel_range: Option<std::ops::Range<usize>> = if selection.is_collapsed() {
        None
    } else {
        Some(selection.range())
    };

    let mut lines: Vec<Line<'static>> = Vec::new();
    let vh = app.viewport_height as usize;
    let end_line = (app.viewport_top_line + vh).min(total_lines);

    for line_idx in app.viewport_top_line..end_line {
        let line_text = app
            .buffer
            .line_text(line_idx)
            .map(|cow| cow.into_owned())
            .unwrap_or_default();

        let prefix = format!("{:>width$} │ ", line_idx + 1, width = gutter_width);
        let prefix_chars = prefix.chars().count();
        let avail = (viewport_width as usize).saturating_sub(prefix_chars);

        // Compute the selected sub-range within this line, if any.
        let line_byte_range = app.buffer.line_byte_range(line_idx).unwrap_or(0..0);
        let selected_in_line = sel_range
            .as_ref()
            .and_then(|sr| selection_in_line(line_byte_range.clone(), sr.clone()));

        // Apply horizontal scroll: skip `scroll_x` chars from the start of
        // each line, then truncate to the available width.
        let scroll_x = app.scroll_x as usize;
        let truncated: String = line_text.chars().skip(scroll_x).take(avail).collect();
        // Convert scroll_x (chars) into a byte offset within this
        // line so selection math can work in bytes.
        let scroll_bytes = core::char_col_to_byte_col(&line_text, scroll_x);

        let mut spans: Vec<Span<'static>> = vec![Span::styled(prefix, gutter_style)];
        push_line_spans(
            &mut spans,
            &truncated,
            line_byte_range,
            &line_text,
            selected_in_line,
            selection_style,
            scroll_bytes,
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
/// to the selected byte range (mapped back to char positions for the
/// truncated visible text).
///
/// `scroll_bytes` is the byte offset within the FULL line where the
/// visible (truncated) window starts. Selection byte offsets are
/// relative to the full line; we shift them by `scroll_bytes` to get
/// offsets within the truncated string.
fn push_line_spans(
    spans: &mut Vec<Span<'static>>,
    truncated: &str,
    line_byte_range: std::ops::Range<usize>,
    full_line_text: &str,
    selected_in_line: Option<std::ops::Range<usize>>,
    selection_style: Style,
    scroll_bytes: usize,
) {
    let trunc_byte_len = truncated.len();
    let line_byte_start = line_byte_range.start;

    let Some(sel) = selected_in_line else {
        spans.push(Span::raw(truncated.to_string()));
        return;
    };

    // Absolute selection byte offsets within this line.
    let sel_byte_lo_full = sel.start - line_byte_start;
    let sel_byte_hi_full = sel.end - line_byte_start;

    // Translate to within-truncated. Anything to the left of
    // `scroll_bytes` is off-screen; anything to the right of
    // `scroll_bytes + trunc_byte_len` is off-screen.
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
        let after = truncated.chars().skip(char_sel_hi_clamped).collect::<String>();
        if !after.is_empty() {
            spans.push(Span::raw(after));
        }
        return;
    }

    // sel_byte_lo_full >= scroll_bytes: selection starts in the visible region.
    let sel_byte_lo_in_trunc = sel_byte_lo_full - scroll_bytes;
    if sel_byte_lo_in_trunc >= trunc_byte_len {
        // Selection starts past what's visible.
        spans.push(Span::raw(truncated.to_string()));
        return;
    }
    let sel_byte_hi_clamped = sel_byte_hi_full.saturating_sub(scroll_bytes).min(trunc_byte_len);

    let char_count = truncated.chars().count();
    let char_sel_lo = core::byte_to_char_col(truncated, sel_byte_lo_in_trunc);
    let char_sel_hi = core::byte_to_char_col(truncated, sel_byte_hi_clamped);
    let _ = full_line_text; // currently unused; reserved for future byte-accurate sel

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
    let after = truncated.chars().skip(char_sel_hi_clamped).collect::<String>();

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

fn compute_cursor_screen_pos(app: &App, area: Rect) -> Option<Position> {
    let cursor_pos = app.buffer.cursor();
    let (cursor_line, cursor_byte_col) = app.buffer.pos_to_linecol(cursor_pos)?;
    if cursor_line < app.viewport_top_line {
        return None;
    }
    let row_in_view = cursor_line - app.viewport_top_line;
    if row_in_view >= app.viewport_height as usize {
        return None;
    }

    let total_lines = app.buffer.line_count();
    let gutter_width = total_lines.to_string().len().max(2);
    let prefix = format!("{:>gutter_width$} │ ", cursor_line + 1);
    let prefix_chars = prefix.chars().count();

    // Convert byte column to char column for the cursor's line.
    let line_text = app
        .buffer
        .line_text(cursor_line)
        .map(|c| c.into_owned())
        .unwrap_or_default();
    let char_col = core::byte_to_char_col(&line_text, cursor_byte_col);
    let scroll_x = app.scroll_x as usize;
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