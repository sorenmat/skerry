//! egui rendering for the editor.
//!
//! Layout (top to bottom):
//! ```text
//! ┌─────────────────────────────────────────────────────────────────┐
//! │  header: file path + dirty marker                              │
//! ├─────────────────────────────────────────────────────────────────┤
//! │  scroll area with line-numbered text                           │
//! │  - cursor line: highlighted background                         │
//! │  - character cursor: 2px caret drawn at exact byte position    │
//! │  - selection: semi-transparent rect over selected byte range   │
//! ├─────────────────────────────────────────────────────────────────┤
//! │  status: message + L{line}:{col} / L{total}                    │
//! └─────────────────────────────────────────────────────────────────┘
//! ```

use core::{byte_to_char_col, format_position, selection_in_line, EditorEvent};
use eframe::egui;

use crate::app::EditorApp;

const FONT_SIZE: f32 = 14.0;
const CARET_WIDTH: f32 = 2.0;

pub fn render(ctx: &egui::Context, app: &mut EditorApp) {
    // Extract everything as owned data so panel closures don't all
    // borrow `app` mutably at once.
    let header_text = {
        let path = app
            .buffer
            .source_path()
            .and_then(|p| p.to_str())
            .unwrap_or("[No Name]");
        let dirty = if app.is_dirty() { " [+]" } else { "" };
        format!(" {path}{dirty}")
    };
    let (status_message, status_pos) = {
        let msg = app.status_message.clone().unwrap_or_default();
        let cursor_pos = app.buffer.cursor();
        let (line, col) = app
            .buffer
            .pos_to_linecol(cursor_pos)
            .unwrap_or((0, 0));
        let pos = format_position(line, col, app.buffer.line_count());
        (msg, pos)
    };

    egui::TopBottomPanel::top("header").show(ctx, |ui| {
        ui.label(
            egui::RichText::new(&header_text)
                .strong()
                .monospace(),
        );
    });

    egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
        ui.label(
            egui::RichText::new(format!(" {status_message}  |  {status_pos}"))
                .monospace(),
        );
    });

    // Find bar — appears above the status bar when open.
    if app.search.bar_open {
        egui::TopBottomPanel::bottom("find_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(" Find: ").strong().monospace());
                let mut query = app.search.query.clone();
                let response = ui.add(
                    egui::TextEdit::singleline(&mut query)
                        .hint_text("type to search")
                        .desired_width(300.0),
                );
                if response.changed() {
                    app.handle_event(EditorEvent::FindQueryChanged(query));
                }
                // Auto-focus the text input so the user can type
                // immediately. Only focus on first appearance to
                // avoid stealing focus every frame.
                if response.gained_focus()
                    || (app.search.query.is_empty() && !response.has_focus())
                {
                    response.request_focus();
                }
                let total = app.search.matches.len();
                let current = app.search.current.map(|i| i + 1).unwrap_or(0);
                ui.label(
                    egui::RichText::new(if total == 0 && !app.search.query.is_empty() {
                        " (no matches)".to_string()
                    } else {
                        format!(" {current}/{total} ")
                    })
                    .monospace(),
                );
                if ui.button("Next").clicked() {
                    app.handle_event(EditorEvent::FindNext);
                }
                if ui.button("Prev").clicked() {
                    app.handle_event(EditorEvent::FindPrev);
                }
                if ui.button("Close").clicked() {
                    app.handle_event(EditorEvent::FindClose);
                }
            });
        });
    }

    egui::CentralPanel::default().show(ctx, |ui| {
        render_text(ui, app);
    });
}

