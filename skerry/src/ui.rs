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

use core::{
    byte_to_char_col, char_col_to_byte_col, format_position, selection_in_line,
    visual_col_to_byte_col, EditorEvent,
};
use eframe::egui;

use crate::app::{CloseChoice, CsvPreviewMode, EditorApp, MarkdownPreviewMode};
use crate::theme::GuiTheme;

const FONT_SIZE: f32 = 14.0;

/// Extra leading between editor lines: the drawn line box is the font's
/// row height times this. Glyphs are centered in the box (see
/// `half_leading` in `render_text`) so the extra space splits evenly
/// above and below each line.
const LINE_SPACING: f32 = 1.5;
const CARET_WIDTH: f32 = 2.0;
/// Width of the dedicated git-gutter column (in pixels) drawn left of
/// the line numbers. Kept separate from the line-number gutter so the
/// green/yellow change marker never overlaps the number.
const GIT_GUTTER_WIDTH: f32 = 14.0;
/// Width of the optional git-blame column (in pixels) drawn left of the
/// git gutter when blame is enabled. Wide enough for "abc1234 Alice 2d".
const BLAME_WIDTH: f32 = 200.0;
/// Exponential-decay speed for caret animation (1/seconds). Higher =
/// snappier. At 25, the time constant is 40 ms — the caret reaches
/// ~70 % of the way in 3 frames (50 ms at 60 fps), which feels
/// responsive without looking like a hard teleport.
const CARET_ANIM_SPEED: f32 = 25.0;
/// Gutter annotations are glanceable metadata, so reveal them sooner than
/// egui's general-purpose 500 ms tooltip delay.
const GUTTER_TOOLTIP_DELAY_SECONDS: f32 = 0.15;

#[derive(Debug, Clone)]
struct GutterHoverInfo {
    line_number: usize,
    git_status: Option<core::LineStatus>,
    removed_count: usize,
    removed_lines: Vec<String>,
    blame: Option<core::BlameEntry>,
    diagnostics: Vec<DiagnosticHover>,
    remaining_diagnostics: usize,
}

#[derive(Debug, Clone)]
struct DiagnosticHover {
    severity: Option<lsp_types::DiagnosticSeverity>,
    source: Option<String>,
    code: Option<String>,
    message: String,
}

fn diagnostic_overlaps_line(range: &lsp_types::Range, line_idx: usize) -> bool {
    let start_line = range.start.line as usize;
    let end_line = range.end.line as usize;
    line_idx >= start_line
        && (line_idx < end_line
            || (line_idx == end_line && (range.end.character > 0 || start_line == end_line)))
}

fn gutter_hover_info(
    app: &EditorApp,
    uri: Option<&url::Url>,
    line_idx: usize,
) -> Option<GutterHoverInfo> {
    let doc = app.active_doc();
    let git_status = (doc.view.git_gutter_enabled && doc.git_gutter.enabled())
        .then(|| doc.git_gutter.status(line_idx))
        .filter(|status| *status != core::LineStatus::Unchanged);
    let removed_blocks = if doc.view.git_gutter_enabled && doc.git_gutter.enabled() {
        doc.git_gutter.removed_blocks_before(line_idx)
    } else {
        &[]
    };
    let removed_count = removed_blocks.iter().map(|block| block.count).sum();
    let removed_lines = removed_blocks
        .iter()
        .flat_map(|block| block.lines.iter())
        .take(6)
        .map(|line| truncate_hover_text(line, 160))
        .collect();
    let blame = (doc.view.git_blame_enabled && doc.git_blame.enabled())
        .then(|| doc.git_blame.entry(line_idx).cloned())
        .flatten();
    // All document borrows are done; only the LSP manager is borrowed
    // from here on, so the hover reads the latest publishDiagnostics
    // without cloning the list.
    let diagnostics: &[lsp_types::Diagnostic] = match uri {
        Some(uri) => app.lsp_manager.diagnostics(uri),
        None => &[],
    };
    let mut matching_diagnostics = diagnostics
        .iter()
        .filter(|diagnostic| diagnostic_overlaps_line(&diagnostic.range, line_idx));
    let diagnostics = matching_diagnostics
        .by_ref()
        .take(8)
        .map(|diagnostic| DiagnosticHover {
            severity: diagnostic.severity,
            source: diagnostic
                .source
                .as_deref()
                .map(|source| truncate_hover_text(source, 80)),
            code: diagnostic
                .code
                .as_ref()
                .map(|code| diagnostic_code_label(code, 80)),
            message: truncate_hover_text(&diagnostic.message, 500),
        })
        .collect::<Vec<_>>();
    let remaining_diagnostics = matching_diagnostics.count();

    if git_status.is_none() && removed_count == 0 && blame.is_none() && diagnostics.is_empty() {
        None
    } else {
        Some(GutterHoverInfo {
            line_number: line_idx + 1,
            git_status,
            removed_count,
            removed_lines,
            blame,
            diagnostics,
            remaining_diagnostics,
        })
    }
}

fn diagnostic_severity_label(severity: Option<lsp_types::DiagnosticSeverity>) -> &'static str {
    match severity {
        Some(lsp_types::DiagnosticSeverity::ERROR) => "Error",
        Some(lsp_types::DiagnosticSeverity::WARNING) => "Warning",
        Some(lsp_types::DiagnosticSeverity::INFORMATION) => "Information",
        Some(lsp_types::DiagnosticSeverity::HINT) => "Hint",
        _ => "Diagnostic",
    }
}

fn diagnostic_code_label(code: &lsp_types::NumberOrString, max_chars: usize) -> String {
    match code {
        lsp_types::NumberOrString::Number(number) => number.to_string(),
        lsp_types::NumberOrString::String(value) => truncate_hover_text(value, max_chars),
    }
}

fn truncate_hover_text(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let mut bounded = String::new();
    for _ in 0..max_chars {
        let Some(character) = chars.next() else {
            return bounded;
        };
        bounded.push(character);
    }
    if chars.next().is_some() {
        bounded.push('…');
    }
    bounded
}

fn render_gutter_hover(ui: &mut egui::Ui, info: &GutterHoverInfo, theme: &GuiTheme) {
    ui.set_max_width(480.0);
    ui.label(
        egui::RichText::new(format!("Line {}", info.line_number))
            .strong()
            .monospace(),
    );

    if let Some(status) = info.git_status {
        let (label, color) = match status {
            core::LineStatus::Added => ("Git · Added line", theme.git_added),
            core::LineStatus::Modified => ("Git · Modified line", theme.git_modified),
            core::LineStatus::Unchanged => unreachable!("unchanged lines are filtered"),
        };
        ui.label(egui::RichText::new(label).color(color));
    }

    if info.removed_count > 0 {
        ui.label(
            egui::RichText::new(format!(
                "Git · {} deleted line(s) above",
                info.removed_count
            ))
            .color(theme.git_deleted),
        );
        for line in &info.removed_lines {
            ui.label(
                egui::RichText::new(format!("− {line}"))
                    .monospace()
                    .color(theme.dim_text),
            );
        }
        if info.removed_count > info.removed_lines.len() {
            ui.label(
                egui::RichText::new(format!(
                    "… {} more",
                    info.removed_count - info.removed_lines.len()
                ))
                .small()
                .color(theme.dim_text),
            );
        }
    }

    if let Some(blame) = &info.blame {
        ui.separator();
        ui.label(
            egui::RichText::new(format!(
                "Git blame · {} · {}",
                blame.commit.full_author, blame.relative_time
            ))
            .strong(),
        );
        if !blame.commit.summary.is_empty() {
            ui.label(truncate_hover_text(&blame.commit.summary, 300));
        }
        ui.label(
            egui::RichText::new(&blame.commit.full_hash)
                .monospace()
                .small()
                .color(theme.dim_text),
        );
    }

    if !info.diagnostics.is_empty() {
        ui.separator();
        for diagnostic in &info.diagnostics {
            let mut heading = diagnostic_severity_label(diagnostic.severity).to_string();
            if let Some(source) = diagnostic.source.as_deref() {
                heading.push_str(" · ");
                heading.push_str(source);
            }
            if let Some(code) = diagnostic.code.as_deref() {
                heading.push(' ');
                heading.push_str(code);
            }
            let color = match diagnostic.severity {
                Some(lsp_types::DiagnosticSeverity::ERROR) => theme.error,
                Some(lsp_types::DiagnosticSeverity::WARNING) => theme.warning,
                Some(lsp_types::DiagnosticSeverity::INFORMATION) => theme.accent,
                _ => theme.dim_text,
            };
            ui.label(egui::RichText::new(heading).strong().color(color));
            ui.label(&diagnostic.message);
        }
        if info.remaining_diagnostics > 0 {
            ui.label(
                egui::RichText::new(format!("… {} more diagnostics", info.remaining_diagnostics))
                    .small()
                    .color(theme.dim_text),
            );
        }
    }
}

fn show_gutter_tooltip(response: egui::Response, info: &GutterHoverInfo, theme: &GuiTheme) {
    if response.has_focus() {
        response.show_tooltip_ui(|ui| render_gutter_hover(ui, info, theme));
        return;
    }

    // Response tooltip timing is read from Context, not the local Ui style.
    // Scope the faster delay synchronously and restore it immediately so all
    // unrelated controls retain egui's normal tooltip timing.
    let ctx = response.ctx.clone();
    let old_delay = ctx.style().interaction.tooltip_delay;
    ctx.style_mut(|style| {
        style.interaction.tooltip_delay = GUTTER_TOOLTIP_DELAY_SECONDS;
    });
    response.on_hover_ui_at_pointer(|ui| render_gutter_hover(ui, info, theme));
    ctx.style_mut(|style| {
        style.interaction.tooltip_delay = old_delay;
    });
}

/// Set the cursor icon to a pointing hand when hovering clickable UI.
fn hand_cursor(response: &egui::Response, ctx: &egui::Context) {
    if response.hovered() {
        ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
    }
}

fn theme(app: &EditorApp) -> &GuiTheme {
    &app.theme
}

/// Render the header strip. For a single document, this is the legacy
/// "filename + dirty marker" header. For multiple documents, it becomes
/// a tab strip — one labelled cell per open doc, with the active doc
/// highlighted. Clicking an inactive tab switches the active document;
/// clicking the active tab is a no-op. The single-doc form keeps the
/// old, quieter look (just one filename) so users without tabs don't
/// see a stray empty strip.
fn render_header_strip(ui: &mut egui::Ui, app: &mut EditorApp) {
    let theme = *theme(app);
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
        ui.label(
            egui::RichText::new(format!(" {path}{dirty}{stale}"))
                .strong()
                .monospace()
                .color(theme.panel_text),
        );
        return;
    }

    // Multi-doc tab strip. Each tab is its own label so egui tracks
    // click responses per-tab. The active tab fills with the editor
    // background — it reads as punched through into the editor — plus
    // an accent underline and bright text; inactive tabs are dimmed.
    // Clicking an inactive tab switches `app.active`; clicking the ×
    // closes that tab.
    ui.horizontal(|ui| {
        ui.add_space(4.0);
        let mut close_idx: Option<usize> = None;
        for i in 0..app.doc_count() {
            let is_active = i == app.active;
            let doc = &app.documents[i];
            let name = doc.display_name();
            let dirty = if doc.is_dirty() { "*" } else { "" };
            let stale = if doc.external_change { "!" } else { "" };

            let bg = if is_active {
                theme.editor_bg
            } else {
                theme.panel_bg
            };
            let text_color = if is_active {
                theme.text
            } else {
                theme.dim_text
            };

            let tab = egui::Frame::none()
                .fill(bg)
                .inner_margin(egui::vec2(8.0, 3.0))
                .rounding(4.0)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let label_text = format!("{}{}{}", name, dirty, stale);
                        let label = ui.add(
                            egui::Label::new(
                                egui::RichText::new(label_text)
                                    .monospace()
                                    .strong()
                                    .color(text_color),
                            )
                            .sense(egui::Sense::click()),
                        );
                        if !is_active {
                            hand_cursor(&label, ui.ctx());
                            if label.clicked() {
                                app.active = i;
                            }
                        }

                        let close = ui.add(
                            egui::Label::new(
                                egui::RichText::new("×")
                                    .monospace()
                                    .strong()
                                    .color(text_color),
                            )
                            .sense(egui::Sense::click()),
                        );
                        hand_cursor(&close, ui.ctx());
                        if close.clicked() {
                            close_idx = Some(i);
                        }
                    });
                });
            if is_active {
                // Accent underline tying the active tab to the editor
                // surface below it.
                let r = tab.response.rect;
                ui.painter().rect_filled(
                    egui::Rect::from_min_size(
                        egui::pos2(r.left(), r.bottom() - 2.0),
                        egui::vec2(r.width(), 2.0),
                    ),
                    0.0,
                    theme.accent,
                );
            }

            if i + 1 < app.doc_count() {
                ui.add_space(6.0);
            }
        }
        if let Some(idx) = close_idx {
            app.request_close_doc(idx);
        }
    });
}

