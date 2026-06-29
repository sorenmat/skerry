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

/// Map a `TokenKind` to a ratatui `Color`. Uses a palette similar to
/// the GUI's (VS Code Dark+ inspired), adjusted for terminal
/// visibility.
fn syntax_color(kind: core::TokenKind) -> Color {
    match kind {
        core::TokenKind::Keyword => Color::Rgb(86, 156, 214),
        core::TokenKind::Type => Color::Rgb(78, 201, 176),
        core::TokenKind::Function => Color::Rgb(220, 220, 170),
        core::TokenKind::String => Color::Rgb(206, 145, 120),
        core::TokenKind::Comment => Color::Rgb(106, 153, 85),
        core::TokenKind::Number => Color::Rgb(181, 206, 168),
        core::TokenKind::Punctuation | core::TokenKind::Identifier => Color::Reset,
    }
}

/// Get syntax tokens for a line, using the per-document cache.
fn get_syntax_tokens(app: &mut App, line_idx: usize, line_text: &str) -> Vec<core::Token> {
    if app.active_buffer().len() > core::SYNTAX_SIZE_LIMIT {
        return Vec::new();
    }
    if !app.active_doc().syntax.dirty {
        if let Some(tokens) = app.active_doc().syntax.lines.get(&line_idx) {
            return tokens.clone();
        }
    }
    let path = app.active_doc().path();
    let tokens = core::tokenize_line(path, line_text.as_bytes());
    let doc = app.active_doc_mut();
    if doc.syntax.dirty {
        doc.syntax.lines.clear();
        doc.syntax.dirty = false;
    }
    doc.syntax.lines.insert(line_idx, tokens.clone());
    tokens
}

