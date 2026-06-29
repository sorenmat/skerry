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
/// Exponential-decay speed for caret animation (1/seconds). Higher =
/// snappier. At 25, the time constant is 40 ms — the caret reaches
/// ~70 % of the way in 3 frames (50 ms at 60 fps), which feels
/// responsive without looking like a hard teleport.
const CARET_ANIM_SPEED: f32 = 25.0;

/// Map a `TokenKind` to a GUI color. Uses a VS Code Dark+ inspired
/// palette — readable on dark backgrounds, distinct enough to scan at
/// a glance.
fn syntax_color(kind: core::TokenKind) -> egui::Color32 {
    match kind {
        core::TokenKind::Keyword => egui::Color32::from_rgb(86, 156, 214),
        core::TokenKind::Type => egui::Color32::from_rgb(78, 201, 176),
        core::TokenKind::Function => egui::Color32::from_rgb(220, 220, 170),
        core::TokenKind::String => egui::Color32::from_rgb(206, 145, 120),
        core::TokenKind::Comment => egui::Color32::from_rgb(106, 153, 85),
        core::TokenKind::Number => egui::Color32::from_rgb(181, 206, 168),
        core::TokenKind::Punctuation | core::TokenKind::Identifier => {
            egui::Color32::from_rgb(212, 212, 212)
        }
    }
}
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
                // avoid stealing focus every frame. Skip auto-focus
                // when the replace bar is also open — the user
                // probably wants to type into the replacement.
                if response.gained_focus()
                    || (app.search.query.is_empty()
                        && !response.has_focus()
                        && !app.search.replace_bar_open)
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
                let replace_toggle_label = if app.search.replace_bar_open {
                    "Hide Replace"
                } else {
                    "Replace"
                };
                if ui.button(replace_toggle_label).clicked() {
                    if app.search.replace_bar_open {
                        app.handle_event(EditorEvent::ReplaceClose);
                    } else {
                        app.handle_event(EditorEvent::ReplaceOpen);
                    }
                }
                if ui.button("Close").clicked() {
                    app.handle_event(EditorEvent::FindClose);
                }
            });
            // Replace row sits directly below the find row. Visible
            // only when `replace_bar_open` is set. Tab / focus shifts
            // are implicit via egui's text-input focus model.
            if app.search.replace_bar_open {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(" Replace: ").strong().monospace());
                    let mut rq = app.search.replace_query.clone();
                    let response = ui.add(
                        egui::TextEdit::singleline(&mut rq)
                            .hint_text("replacement")
                            .desired_width(300.0),
                    );
                    if response.changed() {
                        app.handle_event(EditorEvent::ReplaceQueryChanged(rq));
                    }
                    // Auto-focus the replace input the first time the
                    // bar appears so the user can type immediately.
                    // Don't steal focus every frame.
                    if response.gained_focus()
                        || (app.search.replace_query.is_empty()
                            && !response.has_focus())
                    {
                        response.request_focus();
                    }
                    if ui.button("Replace").clicked() {
                        app.handle_event(EditorEvent::ReplaceOne);
                    }
                    if ui.button("Replace All").clicked() {
                        app.handle_event(EditorEvent::ReplaceAll);
                    }
                });
            }
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
    // Compute the ScrollArea's persistent id up front so we can read
    // its `State.offset` from inside the closure (where the auto-scroll
    // helper needs the current offset to compute its delta). The
    // ScrollArea itself wraps its salt in `Id::new(...)` first and then
    // calls `ui.make_persistent_id(id_salt)`; we mirror that exactly
    // here so the hashes match and `get_persisted` actually returns
    // the State that's been stored under the same id. (Calling
    // `ui.make_persistent_id(("editor_scroll", app.active))` directly
    // would hash the raw tuple, NOT the Id-wrapped salt — different
    // bytes into the hasher, different id, empty lookup.)
    let scroll_id = ui.make_persistent_id(egui::Id::new(("editor_scroll", app.active)));
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

    // Find-match highlight. `current_match()` returns the start byte;
    // the match length equals the query byte length (memchr operates
    // on raw bytes, so start + query.len() is the correct end).
    //
    // Two intensities mirror VSCode / Sublime:
    // - current_match_color: bright amber for the match the user
    //   most recently navigated to (cursor sits at its start).
    // - other_match_color: dimmer amber for every other match so the
    //   eye can scan the cluster at a glance.
    //
    // Visible only when there's no selection — selection preempts
    // matches so the user can see their drag without matches
    // painting over it. Same precedence rule as the TUI frontend.
    let query_byte_len = app.search.query.len();
    let current_match_start = app.search.current_match();
    let current_match_color = egui::Color32::from_rgb(200, 160, 40);
    let other_match_color = egui::Color32::from_rgb(120, 100, 40);

    let visuals = ui.style().visuals.clone();
    let prefix_text = format!("{:>width$} \u{2502} ", 1, width = gutter_width);
    let prefix_chars = prefix_text.chars().count();

    // Compute the desired scroll offset BEFORE the ScrollArea so we
    // can pass it via `.vertical_scroll_offset()`. egui bakes the
    // offset into the inner UI's coordinate space at `begin` time
    // (line 591 of scroll_area.rs: `inner_rect.min - state.offset`),
    // so setting it on the builder makes the SAME frame's painting
    // use the correct offset. This eliminates the one-frame lag that
    // caused visible cursor jumpiness — the caret no longer dips a
    // line below the pin row on each arrow press.
    //
    // We only override when the cursor actually moved; otherwise we
    // leave the offset alone so manual wheel scrolling is preserved.
    let scroll_override_y: Option<f32> = if cursor_moved {
        let current_offset_y: f32 = ui
            .ctx()
            .data_mut(|d| {
                d.get_persisted::<egui::containers::scroll_area::State>(scroll_id)
                    .map(|s| s.offset.y)
            })
            .unwrap_or(0.0);
        let desired = compute_desired_scroll_offset(
            current_offset_y,
            cursor_line,
            line_height,
            app.viewport_lines.max(1),
            app.active_doc().view.scroll_margin_lines,
        );
        if (desired - current_offset_y).abs() > 0.01 {
            Some(desired)
        } else {
            None
        }
    } else {
        None
    };

    // Animate the caret's vertical position so it slides between
    // lines instead of teleporting. Three cases:
    //
    // 1. View scrolled (edge-stick) → snap. The scroll override
    //    already moved the content; animating on top would make the
    //    caret drift from the pin row.
    // 2. Tab switch or first frame → snap. No slide from the old
    //    tab's caret to the new one.
    // 3. Cursor moved freely within the viewport → lerp toward the
    //    target. The caret slides for ~50 ms, which feels smooth
    //    without lagging behind rapid key repeats.
    let target_caret_y = cursor_line as f32 * line_height;
    let tab_switched = app.active != app.prev_active;
    if scroll_override_y.is_some() || tab_switched || app.caret_anim_y.is_nan() {
        app.caret_anim_y = target_caret_y;
    } else {
        let dt = ui.input(|i| i.stable_dt).min(0.1);
        let lerp = 1.0 - (-CARET_ANIM_SPEED * dt).exp();
        app.caret_anim_y += (target_caret_y - app.caret_anim_y) * lerp;
    }
    app.prev_active = app.active;

    let scroll_area = egui::ScrollArea::vertical()
        .id_salt(("editor_scroll", app.active))
        .auto_shrink([false; 2]);

    let scroll_area = if let Some(y) = scroll_override_y {
        scroll_area.vertical_scroll_offset(y)
    } else {
        scroll_area
    };

    scroll_area.show(ui, |ui| {
            let total_height = total_lines as f32 * line_height;
            let response = ui.allocate_response(
                egui::vec2(ui.available_width(), total_height),
                egui::Sense::click_and_drag(),
            );
            let rect = response.rect;
            let painter = ui.painter_at(rect);

            // Tell the app how many lines fit in the visible viewport
            // so PageUp/PageDown and the auto-scroll code can use the
            // REAL viewport height. `ui.clip_rect()` is the actual
            // visible region of the ScrollArea (what the user can see),
            // which is the correct source of truth here. Earlier this
            // used `ui.available_height().min(rect.height())`, but
            // after `allocate_response(total_height)` the available
            // height collapses to ~0 and `rect.height()` is the full
            // allocated content — the `min` picked the wrong thing and
            // `viewport_lines` got stuck at its default (20) forever.
            // That made auto-scroll think vh=20 even in a 40-line
            // window, so the cursor pinned at row 16 — the middle.
            let visible_height = ui.clip_rect().height();
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
                // draw the entire line as one piece. `line_byte_range`
                // is bound in the outer scope so the match-highlights
                // block below can read it without re-querying.
                let line_byte_range =
                    app.active_buffer().line_byte_range(line_idx).unwrap_or(0..0);
                let sel_in_line: Option<(usize, usize)> = sel_range.as_ref().and_then(|sr| {
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

                // Compute match highlights for this line. One entry per match that
                // starts on this line, each tagged with the colour
                // (bright for current, dim for the rest). Same shape
                // as `sel_in_line` (a char-col range) but it's a Vec
                // because there can be many matches on a line. Skipped
                // when a selection is present so the user can see
                // their selection without matches painting over it.
                //
                // Skipping matches that start on a previous line keeps
                // the v1 implementation simple — cross-line matches
                // render their first-line portion only (rare in
                // practice).
                let mut match_highlights: Vec<(usize, usize, egui::Color32)> = Vec::new();
                if sel_in_line.is_none() && query_byte_len > 0 {
                    let start_idx = app.search.matches.partition_point(|&m| m < line_byte_range.start);
                    for &m in &app.search.matches[start_idx..] {
                        if m >= line_byte_range.end {
                            break;
                        }
                        let end = (m + query_byte_len).min(line_byte_range.end);
                        let intersect = (line_byte_range.start.max(m))..(line_byte_range.start.max(end));
                        let start = line_byte_range.start;
                        let total_chars = line_text.chars().count();
                        let take_lo = byte_to_char_col(
                            &line_text,
                            intersect.start - start,
                        )
                        .min(total_chars);
                        let take_hi = byte_to_char_col(
                            &line_text,
                            intersect.end - start,
                        )
                        .min(total_chars);
                        if take_hi <= take_lo {
                            continue;
                        }
                        let color = if Some(m) == current_match_start {
                            current_match_color
                        } else {
                            other_match_color
                        };
                        match_highlights.push((take_lo, take_hi, color));
                    }
                }

                if let Some((take_lo, take_hi)) = sel_in_line {
                    // Selection rendering: three segments
                    // (before / selected / after). Each is drawn
                    // exactly once at an integer-rounded x to avoid
                    // sub-pixel ghosting on the selection rectangle.
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
                } else if match_highlights.is_empty() {
                    // Plain line — check for syntax highlighting.
                    // Tokens come from the per-document SyntaxCache,
                    // lazily populated. Only lines without selection
                    // and without match highlights get syntax colors
                    // (precedence: selection > matches > syntax).
                    let tokens = get_syntax_tokens(app, line_idx, &line_text);
                    if tokens.is_empty() {
                        // No syntax (unknown extension, too large, or
                        // passthrough) — draw as before.
                        painter.text(
                            egui::pos2(text_x, y),
                            egui::Align2::LEFT_TOP,
                            &line_text,
                            font_id.clone(),
                            visuals.text_color(),
                        );
                    } else {
                        // Walk tokens left-to-right, drawing each
                        // segment in its syntax color. Gaps between
                        // tokens (whitespace) use the default color.
                        let mut char_cursor = 0usize;
                        for tok in &tokens {
                            let tok_lo =
                                byte_to_char_col(&line_text, tok.range.start);
                            let tok_hi =
                                byte_to_char_col(&line_text, tok.range.end);
                            // Draw gap before this token.
                            if tok_lo > char_cursor {
                                let gap: String = line_text
                                    .chars()
                                    .skip(char_cursor)
                                    .take(tok_lo - char_cursor)
                                    .collect();
                                if !gap.is_empty() {
                                    let gap_x = (text_x
                                        + width_of(
                                            &line_text
                                                .chars()
                                                .take(char_cursor)
                                                .collect::<String>(),
                                        ))
                                    .round();
                                    painter.text(
                                        egui::pos2(gap_x, y),
                                        egui::Align2::LEFT_TOP,
                                        gap,
                                        font_id.clone(),
                                        visuals.text_color(),
                                    );
                                }
                            }
                            // Draw the token itself.
                            let seg: String = line_text
                                .chars()
                                .skip(tok_lo)
                                .take(tok_hi - tok_lo)
                                .collect();
                            if !seg.is_empty() {
                                let seg_x = (text_x
                                    + width_of(
                                        &line_text
                                            .chars()
                                            .take(tok_lo)
                                            .collect::<String>(),
                                    ))
                                .round();
                                painter.text(
                                    egui::pos2(seg_x, y),
                                    egui::Align2::LEFT_TOP,
                                    seg,
                                    font_id.clone(),
                                    syntax_color(tok.kind),
                                );
                            }
                            char_cursor = tok_hi;
                        }
                        // Trailing gap after last token.
                        let total_chars = line_text.chars().count();
                        if char_cursor < total_chars {
                            let tail: String = line_text
                                .chars()
                                .skip(char_cursor)
                                .collect();
                            if !tail.is_empty() {
                                let tail_x = (text_x
                                    + width_of(
                                        &line_text
                                            .chars()
                                            .take(char_cursor)
                                            .collect::<String>(),
                                    ))
                                .round();
                                painter.text(
                                    egui::pos2(tail_x, y),
                                    egui::Align2::LEFT_TOP,
                                    tail,
                                    font_id.clone(),
                                    visuals.text_color(),
                                );
                            }
                        }
                    }
                } else {
                    // Multi-match highlights. Walk the line text
                    // left-to-right, emitting plain / styled /
                    // plain / styled / ... segments. Each styled
                    // segment gets its own background rectangle so
                    // adjacent matches render as adjacent coloured
                    // bars.
                    let mut highlights = match_highlights;
                    // Stable sort by start (matches on the same
                    // char-col keep insertion order — shouldn't
                    // happen in practice since memchr matches are
                    // non-overlapping).
                    highlights.sort_by_key(|h| h.0);

                    let mut cursor = 0usize;
                    let total_chars = line_text.chars().count();
                    for (lo, hi, color) in highlights {
                        if lo > cursor {
                            let plain: String = line_text
                                .chars()
                                .skip(cursor)
                                .take(lo - cursor)
                                .collect();
                            if !plain.is_empty() {
                                painter.text(
                                    egui::pos2(text_x, y),
                                    egui::Align2::LEFT_TOP,
                                    plain,
                                    font_id.clone(),
                                    visuals.text_color(),
                                );
                            }
                        }
                        let matched: String = line_text
                            .chars()
                            .skip(lo)
                            .take(hi - lo)
                            .collect();
                        if !matched.is_empty() {
                            // Width must be measured from `text_x` so
                            // we account for the chars before this
                            // segment too — `width_of(&matched)` alone
                            // would be wrong if a previous segment
                            // included tabs (tab advance ≠ 1 char).
                            let matched_x = (text_x + width_of(
                                &line_text.chars().take(lo).collect::<String>(),
                            ))
                            .round();
                            let matched_w = width_of(&matched).round();
                            painter.rect_filled(
                                egui::Rect::from_min_size(
                                    egui::pos2(matched_x, y),
                                    egui::vec2(matched_w, line_height),
                                ),
                                0.0,
                                color,
                            );
                            painter.text(
                                egui::pos2(matched_x, y),
                                egui::Align2::LEFT_TOP,
                                matched,
                                font_id.clone(),
                                visuals.text_color(),
                            );
                        }
                        cursor = hi;
                    }
                    if cursor < total_chars {
                        let tail: String =
                            line_text.chars().skip(cursor).collect();
                        if !tail.is_empty() {
                            painter.text(
                                egui::pos2(text_x, y),
                                egui::Align2::LEFT_TOP,
                                tail,
                                font_id.clone(),
                                visuals.text_color(),
                            );
                        }
                    }
                }

                // (Caret painting moved outside the loop — see below.)
            }

            // Character cursor caret — painted after the line loop so
            // it sits on top of all text, at the animated y position.
            // Only draw when the selection is collapsed; a non-empty
            // selection rectangle already marks the head position.
            if sel_range.is_none() && cursor_line < total_lines {
                let caret_line_text = app.active_buffer().line_text(cursor_line)
                    .map(|c| c.into_owned())
                    .unwrap_or_default();
                let char_col = byte_to_char_col(&caret_line_text, cursor_byte_col);
                let text_x = (rect.left() + prefix_chars as f32 * char_width
                    - app.active_doc().view.scroll_x_cols as f32 * char_width)
                    .round();
                let caret_x = (text_x + char_col as f32 * char_width).round();
                let caret_y = (rect.top() + app.caret_anim_y).round();
                painter.rect_filled(
                    egui::Rect::from_min_size(
                        egui::pos2(caret_x, caret_y),
                        egui::vec2(CARET_WIDTH, line_height),
                    ),
                    0.0,
                    visuals.text_color(),
                );
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
        });

    // Mark the cursor position as seen so the next frame's
    // `cursor_moved` check correctly detects fresh motion. Per-doc
    // so we don't conflate "tab switched" with "cursor moved within
    // this doc" — see the comment at the top of `render_text`.
    app.active_doc_mut().view.last_seen_cursor = current_cursor;
}

/// Get syntax tokens for a line, using the per-document cache.
/// Tokenizes on cache miss (lazy population). Returns an empty Vec
/// when syntax highlighting is disabled (file too large, unknown
/// extension, or no path).
fn get_syntax_tokens(
    app: &mut EditorApp,
    line_idx: usize,
    line_text: &str,
) -> Vec<core::Token> {
    // Size gate — skip tokenization for very large files.
    if app.active_buffer().len() > core::SYNTAX_SIZE_LIMIT {
        return Vec::new();
    }

    // Check the cache first.
    if !app.active_doc().syntax.dirty {
        if let Some(tokens) = app.active_doc().syntax.lines.get(&line_idx) {
            return tokens.clone();
        }
    }

    // Cache miss or dirty — tokenize this line.
    let path = app.active_doc().path();
    let tokens = core::tokenize_line(path, line_text.as_bytes());

    // Cache the result. If the cache was dirty, this is the first
    // line of a fresh population pass — clear dirty so subsequent
    // lines use the cache.
    let doc = app.active_doc_mut();
    if doc.syntax.dirty {
        doc.syntax.lines.clear();
        doc.syntax.dirty = false;
    }
    doc.syntax.lines.insert(line_idx, tokens.clone());
    tokens
}

/// Compute the desired vertical scroll offset so the cursor stays
/// inside the viewport's "safe zone" (within `margin_lines` of the
/// top and bottom rows). Pure function — no side effects, no egui
/// calls. The caller passes the result to
/// `ScrollArea::vertical_scroll_offset()` BEFORE the ScrollArea
/// closure, so egui bakes it into the inner UI's coordinate space
/// at `begin` time and the same frame's painting uses it.
///
/// **Edge-stick / scroll-margin semantics** (Emacs `scroll-step: 1`
/// with `scroll-margin`, matches VSCode / Sublime / Atom): when the
/// cursor moves within `margin_lines` of the viewport's bottom row,
/// scroll by exactly enough so the cursor lands at row
/// `vh - margin - 1` (and likewise at the top). When the cursor is
/// already inside the safe zone, the return value equals
/// `current_offset_y` (no-op).
fn compute_desired_scroll_offset(
    current_offset_y: f32,
    cursor_line: usize,
    line_height: f32,
    viewport_lines: usize,
    margin_lines: usize,
) -> f32 {
    let vh_lines = viewport_lines.max(1);
    let visible_height = (vh_lines as f32) * line_height;

    // **Edge case**: if `2 * margin + 1 > vh` the safe zone
    // (rows [margin, vh - 1 - margin]) collapses to nothing and
    // every cursor position triggers a scroll. Fall back to
    // `margin = 0` (legacy edge-stick) so small windows don't
    // trip a scroll on every keypress.
    let effective_margin: usize = if vh_lines > 2 * margin_lines + 1 {
        margin_lines
    } else {
        0
    };
    let margin_px = (effective_margin as f32) * line_height;

    let cursor_top_y = (cursor_line as f32) * line_height;
    let cursor_bottom_y = cursor_top_y + line_height;

    let mut desired_top_y = current_offset_y;

    if cursor_bottom_y > current_offset_y + visible_height - margin_px {
        // Scroll DOWN: cursor's bottom entered the bottom margin.
        // After scrolling, cursor's top lands at row (vh - margin - 1):
        //   cursor_top_y - new_offset = (vh - margin - 1) * lh
        desired_top_y = cursor_top_y - (visible_height - line_height - margin_px);
    } else if cursor_top_y < current_offset_y + margin_px {
        // Scroll UP: cursor's top entered the top margin.
        // After scrolling, cursor's top lands at row `margin`:
        //   cursor_top_y - new_offset = margin * lh
        desired_top_y = cursor_top_y - margin_px;
    }

    desired_top_y.max(0.0)
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