pub fn render(ctx: &egui::Context, app: &mut EditorApp) {
    // The LSP tick (poll + open + change + apply responses) already ran
    // in `update()` before render was called; repeating it here only
    // doubled the try_wait syscalls per server per frame.
    if let Some(status) = app.lsp_manager.take_status() {
        app.status_message = Some(status);
    }

    // Update the LSP completion popup if a response just arrived.
    if app.lsp_completion.pending {
        if let Some(uri) = app.active_doc().uri() {
            if let Some(list) = app.lsp_manager.completion_result(&uri) {
                app.lsp_completion.items = list.items.clone();
                app.lsp_completion.pending = false;
            }
        }
    }

    // Apply rename / format results if they just arrived.
    app.apply_pending_rename();
    app.apply_pending_format();

    // Poll for document symbol results.
    if app.symbol_picker.open {
        if let Some(uri) = app.active_doc().uri() {
            if let Some(symbols) = app.lsp_manager.take_document_symbol_result(&uri) {
                app.symbol_picker.items = symbols.clone();
                app.symbol_picker.filtered = (0..symbols.len()).collect();
            }
        }
    }

    let theme = *theme(app);
    let (status_message, status_pos) = {
        let msg = app.status_message.clone().unwrap_or_default();
        let cursor_pos = app.active_buffer().cursor();
        let (line, col) = app
            .active_buffer()
            .pos_to_linecol(cursor_pos)
            .unwrap_or((0, 0));
        let mut pos = format_position(line, col, app.active_buffer().line_count());
        if app.active_doc().view.git_gutter_enabled && app.active_doc().git_gutter.enabled() {
            let (added, modified, removed) = app.active_doc().git_gutter.summary();
            if added != 0 || modified != 0 || removed != 0 {
                pos.push_str(&format!(
                    "  |  +{added} ~{modified} -{removed}",
                    added = added,
                    modified = modified,
                    removed = removed
                ));
            }
        }
        (msg, pos)
    };

    let lsp_status = app
        .active_doc()
        .uri()
        .and_then(|uri| app.lsp_manager.document_server_status(&uri));
    let lsp_fallback = if lsp_status.is_none() {
        app.active_doc()
            .language_id()
            .filter(|lang| core::lsp::LspManager::is_language_supported(lang))
            .and_then(|lang| core::lsp::LspManager::server_display_name(lang))
            .map(|name| name.to_string())
    } else {
        None
    };

    egui::TopBottomPanel::top("header")
        .frame(egui::Frame::none().fill(theme.panel_bg).inner_margin(6.0))
        .show(ctx, |ui| {
            render_header_strip(ui, app);
        });

    egui::TopBottomPanel::bottom("status")
        .frame(egui::Frame::none().fill(theme.status_bg).inner_margin(6.0))
        .show(ctx, |ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let settings = ui
                    .selectable_label(app.settings_open, egui::RichText::new("⚙").size(16.0))
                    .on_hover_text("Settings");
                hand_cursor(&settings, ui.ctx());
                if settings.clicked() {
                    if app.settings_open && app.settings_dirty {
                        app.sync_config();
                        app.settings_dirty = false;
                    }
                    app.settings_open = !app.settings_open;
                }
                render_document_view_switcher(ui, app);
                ui.label(
                    egui::RichText::new(app.keymap.status_label())
                        .color(theme.status_text)
                        .monospace()
                        .strong(),
                );
                if let Some(status) = lsp_status {
                    let name = core::lsp::LspManager::server_display_name(&status.language_id)
                        .unwrap_or(&status.language_id);
                    let (symbol, color) = if status.running {
                        ("●", theme.git_added)
                    } else {
                        ("○", theme.error)
                    };
                    ui.label(
                        egui::RichText::new(format!("{symbol} {name}"))
                            .color(color)
                            .monospace(),
                    );
                } else if let Some(name) = lsp_fallback.as_ref() {
                    // The language is supported but the document has not
                    // been registered with the LSP manager yet (e.g. first
                    // frame) or server spawn failed. Show a greyed-out
                    // placeholder so the user still sees the language.
                    ui.label(
                        egui::RichText::new(format!("○ {name}"))
                            .color(theme.error)
                            .monospace(),
                    );
                }
                let full_status = format!(" {status_message}  |  {status_pos}");
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(&full_status)
                                .monospace()
                                .color(theme.status_text),
                        )
                        .truncate(),
                    )
                    .on_hover_text(full_status);
                });
            });
        });

    // Find bar — appears above the status bar when open.
    if app.search.bar_open {
        egui::TopBottomPanel::bottom("find_bar")
            .frame(
                egui::Frame::none()
                    .fill(theme.panel_bg)
                    .inner_margin(6.0)
                    .stroke(egui::Stroke::new(1.0_f32, theme.border)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(" Find: ")
                            .strong()
                            .monospace()
                            .color(theme.panel_text),
                    );
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
                    let status_text = if let Some(ref err) = app.search.regex_error {
                        format!(" invalid regex: {err} ")
                    } else if total == 0 && !app.search.query.is_empty() {
                        " (no matches)".to_string()
                    } else {
                        format!(" {current}/{total} ")
                    };
                    let status_color = if app.search.regex_error.is_some() {
                        theme.error
                    } else {
                        theme.panel_text
                    };
                    ui.label(
                        egui::RichText::new(status_text)
                            .monospace()
                            .color(status_color),
                    );
                    let regex_btn = ui.selectable_label(app.search.regex_mode, ".*");
                    hand_cursor(&regex_btn, ui.ctx());
                    if regex_btn.clicked() {
                        app.handle_event(EditorEvent::ToggleFindRegex);
                    }
                    let case_btn = ui.selectable_label(app.search.case_sensitive, "Aa");
                    hand_cursor(&case_btn, ui.ctx());
                    if case_btn.clicked() {
                        app.handle_event(EditorEvent::ToggleFindCaseSensitive);
                    }
                    let word_btn = ui.selectable_label(app.search.whole_word, "W");
                    hand_cursor(&word_btn, ui.ctx());
                    if word_btn.clicked() {
                        app.handle_event(EditorEvent::ToggleFindWholeWord);
                    }
                    let next_btn = ui.button("Next");
                    hand_cursor(&next_btn, ui.ctx());
                    if next_btn.clicked() {
                        app.handle_event(EditorEvent::FindNext);
                    }
                    let prev_btn = ui.button("Prev");
                    hand_cursor(&prev_btn, ui.ctx());
                    if prev_btn.clicked() {
                        app.handle_event(EditorEvent::FindPrev);
                    }
                    let replace_toggle_label = if app.search.replace_bar_open {
                        "Hide Replace"
                    } else {
                        "Replace"
                    };
                    let replace_toggle_btn = ui.button(replace_toggle_label);
                    hand_cursor(&replace_toggle_btn, ui.ctx());
                    if replace_toggle_btn.clicked() {
                        if app.search.replace_bar_open {
                            app.handle_event(EditorEvent::ReplaceClose);
                        } else {
                            app.handle_event(EditorEvent::ReplaceOpen);
                        }
                    }
                    let close_btn = ui.button("Close");
                    hand_cursor(&close_btn, ui.ctx());
                    if close_btn.clicked() {
                        app.handle_event(EditorEvent::FindClose);
                    }
                });
                // Replace row sits directly below the find row. Visible
                // only when `replace_bar_open` is set. Tab / focus shifts
                // are implicit via egui's text-input focus model.
                if app.search.replace_bar_open {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(" Replace: ")
                                .strong()
                                .monospace()
                                .color(theme.panel_text),
                        );
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
                            || (app.search.replace_query.is_empty() && !response.has_focus())
                        {
                            response.request_focus();
                        }
                        let replace_btn = ui.button("Replace");
                        hand_cursor(&replace_btn, ui.ctx());
                        if replace_btn.clicked() {
                            app.handle_event(EditorEvent::ReplaceOne);
                        }
                        let replace_all_btn = ui.button("Replace All");
                        hand_cursor(&replace_all_btn, ui.ctx());
                        if replace_all_btn.clicked() {
                            app.handle_event(EditorEvent::ReplaceAll);
                        }
                    });
                }
            });
    }

    // Go-to-line bar — appears above the status bar when open.
    if app.go_to_line_dialog.is_some() {
        egui::TopBottomPanel::bottom("go_to_line_bar")
            .frame(
                egui::Frame::none()
                    .fill(theme.panel_bg)
                    .inner_margin(6.0)
                    .stroke(egui::Stroke::new(1.0_f32, theme.border)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let query = app
                        .go_to_line_dialog
                        .as_ref()
                        .map(|d| d.query.clone())
                        .unwrap_or_default();
                    ui.label(
                        egui::RichText::new(" Go to line: ")
                            .strong()
                            .monospace()
                            .color(theme.panel_text),
                    );
                    ui.label(
                        egui::RichText::new(format!("{query}█"))
                            .monospace()
                            .color(theme.panel_text),
                    );
                });
            });
    }

    // Rename-symbol bar — appears above the status bar when open.
    if app.rename_dialog.is_some() {
        egui::TopBottomPanel::bottom("rename_bar")
            .frame(
                egui::Frame::none()
                    .fill(theme.panel_bg)
                    .inner_margin(6.0)
                    .stroke(egui::Stroke::new(1.0_f32, theme.border)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(" Rename symbol: ")
                            .strong()
                            .monospace()
                            .color(theme.panel_text),
                    );
                    let mut name = app
                        .rename_dialog
                        .as_ref()
                        .map(|d| d.new_name.clone())
                        .unwrap_or_default();
                    let response =
                        ui.add(egui::TextEdit::singleline(&mut name).desired_width(300.0));
                    if response.changed() {
                        if let Some(d) = app.rename_dialog.as_mut() {
                            d.new_name = name;
                        }
                    }
                    if response.gained_focus()
                        || (app
                            .rename_dialog
                            .as_ref()
                            .map_or(true, |d| d.new_name.is_empty() && !response.has_focus()))
                    {
                        response.request_focus();
                    }
                    if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        if let Some(d) = app.rename_dialog.take() {
                            if !d.new_name.is_empty() {
                                app.handle_event(EditorEvent::RenameApply {
                                    new_name: d.new_name,
                                });
                            }
                        }
                    }
                    let cancel_btn = ui.button("Cancel");
                    if cancel_btn.clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        app.rename_dialog = None;
                    }
                });
            });
    }

    // Project-tree sidebar. Lives on the left, collapsible via F8.
    if app.project_tree_open {
        render_project_tree_sidebar(ctx, app);
    }

    let markdown_mode = if app.active_doc().language_id() == Some("markdown") {
        app.markdown_preview_mode
    } else {
        MarkdownPreviewMode::Source
    };
    let markdown_document_id = app.active_doc().id();
    if markdown_mode != MarkdownPreviewMode::Source {
        let preview_key = (markdown_document_id, app.active_buffer().revision());
        if app.markdown_preview.needs_refresh(preview_key) {
            let markdown = app.active_doc().text();
            app.markdown_preview.refresh(preview_key, &markdown);
        }
    }
    let csv_mode = if app.active_doc().language_id() == Some("csv") {
        app.csv_preview_mode
    } else {
        CsvPreviewMode::Source
    };
    let csv_document_id = app.active_doc().id();
    if csv_mode == CsvPreviewMode::Table {
        let preview_key = (csv_document_id, app.active_buffer().revision());
        if app.csv_preview.needs_refresh(preview_key) {
            let byte_len = app.active_buffer().len();
            if byte_len > crate::csv_preview::MAX_PREVIEW_BYTES {
                app.csv_preview.reject_oversized(preview_key, byte_len);
            } else {
                let csv = app.active_buffer().to_bytes();
                app.csv_preview.refresh(preview_key, &csv);
            }
        }
    }

    // Minimap sidebar. Lives on the right, toggleable. It is hidden when
    // source text itself is hidden.
    if app.minimap_open
        && markdown_mode != MarkdownPreviewMode::Preview
        && csv_mode != CsvPreviewMode::Table
    {
        render_minimap(ctx, app);
    }

    if markdown_mode == MarkdownPreviewMode::Split {
        egui::SidePanel::right("markdown_preview")
            .resizable(true)
            .default_width(ctx.available_rect().width() * 0.5)
            .frame(
                egui::Frame::none()
                    .fill(theme.editor_bg)
                    .inner_margin(egui::Margin::symmetric(14.0, 0.0))
                    .stroke(egui::Stroke::new(1.0_f32, theme.separator)),
            )
            .show(ctx, |ui| {
                app.markdown_preview
                    .render(ui, markdown_document_id, &theme)
            });
    }

    egui::CentralPanel::default()
        .frame(egui::Frame::none().fill(theme.editor_bg))
        .show(ctx, |ui| {
            if csv_mode == CsvPreviewMode::Table {
                app.csv_preview.render(ui, csv_document_id, &theme);
            } else if markdown_mode == MarkdownPreviewMode::Preview {
                app.markdown_preview
                    .render(ui, markdown_document_id, &theme);
            } else {
                render_text(ui, app, &theme);
            }
        });

    // Modal prompts render on top. Close-confirm is a centred dialog;
    // the open-file dialog is also centred. The order matters only if
    // both were up (they shouldn't be — opening one drops the other)
    // and we want close-confirm on top because it's tied to a
    // destructive action.
    if app.close_confirm.is_some() {
        render_close_confirm_window(ctx, app);
    }
    if app.lsp_missing_prompt.is_some() {
        render_lsp_missing_window(ctx, app);
    }
    if app.project_search.open {
        render_project_search_window(ctx, app);
    }
    if app.command_palette.open {
        render_command_palette_window(ctx, app);
    }
    if app.fuzzy_finder.open {
        render_fuzzy_finder_window(ctx, app);
    }
    if app.symbol_picker.open {
        render_symbol_picker_window(ctx, app);
    }
    if app.code_action_picker.open {
        render_code_action_window(ctx, app);
    }
    if app.keybindings_help_open {
        render_keybindings_help_window(ctx, app);
    }
    if app.settings_open {
        render_settings_window(ctx, app);
    }
    if app.lsp_completion.open {
        render_lsp_completion_popup(ctx, app);
    }
    if app.lsp_hover.open {
        render_lsp_hover_tooltip(ctx, app);
    }
}

/// Render the compact presentation-mode control beside the settings button.
///
/// Settings owns the persisted default; this control is the quick way to
/// change the active Markdown or CSV presentation without opening a dialog.
fn render_document_view_switcher(ui: &mut egui::Ui, app: &mut EditorApp) {
    enum Selection {
        Markdown(MarkdownPreviewMode),
        Csv(CsvPreviewMode),
    }

    let mut selection = None;
    ui.horizontal(|ui| match app.active_doc().language_id() {
        Some("markdown") => {
            ui.spacing_mut().item_spacing.x = 2.0;
            // This switcher lives in the status bar's right-to-left layout.
            // Emit modes in reverse so their visible order remains natural.
            for mode in MarkdownPreviewMode::ALL.into_iter().rev() {
                let response = ui
                    .selectable_label(app.markdown_preview_mode == mode, mode.label())
                    .on_hover_text(format!("Show Markdown {} view", mode.label()));
                hand_cursor(&response, ui.ctx());
                if response.clicked() {
                    selection = Some(Selection::Markdown(mode));
                }
            }
        }
        Some("csv") => {
            ui.spacing_mut().item_spacing.x = 2.0;
            for mode in CsvPreviewMode::ALL.into_iter().rev() {
                let response = ui
                    .selectable_label(app.csv_preview_mode == mode, mode.label())
                    .on_hover_text(format!("Show CSV {} view", mode.label()));
                hand_cursor(&response, ui.ctx());
                if response.clicked() {
                    selection = Some(Selection::Csv(mode));
                }
            }
        }
        _ => {}
    });

    match selection {
        Some(Selection::Markdown(mode)) if mode != app.markdown_preview_mode => {
            app.set_markdown_preview_mode(mode);
            app.sync_config();
        }
        Some(Selection::Csv(mode)) if mode != app.csv_preview_mode => {
            app.set_csv_preview_mode(mode);
            app.sync_config();
        }
        _ => {}
    }
}

/// Render the project-tree sidebar on the left of the window.
/// Directories are collapsible nodes; files open on click. Selected
/// row is highlighted for keyboard navigation.
fn render_project_tree_sidebar(ctx: &egui::Context, app: &mut EditorApp) {
    let theme = *theme(app);
    let project_root = app.active_doc().project.clone();
    let rows: Vec<(usize, core::FsNode)> = app
        .project_tree_rows()
        .into_iter()
        .map(|(d, n)| (d, n.clone()))
        .collect();
    let selected = app.project_tree_selected;

    egui::SidePanel::left("project_tree")
        .default_width(220.0)
        .width_range(120.0..=400.0)
        .frame(egui::Frame::none().fill(theme.sidebar_bg).inner_margin(6.0))
        .show(ctx, |ui| {
            ui.add_space(4.0);
            let title = project_root
                .as_ref()
                .and_then(|p| p.root.file_name())
                .and_then(|n| n.to_str())
                .unwrap_or("Project");
            ui.label(
                egui::RichText::new(format!("📁 {title}"))
                    .strong()
                    .monospace()
                    .color(theme.panel_text),
            );
            ui.separator();

            if project_root.is_none() {
                ui.label("No project detected.");
                return;
            }
            if rows.is_empty() {
                ui.label("No files found.");
                return;
            }

            // SelectableLabel is at least `interact_size.y` tall. This must
            // match the height passed to `show_rows` or virtualization and
            // programmatic scrolling disagree about every row's position.
            let row_height = ui.spacing().interact_size.y;
            let mut scroll_area = egui::ScrollArea::vertical()
                .id_salt("project_tree_scroll")
                .auto_shrink([false; 2]);
            if app.project_tree_reveal_pending {
                let centered = project_tree_reveal_offset(
                    selected,
                    row_height,
                    ui.spacing().item_spacing.y,
                    ui.available_height(),
                );
                scroll_area = scroll_area.vertical_scroll_offset(centered);
                app.project_tree_reveal_pending = false;
            }
            scroll_area.show_rows(ui, row_height, rows.len(), |ui, row_range| {
                // Virtualized rows must stay at the fixed height supplied
                // to `show_rows`, even for deeply nested or long names.
                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
                for i in row_range {
                    let (depth, node) = &rows[i];
                    let is_selected = i == selected;
                    let is_dir = node.is_dir();
                    let expanded = app
                        .project_tree
                        .as_ref()
                        .map(|t| t.expanded.contains(node.rel_path()))
                        .unwrap_or(false);

                    let indent = "  ".repeat(*depth);
                    let icon = if is_dir {
                        if expanded {
                            "📂"
                        } else {
                            "📁"
                        }
                    } else {
                        "  "
                    };
                    let label = format!("{indent}{icon} {}", node.name());
                    let text = egui::RichText::new(label).monospace();
                    let response = if is_selected {
                        ui.selectable_label(true, text.strong())
                    } else {
                        ui.selectable_label(false, text)
                    };
                    hand_cursor(&response, ui.ctx());
                    if response.clicked() {
                        app.project_tree_focused = true;
                        app.project_tree_selected = i;
                        if is_dir {
                            if let Some(tree) = app.project_tree.as_mut() {
                                tree.toggle(node.rel_path());
                            }
                        } else if let Some(project) = project_root.as_ref() {
                            let path = project.root.join(node.rel_path());
                            app.open_or_switch_to_path(&path);
                        }
                    }
                }
            });
        });
}

