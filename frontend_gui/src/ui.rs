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

use crate::app::{CloseChoice, EditorApp};

const FONT_SIZE: f32 = 14.0;
const CARET_WIDTH: f32 = 2.0;
const TAB_ACTIVE_BG: egui::Color32 = egui::Color32::from_rgb(60, 80, 140);
const TAB_INACTIVE_FG: egui::Color32 = egui::Color32::from_rgb(150, 150, 150);
const TAB_SEPARATOR: egui::Color32 = egui::Color32::from_rgb(80, 80, 80);

/// Render the header strip. For a single document, this is the legacy
/// "filename + dirty marker" header. For multiple documents, it becomes
/// a tab strip — one labelled cell per open doc, with the active doc
/// highlighted. Clicking an inactive tab switches the active document;
/// clicking the active tab is a no-op. The single-doc form keeps the
/// old, quieter look (just one filename) so users without tabs don't
/// see a stray empty strip.
fn render_header_strip(ui: &mut egui::Ui, app: &mut EditorApp) {
    if app.doc_count() == 1 {
        let path = app
            .active_buffer()
            .source_path()
            .and_then(|p| p.to_str())
            .unwrap_or("[No Name]");
        let dirty = if app.is_dirty() { " [+]" } else { "" };
        ui.label(
            egui::RichText::new(format!(" {path}{dirty}"))
                .strong()
                .monospace(),
        );
        return;
    }

    // Multi-doc tab strip. Each tab is its own label so egui tracks
    // click responses per-tab. The active tab's label has a coloured
    // background; inactive tabs are dimmed. Clicking an inactive tab
    // switches `app.active`.
    ui.horizontal(|ui| {
        ui.add_space(4.0);
        for i in 0..app.doc_count() {
            let is_active = i == app.active;
            let doc = &app.documents[i];
            let name = doc.display_name();
            let dirty = if doc.is_dirty() { "*" } else { "" };
            let label = format!(" {}{} ", name, dirty);

            let text = if is_active {
                egui::RichText::new(&label)
                    .monospace()
                    .strong()
                    .color(egui::Color32::WHITE)
                    .background_color(TAB_ACTIVE_BG)
            } else {
                egui::RichText::new(&label)
                    .monospace()
                    .color(TAB_INACTIVE_FG)
            };

            let response = ui.label(text);
            if !is_active && response.clicked() {
                app.active = i;
            }

            if i + 1 < app.doc_count() {
                // Thin separator between tabs. egui's add_space gives
                // us a uniform gap; the visual separator char keeps the
                // tab boundary obvious.
                ui.add_space(2.0);
                ui.label(
                    egui::RichText::new("│")
                        .monospace()
                        .color(TAB_SEPARATOR),
                );
                ui.add_space(2.0);
            }
        }
    });
}