pub fn render(f: &mut Frame, app: &mut App) {
    let area = f.area();

    // Vertical chunks: header, content, status, and a row each for the
    // find bar, replace bar, close-confirm prompt, and open-file
    // dialog (whichever are open). Modals take priority: we always
    // reserve their row when their state is set so opening/closing
    // one doesn't make the rest of the layout jump.
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
    let content = render_content(app, chunks[1].width);    f.render_widget(Paragraph::new(content), chunks[1]);

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
    if let Some(dialog) = &app.open_file_dialog {
        let line = render_open_file_dialog(dialog);
        f.render_widget(Paragraph::new(line), chunks[idx]);
        // No further modals to index after this.
    }

    // Position the terminal cursor. The find bar, replace bar, and
    // open-file dialog all have a text input — they want the cursor
    // at end of their query string. Close-confirm has no input;
    // cursor stays on the buffer if visible. Replace bar takes
    // priority over find bar (it's the more recently opened
    // modal); find bar takes priority over no modal.
    if app.search.replace_bar_open {
        let replace_idx = if app.search.bar_open { 4 } else { 3 };
        let prefix_chars = " Replace: ".chars().count() as u16;
        let cursor_x = chunks[replace_idx].x + prefix_chars
            + app.search.replace_query.chars().count() as u16;
        f.set_cursor_position(Position::new(cursor_x, chunks[replace_idx].y));
    } else if app.search.bar_open {
        let find_idx = 3;
        let query_prefix_chars = " Find: ".chars().count() as u16;
        let cursor_x = chunks[find_idx].x + query_prefix_chars
            + app.search.query.chars().count() as u16;
        f.set_cursor_position(Position::new(cursor_x, chunks[find_idx].y));
    } else if let Some(dialog) = app.open_file_dialog.as_ref() {
        // The dialog row sits at the last allocated chunk — chunks is
        // built in declaration order so the dialog row is always the
        // final entry. `unwrap` is safe: we just rendered into it.
        let last = chunks.last().unwrap();
        let prefix_chars = " Open: ".chars().count() as u16;
        let cursor_x = last.x + prefix_chars
            + dialog.query.chars().count() as u16;
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
    // Soft-wrap indicator: shows "wrap" when on so the user has a
    // persistent visual confirmation of the toggle (the status
    // message itself disappears after a few seconds). Empty when
    // off so the status bar stays clean in the common case.
    let wrap_indicator = if app.active_doc().view.soft_wrap {
        "  |  wrap"
    } else {
        ""
    };
    Line::from(format!(" {message}  |  {pos}{wrap_indicator}"))
}

fn render_content(app: &mut App, viewport_width: u16) -> Vec<Line<'static>> {
    let total_lines = app.active_buffer().line_count();
    // Gutter: enough digits to fit the largest line number, minimum 2.
    let gutter_width = total_lines.to_string().len().max(2);

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

    let selection = app.active_buffer().selection();
    let sel_range: Option<std::ops::Range<usize>> = if selection.is_collapsed() {
        None
    } else {
        Some(selection.range())
    };

    // Snapshot the search state once per frame so the per-line loop
    // doesn't have to look it up. `current_match_start` is the byte
    // position of the active match (or None when there's no current
    // match — empty query, no matches, or before the first FindNext).
    let query_byte_len = app.search.query.len();
    let current_match_start = app.search.current_match();

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
        // Compute the match highlights for this line. Only when there's
        // no active selection — selection takes priority visually so the
        // user can see their drag without matches painting over it.
        //
        // For each match start that falls on this line (binary-search
        // the first one, then iterate forward until past line end), emit
        // a (byte_range, style) tuple. Style is `current_match_style`
        // for the match the cursor is on, `other_match_style` for the
        // rest. Skipping matches that start on a previous line keeps the
        // v1 implementation simple — cross-line matches render their
        // first-line portion only (rare in practice).
        let mut match_highlights: Vec<(std::ops::Range<usize>, Style)> = Vec::new();
        if sel_range.is_none() && query_byte_len > 0 {
            let start_idx = app.search.matches.partition_point(|&m| m < line_byte_range.start);
            for &m in &app.search.matches[start_idx..] {
                if m >= line_byte_range.end {
                    break;
                }
                let end = (m + query_byte_len).min(line_byte_range.end);
                let style = if Some(m) == current_match_start {
                    current_match_style
                } else {
                    other_match_style
                };
                match_highlights.push((m..end, style));
            }
        }

        // Apply horizontal scroll: skip `scroll_x` chars from the start of
        // each line, then truncate to the available width.
        let scroll_x = app.active_doc().view.scroll_x_cols;
        let truncated: String = line_text.chars().skip(scroll_x).take(avail).collect();
        // Convert scroll_x (chars) into a byte offset within this
        // line so selection math can work in bytes.
        let scroll_bytes = core::char_col_to_byte_col(&line_text, scroll_x);

        let mut spans: Vec<Span<'static>> = vec![Span::styled(prefix, gutter_style)];

        // Get syntax tokens for this line (lazy cache population).
        let syntax_tokens = get_syntax_tokens(app, line_idx, &line_text);

        push_line_spans(
            &mut spans,
            &truncated,
            line_byte_range,
            &line_text,
            selected_in_line,
            selection_style,
            &match_highlights,
            scroll_bytes,
            &syntax_tokens,
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
    selected_in_line: Option<std::ops::Range<usize>>,
    selection_style: Style,
    match_highlights: &[(std::ops::Range<usize>, Style)],
    scroll_bytes: usize,
    syntax_tokens: &[core::Token],
) {
    let line_byte_start = line_byte_range.start;

    // Selection path: matches are hidden behind the selection. Keeps
    // the visual simple — one highlight style at a time.
    if let Some(sel) = selected_in_line {
        push_selection_spans(
            spans,
            truncated,
            line_byte_start,
            sel,
            selection_style,
            scroll_bytes,
            full_line_text,
        );
        return;
    }

    // No-selection path: render the match highlights, or fall back
    // to syntax-colored text. `match_highlights` is already
    // pre-filtered to matches that start on this line.
    if match_highlights.is_empty() {
        if syntax_tokens.is_empty() {
            // No syntax (unknown extension, too large, or passthrough).
            spans.push(Span::raw(truncated.to_string()));
        } else {
            push_syntax_spans(
                spans,
                truncated,
                syntax_tokens,
                scroll_bytes,
            );
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
        let after = truncated.chars().skip(char_sel_hi_clamped).collect::<String>();
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
    let sel_byte_hi_clamped = sel_byte_hi_full.saturating_sub(scroll_bytes).min(trunc_byte_len);

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
            let plain: String = truncated
                .chars()
                .skip(cursor)
                .take(lo - cursor)
                .collect();
            if !plain.is_empty() {
                spans.push(Span::raw(plain));
            }
        }
        let inside: String = truncated
            .chars()
            .skip(lo)
            .take(hi - lo)
            .collect();
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
fn merge_adjacent_same_style(
    segments: Vec<(usize, usize, Style)>,
) -> Vec<(usize, usize, Style)> {
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
    tokens: &[core::Token],
    scroll_bytes: usize,
) {
    let trunc_len = truncated.len();
    let mut char_cursor = 0usize; // char position in truncated

    for tok in tokens {
        // Map token byte range (in full line) to byte range in truncated.
        let vis_start = tok.range.start.saturating_sub(scroll_bytes);
        let vis_end = tok.range.end.saturating_sub(scroll_bytes);
        if vis_end == 0 || vis_start >= trunc_len {
            continue; // entirely off-screen
        }
        let vis_start = vis_start.min(trunc_len);
        let vis_end = vis_end.min(trunc_len);

        let char_lo = core::byte_to_char_col(truncated, vis_start);
        let char_hi = core::byte_to_char_col(truncated, vis_end);

        // Gap before this token.
        if char_lo > char_cursor {
            let gap: String = truncated.chars().skip(char_cursor).take(char_lo - char_cursor).collect();
            if !gap.is_empty() {
                spans.push(Span::raw(gap));
            }
        }

        // The token itself.
        let seg: String = truncated.chars().skip(char_lo).take(char_hi.saturating_sub(char_lo)).collect();
        if !seg.is_empty() {
            spans.push(Span::styled(seg, Style::default().fg(syntax_color(tok.kind))));
        }
        char_cursor = char_hi;
    }

    // Trailing gap.
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