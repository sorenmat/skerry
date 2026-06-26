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

    // Three vertical chunks: header, content, status.
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(1),
        ])
        .split(area);

    let header = render_header(app);
    f.render_widget(Paragraph::new(header), chunks[0]);

    app.adjust_viewport(chunks[1].height);
    let content = render_content(app, chunks[1].width);
    f.render_widget(Paragraph::new(content), chunks[1]);

    let status = render_status(app);
    f.render_widget(Paragraph::new(status), chunks[2]);

    // Position the terminal cursor over the buffer cursor.
    if let Some(pos) = compute_cursor_screen_pos(app, chunks[1]) {
        f.set_cursor_position(pos);
    }
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

        // Truncate the line to the available width.
        let truncated: String = line_text.chars().take(avail).collect();

        let mut spans: Vec<Span<'static>> = vec![Span::styled(prefix, gutter_style)];
        push_line_spans(
            &mut spans,
            &truncated,
            line_byte_range,
            &line_text,
            selected_in_line,
            selection_style,
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
fn push_line_spans(
    spans: &mut Vec<Span<'static>>,
    truncated: &str,
    line_byte_range: std::ops::Range<usize>,
    full_line_text: &str,
    selected_in_line: Option<std::ops::Range<usize>>,
    selection_style: Style,
) {
    // Byte offsets of the truncated portion relative to full_line_text.
    let trunc_byte_len = truncated.len();
    let line_byte_start = line_byte_range.start;

    let Some(sel) = selected_in_line else {
        spans.push(Span::raw(truncated.to_string()));
        return;
    };

    // Convert absolute byte offsets to within-line byte offsets.
    let sel_byte_lo = sel.start - line_byte_start;
    let sel_byte_hi = sel.end - line_byte_start;

    // Clamp to the truncated region (which may be a prefix of the line).
    if sel_byte_lo >= trunc_byte_len {
        // Selection starts past what's visible.
        spans.push(Span::raw(truncated.to_string()));
        return;
    }
    let sel_byte_hi_clamped = sel_byte_hi.min(trunc_byte_len);

    // Walk the truncated text by char positions, splitting at the
    // selection boundary. We have byte offsets; convert via
    // char_to_byte / byte_to_char on the truncated string.
    let char_count = truncated.chars().count();
    let char_sel_lo = core::byte_to_char_col(truncated, sel_byte_lo);
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

    Some(Position::new(
        area.x + (prefix_chars + char_col) as u16,
        area.y + row_in_view as u16,
    ))
}