/// Scroll offset that centers a row in egui's virtualized project tree.
/// `ScrollArea::show_rows` includes item spacing in every row stride, so the
/// same spacing must be included here or the target drifts off-screen in long
/// trees.
fn project_tree_reveal_offset(
    selected: usize,
    row_height: f32,
    row_spacing: f32,
    viewport_height: f32,
) -> f32 {
    let row_stride = row_height + row_spacing;
    let row_center = (selected as f32 + 0.5) * row_stride;
    (row_center - viewport_height * 0.5).max(0.0)
}

/// Render the minimap — a zoomed-out document overview on the right
/// side. Shows colored rects per syntax token, with a viewport
/// highlight rect. Click/drag to scroll the editor.
fn render_minimap(ctx: &egui::Context, app: &mut EditorApp) {
    let theme = *theme(app);
    let total_lines = app.active_buffer().line_count();
    let scroll_id = egui::Id::new(("editor_scroll", app.active));

    // Skip minimap rendering for very large files — the per-line
    // line_text() calls for uncached lines are too expensive.
    const MINIMAP_MAX_LINES: usize = 5000;
    if total_lines > MINIMAP_MAX_LINES {
        egui::SidePanel::right("minimap")
            .resizable(false)
            .exact_width(80.0)
            .frame(egui::Frame::none().fill(theme.editor_bg))
            .show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(ui.available_height() / 2.0 - 10.0);
                    ui.label(
                        egui::RichText::new("File too large\nfor minimap")
                            .small()
                            .color(theme.dim_text),
                    );
                });
            });
        return;
    }

    egui::SidePanel::right("minimap")
        .resizable(false)
        .exact_width(80.0)
        .frame(egui::Frame::none().fill(theme.editor_bg))
        .show(ctx, |ui| {
            let mini_line_height = 2.0_f32;
            let mini_width = 72.0_f32;

            // Read the editor's current scroll offset to position the
            // viewport highlight.
            let editor_offset_y: f32 = ctx
                .data_mut(|d| {
                    d.get_persisted::<egui::containers::scroll_area::State>(scroll_id)
                        .map(|s| s.offset.y)
                })
                .unwrap_or(0.0);

            // Measure the editor's line_height to compute the scale ratio.
            let font_id = egui::FontId::monospace(FONT_SIZE);
            let editor_line_height = ui.fonts(|f| f.row_height(&font_id)) * LINE_SPACING;

            let _total_height = total_lines as f32 * mini_line_height;
            let (rect, response) = ui.allocate_exact_size(
                egui::vec2(mini_width, ui.available_height()),
                egui::Sense::click_and_drag(),
            );
            let painter = ui.painter_at(rect);

            // Background.
            painter.rect_filled(rect, 0.0, theme.editor_bg);

            // Determine which minimap lines are visible (clip).
            let clip_top = 0.0_f32;
            let clip_bottom = rect.height();
            let first_mini_line = ((clip_top) / mini_line_height).floor() as usize;
            let last_mini_line = ((clip_bottom) / mini_line_height).ceil() as usize;
            let last_mini_line = last_mini_line.min(total_lines);

            // Draw colored rects for each visible line using the syntax cache.
            let doc_idx = app.active;
            let doc = &app.documents[doc_idx];
            let _syntax_theme = app.syntax.ts_theme();
            for line_idx in first_mini_line..last_mini_line {
                let y = rect.top() + line_idx as f32 * mini_line_height;
                // Try the cache first.
                if let Some(segments) = doc.syntax.lines.get(&line_idx) {
                    for seg in segments.iter() {
                        // Scale byte positions to minimap pixel positions.
                        // Approximate: 1 char ≈ 1 byte for most code.
                        // Scale factor: mini_width / max_line_width_estimate.
                        let scale = mini_width / 120.0; // assume ~120 char lines max
                        let x =
                            rect.left() + (seg.range.start as f32 * scale).min(mini_width - 2.0);
                        let w = ((seg.range.end - seg.range.start) as f32 * scale).max(1.0);
                        let w = (x + w).min(rect.right()) - x;
                        if w > 0.0 {
                            painter.rect_filled(
                                egui::Rect::from_min_size(
                                    egui::pos2(x, y),
                                    egui::vec2(w, mini_line_height),
                                ),
                                0.0,
                                egui::Color32::from_rgb(seg.color.r, seg.color.g, seg.color.b),
                            );
                        }
                    }
                } else {
                    // Uncached line — draw a dim line representing its length.
                    if let Some(text) = doc.buffer.line_text(line_idx) {
                        let char_count = text.chars().count();
                        let scale = mini_width / 120.0;
                        let w = (char_count as f32 * scale).min(mini_width - 2.0);
                        if w > 0.0 {
                            painter.rect_filled(
                                egui::Rect::from_min_size(
                                    egui::pos2(rect.left(), y),
                                    egui::vec2(w, mini_line_height),
                                ),
                                0.0,
                                theme.dim_text,
                            );
                        }
                    }
                }
            }

            // Viewport highlight rect.
            let view_first_line = (editor_offset_y / editor_line_height).floor();
            let view_lines = app.viewport_lines.max(1);
            let view_top = rect.top() + view_first_line * mini_line_height;
            let view_height = view_lines as f32 * mini_line_height;
            let view_rect = egui::Rect::from_min_size(
                egui::pos2(rect.left(), view_top),
                egui::vec2(rect.width(), view_height),
            );
            painter.rect_filled(view_rect, 0.0, theme.selection_bg);

            // Click/drag to scroll.
            if response.dragged() || response.clicked() {
                if let Some(pos) = response.interact_pointer_pos() {
                    let click_y = pos.y - rect.top();
                    // Target: center the viewport on the clicked line.
                    let target_line =
                        (click_y / mini_line_height).floor() - (view_lines as f32 / 2.0);
                    let target_offset = (target_line.max(0.0) * editor_line_height).round();
                    // Write the new scroll offset to the persisted state.
                    ui.ctx().data_mut(|d| {
                        if let Some(mut state) =
                            d.get_persisted::<egui::containers::scroll_area::State>(scroll_id)
                        {
                            state.offset.y = target_offset;
                            d.insert_persisted(scroll_id, state);
                        }
                    });
                }
            }
            // Cursor feedback.
            if response.hovered() || response.dragged() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
            }
        });
}

/// Render the close-on-dirty dialog as a centred egui::Window.
/// Three buttons (Save / Discard / Cancel); the focused choice has a
/// coloured background. Tab / Shift+Tab cycle the focused choice via
/// [`crate::app::EditorApp::cycle_close_choice`] which we call from
/// `dispatch_modal_event`; Enter / `y` confirm via
/// [`crate::app::EditorApp::confirm_close_choice`].
/// Popup for a missing language server: what's disabled, how to
/// install it, and a persistent per-language "don't show again".
fn render_lsp_missing_window(ctx: &egui::Context, app: &mut EditorApp) {
    let mut open = true;
    // Snapshot everything the window body needs so the egui closure
    // doesn't hold a borrow on `app` (same pattern as the
    // close-confirm window).
    let (language, server, hint, mut checked) = match app.lsp_missing_prompt.as_ref() {
        Some(p) => (
            p.info.language_id.clone(),
            p.info.server_name.clone(),
            p.hint,
            p.dont_show_again,
        ),
        None => return,
    };
    let mut ok_clicked = false;

    egui::Window::new("Language server unavailable")
        .collapsible(false)
        .resizable(false)
        .default_width(430.0)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .open(&mut open)
        .show(ctx, |ui| {
            ui.label(format!(
                "{server} is not installed, so {language} diagnostics, \
                 completions, hover, and code actions are disabled."
            ));
            ui.add_space(6.0);
            if let Some(cmd) = hint {
                ui.label("To enable it, install the server, e.g.:");
                ui.add_space(2.0);
                ui.code(cmd);
            }
            ui.add_space(8.0);
            ui.checkbox(&mut checked, format!("Don't show again for {language}"));
            ui.add_space(6.0);
            ok_clicked = ui.button("OK").clicked();
            ui.add_space(2.0);
            ui.label(
                egui::RichText::new("Enter to close · Space toggles the checkbox")
                    .small()
                    .weak(),
            );
        });

    if let Some(prompt) = app.lsp_missing_prompt.as_mut() {
        prompt.dont_show_again = checked;
    }
    if ok_clicked || !open {
        app.dismiss_lsp_missing_prompt();
    }
}

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
                if focused_button(ui, app, focused, CloseChoice::Save, "Save").clicked() {
                    app.close_confirm = None;
                    match app.active_buffer_mut().save() {
                        Ok(()) => {
                            app.status_message = Some("Saved.".to_string());
                            app.perform_close_active();
                        }
                        Err(e) => app.status_message = Some(format!("Save error: {e}")),
                    }
                }
                if focused_button(ui, app, focused, CloseChoice::Discard, "Discard").clicked() {
                    app.close_confirm = None;
                    app.perform_close_active();
                }
                if focused_button(ui, app, focused, CloseChoice::Cancel, "Cancel").clicked() {
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
    app: &EditorApp,
    focused: CloseChoice,
    this: CloseChoice,
    label: &str,
) -> egui::Response {
    let theme = theme(app);
    let is_focused = focused == this;
    let text = if is_focused {
        egui::RichText::new(label)
            .strong()
            .color(theme.accent_text)
            .background_color(theme.accent)
    } else {
        egui::RichText::new(label).color(theme.dim_text)
    };
    let response = ui.button(text);
    hand_cursor(&response, ui.ctx());
    response
}

/// Render the project-wide search / replace dialog as a centred window.
/// Shows find and replace inputs, live results or replace preview, and
/// a status hint.
fn render_project_search_window(ctx: &egui::Context, app: &mut EditorApp) {
    let mut open = true;
    egui::Window::new("Project search & replace")
        .collapsible(false)
        .resizable(true)
        .default_size([520.0, 420.0])
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .open(&mut open)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Find:").strong().monospace());
                let mut query = app.project_search.query.clone();
                let response = ui.add(
                    egui::TextEdit::singleline(&mut query)
                        .hint_text("search across project files")
                        .desired_width(300.0),
                );
                if response.changed() {
                    app.handle_event(EditorEvent::ProjectSearchQueryChanged(query));
                }
                if response.gained_focus()
                    || (app.project_search.query.is_empty() && !response.has_focus())
                {
                    response.request_focus();
                }
            });
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Replace:").strong().monospace());
                let mut replace_query = app.project_search.replace_query.clone();
                let response = ui.add(
                    egui::TextEdit::singleline(&mut replace_query)
                        .hint_text("leave empty to search only")
                        .desired_width(300.0),
                );
                if response.changed() {
                    app.handle_event(EditorEvent::ProjectSearchReplaceQueryChanged(replace_query));
                }
            });
            ui.separator();

            let selected = app.project_search.selected;
            let showing_replace = !app.project_search.replace_query.is_empty();
            let (item_count, hint) = if showing_replace {
                let line_count = app.project_search.replace_previews.len();
                let occurrence_count: usize = app
                    .project_search
                    .replace_previews
                    .iter()
                    .map(|p| p.occurrence_count)
                    .sum();
                (
                    line_count,
                    format!(
                        "{} lines · {} occurrences · Ctrl+Enter to confirm · Esc to close",
                        line_count, occurrence_count
                    ),
                )
            } else {
                let count = app.project_search.results.len();
                (
                    count,
                    format!("{} results · Enter to open · Up/Down · Esc to close", count),
                )
            };

            egui::ScrollArea::vertical()
                .id_salt("project_search_results")
                .auto_shrink([false; 2])
                .show_rows(
                    ui,
                    ui.text_style_height(&egui::TextStyle::Body),
                    item_count,
                    |ui, row_range| {
                        if showing_replace {
                            for i in row_range {
                                let preview = &app.project_search.replace_previews[i];
                                let label = format!(
                                    "{}:{}  {} → {}",
                                    preview.rel_path.to_string_lossy(),
                                    preview.line,
                                    preview.before,
                                    preview.after
                                );
                                let is_selected = i == selected;
                                let text = egui::RichText::new(label).monospace().size(13.0);
                                if is_selected {
                                    ui.label(text.strong());
                                } else {
                                    ui.label(text);
                                }
                            }
                        } else {
                            for i in row_range {
                                let result = &app.project_search.results[i];
                                let label = format!(
                                    "{}:{}:{}",
                                    result.rel_path.to_string_lossy(),
                                    result.line,
                                    result.text
                                );
                                let is_selected = i == selected;
                                let text = egui::RichText::new(label).monospace().size(13.0);
                                let response = if is_selected {
                                    ui.selectable_label(true, text.strong())
                                } else {
                                    ui.selectable_label(false, text)
                                };
                                hand_cursor(&response, ui.ctx());
                                if response.clicked() {
                                    app.project_search.selected = i;
                                    app.handle_event(EditorEvent::ProjectSearchOpenResult);
                                }
                            }
                        }
                    },
                );

            ui.separator();
            ui.label(egui::RichText::new(hint).small().weak());

            if app.project_search.confirm_replace {
                ui.separator();
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
                let file_count = files.len();
                ui.vertical_centered(|ui| {
                    ui.label(
                        egui::RichText::new(format!(
                            "Replace {occurrence_count} occurrences in {file_count} files?"
                        ))
                        .strong(),
                    );
                    ui.horizontal(|ui| {
                        let replace_btn = ui.button("Replace");
                        hand_cursor(&replace_btn, ui.ctx());
                        if replace_btn.clicked() {
                            app.handle_event(EditorEvent::ProjectSearchReplaceAllConfirm);
                        }
                        let cancel_btn = ui.button("Cancel");
                        hand_cursor(&cancel_btn, ui.ctx());
                        if cancel_btn.clicked() {
                            app.handle_event(EditorEvent::ProjectSearchReplaceAllCancel);
                        }
                    });
                });
            }
        });

    if !open {
        app.handle_event(EditorEvent::ProjectSearchClose);
    }
}