fn render_text(ui: &mut egui::Ui, app: &mut EditorApp) {
    let total_lines = app.buffer.line_count();
    let gutter_width = total_lines.to_string().len().max(2);
    let font_id = egui::FontId::monospace(FONT_SIZE);

    // Measure glyph / line dimensions once per frame.
    //
    // Important: egui's monospace font renders tabs as ~4 character
    // widths of advance (not 1). Using `char_width` for every char — tab
    // included — makes every x position after the first tab drift right
    // by `3 * char_width` per tab. That drift makes the selection rect
    // and after-selection text land at the wrong column, which is what
    // caused the "garbled text after a tabbed selection" bug. Use
    // `glyph_width` per character (including `'\t'`) when computing
    // segment widths so positions match what `painter.text` actually
    // renders.
    let (char_width, tab_width, line_height) = ui.fonts(|f| {
        let cw = f.glyph_width(&font_id, 'M');
        let tw = f.glyph_width(&font_id, '\t');
        let lh = f.row_height(&font_id);
        (cw, tw, lh)
    });

    let advance_of = |c: char| -> f32 {
        if c == '\t' {
            tab_width
        } else {
            char_width
        }
    };
    let width_of = |s: &str| -> f32 { s.chars().map(advance_of).sum() };

    // Cursor + selection state.
    let cursor_pos = app.buffer.cursor();
    let (cursor_line, cursor_byte_col) = app
        .buffer
        .pos_to_linecol(cursor_pos)
        .unwrap_or((0, 0));

    let selection = app.buffer.selection();
    let sel_range: Option<std::ops::Range<usize>> = if selection.is_collapsed() {
        None
    } else {
        Some(selection.range())
    };

    let visuals = ui.style().visuals.clone();
    let prefix_text = format!("{:>width$} \u{2502} ", 1, width = gutter_width);
    let prefix_chars = prefix_text.chars().count();

    egui::ScrollArea::vertical()
        .auto_shrink([false; 2])
        .show(ui, |ui| {
            let total_height = total_lines as f32 * line_height;
            let response = ui.allocate_response(
                egui::vec2(ui.available_width(), total_height),
                egui::Sense::click_and_drag(),
            );
            let rect = response.rect;
            let painter = ui.painter_at(rect);

            // Tell the app how many lines fit in the visible viewport
            // so PageUp/PageDown can scroll by a real page instead of a
            // guessed default. Use the response rect height (visible
            // area only) divided by line height.
            let visible_height = ui.available_height().min(rect.height());
            let visible_lines = (visible_height / line_height).floor() as usize;
            if visible_lines > 0 {
                app.viewport_lines = visible_lines;
            }

            // Draw cursor line background first (under everything).
            if cursor_line < total_lines {
                let y = (rect.top() + cursor_line as f32 * line_height).round();
                painter.rect_filled(
                    egui::Rect::from_min_size(
                        egui::pos2(rect.left(), y),
                        egui::vec2(rect.width(), line_height),
                    ),
                    0.0,
                    visuals.faint_bg_color,
                );
            }

            // Draw each line: gutter, text segments (before/inside/after
            // selection), and the cursor caret. Drawing the line as three
            // separate text segments — instead of drawing the full line
            // and then re-drawing the selected portion on top of the
            // selection rectangle — eliminates the "ghost" / "shadow"
            // effect that comes from any subpixel positioning mismatch
            // between the two text draws.
            for line_idx in 0..total_lines {
                // Round y to integer pixels so glyphs align cleanly with
                // the selection rectangle.
                let y = (rect.top() + line_idx as f32 * line_height).round();

                let line_text = app
                    .buffer
                    .line_text(line_idx)
                    .map(|c| c.into_owned())
                    .unwrap_or_default();

                // Gutter (line number + separator).
                let gutter = format!("{:>width$} \u{2502} ", line_idx + 1, width = gutter_width);
                painter.text(
                    egui::pos2((rect.left()).round(), y),
                    egui::Align2::LEFT_TOP,
                    gutter,
                    font_id.clone(),
                    visuals.weak_text_color(),
                );

                let text_x = (rect.left() + prefix_chars as f32 * char_width
            - app.scroll_x_cols as f32 * char_width)
            .round();

                // Compute selection-in-this-line once. If there's no
                // selection, `seg` stays at the default (None) and we
                // draw the entire line as one piece.
                let sel_in_line: Option<(usize, usize)> = sel_range.as_ref().and_then(|sr| {
                    let line_byte_range = app.buffer.line_byte_range(line_idx)?;
                    let intersect = selection_in_line(line_byte_range.clone(), sr.clone())?;
                    let start = line_byte_range.start;
                    let total_chars = line_text.chars().count();
                    let take_lo =
                        byte_to_char_col(&line_text, intersect.start - start).min(total_chars);
                    let take_hi =
                        byte_to_char_col(&line_text, intersect.end - start).min(total_chars);
                    if take_hi > take_lo {
                        Some((take_lo, take_hi))
                    } else {
                        None
                    }
                });

                match sel_in_line {
                    None => {
                        // No selection on this line — draw the whole line.
                        painter.text(
                            egui::pos2(text_x, y),
                            egui::Align2::LEFT_TOP,
                            &line_text,
                            font_id.clone(),
                            visuals.text_color(),
                        );
                    }
                    Some((take_lo, take_hi)) => {
                        // Three segments: before / selected / after.
                        // Each is drawn exactly once in the normal text color.
                        let before: String =
                            line_text.chars().take(take_lo).collect();
                        let selected: String = line_text
                            .chars()
                            .skip(take_lo)
                            .take(take_hi - take_lo)
                            .collect();
                        let after: String =
                            line_text.chars().skip(take_hi).collect();

                        let sel_x = (text_x + width_of(&before)).round();
                        let sel_w = width_of(&selected).round();

                        if !before.is_empty() {
                            painter.text(
                                egui::pos2(text_x, y),
                                egui::Align2::LEFT_TOP,
                                before,
                                font_id.clone(),
                                visuals.text_color(),
                            );
                        }

                        painter.rect_filled(
                            egui::Rect::from_min_size(
                                egui::pos2(sel_x, y),
                                egui::vec2(sel_w, line_height),
                            ),
                            0.0,
                            visuals.selection.bg_fill,
                        );
                        painter.text(
                            egui::pos2(sel_x, y),
                            egui::Align2::LEFT_TOP,
                            selected,
                            font_id.clone(),
                            visuals.text_color(),
                        );

                        if !after.is_empty() {
                            painter.text(
                                egui::pos2((sel_x + sel_w).round(), y),
                                egui::Align2::LEFT_TOP,
                                after,
                                font_id.clone(),
                                visuals.text_color(),
                            );
                        }
                    }
                }

                // Character cursor caret (drawn last, on top).
                // Only draw the caret when the selection is collapsed.
                // When a non-empty selection exists, the selection rectangle
                // already marks the head position — drawing the caret on top
                // of it would cover the first character of the after-selection
                // text on the head line, making it look like that character
                // was eaten by the selection.
                if line_idx == cursor_line && sel_range.is_none() {
                    let char_col = byte_to_char_col(&line_text, cursor_byte_col);
                    let caret_x = (text_x + char_col as f32 * char_width).round();
                    painter.rect_filled(
                        egui::Rect::from_min_size(
                            egui::pos2(caret_x, y),
                            egui::vec2(CARET_WIDTH, line_height),
                        ),
                        0.0,
                        visuals.text_color(),
                    );
                }
            }

            // Mouse handling: convert pointer position to byte position
            // and dispatch SetCursor (click) or SelectExtendTo (drag).
            // text_x is the SCREEN position of the text origin (gutter
            // right-edge minus horizontal scroll). pixel_to_byte_pos
            // uses it to map pointer.x → char_col.
            let text_x = rect.left() + prefix_chars as f32 * char_width
                - app.scroll_x_cols as f32 * char_width;
            if response.clicked() || response.drag_started() || response.dragged() {
                if let Some(pos) = response.interact_pointer_pos() {
                    if let Some(byte_pos) = pixel_to_byte_pos(
                        app,
                        pos,
                        rect,
                        text_x,
                        char_width,
                        line_height,
                        prefix_chars,
                        gutter_width,
                    ) {
                        if response.drag_started() || response.dragged() {
                            app.handle_event(EditorEvent::SelectExtendTo { pos: byte_pos });
                        } else {
                            app.handle_event(EditorEvent::SetCursor { pos: byte_pos });
                        }
                    }
                }
            }

            // Horizontal scroll: Shift+scroll wheel scrolls left/right
            // instead of up/down. egui's ScrollArea already eats the
            // wheel for vertical scroll; we hijack the modifier.
            let scroll_delta = ui.input(|i| i.smooth_scroll_delta);
            if scroll_delta != egui::Vec2::ZERO {
                let shift = ui.input(|i| i.modifiers.shift);
                if shift {
                    // Shift+wheel: horizontal scroll. Convert pixel
                    // delta to column delta using char_width.
                    let cols_delta = (scroll_delta.y / char_width).round() as i32;
                    let new_cols = (app.scroll_x_cols as i32 + cols_delta).max(0) as usize;
                    app.scroll_x_cols = new_cols;
                }
            }
        });
}