pub fn render(ctx: &egui::Context, app: &mut EditorApp) {
    let (status_message, status_pos) = {
        let msg = app.status_message.clone().unwrap_or_default();
        let cursor_pos = app.active_buffer().cursor();
        let (line, col) = app.active_buffer().pos_to_linecol(cursor_pos)
            .unwrap_or((0, 0));
        let pos = format_position(line, col, app.active_buffer().line_count());
        (msg, pos)
    };

    egui::TopBottomPanel::top("header").show(ctx, |ui| {
        render_header_strip(ui, app);
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

    // Modal prompts render on top. Close-confirm is a centred dialog;
    // the open-file dialog is also centred. The order matters only if
    // both were up (they shouldn't be — opening one drops the other)
    // and we want close-confirm on top because it's tied to a
    // destructive action.
    if app.close_confirm.is_some() {
        render_close_confirm_window(ctx, app);
    }
    if app.open_file_dialog.is_some() {
        render_open_file_window(ctx, app);
    }
}

/// Render the close-on-dirty dialog as a centred egui::Window.
/// Three buttons (Save / Discard / Cancel); the focused choice has a
/// coloured background. Tab / Shift+Tab cycle the focused choice via
/// [`crate::app::EditorApp::cycle_close_choice`] which we call from
/// `dispatch_modal_event`; Enter / `y` confirm via
/// [`crate::app::EditorApp::confirm_close_choice`].
fn render_close_confirm_window(ctx: &egui::Context, app: &mut EditorApp) {
    let mut open = true;
    // Snapshot the bits we need before opening the window so we can
    // take a `&mut` borrow on `app` inside the closure without
    // colliding with the read of `app.close_confirm`.
    let (doc_name, focused) = match app.close_confirm.as_ref() {
        Some(c) => (app.documents[c.doc_index].display_name(), c.choice),
        None => return,
    };

    egui::Window::new("Unsaved changes")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .open(&mut open)
        .show(ctx, |ui| {
            ui.label(format!("'{doc_name}' has unsaved changes."));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if focused_button(ui, focused, CloseChoice::Save, "Save").clicked() {
                    app.close_confirm = None;
                    match app.active_buffer_mut().save() {
                        Ok(()) => {
                            app.status_message = Some("Saved.".to_string());
                            app.perform_close_active();
                        }
                        Err(e) => app.status_message = Some(format!("Save error: {e}")),
                    }
                }
                if focused_button(ui, focused, CloseChoice::Discard, "Discard").clicked() {
                    app.close_confirm = None;
                    app.perform_close_active();
                }
                if focused_button(ui, focused, CloseChoice::Cancel, "Cancel").clicked() {
                    app.close_confirm = None;
                    app.status_message = Some("Close cancelled.".to_string());
                }
            });
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new("Tab to switch · Enter to confirm · Esc to cancel")
                    .small()
                    .weak(),
            );
        });

    // If the user clicked the window's X, treat as cancel.
    if !open && app.close_confirm.is_some() {
        app.close_confirm = None;
        app.status_message = Some("Close cancelled.".to_string());
    }
}

/// Draw a button whose label is styled to reflect whether it is the
/// currently-focused choice in the close-confirm prompt. Returns the
/// response so the caller can test `.clicked()`.
fn focused_button(
    ui: &mut egui::Ui,
    focused: CloseChoice,
    this: CloseChoice,
    label: &str,
) -> egui::Response {
    let is_focused = focused == this;
    let text = if is_focused {
        egui::RichText::new(label)
            .strong()
            .color(egui::Color32::WHITE)
            .background_color(TAB_ACTIVE_BG)
    } else {
        egui::RichText::new(label).color(TAB_INACTIVE_FG)
    };
    ui.button(text)
}

/// Render the open-file dialog as a centred egui::Window with a single
/// text input for the path. Enter / Esc are intercepted in
/// `dispatch_modal_event`.
fn render_open_file_window(ctx: &egui::Context, app: &mut EditorApp) {
    let mut open = true;
    egui::Window::new("Open file")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .open(&mut open)
        .show(ctx, |ui| {
            ui.label("Path:");
            let mut query = app
                .open_file_dialog
                .as_ref()
                .map(|d| d.query.clone())
                .unwrap_or_default();
            let response = ui.add(
                egui::TextEdit::singleline(&mut query)
                    .hint_text("/path/to/file")
                    .desired_width(400.0),
            );
            if response.changed() {
                if let Some(d) = app.open_file_dialog.as_mut() {
                    d.query = query;
                }
            }
            if response.gained_focus()
                || (app.open_file_dialog.as_ref().map(|d| d.query.is_empty()).unwrap_or(false)
                    && !response.has_focus())
            {
                response.request_focus();
            }
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new("Enter to open · Esc to cancel")
                    .small()
                    .weak(),
            );
        });

    if !open {
        app.cancel_open_file_dialog();
    }
}