/// Render the command palette as a centred window. Shows a filter input
/// and a scrollable list of matching commands.
fn render_command_palette_window(ctx: &egui::Context, app: &mut EditorApp) {
    let mut open = true;
    egui::Window::new("Command palette")
        .collapsible(false)
        .resizable(false)
        .default_size([400.0, 320.0])
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .open(&mut open)
        .show(ctx, |ui| {
            let mut query = app.command_palette.query.clone();
            let response = ui.add(
                egui::TextEdit::singleline(&mut query)
                    .hint_text("type a command")
                    .desired_width(350.0),
            );
            if response.changed() {
                app.handle_event(EditorEvent::CommandPaletteQueryChanged(query));
            }
            if response.gained_focus()
                || (app.command_palette.query.is_empty() && !response.has_focus())
            {
                response.request_focus();
            }
            ui.separator();

            let selected = app.command_palette.selected;
            let item_count = app.command_palette.filtered.len();
            egui::ScrollArea::vertical()
                .id_salt("command_palette_items")
                .auto_shrink([false; 2])
                .show_rows(
                    ui,
                    ui.text_style_height(&egui::TextStyle::Body),
                    item_count,
                    |ui, row_range| {
                        for i in row_range {
                            let command = &app.command_palette.filtered[i];
                            let binding = command.keybinding_for(app.keymap.mode());
                            let label = if binding.is_empty() {
                                command.label.to_string()
                            } else {
                                format!("{}  ({binding})", command.label)
                            };
                            let is_selected = i == selected;
                            let text = egui::RichText::new(label).monospace().size(14.0);
                            let response = if is_selected {
                                ui.selectable_label(true, text.strong())
                            } else {
                                ui.selectable_label(false, text)
                            };
                            hand_cursor(&response, ui.ctx());
                            if response.clicked() {
                                app.command_palette.selected = i;
                                app.handle_event(EditorEvent::CommandPaletteExecute);
                            }
                        }
                    },
                );

            ui.separator();
            ui.label(
                egui::RichText::new(format!(
                    "{} commands · Enter to run · Up/Down · Esc to close",
                    item_count
                ))
                .small()
                .weak(),
            );
        });

    if !open {
        app.handle_event(EditorEvent::CommandPaletteClose);
    }
}

fn render_fuzzy_finder_window(ctx: &egui::Context, app: &mut EditorApp) {
    let mut open = true;

    egui::Window::new("Fuzzy finder")
        .collapsible(false)
        .resizable(false)
        .default_size([500.0, 360.0])
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .open(&mut open)
        .show(ctx, |ui| {
            let mut query = app.fuzzy_finder.query.clone();
            let response = ui.add(
                egui::TextEdit::singleline(&mut query)
                    .hint_text("type a file name...")
                    .desired_width(f32::INFINITY),
            );
            if response.changed() {
                app.handle_event(EditorEvent::FuzzyFinderQueryChanged(query));
            }
            if response.gained_focus()
                || (app.fuzzy_finder.query.is_empty() && !response.has_focus())
            {
                response.request_focus();
            }
            ui.separator();

            let item_count = app.fuzzy_finder.filtered.len();
            let selected = app.fuzzy_finder.selected;

            egui::ScrollArea::vertical()
                .id_salt("fuzzy_finder_items")
                .auto_shrink([false; 2])
                .show_rows(
                    ui,
                    ui.text_style_height(&egui::TextStyle::Body),
                    item_count,
                    |ui, row_range| {
                        for row in row_range {
                            let Some((idx, _)) = app.fuzzy_finder.filtered.get(row) else {
                                continue;
                            };
                            let candidate = &app.fuzzy_finder.items[*idx];
                            let is_selected = row == selected;
                            let text = egui::RichText::new(&candidate.display)
                                .monospace()
                                .size(14.0);
                            let response = if is_selected {
                                ui.selectable_label(true, text.strong())
                            } else {
                                ui.selectable_label(false, text)
                            };
                            hand_cursor(&response, ui.ctx());
                            if response.clicked() {
                                app.fuzzy_finder.selected = row;
                                app.handle_event(EditorEvent::FuzzyFinderExecute);
                            }
                        }
                    },
                );

            ui.separator();
            ui.label(
                egui::RichText::new(format!(
                    "{} files · Enter to open · Up/Down · Esc to close",
                    item_count
                ))
                .small()
                .weak(),
            );
        });

    if !open {
        app.handle_event(EditorEvent::FuzzyFinderClose);
    }
}

fn render_symbol_picker_window(ctx: &egui::Context, app: &mut EditorApp) {
    let mut open = true;

    egui::Window::new("Go to Symbol")
        .collapsible(false)
        .resizable(false)
        .default_size([500.0, 360.0])
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .open(&mut open)
        .show(ctx, |ui| {
            let mut query = app.symbol_picker.query.clone();
            let response = ui.add(
                egui::TextEdit::singleline(&mut query)
                    .hint_text("type a symbol name...")
                    .desired_width(f32::INFINITY),
            );
            if response.changed() {
                app.symbol_picker.query = query.clone();
                let q = query.to_lowercase();
                app.symbol_picker.filtered = app
                    .symbol_picker
                    .items
                    .iter()
                    .enumerate()
                    .filter(|(_, sym)| sym.name.to_lowercase().contains(&q))
                    .map(|(i, _)| i)
                    .collect();
                app.symbol_picker.selected = 0;
            }
            if response.gained_focus()
                || (app.symbol_picker.query.is_empty() && !response.has_focus())
            {
                response.request_focus();
            }
            ui.separator();

            let selected = app.symbol_picker.selected;
            let item_count = app.symbol_picker.filtered.len();
            let items_clone: Vec<lsp_types::DocumentSymbol> = app.symbol_picker.items.clone();
            let filtered_clone = app.symbol_picker.filtered.clone();

            egui::ScrollArea::vertical()
                .id_salt("symbol_picker_items")
                .auto_shrink([false; 2])
                .show_rows(
                    ui,
                    ui.text_style_height(&egui::TextStyle::Body),
                    item_count,
                    |ui, row_range| {
                        for row in row_range {
                            let Some(&idx) = filtered_clone.get(row) else {
                                continue;
                            };
                            let Some(sym) = items_clone.get(idx) else {
                                continue;
                            };
                            let is_selected = row == selected;
                            let label =
                                format!("{}  L{}", sym.name, sym.selection_range.start.line + 1);
                            let text = egui::RichText::new(label).monospace().size(14.0);
                            let response = if is_selected {
                                ui.selectable_label(true, text.strong())
                            } else {
                                ui.selectable_label(false, text)
                            };
                            hand_cursor(&response, ui.ctx());
                            if response.clicked() {
                                app.symbol_picker.selected = row;
                                if let Some(sym) = app.symbol_picker.items.get(idx).cloned() {
                                    let line = sym.selection_range.start.line as usize;
                                    app.go_to_line(line + 1);
                                    app.symbol_picker.open = false;
                                }
                            }
                        }
                    },
                );

            ui.separator();
            ui.label(
                egui::RichText::new(format!(
                    "{} symbols · Enter to jump · Esc to close",
                    item_count
                ))
                .small()
                .weak(),
            );
        });

    if !open {
        app.symbol_picker.open = false;
    }
}

/// Centered picker for LSP code actions (quick fixes). No query box —
/// servers return a short, already-relevant list for the cursor line.
fn render_code_action_window(ctx: &egui::Context, app: &mut EditorApp) {
    let mut open = true;

    egui::Window::new("Code Actions")
        .collapsible(false)
        .resizable(false)
        .default_size([420.0, 220.0])
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .open(&mut open)
        .show(ctx, |ui| {
            let selected = app.code_action_picker.selected;
            let items: Vec<String> = app
                .code_action_picker
                .items
                .iter()
                .map(code_action_label)
                .collect();
            let item_count = items.len();

            egui::ScrollArea::vertical()
                .id_salt("code_action_items")
                .auto_shrink([false; 2])
                .show_rows(
                    ui,
                    ui.text_style_height(&egui::TextStyle::Body),
                    item_count,
                    |ui, row_range| {
                        for row in row_range {
                            let Some(label) = items.get(row) else {
                                continue;
                            };
                            let is_selected = row == selected;
                            let text = egui::RichText::new(label).monospace().size(14.0);
                            let response = if is_selected {
                                ui.selectable_label(true, text.strong())
                            } else {
                                ui.selectable_label(false, text)
                            };
                            hand_cursor(&response, ui.ctx());
                            if response.clicked() {
                                app.code_action_picker.selected = row;
                                app.execute_selected_code_action();
                            }
                        }
                    },
                );

            ui.separator();
            let footer = if app.code_action_picker.pending {
                "requesting... · Esc to close".to_string()
            } else {
                format!("{item_count} actions · Enter to apply · Esc to close")
            };
            ui.label(egui::RichText::new(footer).small().weak());
        });

    if !open {
        app.handle_event(core::EditorEvent::CodeActionsClose);
    }
}

/// Display label for a code action or command: its title, optionally
/// suffixed with the kind ("quickfix", "refactor", ...).
fn code_action_label(action: &lsp_types::CodeActionOrCommand) -> String {
    match action {
        lsp_types::CodeActionOrCommand::CodeAction(a) => match &a.kind {
            Some(kind) => format!("{}  [{}]", a.title, kind.as_str()),
            None => a.title.clone(),
        },
        lsp_types::CodeActionOrCommand::Command(c) => c.title.clone(),
    }
}

fn render_settings_window(ctx: &egui::Context, app: &mut EditorApp) {
    let mut open = true;
    let current_ui_theme = app.theme.name.to_owned();
    let mut selected_ui_theme = None;

    let mut project_tree_open = app.project_tree_open;
    let mut caret_animation = app.config.caret_animation;
    let mut keybinding_mode = app.keymap.mode();
    let mut auto_save = app.config.auto_save;
    let mut auto_save_on_focus_change = app.config.auto_save_on_focus_change;
    let mut auto_save_delay_ms = app.config.auto_save_delay_ms;
    let view = &app.active_doc().view;
    let mut use_spaces = view.use_spaces;
    let mut tab_width = view.tab_width;
    let mut soft_wrap = view.soft_wrap;
    let mut scroll_margin_lines = view.scroll_margin_lines;
    let mut git_gutter = view.git_gutter_enabled;
    let mut git_blame = view.git_blame_enabled;
    let markdown_available = app.active_doc().language_id() == Some("markdown");
    let mut markdown_mode = app.markdown_preview_mode;
    let csv_available = app.active_doc().language_id() == Some("csv");
    let mut csv_mode = app.csv_preview_mode;
    let mut show_keyboard_shortcuts = false;

    egui::Window::new("Settings")
        .collapsible(false)
        .resizable(true)
        .default_size([560.0, 620.0])
        .min_width(460.0)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .open(&mut open)
        .show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .id_salt("settings_scroll")
                .auto_shrink([false; 2])
                .show(ui, |ui| {
                    ui.heading("Appearance");
                    ui.add_space(4.0);
                    egui::Grid::new("settings_appearance_grid")
                        .num_columns(2)
                        .spacing([18.0, 10.0])
                        .show(ui, |ui| {
                            ui.label("Theme");
                            let combo = egui::ComboBox::from_id_salt("settings_ui_theme")
                                .selected_text(&current_ui_theme)
                                .width(220.0)
                                .show_ui(ui, |ui| {
                                    for candidate in GuiTheme::all() {
                                        ui.horizontal(|ui| {
                                            ui.colored_label(candidate.accent, "●");
                                            let response = ui.selectable_label(
                                                candidate.name == current_ui_theme,
                                                candidate.name,
                                            );
                                            hand_cursor(&response, ui.ctx());
                                            if response.clicked() {
                                                selected_ui_theme = Some(candidate.name.to_owned());
                                            }
                                        });
                                    }
                                });
                            hand_cursor(&combo.response, ui.ctx());
                            ui.end_row();
                        });

                    ui.add_space(12.0);
                    ui.separator();
                    ui.add_space(8.0);
                    ui.heading("Editor");
                    ui.add_space(4.0);
                    egui::Grid::new("settings_editor_grid")
                        .num_columns(2)
                        .spacing([18.0, 10.0])
                        .show(ui, |ui| {
                            ui.label("Indentation");
                            ui.horizontal(|ui| {
                                ui.selectable_value(&mut use_spaces, true, "Spaces");
                                ui.selectable_value(&mut use_spaces, false, "Tabs");
                                ui.label("Width");
                                ui.add(egui::DragValue::new(&mut tab_width).range(1..=16));
                            });
                            ui.end_row();

                            ui.label("Reading");
                            ui.vertical(|ui| {
                                ui.checkbox(&mut soft_wrap, "Soft-wrap long lines");
                                ui.horizontal(|ui| {
                                    ui.label("Scroll margin");
                                    ui.add(
                                        egui::DragValue::new(&mut scroll_margin_lines)
                                            .range(0..=50)
                                            .suffix(" lines"),
                                    );
                                });
                            });
                            ui.end_row();

                            ui.label("Cursor");
                            ui.checkbox(&mut caret_animation, "Animate vertical movement");
                            ui.end_row();

                            ui.label("Keybindings");
                            ui.horizontal(|ui| {
                                for mode in core::KeybindingMode::ALL {
                                    ui.selectable_value(&mut keybinding_mode, mode, mode.label());
                                }
                            });
                            ui.end_row();

                            ui.label("Git annotations");
                            ui.vertical(|ui| {
                                ui.checkbox(&mut git_gutter, "Show change gutter");
                                ui.checkbox(&mut git_blame, "Show inline blame");
                            });
                            ui.end_row();
                        });

                    ui.add_space(12.0);
                    ui.separator();
                    ui.add_space(8.0);
                    ui.heading("Files & workspace");
                    ui.add_space(4.0);
                    ui.checkbox(&mut project_tree_open, "Show project tree");
                    ui.checkbox(&mut auto_save, "Auto-save after inactivity");
                    ui.add_enabled_ui(auto_save, |ui| {
                        ui.horizontal(|ui| {
                            ui.add_space(20.0);
                            ui.label("Delay");
                            ui.add(
                                egui::DragValue::new(&mut auto_save_delay_ms)
                                    .range(250..=30_000)
                                    .speed(100.0)
                                    .suffix(" ms"),
                            );
                        });
                    });
                    ui.checkbox(
                        &mut auto_save_on_focus_change,
                        "Save when the window loses focus",
                    );

                    if markdown_available {
                        ui.add_space(12.0);
                        ui.separator();
                        ui.add_space(8.0);
                        ui.heading("Markdown");
                        ui.horizontal(|ui| {
                            ui.label("Default view");
                            for mode in MarkdownPreviewMode::ALL {
                                ui.selectable_value(&mut markdown_mode, mode, mode.label());
                            }
                        });
                    }

                    if csv_available {
                        ui.add_space(12.0);
                        ui.separator();
                        ui.add_space(8.0);
                        ui.heading("CSV");
                        ui.horizontal(|ui| {
                            ui.label("Default view");
                            for mode in CsvPreviewMode::ALL {
                                ui.selectable_value(&mut csv_mode, mode, mode.label());
                            }
                        });
                        ui.label(
                            egui::RichText::new(
                                "Table view uses the first record as column headings.",
                            )
                            .small()
                            .weak(),
                        );
                    }

                    ui.add_space(12.0);
                    ui.separator();
                    ui.add_space(8.0);
                    if ui.button("Keyboard shortcuts…").clicked() {
                        show_keyboard_shortcuts = true;
                    }
                    if let Some(path) = core::Config::config_path() {
                        ui.add_space(6.0);
                        ui.label(
                            egui::RichText::new(format!(
                                "Advanced formatter and snippet settings: {}",
                                path.display()
                            ))
                            .small()
                            .weak(),
                        );
                    }
                });
        });

    let mut changed = false;
    if let Some(name) = selected_ui_theme {
        if app.set_color_theme_by_name(&name) {
            app.status_message = Some(format!("Theme: {name}"));
            changed = true;
        }
    }
    if project_tree_open != app.project_tree_open {
        app.toggle_project_tree();
        changed = true;
    }
    if caret_animation != app.config.caret_animation {
        app.toggle_caret_animation();
        changed = true;
    }
    if keybinding_mode != app.keymap.mode() {
        app.set_keybinding_mode(keybinding_mode);
        changed = true;
    }
    if auto_save != app.config.auto_save
        || auto_save_on_focus_change != app.config.auto_save_on_focus_change
        || auto_save_delay_ms != app.config.auto_save_delay_ms
    {
        app.config.auto_save = auto_save;
        app.config.auto_save_on_focus_change = auto_save_on_focus_change;
        app.config.auto_save_delay_ms = auto_save_delay_ms;
        changed = true;
    }
    let active_view = &app.active_doc().view;
    if use_spaces != active_view.use_spaces || tab_width != active_view.tab_width {
        app.set_indent_mode(use_spaces, tab_width);
        changed = true;
    }
    if soft_wrap != app.active_doc().view.soft_wrap {
        app.toggle_soft_wrap();
        changed = true;
    }
    if scroll_margin_lines != app.active_doc().view.scroll_margin_lines {
        app.active_doc_mut().view.scroll_margin_lines = scroll_margin_lines;
        changed = true;
    }
    if git_gutter != app.active_doc().view.git_gutter_enabled {
        app.active_doc_mut().view.git_gutter_enabled = git_gutter;
        if git_gutter {
            app.active_doc_mut().refresh_git_gutter();
        }
        changed = true;
    }
    if git_blame != app.active_doc().view.git_blame_enabled {
        app.active_doc_mut().view.git_blame_enabled = git_blame;
        if git_blame {
            app.active_doc_mut().git_blame.mark_dirty();
        }
        changed = true;
    }
    if markdown_available && markdown_mode != app.markdown_preview_mode {
        app.set_markdown_preview_mode(markdown_mode);
        changed = true;
    }
    if csv_available && csv_mode != app.csv_preview_mode {
        app.set_csv_preview_mode(csv_mode);
        changed = true;
    }
    if changed {
        app.settings_dirty = true;
    }
    if show_keyboard_shortcuts {
        if app.settings_dirty {
            app.sync_config();
            app.settings_dirty = false;
        }
        app.settings_open = false;
        app.keybindings_help_open = true;
    } else if !open {
        if app.settings_dirty {
            app.sync_config();
            app.settings_dirty = false;
        }
        app.settings_open = false;
    }
}