/// Convert a pointer position (relative to the editor window) to a byte
/// position in the buffer. Returns None if the position is outside the
/// text area (e.g. above the viewport top).
#[allow(clippy::too_many_arguments)]
fn pixel_to_byte_pos(
    app: &EditorApp,
    pointer: egui::Pos2,
    rect: egui::Rect,
    text_x: f32,
    char_width: f32,
    line_height: f32,
    prefix_chars: usize,
    gutter_width: usize,
) -> Option<core::BytePos> {
    let rel_x = pointer.x - rect.left();
    let rel_y = pointer.y - rect.top();
    if rel_y < 0.0 {
        return None;
    }
    let line_offset = (rel_y / line_height).floor() as usize;
    let total_lines = app.buffer.line_count();
    if line_offset >= total_lines {
        // Click past the last line — position at end of buffer.
        return Some(app.buffer.len());
    }

    // Determine char column from x. If the click is in the gutter, snap
    // to the start of the line.
let text_x_relative = text_x - rect.left();
        let char_col = if rel_x < text_x_relative {
            0usize
        } else {
            ((rel_x - text_x_relative) / char_width) as usize
        };
        let _ = (prefix_chars, gutter_width); // currently unused; reserved for future

    let line_text = app
        .buffer
        .line_text(line_offset)
        .map(|c| c.into_owned())
        .unwrap_or_default();
    let total_chars = line_text.chars().count();
    let char_col = char_col.min(total_chars);
    let byte_col = core::char_col_to_byte_col(&line_text, char_col);

    let line_byte_start = app.buffer.line_byte_range(line_offset)?.start;
    Some(line_byte_start + byte_col)
}