fn render_text(ui: &mut egui::Ui, app: &mut EditorApp) {
    let total_lines = app.active_buffer().line_count();
    let gutter_width = total_lines.to_string().len().max(2);
    let font_id = egui::FontId::monospace(FONT_SIZE);

    // Detect cursor movement so we can auto-scroll the ScrollArea to
    // bring the cursor back into view. Without this, PageUp/PageDown
    // and Find jumps move the cursor off-screen and the GUI's
    // ScrollArea never follows — it only scrolls on user wheel/scrollbar
    // input. `last_seen_cursor` lives on the document so switching
    // tabs doesn't trip a spurious "cursor moved" event for the
    // newly-activated doc (which would otherwise blow away the
    // preserved scroll offset the user had set for it).
    let current_cursor = app.active_buffer().cursor();
    let cursor_moved = current_cursor != app.active_doc().view.last_seen_cursor;

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
    let cursor_pos = app.active_buffer().cursor();
    let (cursor_line, cursor_byte_col) = app.active_buffer().pos_to_linecol(cursor_pos)
        .unwrap_or((0, 0));

    let selection = app.active_buffer().selection();
    let sel_range: Option<std::ops::Range<usize>> = if selection.is_collapsed() {
        None
    } else {
        Some(selection.range())
    };

    let visuals = ui.style().visuals.clone();
    let prefix_text = format!("{:>width$} \u{2502} ", 1, width = gutter_width);
    let prefix_chars = prefix_text.chars().count();

    egui::ScrollArea::vertical()
        .id_salt(("editor_scroll", app.active))
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

                let line_text = app.active_buffer().line_text(line_idx)
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
            - app.active_doc().view.scroll_x_cols as f32 * char_width)
            .round();

                // Compute selection-in-this-line once. If there's no
                // selection, `seg` stays at the default (None) and we
                // draw the entire line as one piece.
                let sel_in_line: Option<(usize, usize)> = sel_range.as_ref().and_then(|sr| {
                    let line_byte_range = app.active_buffer().line_byte_range(line_idx)?;
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
                - app.active_doc().view.scroll_x_cols as f32 * char_width;
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
                    let new_cols = (app.active_doc().view.scroll_x_cols as i32 + cols_delta).max(0) as usize;
                    app.active_doc_mut().view.scroll_x_cols = new_cols;
                }
            }

            // Auto-scroll to cursor when it has moved off-screen. Only
            // fires when the cursor position changed since the last
            // frame (so manual wheel-scrolling past the cursor still
            // works). The cursor's content-space rect is the line at
            // `cursor_line * line_height` — passing that to
            // `scroll_to_rect` with `Align::Center` gives a PageUp/PageDown
            // feel (cursor lands in the middle) which matches most
            // editors' behavior.
            if cursor_moved {
                let cursor_content_y = cursor_line as f32 * line_height;
                let cursor_screen_y = rect.top() + cursor_content_y;
                let above = cursor_screen_y < rect.top();
                let below = cursor_screen_y + line_height > rect.bottom();
                if above || below {
                    let cursor_rect = egui::Rect::from_min_size(
                        egui::pos2(0.0, cursor_content_y),
                        egui::vec2(rect.width(), line_height),
                    );
                    ui.scroll_to_rect(cursor_rect, Some(egui::Align::Center));
                }
            }
        });

    // Mark the cursor position as seen so the next frame's
    // `cursor_moved` check correctly detects fresh motion. Per-doc
    // so we don't conflate "tab switched" with "cursor moved within
    // this doc" — see the comment at the top of `render_text`.
    app.active_doc_mut().view.last_seen_cursor = current_cursor;
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
    let total_lines = app.active_buffer().line_count();
    if line_offset >= total_lines {
        // Click past the last line — position at end of buffer.
        return Some(app.active_buffer().len());
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

    let line_text = app.active_buffer().line_text(line_offset)
        .map(|c| c.into_owned())
        .unwrap_or_default();
    let total_chars = line_text.chars().count();
    let char_col = char_col.min(total_chars);
    let byte_col = core::char_col_to_byte_col(&line_text, char_col);

    let line_byte_start = app.active_buffer().line_byte_range(line_offset)?.start;
    Some(line_byte_start + byte_col)
}