fn render_keybindings_help_window(ctx: &egui::Context, app: &mut EditorApp) {
    let mut open = true;
    let is_mac = std::env::consts::OS == "macos";

    egui::Window::new("Keyboard shortcuts")
        .collapsible(false)
        .resizable(false)
        .default_size([420.0, 520.0])
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .open(&mut open)
        .show(ctx, |ui| {
            ui.label("Click a row or press Esc to close.");
            ui.separator();

            egui::ScrollArea::vertical()
                .id_salt("keybindings_help_scroll")
                .auto_shrink([false; 2])
                .show(ui, |ui| {
                    let mut rows: Vec<(String, &str)> = Vec::new();

                    match app.keymap.mode() {
                        core::KeybindingMode::Vim => {
                            rows.extend([
                                ("h j k l / arrows".into(), "Move in Normal/Visual mode"),
                                (
                                    "w b e / 0 ^ $ / gg G".into(),
                                    "Word, line, and document motions",
                                ),
                                ("i a I A o O".into(), "Enter Insert mode"),
                                ("v / V / Esc".into(), "Visual modes / Normal mode"),
                                ("d c y + motion".into(), "Delete, change, or yank"),
                                ("dd cc yy / x X s D C".into(), "Line and character edits"),
                                ("u / Ctrl+R / p P".into(), "Undo, redo, and paste"),
                                ("/ ? / n N".into(), "Search and navigate matches"),
                                (":w :q :q! :wq :x :e".into(), "File commands"),
                            ]);
                        }
                        core::KeybindingMode::Emacs => {
                            rows.extend([
                                ("C-f C-b C-n C-p".into(), "Move by character or line"),
                                ("M-f M-b / C-a C-e".into(), "Move by word or line boundary"),
                                ("M-< M-> / C-v M-v".into(), "Document and page movement"),
                                (
                                    "C-Space / C-x C-x / C-g".into(),
                                    "Set, exchange, or cancel mark",
                                ),
                                ("C-k M-d M-Backspace C-w".into(), "Kill text"),
                                ("M-w / C-y / M-y".into(), "Copy, yank, and rotate kill ring"),
                                ("C-s / C-r".into(), "Search forward / backward"),
                                ("C-x C-s / C-x C-w".into(), "Save / save as"),
                                ("C-x C-f / C-x k / C-x C-c".into(), "Open, close, or quit"),
                            ]);
                        }
                        core::KeybindingMode::Standard if is_mac => {
                            rows.push(("Cmd+← / Cmd+→".to_string(), "Line start / end"));
                            rows.push(("Cmd+↑ / Cmd+↓".to_string(), "Document start / end"));
                            rows.push(("Opt+← / Opt+→".to_string(), "Word left / right"));
                            rows.push(("Opt+Delete".to_string(), "Delete word right"));
                            rows.push(("Opt+Backspace".to_string(), "Delete word left"));
                        }
                        core::KeybindingMode::Standard => {
                            rows.push(("Home / End".to_string(), "Line start / end"));
                            rows.push(("Ctrl+Home / Ctrl+End".to_string(), "Document start / end"));
                            rows.push(("Ctrl+← / Ctrl+→".to_string(), "Word left / right"));
                            rows.push(("Ctrl+Delete".to_string(), "Delete word right"));
                            rows.push(("Ctrl+Backspace".to_string(), "Delete word left"));
                        }
                    }
                    if app.keymap.mode() == core::KeybindingMode::Standard {
                        rows.push(("↑ / ↓ / ← / →".to_string(), "Move cursor"));
                        rows.push(("Shift + arrows".to_string(), "Select text"));
                        rows.push(("PageUp / PageDown".to_string(), "Page up / down"));
                        if is_mac {
                            rows.push(("Cmd+D".to_string(), "Select next occurrence"));
                            rows.push(("Cmd+A".to_string(), "Select all"));
                            rows.push(("Opt+Click".to_string(), "Add cursor at click"));
                            rows.push(("Shift+Cmd+D".to_string(), "Duplicate line"));
                            rows.push(("Cmd+K".to_string(), "Hover documentation"));
                        } else {
                            rows.push(("Ctrl+D".to_string(), "Select next occurrence"));
                            rows.push(("Ctrl+A".to_string(), "Select all"));
                            rows.push(("Alt+Click".to_string(), "Add cursor at click"));
                            rows.push(("Shift+Ctrl+D".to_string(), "Duplicate line"));
                            rows.push(("Ctrl+K".to_string(), "Hover documentation"));
                        }
                        rows.push(("F2".to_string(), "Rename symbol"));
                        rows.push(("F12".to_string(), "Go to definition"));
                        rows.push(("( ) [ ] { } \" '".to_string(), "Auto-pair brackets/quotes"));
                        rows.push(("Enter after {".to_string(), "Auto-indent"));
                        rows.push((
                            "Home".to_string(),
                            "Smart Home (toggle col 0 / first non-WS)",
                        ));
                        rows.push((
                            if is_mac { "Cmd+/" } else { "Ctrl+/" }.to_string(),
                            "Toggle comment",
                        ));
                    }

                    // Command palette entries. Their stored keybinding
                    // strings are Windows/Linux style, so swap Ctrl for
                    // Cmd and Alt for Option on macOS.
                    for cmd in core::COMMANDS {
                        let binding = cmd.keybinding_for(app.keymap.mode());
                        if binding.is_empty() {
                            continue;
                        }
                        let key = if is_mac && app.keymap.mode() == core::KeybindingMode::Standard {
                            binding.replace("Ctrl+", "Cmd+").replace("Alt+", "Opt+")
                        } else {
                            binding.to_string()
                        };
                        rows.push((key, cmd.label));
                    }

                    rows.sort_by(|a, b| a.1.cmp(b.1));

                    for (key, action) in rows {
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(key)
                                    .monospace()
                                    .strong()
                                    .color(ui.visuals().hyperlink_color),
                            );
                            ui.with_layout(egui::Layout::top_down(egui::Align::LEFT), |ui| {
                                ui.label(action);
                            });
                        });
                    }
                });
        });

    if !open {
        app.keybindings_help_open = false;
    }
}

fn render_lsp_hover_tooltip(ctx: &egui::Context, app: &mut EditorApp) {
    if !app.lsp_hover.open || app.lsp_hover.text.is_empty() {
        return;
    }
    let theme = *theme(app);
    let text = app.lsp_hover.text.clone();
    let pos = ctx
        .input(|i| i.pointer.latest_pos())
        .unwrap_or_else(|| ctx.screen_rect().center());
    egui::Window::new("Hover")
        .collapsible(false)
        .resizable(false)
        .title_bar(false)
        .fixed_pos(pos + egui::vec2(12.0, 12.0))
        .default_size([320.0, 120.0])
        .show(ctx, |ui| {
            ui.label(
                egui::RichText::new(text)
                    .color(theme.panel_text)
                    .monospace(),
            );
        });
}

fn render_lsp_completion_popup(ctx: &egui::Context, app: &mut EditorApp) {
    let theme = *theme(app);
    egui::Window::new("Completions")
        .collapsible(false)
        .resizable(false)
        .default_size([320.0, 240.0])
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            if app.lsp_completion.pending && app.lsp_completion.items.is_empty() {
                ui.label("Loading completions...");
                return;
            }
            if app.lsp_completion.items.is_empty() {
                ui.label("No completions.");
                return;
            }

            egui::ScrollArea::vertical()
                .id_salt("lsp_completion_scroll")
                .auto_shrink([false; 2])
                .max_height(200.0)
                .show(ui, |ui| {
                    // Clone the items so clicking can mutate app state.
                    let items: Vec<lsp_types::CompletionItem> = app.lsp_completion.items.clone();
                    for (i, item) in items.iter().enumerate() {
                        let is_selected = i == app.lsp_completion.selected;
                        let label = if let Some(detail) = item.detail.as_deref() {
                            format!("{} — {}", item.label, detail)
                        } else {
                            item.label.clone()
                        };
                        let text = if is_selected {
                            egui::RichText::new(label)
                                .monospace()
                                .strong()
                                .color(theme.accent_text)
                                .background_color(theme.accent)
                        } else {
                            egui::RichText::new(label)
                                .monospace()
                                .color(theme.panel_text)
                        };
                        let response = ui.add(egui::Label::new(text).sense(egui::Sense::click()));
                        hand_cursor(&response, ui.ctx());
                        if response.clicked() {
                            app.lsp_completion.selected = i;
                            app.apply_lsp_completion();
                        }
                    }
                });
        });
}

