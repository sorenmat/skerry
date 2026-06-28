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
    widgets::{Clear, Paragraph},
    Frame,
};

use crate::app::{App, CloseChoice};

pub fn render(f: &mut Frame, app: &mut App) {
    let area = f.area();

    // Vertical chunks: header, content, status, and a row each for the
    // find bar, close-confirm prompt, and open-file dialog (whichever
    // are open). Modals take priority: we always reserve their row
    // when their state is Some so opening/closing one doesn't make the
    // rest of the layout jump.
    let mut constraints = vec![
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
    ];
    if app.search.bar_open {
        constraints.push(Constraint::Length(1));
    }
    if app.close_confirm.is_some() {
        constraints.push(Constraint::Length(1));
    }
    if app.open_file_dialog.is_some() {
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

    // Modal rows, in order. Index = 3 + (1 per preceding modal that's
    // open). We track it explicitly to avoid the off-by-one that
    // would come from chained `if` indexing.
    let mut idx = 3;
    if app.search.bar_open {
        let find_line = render_find_bar(app);
        f.render_widget(Paragraph::new(find_line), chunks[idx]);
        idx += 1;
    }
    if let Some(confirm) = &app.close_confirm {
        let line = render_close_confirm(confirm, &app.documents[confirm.doc_index]);
        f.render_widget(Paragraph::new(line), chunks[idx]);
        idx += 1;
    }
    if let Some(dialog) = &app.open_file_dialog {
        let line = render_open_file_dialog(dialog);
        f.render_widget(Paragraph::new(line), chunks[idx]);
        // No further modals to index after this.
    }

    // Position the terminal cursor. The find bar and the open-file
    // dialog both have a text input — they want the cursor at end of
    // their query string. Close-confirm has no input; cursor stays on
    // the buffer if visible.
    if app.search.bar_open {
        let find_idx = 3;
        let query_prefix_chars = " Find: ".chars().count() as u16;
        let cursor_x = chunks[find_idx].x + query_prefix_chars
            + app.search.query.chars().count() as u16;
        f.set_cursor_position(Position::new(cursor_x, chunks[find_idx].y));
    } else if app.open_file_dialog.is_some() {
        // The dialog row sits at the last allocated chunk — chunks is
        // built in declaration order so the dialog row is always the
        // final entry. `unwrap` is safe: we just rendered into it.
        let last = chunks.last().unwrap();
        let prefix_chars = " Open: ".chars().count() as u16;
        let cursor_x = last.x + prefix_chars
            + app.open_file_dialog.as_ref().unwrap().query.chars().count() as u16;
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
}

/// Render the close-on-dirty prompt as a single line at the bottom.
/// The line shows the three choices with the focused one highlighted
/// by reverse video. Also includes a hint about the key bindings.
fn render_close_confirm(
    confirm: &crate::app::CloseConfirm,
    doc: &core::Document,
) -> Line<'static> {
    let doc_name = doc.display_name();
    let dirty_msg = format!("'{doc_name}' has unsaved changes.");
    let choice_label = |c: CloseChoice, label: &str| -> Span<'static> {
        let focused = confirm.choice == c;
        let mut style = Style::default();
        if focused {
            style = style.bg(Color::Rgb(60, 80, 140)).fg(Color::White)
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
fn render_close_confirm_overlay(
    f: &mut Frame,
    area: Rect,
    confirm: &crate::app::CloseConfirm,
) {
    // Centre horizontally on the available area, with one row of padding
    // above and below.
    let label_w = 60usize.min(area.width as usize);
    let x = area.x + (area.width.saturating_sub(label_w as u16)) / 2;
    let y = area.y + area.height / 2;
    let overlay_rect = Rect::new(x, y, label_w as u16, 3);
    f.render_widget(Clear, overlay_rect);

    let focused = confirm.choice;
    let save_style = if focused == CloseChoice::Save {
        Style::default().bg(Color::Rgb(60, 80, 140)).fg(Color::White)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let discard_style = if focused == CloseChoice::Discard {
        Style::default().bg(Color::Rgb(60, 80, 140)).fg(Color::White)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let cancel_style = if focused == CloseChoice::Cancel {
        Style::default().bg(Color::Rgb(60, 80, 140)).fg(Color::White)
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

/// Render the open-file text-input prompt.
fn render_open_file_dialog(dialog: &crate::app::OpenFileDialog) -> Line<'static> {
    Line::from(format!(" Open: {}█", dialog.query))
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
        return Line::from(format!(" {path}{dirty}"));
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
        let label = format!(" {}{} ", name, dirty);
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

fn render_status(app: &App) -> Line<'static> {
    let message = app.status_message.as_deref().unwrap_or("");
    let cursor_pos = app.active_buffer().cursor();
    let (line, col) = app.active_buffer().pos_to_linecol(cursor_pos)
        .unwrap_or((0, 0));
    let pos = core::format_position(line, col, app.active_buffer().line_count());
    Line::from(format!(" {message}  |  {pos}"))
}

fn render_content(app: &App, viewport_width: u16) -> Vec<Line<'static>> {
    let total_lines = app.active_buffer().line_count();
    // Gutter: enough digits to fit the largest line number, minimum 2.
    let gutter_width = total_lines.to_string().len().max(2);

    let selection_style = Style::default()
        .bg(Color::Rgb(60, 80, 140))
        .fg(Color::White)
        .add_modifier(Modifier::BOLD);
    // Find-match highlight. Distinct colour from selection so the
    // user can tell "this is the match the search landed on" from
    // "this is a selection I dragged out".
    let match_style = Style::default()
        .bg(Color::Rgb(200, 160, 40))
        .fg(Color::Black)
        .add_modifier(Modifier::BOLD);
    let gutter_style = Style::default().fg(Color::DarkGray);

    let selection = app.active_buffer().selection();
    let sel_range: Option<std::ops::Range<usize>> = if selection.is_collapsed() {
        None
    } else {
        Some(selection.range())
    };

    // The byte range of the currently-active find match. `search.query`
    // is UTF-8; the byte length of the match equals the byte length of
    // the query (memchr operates on raw bytes, so start + len is the
    // correct end). If the match straddles a newline the visual will
    // look odd, but that's a v2 issue — for v1 matches are expected to
    // stay within a single line in normal usage.
    let match_range: Option<std::ops::Range<usize>> = app
        .search
        .current_match()
        .map(|pos| pos..pos.saturating_add(app.search.query.len()));

    let mut lines: Vec<Line<'static>> = Vec::new();
    let vh = app.viewport_height as usize;
    let top_line = app.active_doc().view.scroll_top_line;
    let end_line = (top_line + vh).min(total_lines);

    for line_idx in top_line..end_line {
        let line_text = app.active_buffer().line_text(line_idx)
            .map(|cow| cow.into_owned())
            .unwrap_or_default();

        let prefix = format!("{:>width$} │ ", line_idx + 1, width = gutter_width);
        let prefix_chars = prefix.chars().count();
        let avail = (viewport_width as usize).saturating_sub(prefix_chars);

        // Compute the selected sub-range within this line, if any.
        let line_byte_range = app.active_buffer().line_byte_range(line_idx).unwrap_or(0..0);
        let selected_in_line = sel_range
            .as_ref()
            .and_then(|sr| selection_in_line(line_byte_range.clone(), sr.clone()));
        // Compute the match sub-range within this line, if any.
        let match_in_line = match_range
            .as_ref()
            .and_then(|mr| selection_in_line(line_byte_range.clone(), mr.clone()));

        // Apply horizontal scroll: skip `scroll_x` chars from the start of
        // each line, then truncate to the available width.
        let scroll_x = app.active_doc().view.scroll_x_cols;
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
            match_in_line,
            match_style,
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
/// to the selected byte range and match styling to the matched byte
/// range (mapped back to char positions for the truncated visible
/// text).
///
/// Precedence: selection > match. When the user has an active drag
/// selection, that wins visually and the match highlight is hidden.
/// When the selection is collapsed (the post-FindNext state), the
/// match shows through. This avoids the complexity of stacking two
/// highlight styles on the same span while still giving the user
/// "find landed here, this is what it found" feedback in the common
/// case.
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
    selected_in_line: Option<std::ops::Range<usize>>,
    selection_style: Style,
    match_in_line: Option<std::ops::Range<usize>>,
    match_style: Style,
    scroll_bytes: usize,
) {
    let trunc_byte_len = truncated.len();
    let line_byte_start = line_byte_range.start;

    // No selection visible — render the match (if any) as the lone
    // highlight, or fall back to plain text.
    let Some(sel) = selected_in_line else {
        if let Some(m) = match_in_line {
            push_single_range(
                spans,
                truncated,
                line_byte_start,
                m,
                match_style,
                scroll_bytes,
            );
        } else {
            spans.push(Span::raw(truncated.to_string()));
        }
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

/// Push spans for a single highlighted byte range (selection OR match
/// — same code path). Translates the range's full-line byte offsets
/// into within-truncated offsets, then emits at most three spans:
/// before (plain) / inside (styled) / after (plain). Out-of-view
/// portions of the range are clipped silently.
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
    let range_hi_in_trunc = range_hi_full.saturating_sub(scroll_bytes).min(trunc_byte_len);

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
    let prefix_chars = prefix.chars().count();

    // Convert byte column to char column for the cursor's line.
    let line_text = app.active_buffer().line_text(cursor_line)
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