fn render_text(ui: &mut egui::Ui, app: &mut EditorApp, theme: &GuiTheme) {
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
    // The active document's URI (owned). Per-line lookups borrow it to
    // reach the LSP manager without holding a borrow of `app` across the
    // whole line loop, which would block the loop's mutations.
    let active_uri = app.active_doc().uri();

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
    let (char_width, tab_width, font_row_height, line_height) = ui.fonts(|f| {
        let cw = f.glyph_width(&font_id, 'M');
        let tw = f.glyph_width(&font_id, '\t');
        let rh = f.row_height(&font_id);
        (cw, tw, rh, rh * LINE_SPACING)
    });
    // Glyph tops sit half a leading below each line-box top so the extra
    // spacing splits evenly above and below the text. Backgrounds
    // (selection, line highlight, stripes) keep filling the full box.
    let half_leading = (line_height - font_row_height) / 2.0;

    let advance_of = |c: char| -> f32 {
        if c == '\t' {
            tab_width
        } else {
            char_width
        }
    };
    // Cursor + selection state.
    let cursor_pos = app.active_buffer().cursor();
    let (cursor_line, _cursor_byte_col) = app
        .active_buffer()
        .pos_to_linecol(cursor_pos)
        .unwrap_or((0, 0));

    // Clear the Cmd-key link highlight; it will be recomputed below
    // if Command is held and the pointer is over an identifier.
    app.cmd_link.range = None;

    // All non-collapsed selections (for rendering selection rects).
    // Collapsed selections are carets (drawn separately below).
    let sel_ranges: Vec<std::ops::Range<usize>> = app
        .active_buffer()
        .selections()
        .iter()
        .filter(|s| !s.is_collapsed())
        .map(|s| s.range())
        .collect();
    // For backward compatibility with the match-highlight suppression
    // logic: if ANY selection is active on this line, skip match highlights.
    let _has_selection = !sel_ranges.is_empty();

    // Find-match highlight colors come from the active theme. Two
    // intensities mirror VSCode / Sublime: bright for the current
    // match, dimmer for the rest.
    let query_nonempty = !app.search.query.is_empty();
    let current_match_start = app.search.current_match();
    let current_match_color = theme.match_current;
    let other_match_color = theme.match_other;

    let prefix_text = format!("{:>width$} \u{2502} ", 1, width = gutter_width);
    let prefix_chars = prefix_text.chars().count();

    // When git blame is active, the gutter origin shifts right by
    // BLAME_WIDTH to make room for the blame column.
    let blame_on = app.active_doc().view.git_blame_enabled && app.active_doc().git_blame.enabled();
    let gw = if blame_on {
        GIT_GUTTER_WIDTH + BLAME_WIDTH
    } else {
        GIT_GUTTER_WIDTH
    };

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

    // If enabled, animate the caret's vertical position so it slides
    // between lines instead of teleporting. Otherwise, snap directly
    // to the target line. Three animation snap cases:
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
    if !app.config.caret_animation
        || scroll_override_y.is_some()
        || tab_switched
        || app.caret_anim_y.is_nan()
    {
        app.caret_anim_y = target_caret_y;
    } else {
        let dt = ui.input(|i| i.stable_dt).min(0.1);
        let lerp = 1.0 - (-CARET_ANIM_SPEED * dt).exp();
        app.caret_anim_y += (target_caret_y - app.caret_anim_y) * lerp;
        // Snap to the target line once we're within a pixel so the
        // caret doesn't hover slightly above/below the line forever.
        if (app.caret_anim_y - target_caret_y).abs() < 1.0 {
            app.caret_anim_y = target_caret_y;
        } else {
            // The caret is mid-slide; keep repainting until it lands so
            // it doesn't get stuck between lines when the app is idle.
            ui.ctx().request_repaint();
        }
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

    // Captured outside the ScrollArea closure so we can draw the
    // horizontal scrollbar overlay after the closure ends.
    let mut hbar_viewport: Option<(f32, f32, egui::Rect)> = None;

    scroll_area.show(ui, |ui| {
        let total_height = total_lines as f32 * line_height;
        let alloc = ui.allocate_response(
            egui::vec2(ui.available_width(), total_height),
            egui::Sense::click_and_drag(),
        );
        // The text surface carries a stable, focusable id so overlays
        // (fuzzy finder, command palette, …) can hand keyboard focus
        // back to the editor when they close.
        let response = ui.interact(
            alloc.rect,
            egui::Id::new(("editor_surface", app.active)),
            egui::Sense {
                focusable: true,
                ..egui::Sense::click_and_drag()
            },
        );
        if app.focus_editor {
            response.request_focus();
            app.focus_editor = false;
        }
        let rect = response.rect;
        let painter = ui.painter_at(rect);

        // Show a text cursor over the editable area (right of the
        // gutter), a pointing hand when Command is held over a linkable
        // token, and a pointing hand over the gutter numbers.
        if response.hovered() {
            if app.cmd_link.range.is_some() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            } else if let Some(pos) = response.hover_pos() {
                let gutter_right = rect.left() + gw + gutter_width as f32 * char_width + char_width;
                if pos.x >= gutter_right {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::Text);
                } else {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
            }
        }

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

        // While Command is held on macOS, track the token under the
        // pointer so we can underline it and turn clicks into
        // go-to-definition requests.
        let cmd_pressed = ui.input(|i| i.modifiers.command);
        let cmd_pointer: Option<(usize, usize)> = if cmd_pressed && response.hovered() {
            response.hover_pos().and_then(|pos| {
                let rel_x = pos.x - rect.left();
                let rel_y = pos.y - rect.top();
                if rel_y < 0.0 {
                    return None;
                }
                let line_offset = (rel_y / line_height).floor() as usize;
                if line_offset >= total_lines {
                    return None;
                }
                let text_x_relative = gw + prefix_chars as f32 * char_width
                    - app.active_doc().view.scroll_x_cols as f32 * char_width;
                let char_col = if rel_x < text_x_relative {
                    0usize
                } else {
                    ((rel_x - text_x_relative) / char_width) as usize
                };
                Some((line_offset, char_col))
            })
        } else {
            None
        };

        // Compute the visible line range from the ScrollArea's clip
        // rect. Rendering only visible lines keeps large files
        // responsive; without this, every frame would tokenize and
        // draw every line in the buffer.
        let clip_top = ui.clip_rect().top();
        let clip_bottom = ui.clip_rect().bottom();
        let viewport_width = ui.clip_rect().width();
        let first_visible = ((clip_top - rect.top()) / line_height).floor() as isize;
        let last_visible = ((clip_bottom - rect.top()) / line_height).ceil() as isize;
        let start_line = first_visible.max(0) as usize;
        let end_line = (last_visible as usize).min(total_lines);

        // Track the widest visible line so we can decide whether a
        // horizontal scrollbar is needed and size it correctly.
        let mut max_visible_line_cols: usize = 0;

        // Editor background fills the entire allocated content area.
        painter.rect_filled(rect, 0.0, theme.editor_bg);

        // Draw cursor line background first (under everything).
        if cursor_line >= start_line && cursor_line < end_line {
            let y = (rect.top() + cursor_line as f32 * line_height).round();
            painter.rect_filled(
                egui::Rect::from_min_size(
                    egui::pos2(rect.left(), y),
                    egui::vec2(rect.width(), line_height),
                ),
                0.0,
                theme.line_highlight,
            );
        }

        // Draw each line: gutter, text segments (before/inside/after
        // selection), and the cursor caret. Drawing the line as three
        // separate text segments — instead of drawing the full line
        // and then re-drawing the selected portion on top of the
        // selection rectangle — eliminates the "ghost" / "shadow"
        // effect that comes from any subpixel positioning mismatch
        // between the two text draws.
        for line_idx in start_line..end_line {
            // Skip folded (hidden) lines — they render as blank space.
            if app.active_doc().folds.is_hidden(line_idx) {
                continue;
            }
            // Round y to integer pixels so glyphs align cleanly with
            // the selection rectangle.
            let y = (rect.top() + line_idx as f32 * line_height).round();
            // Glyph band top: text, gutter numbers, carets and underlines
            // anchor here; full-height backgrounds still use `y`.
            let text_y = y + half_leading;

            // Byte range of this line in the buffer. Bound early so the
            // Cmd-click handler below (and the match-highlights block
            // later) can read it without re-querying.
            let line_byte_range = app
                .active_buffer()
                .line_byte_range(line_idx)
                .unwrap_or(0..0);

            // If Command is held and the pointer is on this line,
            // identify the identifier token under the pointer. Kept
            // before the `line_text` borrow below so the `app.cmd_link`
            // write does not straddle that (whole-app) borrow.
            if cmd_pointer.map(|(l, _)| l) == Some(line_idx) {
                let (_, char_col) = cmd_pointer.unwrap();
                let cmd_line_cow = app.active_buffer().line_text(line_idx).unwrap_or_default();
                let cmd_line: &str = &cmd_line_cow;
                let (start_char, end_char) = word_range_at_char_col(cmd_line, char_col);
                let start_byte =
                    line_byte_range.start + core::char_col_to_byte_col(cmd_line, start_char);
                let end_byte =
                    line_byte_range.start + core::char_col_to_byte_col(cmd_line, end_char);
                if end_byte > start_byte {
                    app.cmd_link.range = Some((start_byte, end_byte));
                }
            }

            // Each annotated gutter row is a focusable, non-interactive
            // accessibility target. Pointer hover shows the tooltip beside
            // the pointer; Tab focus shows the same details beside the row.
            // It remains non-interactive so editor click/drag selection keeps
            // using the enclosing response unchanged.
            if let Some(info) = gutter_hover_info(app, active_uri.as_ref(), line_idx) {
                let gutter_rect = egui::Rect::from_min_size(
                    egui::pos2(rect.left(), y),
                    egui::vec2(gw + prefix_chars as f32 * char_width, line_height),
                );
                let gutter_response = ui.interact(
                    gutter_rect,
                    egui::Id::new(("gutter_annotation", app.active, line_idx)),
                    egui::Sense::focusable_noninteractive(),
                );
                gutter_response.widget_info(|| {
                    egui::WidgetInfo::labeled(
                        egui::WidgetType::Label,
                        true,
                        format!("Line {} gutter annotations", info.line_number),
                    )
                });
                show_gutter_tooltip(gutter_response, &info, theme);
            }

            // Borrowed from the piece table when the whole line fits one
            // piece — no per-frame String copy for the common case. This
            // "pre" copy feeds the gutter/column/selection math below.
            // It must be fully consumed before the syntax cache-miss
            // takes `&mut` doc further down, so each text-draw branch
            // re-borrows the line after its cache lookup (same line —
            // the buffer does not change within a frame).
            let line_text_cow = app.active_buffer().line_text(line_idx).unwrap_or_default();
            let line_text: &str = &line_text_cow;

            let line_cols = line_text
                .chars()
                .map(|c| {
                    if c == '\t' {
                        app.active_doc().view.tab_width
                    } else {
                        1
                    }
                })
                .sum::<usize>();
            if line_cols > max_visible_line_cols {
                max_visible_line_cols = line_cols;
            }

            // Indent guides: thin vertical lines at each indent level
            // covered by the line's leading whitespace.
            let tab_width = app.active_doc().view.tab_width.max(1);
            let leading_cols: usize = line_text
                .chars()
                .take_while(|c| *c == ' ' || *c == '\t')
                .map(|c| if c == '\t' { tab_width } else { 1 })
                .sum();
            let indent_text_x = (rect.left() + gw + prefix_chars as f32 * char_width
                - app.active_doc().view.scroll_x_cols as f32 * char_width)
                .round();
            for level in 1.. {
                let guide_col = level * tab_width;
                if guide_col >= leading_cols {
                    break;
                }
                let gx = (indent_text_x + guide_col as f32 * char_width).round();
                painter.line_segment(
                    [egui::pos2(gx, y), egui::pos2(gx, y + line_height)],
                    egui::Stroke::new(1.0_f32, theme.indent_guide),
                );
            }

            // Git gutter column (left of the line-number gutter).
            let git_enabled =
                app.active_doc().view.git_gutter_enabled && app.active_doc().git_gutter.enabled();
            if git_enabled {
                let status = app.active_doc().git_gutter.status(line_idx);
                let bar_color = match status {
                    core::LineStatus::Added => Some(theme.git_added),
                    core::LineStatus::Modified => Some(theme.git_modified),
                    core::LineStatus::Unchanged => None,
                };
                if let Some(color) = bar_color {
                    let bar_rect = egui::Rect::from_min_size(
                        egui::pos2(rect.left() + (GIT_GUTTER_WIDTH - 4.0) / 2.0, y + 3.0),
                        egui::vec2(4.0, line_height - 6.0),
                    );
                    painter.rect_filled(bar_rect, 2.0, color);
                }
                let removed = app.active_doc().git_gutter.removed_count_before(line_idx);
                if removed > 0 {
                    let marker = if removed > 1 {
                        format!("▼{removed}")
                    } else {
                        "▼".to_string()
                    };
                    painter.text(
                        egui::pos2(rect.left() + GIT_GUTTER_WIDTH / 2.0, y - line_height / 2.0),
                        egui::Align2::CENTER_CENTER,
                        marker,
                        egui::FontId::proportional(8.0),
                        theme.git_deleted,
                    );
                }
            }

            // LSP diagnostic gutter stripe — a thin colored bar at the
            // far-left edge of the editor for every line that has a
            // diagnostic. Uses the most severe diagnostic on the line.
            // This makes errors unmissable even when the inline underline
            // is subtle.
            // O(1) per line: the per-line max-severity map is maintained
            // by the LSP manager on publishDiagnostics.
            let diag_severity = active_uri
                .as_ref()
                .and_then(|uri| app.lsp_manager.diagnostic_severity_on_line(uri, line_idx));
            if let Some(sev) = diag_severity {
                let stripe_color = match sev {
                    lsp_types::DiagnosticSeverity::ERROR => theme.error,
                    lsp_types::DiagnosticSeverity::WARNING => theme.warning,
                    lsp_types::DiagnosticSeverity::INFORMATION => theme.accent,
                    _ => theme.dim_text,
                };
                painter.rect_filled(
                    egui::Rect::from_min_size(
                        egui::pos2(rect.left(), y),
                        egui::vec2(3.0, line_height),
                    ),
                    0.0,
                    stripe_color,
                );
            }

            // Gutter (line number + separator).
            // When blame is active, draw the blame text in the blame
            // column before the line number.
            if blame_on {
                if let Some(entry) = app.active_doc().git_blame.entry(line_idx) {
                    let blame_text = format!(
                        "{} {} {}",
                        entry.short_hash, entry.author, entry.relative_time
                    );
                    painter.text(
                        egui::pos2((rect.left() + BLAME_WIDTH - 4.0).round(), text_y),
                        egui::Align2::RIGHT_TOP,
                        blame_text,
                        egui::FontId::monospace(FONT_SIZE - 1.0),
                        theme.dim_text,
                    );
                }
            }
            // Fold marker in the gutter: ▼ if folded, ▶ if foldable.
            let fold_marker = if app.active_doc().folds.is_folded_at(line_idx) {
                Some("▼")
            } else if app.active_doc().folds.is_foldable(line_idx) {
                Some("▶")
            } else {
                None
            };
            if let Some(marker) = fold_marker {
                painter.text(
                    egui::pos2(
                        (rect.left() + gw + gutter_width as f32 * char_width + char_width).round(),
                        text_y,
                    ),
                    egui::Align2::RIGHT_TOP,
                    marker,
                    egui::FontId::proportional(FONT_SIZE - 3.0),
                    theme.dim_text,
                );
            }

            let gutter = format!("{:>width$} \u{2502} ", line_idx + 1, width = gutter_width);
            let gutter_color = if line_idx == cursor_line {
                theme.line_number_active
            } else {
                theme.gutter_text
            };
            painter.text(
                egui::pos2((rect.left() + gw).round(), text_y),
                egui::Align2::LEFT_TOP,
                gutter,
                font_id.clone(),
                gutter_color,
            );

            let text_x = (rect.left() + gw + prefix_chars as f32 * char_width
                - app.active_doc().view.scroll_x_cols as f32 * char_width)
                .round();

            // Compute all selection intersections with this line. Can
            // be multiple with multi-cursor; each is a char-col range.
            // If none, `sel_in_line` stays empty and the line is drawn
            // as one piece.
            let sel_in_line: Vec<(usize, usize)> = sel_ranges
                .iter()
                .filter_map(|sr| {
                    let intersect = selection_in_line(line_byte_range.clone(), sr.clone())?;
                    let start = line_byte_range.start;
                    let total_chars = line_text.chars().count();
                    let take_lo =
                        byte_to_char_col(line_text, intersect.start - start).min(total_chars);
                    let take_hi =
                        byte_to_char_col(line_text, intersect.end - start).min(total_chars);
                    if take_hi > take_lo {
                        Some((take_lo, take_hi))
                    } else {
                        None
                    }
                })
                .collect();

            // Compute match highlights for this line. Skipped when any
            // selection is active on this line so the user can see their
            // selection without matches painting over it.
            let mut match_highlights: Vec<(usize, usize, egui::Color32)> = Vec::new();
            if sel_in_line.is_empty() && query_nonempty {
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
                    let intersect =
                        line_byte_range.start.max(m_start)..line_byte_range.end.min(m_end);
                    let start = line_byte_range.start;
                    let total_chars = line_text.chars().count();
                    let take_lo =
                        byte_to_char_col(line_text, intersect.start - start).min(total_chars);
                    let take_hi =
                        byte_to_char_col(line_text, intersect.end - start).min(total_chars);
                    if take_hi <= take_lo {
                        continue;
                    }
                    let color = if Some(m_start) == current_match_start {
                        current_match_color
                    } else {
                        other_match_color
                    };
                    match_highlights.push((take_lo, take_hi, color));
                }
            }

            if !sel_in_line.is_empty() {
                // Selection rendering: draw each selection rectangle
                // first, then the full line text on top. This handles
                // multiple selections per line (multi-cursor) cleanly
                // without complex segment splitting.
                for &(take_lo, take_hi) in &sel_in_line {
                    // Tab-aware widths straight from the font metrics —
                    // no String built per selection.
                    let sel_x = (text_x
                        + line_text.chars().take(take_lo).map(advance_of).sum::<f32>())
                    .round();
                    let sel_w = line_text
                        .chars()
                        .skip(take_lo)
                        .take(take_hi - take_lo)
                        .map(advance_of)
                        .sum::<f32>()
                        .round();
                    painter.rect_filled(
                        egui::Rect::from_min_size(
                            egui::pos2(sel_x, y),
                            egui::vec2(sel_w, line_height),
                        ),
                        0.0,
                        theme.selection_bg,
                    );
                }
                // Draw the full line text on top of the selection rects.
                // For syntax-less lines; syntax-highlighted lines with
                // selection still show segments (rare with multi-cursor).
                let doc = &mut app.documents[app.active];
                let syntax_theme = app.syntax.ts_theme();
                let cached = if !doc.syntax.dirty {
                    doc.syntax.lines.get(&line_idx).cloned()
                } else {
                    None
                };
                // `Rc` handle, not a copied Vec: a cache hit bumps a
                // refcount instead of re-allocating the line's segments
                // every frame.
                let segments: std::rc::Rc<Vec<core::ColorSegment>> = match cached {
                    Some(s) => s,
                    None => {
                        let (per_line, complete) =
                            doc.highlight_lines_ts(line_idx, line_idx + 1, syntax_theme);
                        let segs = per_line.into_iter().next().unwrap_or_default();
                        let cached_segs = std::rc::Rc::new(segs);
                        if complete {
                            if doc.syntax.dirty {
                                doc.syntax.lines.clear();
                                doc.syntax.dirty = false;
                            }
                            doc.syntax.lines.insert(line_idx, cached_segs.clone());
                        } else {
                            ui.ctx().request_repaint();
                        }
                        cached_segs
                    }
                };
                // Re-borrow after the cache lookup: the miss arm above
                // took `&mut` doc, which the buffer borrow held by
                // `line_text` above may not overlap. Same line.
                let line_text_cow = app.active_buffer().line_text(line_idx).unwrap_or_default();
                let line_text: &str = &line_text_cow;
                if segments.is_empty() {
                    painter.text(
                        egui::pos2(text_x, text_y),
                        egui::Align2::LEFT_TOP,
                        line_text,
                        font_id.clone(),
                        theme.text,
                    );
                } else {
                    // Walk segments, drawing each in its color. Each chunk
                    // must be offset by the width of everything before it —
                    // painter.text doesn't advance a cursor, so drawing
                    // every chunk at text_x stacks them on top of each
                    // other (the garbled-selection bug).
                    // `x_cursor` advances as we draw: everything before a
                    // piece is exactly what has already been placed, so
                    // no prefix String or per-segment width walk is
                    // needed. Slices are zero-copy char-boundary-safe
                    // because both column helpers clamp to the line.
                    let mut char_cursor = 0usize;
                    let mut x_cursor = text_x;
                    for seg in segments.iter() {
                        let seg_lo = byte_to_char_col(line_text, seg.range.start);
                        let seg_hi = byte_to_char_col(line_text, seg.range.end);
                        if seg_lo > char_cursor {
                            let gap: &str =
                                &line_text[core::char_col_to_byte_col(line_text, char_cursor)
                                    ..core::char_col_to_byte_col(line_text, seg_lo)];
                            if !gap.is_empty() {
                                painter.text(
                                    egui::pos2(x_cursor.round(), text_y),
                                    egui::Align2::LEFT_TOP,
                                    gap,
                                    font_id.clone(),
                                    theme.text,
                                );
                            }
                            x_cursor += gap.chars().map(advance_of).sum::<f32>();
                            char_cursor = seg_lo;
                        }
                        if seg_hi > seg_lo {
                            let seg_text: &str =
                                &line_text[core::char_col_to_byte_col(line_text, seg_lo)
                                    ..core::char_col_to_byte_col(line_text, seg_hi)];
                            if !seg_text.is_empty() {
                                let c = seg.color;
                                painter.text(
                                    egui::pos2(x_cursor.round(), text_y),
                                    egui::Align2::LEFT_TOP,
                                    seg_text,
                                    font_id.clone(),
                                    egui::Color32::from_rgb(c.r, c.g, c.b),
                                );
                                x_cursor += seg_text.chars().map(advance_of).sum::<f32>();
                            }
                            char_cursor = seg_hi;
                        }
                    }
                    // Trailing gap after the last segment.
                    let total_chars = line_text.chars().count();
                    if char_cursor < total_chars {
                        let tail: &str =
                            &line_text[core::char_col_to_byte_col(line_text, char_cursor)..];
                        if !tail.is_empty() {
                            painter.text(
                                egui::pos2(x_cursor.round(), y),
                                egui::Align2::LEFT_TOP,
                                tail,
                                font_id.clone(),
                                theme.text,
                            );
                        }
                    }
                }
            } else if match_highlights.is_empty() {
                // Plain line — check for syntax highlighting.
                // Tokens come from the per-document SyntaxCache,
                // lazily populated. Only lines without selection
                // and without match highlights get syntax colors
                // (precedence: selection > matches > syntax).
                // Cache lookup first (immutable borrow of doc.syntax). If
                // present and not dirty, skip the tree-sitter query.
                let cached = if !app.documents[app.active].syntax.dirty {
                    app.documents[app.active]
                        .syntax
                        .lines
                        .get(&line_idx)
                        .cloned()
                } else {
                    None
                };
                // `Rc` handle, not a copied Vec: a cache hit bumps a
                // refcount instead of re-allocating the line's segments
                // every frame.
                let segments: std::rc::Rc<Vec<core::ColorSegment>> =
                    match cached {
                        Some(s) => s,
                        None => {
                            // Cache miss: highlight this line via tree-sitter
                            // (immutable doc borrow ends here, producing an
                            // owned Vec), then insert into the cache.
                            let syntax_theme = app.syntax.ts_theme();
                            let (per_line, complete) = app.documents[app.active]
                                .highlight_lines_ts(line_idx, line_idx + 1, syntax_theme);
                            let segs = per_line.into_iter().next().unwrap_or_default();
                            let cached_segs = std::rc::Rc::new(segs);
                            let doc = &mut app.documents[app.active];
                            if complete {
                                if doc.syntax.dirty {
                                    doc.syntax.lines.clear();
                                    doc.syntax.dirty = false;
                                }
                                doc.syntax.lines.insert(line_idx, cached_segs.clone());
                            } else {
                                ui.ctx().request_repaint();
                            }
                            cached_segs
                        }
                    };
                // Re-borrow after the cache lookup (see the selection
                // branch above) — the miss arm took `&mut` doc.
                let line_text_cow = app.active_buffer().line_text(line_idx).unwrap_or_default();
                let line_text: &str = &line_text_cow;
                if segments.is_empty() {
                    // No syntax (unknown extension, too large, or
                    // passthrough) — draw as before.
                    painter.text(
                        egui::pos2(text_x, text_y),
                        egui::Align2::LEFT_TOP,
                        line_text,
                        font_id.clone(),
                        theme.text,
                    );
                } else {
                    // Batch the whole line into ONE LayoutJob: a single
                    // galley (cached by egui across frames) instead of
                    // one painter.text call per segment. Syntax-dense
                    // lines (e.g. minified JSON) produce hundreds of
                    // segments; drawing each separately cost tens of ms
                    // per frame and made large files unusable.
                    let mut job = egui::text::LayoutJob::default();
                    let mut byte_cursor = 0usize;
                    for seg in segments.iter() {
                        let seg_start = seg.range.start.min(line_text.len());
                        let seg_end = seg.range.end.min(line_text.len());
                        if seg_start > byte_cursor {
                            job.append(
                                &line_text[byte_cursor..seg_start],
                                0.0,
                                egui::TextFormat {
                                    font_id: font_id.clone(),
                                    color: theme.text,
                                    ..Default::default()
                                },
                            );
                        }
                        if seg_end > seg_start {
                            let c = seg.color;
                            job.append(
                                &line_text[seg_start..seg_end],
                                0.0,
                                egui::TextFormat {
                                    font_id: font_id.clone(),
                                    color: egui::Color32::from_rgb(c.r, c.g, c.b),
                                    ..Default::default()
                                },
                            );
                            byte_cursor = seg_end;
                        }
                    }
                    if byte_cursor < line_text.len() {
                        job.append(
                            &line_text[byte_cursor..],
                            0.0,
                            egui::TextFormat {
                                font_id: font_id.clone(),
                                color: theme.text,
                                ..Default::default()
                            },
                        );
                    }
                    let galley = ui.fonts(|f| f.layout_job(job));
                    painter.galley(egui::pos2(text_x, y), galley, theme.text);
                }
            } else {
                // Multi-match highlights. Walk the line text
                // left-to-right, emitting plain / styled /
                // plain / styled / ... segments. Each styled
                // segment gets its own background rectangle so
                // adjacent matches render as adjacent coloured
                // bars.
                // Normalize possibly-overlapping match ranges into disjoint
                // spans. The current match wins wherever ranges overlap so
                // its complete range stays visually identifiable while the
                // user navigates between hits.
                let mut events: Vec<(usize, i32, i32)> = Vec::new();
                for (lo, hi, color) in match_highlights {
                    let is_current = color == current_match_color;
                    let depths = if is_current { (0, 1) } else { (1, 0) };
                    events.push((lo, depths.0, depths.1));
                    events.push((hi, -depths.0, -depths.1));
                }
                events.sort_unstable_by_key(|event| event.0);

                let mut highlights = Vec::new();
                let mut other_depth = 0i32;
                let mut current_depth = 0i32;
                let mut event_idx = 0usize;
                while event_idx < events.len() {
                    let pos = events[event_idx].0;
                    while event_idx < events.len() && events[event_idx].0 == pos {
                        other_depth += events[event_idx].1;
                        current_depth += events[event_idx].2;
                        event_idx += 1;
                    }
                    let Some(next_pos) = events.get(event_idx).map(|event| event.0) else {
                        break;
                    };
                    let color = if current_depth > 0 {
                        Some(current_match_color)
                    } else if other_depth > 0 {
                        Some(other_match_color)
                    } else {
                        None
                    };
                    if let Some(color) = color {
                        if let Some((_, previous_hi, previous_color)) = highlights.last_mut() {
                            if *previous_hi == pos && *previous_color == color {
                                *previous_hi = next_pos;
                                continue;
                            }
                        }
                        highlights.push((pos, next_pos, color));
                    }
                }

                let line_chars: Vec<char> = line_text.chars().collect();
                let mut cursor = 0usize;
                let mut segment_x = text_x.round();
                let total_chars = line_chars.len();
                for (lo, hi, color) in highlights {
                    if lo > cursor {
                        let plain: String = line_chars[cursor..lo].iter().collect();
                        if !plain.is_empty() {
                            let galley = painter.layout_no_wrap(plain, font_id.clone(), theme.text);
                            let width = galley.size().x;
                            painter.galley(egui::pos2(segment_x, text_y), galley, theme.text);
                            segment_x += width;
                        }
                    }
                    let matched: String = line_chars[lo..hi].iter().collect();
                    if !matched.is_empty() {
                        let match_text_color = if color == current_match_color {
                            theme.match_current_text
                        } else {
                            theme.match_other_text
                        };
                        let galley =
                            painter.layout_no_wrap(matched, font_id.clone(), match_text_color);
                        let matched_w = galley.size().x;
                        painter.rect_filled(
                            egui::Rect::from_min_size(
                                egui::pos2(segment_x, y),
                                egui::vec2(matched_w, line_height),
                            ),
                            0.0,
                            color,
                        );
                        painter.galley(egui::pos2(segment_x, text_y), galley, match_text_color);
                        segment_x += matched_w;
                    }
                    cursor = hi;
                }
                if cursor < total_chars {
                    let tail: String = line_chars[cursor..].iter().collect();
                    if !tail.is_empty() {
                        let galley = painter.layout_no_wrap(tail, font_id.clone(), theme.text);
                        painter.galley(egui::pos2(segment_x, y), galley, theme.text);
                    }
                }
            }

            // Re-borrow the line for the underline pass: the syntax
            // cache miss in the branches above may have taken `&mut`
            // doc, which the first buffer borrow cannot span. Same line.
            let line_text_cow = app.active_buffer().line_text(line_idx).unwrap_or_default();
            let line_text: &str = &line_text_cow;

            // LSP diagnostic underlines.
            for diag in active_uri
                .as_ref()
                .map(|uri| app.lsp_manager.diagnostics(uri))
                .unwrap_or(&[])
            {
                let diag_start_line = diag.range.start.line as usize;
                let diag_end_line = diag.range.end.line as usize;
                if !diagnostic_overlaps_line(&diag.range, line_idx) {
                    continue;
                }

                let start_byte_in_line = if diag_start_line < line_idx {
                    line_byte_range.start
                } else {
                    line_byte_range.start
                        + char_col_to_byte_col(line_text, diag.range.start.character as usize)
                };
                let end_byte_in_line = if diag_end_line > line_idx {
                    line_byte_range.end
                } else {
                    line_byte_range.start
                        + char_col_to_byte_col(line_text, diag.range.end.character as usize)
                };
                let start_byte =
                    start_byte_in_line.clamp(line_byte_range.start, line_byte_range.end);
                let end_byte = end_byte_in_line.clamp(line_byte_range.start, line_byte_range.end);
                if end_byte <= start_byte {
                    continue;
                }

                let start_col = byte_to_char_col(line_text, start_byte - line_byte_range.start);
                let end_col = byte_to_char_col(line_text, end_byte - line_byte_range.start);
                let x1 = text_x
                    + line_text
                        .chars()
                        .take(start_col)
                        .map(advance_of)
                        .sum::<f32>();
                let x2 = text_x + line_text.chars().take(end_col).map(advance_of).sum::<f32>();
                let color = match diag.severity {
                    Some(lsp_types::DiagnosticSeverity::ERROR) => theme.error,
                    Some(lsp_types::DiagnosticSeverity::WARNING) => theme.warning,
                    _ => theme.dim_text,
                };
                let y_under = text_y + font_row_height - 3.0;
                painter.rect_filled(
                    egui::Rect::from_min_max(
                        egui::pos2(x1, y_under),
                        egui::pos2(x2, y_under + 3.0),
                    ),
                    0.0,
                    color,
                );
            }

            // macOS Cmd-link underline for the token under the pointer.
            if let Some((link_start, link_end)) = app.cmd_link.range {
                let start = link_start.clamp(line_byte_range.start, line_byte_range.end);
                let end = link_end.clamp(line_byte_range.start, line_byte_range.end);
                if end > start {
                    let start_col = byte_to_char_col(line_text, start - line_byte_range.start);
                    let end_col = byte_to_char_col(line_text, end - line_byte_range.start);
                    let x1 = text_x
                        + line_text
                            .chars()
                            .take(start_col)
                            .map(advance_of)
                            .sum::<f32>();
                    let x2 = text_x + line_text.chars().take(end_col).map(advance_of).sum::<f32>();
                    let y_under = text_y + font_row_height - 3.0;
                    painter.line_segment(
                        [egui::pos2(x1, y_under), egui::pos2(x2, y_under)],
                        egui::Stroke::new(1.5_f32, theme.accent),
                    );
                }
            }

            // (Caret painting moved outside the loop — see below.)
        }

        // Caret x-position (shared by bracket highlight and carets).
        let text_x_caret = (rect.left() + gw + prefix_chars as f32 * char_width
            - app.active_doc().view.scroll_x_cols as f32 * char_width)
            .round();

        // Bracket match highlight: if the cursor is next to a bracket,
        // highlight both brackets with a subtle background rect.
        let cursor_pos = app.active_buffer().cursor();
        if app.active_buffer().selection().is_collapsed()
            && app.active_buffer().selections().len() == 1
        {
            if let Some((bracket_pos, match_pos)) =
                core::matching_bracket(app.active_buffer(), cursor_pos)
            {
                for pos in [bracket_pos, match_pos] {
                    let (bl, bc) = app.active_buffer().pos_to_linecol(pos).unwrap_or((0, 0));
                    if bl < start_line || bl >= end_line {
                        continue; // only draw if visible
                    }
                    let line_text_cow = app.active_buffer().line_text(bl).unwrap_or_default();
                    let line_text: &str = &line_text_cow;
                    let char_col = byte_to_char_col(line_text, bc);
                    let bx = (text_x_caret + char_col as f32 * char_width).round();
                    let by = (rect.top() + bl as f32 * line_height + half_leading).round();
                    painter.rect_filled(
                        egui::Rect::from_min_size(
                            egui::pos2(bx, by),
                            egui::vec2(char_width, font_row_height),
                        ),
                        0.0,
                        theme.selection_bg,
                    );
                }
            }
        }

        // Carets — painted after the line loop so they sit on top of all
        // text. Draw one caret per collapsed selection (multi-cursor).
        // Non-collapsed selections are already marked by their rectangles.
        for sel in app.active_buffer().selections() {
            if !sel.is_collapsed() {
                continue;
            }
            let pos = sel.head;
            let (cl, bc) = app.active_buffer().pos_to_linecol(pos).unwrap_or((0, 0));
            if cl >= total_lines {
                continue;
            }
            let caret_line_text_cow = app.active_buffer().line_text(cl).unwrap_or_default();
            let caret_line_text: &str = &caret_line_text_cow;
            let char_col = byte_to_char_col(caret_line_text, bc);
            let caret_x = (text_x_caret + char_col as f32 * char_width).round();
            // The primary caret uses the animated y; additional carets
            // snap to their line (animation across N carets is a follow-up).
            // `half_leading` keeps the caret on the glyph band, and its
            // height is the font's row — not the taller line box.
            let caret_y = if cl == cursor_line {
                (rect.top() + app.caret_anim_y + half_leading).round()
            } else {
                (rect.top() + cl as f32 * line_height + half_leading).round()
            };
            let block_caret = matches!(
                app.keymap.vim_mode(),
                Some(core::VimMode::Normal | core::VimMode::Visual | core::VimMode::VisualLine)
            );
            painter.rect_filled(
                egui::Rect::from_min_size(
                    egui::pos2(caret_x, caret_y),
                    egui::vec2(
                        if block_caret { char_width } else { CARET_WIDTH },
                        font_row_height,
                    ),
                ),
                0.0,
                theme.caret,
            );
        }

        // Mouse handling: convert pointer position to byte position
        // and dispatch SetCursor (click) or SelectExtendTo (drag).
        // text_x is the SCREEN position of the text origin (gutter
        // right-edge minus horizontal scroll). pixel_to_byte_pos
        // uses it to map pointer.x → char_col.
        let text_x = rect.left() + gw + prefix_chars as f32 * char_width
            - app.active_doc().view.scroll_x_cols as f32 * char_width;
        if response.clicked() || response.dragged() {
            // Clicking or dragging in the editor gives the editor focus.
            app.project_tree_focused = false;
            if let Some(pos) = response.interact_pointer_pos() {
                // Alt/Opt+click adds a cursor (multi-cursor) instead of
                // replacing the selection list.
                let alt_held = ui.input(|i| i.modifiers.alt);
                let alt_click = response.clicked() && alt_held;
                let alt_drag = response.dragged() && alt_held;
                let cmd_click = response.clicked() && ui.input(|i| i.modifiers.command);
                if alt_click {
                    // Start of a potential column-select drag. Record the
                    // (line, col). If it turns into a drag, we'll fan out.
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
                        if let Some((line, col)) = app.active_buffer().pos_to_linecol(byte_pos) {
                            app.column_select_start = Some((line, col));
                        }
                        app.handle_event(EditorEvent::AddCursor { pos: byte_pos });
                    }
                } else if alt_drag {
                    // Column selection: fan out from the start to the
                    // current position, one selection per line.
                    if let Some((from_line, from_col)) = app.column_select_start {
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
                            if let Some((to_line, to_col)) =
                                app.active_buffer().pos_to_linecol(byte_pos)
                            {
                                app.handle_event(EditorEvent::ColumnSelect {
                                    from_line,
                                    from_col,
                                    to_line,
                                    to_col,
                                });
                            }
                        }
                    }
                } else if cmd_click {
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
                        if let Some((uri, lsp_pos)) = app.lsp_position_for_byte(byte_pos) {
                            if let Some(resp) = app.lsp_manager.request_definition(&uri, lsp_pos) {
                                if let Some((target_uri, target_pos)) =
                                    core::lsp::LspManager::definition_target(resp)
                                {
                                    app.jump_to_lsp_location(target_uri, target_pos);
                                } else {
                                    app.status_message =
                                        Some("LSP: no definition found.".to_string());
                                }
                            } else {
                                app.lsp_definition.pending = true;
                                app.lsp_definition.request_pos = Some(lsp_pos);
                                app.lsp_definition.from_cmd_click = true;
                                app.status_message =
                                    Some("LSP: requesting definition...".to_string());
                            }
                        }
                    }
                } else if let Some(byte_pos) = pixel_to_byte_pos(
                    app,
                    pos,
                    rect,
                    text_x,
                    char_width,
                    line_height,
                    prefix_chars,
                    gutter_width,
                ) {
                    if response.dragged() {
                        app.handle_event(EditorEvent::SelectExtendTo { pos: byte_pos });
                    } else {
                        app.handle_event(EditorEvent::SetCursor { pos: byte_pos });
                    }
                }
            }
        }

        // Horizontal scroll: trackpad horizontal swipe (scroll_delta.x)
        // or Shift+vertical wheel scrolls left/right. egui's
        // ScrollArea is vertical-only, so any horizontal scroll delta
        // is free for us to consume.
        let scroll_delta = ui.input(|i| i.smooth_scroll_delta);
        if scroll_delta != egui::Vec2::ZERO {
            let shift = ui.input(|i| i.modifiers.shift);
            let h_delta = if shift {
                scroll_delta.y
            } else {
                scroll_delta.x
            };
            if h_delta != 0.0 {
                let cols_delta = (h_delta / char_width).round() as i32;
                let new_cols =
                    (app.active_doc().view.scroll_x_cols as i32 + cols_delta).max(0) as usize;
                app.active_doc_mut().view.scroll_x_cols = new_cols;
            }
        }

        // Decide whether a horizontal scrollbar is needed based on the
        // widest line currently visible.
        let content_width = GIT_GUTTER_WIDTH
            + prefix_chars as f32 * char_width
            + max_visible_line_cols as f32 * char_width
            + 4.0 * char_width;
        if content_width > viewport_width {
            hbar_viewport = Some((viewport_width, content_width, ui.clip_rect()));
        }
    });

    if let Some((viewport_width, content_width, clip_rect)) = hbar_viewport {
        render_horizontal_scrollbar(
            ui,
            app,
            clip_rect,
            viewport_width,
            content_width,
            char_width,
        );
    }

    // Mark the cursor position as seen so the next frame's
    // `cursor_moved` check correctly detects fresh motion. Per-doc
    // so we don't conflate "tab switched" with "cursor moved within
    // this doc" — see the comment at the top of `render_text`.
    app.active_doc_mut().view.last_seen_cursor = current_cursor;
}

/// Draw a horizontal scrollbar overlay at the bottom of the editor
/// viewport when at least one visible line is wider than the viewport.
/// Handles drag and click-to-jump.
fn render_horizontal_scrollbar(
    ui: &mut egui::Ui,
    app: &mut EditorApp,
    viewport_rect: egui::Rect,
    viewport_width: f32,
    content_width: f32,
    char_width: f32,
) {
    let theme = *theme(app);
    let bar_height = 10.0;
    let track_rect = egui::Rect::from_min_size(
        egui::pos2(viewport_rect.left(), viewport_rect.bottom() - bar_height),
        egui::vec2(viewport_width, bar_height),
    );

    let ratio = viewport_width / content_width;
    let thumb_width = (viewport_width * ratio).max(20.0).min(viewport_width);
    let max_scroll_px = content_width - viewport_width;
    let current_scroll_px = app.active_doc().view.scroll_x_cols as f32 * char_width;
    let scroll_frac = if max_scroll_px > 0.0 {
        (current_scroll_px / max_scroll_px).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let thumb_x = track_rect.left() + scroll_frac * (track_rect.width() - thumb_width);
    let thumb_rect = egui::Rect::from_min_size(
        egui::pos2(thumb_x, track_rect.top()),
        egui::vec2(thumb_width, bar_height),
    );

    let response = ui.interact(
        track_rect,
        egui::Id::new("hscrollbar"),
        egui::Sense::click_and_drag(),
    );
    if response.hovered() || response.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
    }

    if response.dragged() {
        let delta = response.drag_delta().x;
        let content_delta = delta / ratio;
        let cols_delta = (content_delta / char_width).round() as i32;
        let new_cols = (app.active_doc().view.scroll_x_cols as i32 + cols_delta).max(0) as usize;
        app.active_doc_mut().view.scroll_x_cols = new_cols;
    }

    if response.clicked() && !response.dragged() {
        if let Some(pos) = response.interact_pointer_pos() {
            let frac = ((pos.x - track_rect.left()) / track_rect.width()).clamp(0.0, 1.0);
            let max_scroll_cols = (max_scroll_px / char_width).round() as usize;
            app.active_doc_mut().view.scroll_x_cols =
                (frac * max_scroll_cols as f32).round() as usize;
        }
    }

    let painter = ui.painter();
    painter.rect_filled(track_rect, 4.0, theme.panel_bg);
    painter.rect_filled(thumb_rect, 4.0, theme.dim_text);
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

    // Determine visual column from x. If the click is in the gutter,
    // snap to the start of the line. Tabs are treated as `tab_width`
    // visual columns so clicks on indented lines land where the user
    // expects.
    let text_x_relative = text_x - rect.left();
    let visual_col = if rel_x < text_x_relative {
        0usize
    } else {
        ((rel_x - text_x_relative) / char_width) as usize
    };
    let _ = (prefix_chars, gutter_width); // currently unused; reserved for future

    let line_text = app
        .active_buffer()
        .line_text(line_offset)
        .map(|c| c.into_owned())
        .unwrap_or_default();
    let tab_width = app.active_doc().view.tab_width;
    let byte_col = visual_col_to_byte_col(&line_text, visual_col, tab_width);

    let line_byte_start = app.active_buffer().line_byte_range(line_offset)?.start;
    Some(line_byte_start + byte_col)
}

/// Expand a character column in a line to the boundaries of the
/// identifier-like token it sits on. Returns `(start_char, end_char)`
/// using character (not byte) indices. If the column is not on an
/// identifier character, the nearest preceding identifier character is
/// used so that clicking just after a token still selects it.
fn word_range_at_char_col(line_text: &str, char_col: usize) -> (usize, usize) {
    let chars: Vec<char> = line_text.chars().collect();
    if chars.is_empty() {
        return (0, 0);
    }
    let is_id = |c: char| c.is_alphanumeric() || c == '_';

    let mut col = char_col.min(chars.len().saturating_sub(1));
    if !is_id(chars[col]) && col > 0 && is_id(chars[col - 1]) {
        col -= 1;
    }
    if !is_id(chars[col]) {
        return (col, col);
    }

    let mut start = col;
    while start > 0 && is_id(chars[start - 1]) {
        start -= 1;
    }
    let mut end = col;
    while end < chars.len() && is_id(chars[end]) {
        end += 1;
    }
    (start, end)
}

#[cfg(test)]
mod tests {
    use super::{
        diagnostic_overlaps_line, project_tree_reveal_offset, render_gutter_hover,
        render_settings_window, show_gutter_tooltip, truncate_hover_text, word_range_at_char_col,
        DiagnosticHover, GutterHoverInfo, GUTTER_TOOLTIP_DELAY_SECONDS,
    };

    #[test]
    fn project_tree_reveal_offset_accounts_for_virtual_row_spacing() {
        let offset = project_tree_reveal_offset(100, 18.0, 6.0, 240.0);
        assert_eq!(offset, 2_292.0);
    }

    #[test]
    fn truncated_project_tree_label_stays_one_virtual_row_high() {
        let ctx = egui::Context::default();
        let mut measured = None;
        ctx.begin_pass(egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(120.0, 100.0),
            )),
            ..Default::default()
        });
        egui::CentralPanel::default().show(&ctx, |ui| {
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
            let row_height = ui.spacing().interact_size.y;
            let response = ui.selectable_label(
                false,
                egui::RichText::new(format!(
                    "{}a_very_long_filename_that_cannot_fit.rs",
                    "  ".repeat(20)
                ))
                .monospace(),
            );
            measured = Some((response.rect.height(), row_height));
        });
        let _ = ctx.end_pass();

        let (actual, expected) = measured.unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn settings_window_renders_in_a_small_viewport() {
        let ctx = egui::Context::default();
        let buffer: Box<dyn core::Buffer> =
            Box::new(core::PieceTableBuffer::from_bytes(b"# Settings\n".to_vec()));
        let mut app = crate::app::EditorApp::new(buffer);
        app.settings_open = true;
        ctx.begin_pass(egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(480.0, 360.0),
            )),
            ..Default::default()
        });

        render_settings_window(&ctx, &mut app);
        let output = ctx.end_pass();

        assert!(app.settings_open);
        assert!(!output.shapes.is_empty());
    }

    #[test]
    fn gutter_hover_renders_blame_git_and_lsp_details() {
        let ctx = egui::Context::default();
        let info = GutterHoverInfo {
            line_number: 7,
            git_status: Some(core::LineStatus::Modified),
            removed_count: 1,
            removed_lines: vec!["old value".to_string()],
            blame: Some(core::BlameEntry {
                short_hash: "abcdef0".to_string(),
                author: "Alice".to_string(),
                relative_time: "2d".to_string(),
                commit: std::sync::Arc::new(core::BlameCommit {
                    full_hash: "abcdef0123456789abcdef0123456789abcdef01".to_string(),
                    full_author: "Alice Example".to_string(),
                    summary: "Explain gutter annotations".to_string(),
                }),
            }),
            diagnostics: vec![DiagnosticHover {
                severity: Some(lsp_types::DiagnosticSeverity::ERROR),
                source: Some("rust-analyzer".to_string()),
                code: Some("E0308".to_string()),
                message: "mismatched types".to_string(),
            }],
            remaining_diagnostics: 0,
        };
        ctx.begin_pass(egui::RawInput::default());
        egui::CentralPanel::default().show(&ctx, |ui| {
            render_gutter_hover(ui, &info, crate::theme::GuiTheme::default_dark());
        });
        let output = ctx.end_pass();

        assert!(!output.shapes.is_empty());
    }

    #[test]
    fn diagnostic_line_overlap_respects_end_exclusivity() {
        let multi_line = lsp_types::Range::new(
            lsp_types::Position::new(3, 2),
            lsp_types::Position::new(5, 0),
        );
        assert!(diagnostic_overlaps_line(&multi_line, 3));
        assert!(diagnostic_overlaps_line(&multi_line, 4));
        assert!(!diagnostic_overlaps_line(&multi_line, 5));

        let zero_width = lsp_types::Range::new(
            lsp_types::Position::new(7, 4),
            lsp_types::Position::new(7, 4),
        );
        assert!(diagnostic_overlaps_line(&zero_width, 7));
    }

    #[test]
    fn hover_text_truncation_only_returns_the_bounded_prefix() {
        assert_eq!(truncate_hover_text("short", 10), "short");
        assert_eq!(truncate_hover_text("abcdef", 3), "abc…");
        assert_eq!(truncate_hover_text("åßç∂", 2), "åß…");
    }

    #[test]
    fn gutter_tooltip_uses_fast_delay_and_restores_global_style() {
        let ctx = egui::Context::default();
        let default_delay = ctx.style().interaction.tooltip_delay;
        assert!(GUTTER_TOOLTIP_DELAY_SECONDS < default_delay);
        let info = GutterHoverInfo {
            line_number: 1,
            git_status: Some(core::LineStatus::Modified),
            removed_count: 0,
            removed_lines: Vec::new(),
            blame: None,
            diagnostics: Vec::new(),
            remaining_diagnostics: 0,
        };
        let pointer = egui::pos2(20.0, 20.0);

        let mut tooltip_was_open = false;
        for (pass, time) in [0.0, 0.05, 0.10, 0.16, 0.20, 0.25, 0.30, 0.31]
            .into_iter()
            .enumerate()
        {
            let events = if pass == 0 {
                vec![egui::Event::PointerMoved(pointer)]
            } else {
                Vec::new()
            };
            ctx.begin_pass(egui::RawInput {
                time: Some(time),
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(200.0, 120.0),
                )),
                events,
                ..Default::default()
            });
            egui::CentralPanel::default().show(&ctx, |ui| {
                let rect =
                    egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(100.0, 30.0));
                let response = ui.interact(
                    rect,
                    egui::Id::new("fast_gutter_tooltip_test"),
                    egui::Sense::hover(),
                );
                tooltip_was_open |= response.is_tooltip_open();
                show_gutter_tooltip(response, &info, crate::theme::GuiTheme::default_dark());
            });
            let _ = ctx.end_pass();
            assert_eq!(ctx.style().interaction.tooltip_delay, default_delay);
        }
        assert!(
            tooltip_was_open,
            "tooltip should open before egui's default 500 ms delay"
        );
    }

    #[test]
    fn word_range_finds_identifier_at_col() {
        let line = "let foo_bar = 42;";
        // "foo_bar" starts at char column 4.
        assert_eq!(word_range_at_char_col(line, 6), (4, 11));
    }

    #[test]
    fn word_range_uses_preceding_identifier_when_after_token() {
        let line = "let foo = 42;";
        // Column 7 is the space immediately after "foo".
        assert_eq!(word_range_at_char_col(line, 7), (4, 7));
    }

    #[test]
    fn word_range_returns_empty_for_non_identifier() {
        let line = "let foo = 42;";
        // Column 9 is the space between '=' and '42'; neither side is
        // an identifier, so no token is selected.
        assert_eq!(word_range_at_char_col(line, 9), (9, 9));
    }

    #[test]
    fn word_range_handles_empty_line() {
        assert_eq!(word_range_at_char_col("", 0), (0, 0));
    }
}
