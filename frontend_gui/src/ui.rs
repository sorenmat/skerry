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
use crate::theme::GuiTheme;

const FONT_SIZE: f32 = 14.0;
const CARET_WIDTH: f32 = 2.0;
/// Exponential-decay speed for caret animation (1/seconds). Higher =
/// snappier. At 25, the time constant is 40 ms — the caret reaches
/// ~70 % of the way in 3 frames (50 ms at 60 fps), which feels
/// responsive without looking like a hard teleport.
const CARET_ANIM_SPEED: f32 = 25.0;

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
    // click responses per-tab. The active tab uses the accent color;
    // inactive tabs are dimmed. Clicking an inactive tab switches
    // `app.active`.
    ui.horizontal(|ui| {
        ui.add_space(4.0);
        for i in 0..app.doc_count() {
            let is_active = i == app.active;
            let doc = &app.documents[i];
            let name = doc.display_name();
            let dirty = if doc.is_dirty() { "*" } else { "" };
            let stale = if doc.external_change { "!" } else { "" };
            let label = format!(" {}{}{} ", name, dirty, stale);

            let text = if is_active {
                egui::RichText::new(&label)
                    .monospace()
                    .strong()
                    .color(theme.accent_text)
                    .background_color(theme.accent)
            } else {
                egui::RichText::new(&label)
                    .monospace()
                    .color(theme.dim_text)
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
                ui.label(egui::RichText::new("│").monospace().color(theme.separator));
                ui.add_space(2.0);
            }
        }
    });
}

pub fn render(ctx: &egui::Context, app: &mut EditorApp) {
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

    egui::TopBottomPanel::top("header")
        .frame(egui::Frame::none().fill(theme.panel_bg).inner_margin(6.0))
        .show(ctx, |ui| {
            render_header_strip(ui, app);
        });

    egui::TopBottomPanel::bottom("status")
        .frame(egui::Frame::none().fill(theme.status_bg).inner_margin(6.0))
        .show(ctx, |ui| {
            // Snapshot theme lists + current names as owned data so the
            // ComboBox closures don't borrow `app`.
            let syntax_theme_names: Vec<String> = app
                .syntax
                .theme_names()
                .into_iter()
                .map(|s| s.to_string())
                .collect();
            let current_syntax_theme = app.syntax.theme_name().to_string();
            let mut selected_syntax_theme: Option<String> = None;

            let ui_theme_names: Vec<String> =
                GuiTheme::all().iter().map(|t| t.name.to_string()).collect();
            let current_ui_theme = app.theme.name.to_string();
            let mut selected_ui_theme: Option<String> = None;

            let mut toggle_tree = false;

            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(format!(" {status_message}  |  {status_pos}"))
                        .monospace()
                        .color(theme.status_text),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    egui::ComboBox::from_id_salt("syntax_theme_selector")
                        .selected_text(&current_syntax_theme)
                        .width(160.0)
                        .show_ui(ui, |ui| {
                            for name in &syntax_theme_names {
                                let is_active = name == &current_syntax_theme;
                                if ui.selectable_label(is_active, name).clicked() {
                                    selected_syntax_theme = Some(name.clone());
                                }
                            }
                        });
                    egui::ComboBox::from_id_salt("ui_theme_selector")
                        .selected_text(&current_ui_theme)
                        .width(100.0)
                        .show_ui(ui, |ui| {
                            for name in &ui_theme_names {
                                let is_active = name == &current_ui_theme;
                                if ui.selectable_label(is_active, name).clicked() {
                                    selected_ui_theme = Some(name.clone());
                                }
                            }
                        });
                    let tree_label = if app.project_tree_open {
                        "🌳 Tree"
                    } else {
                        "Tree"
                    };
                    if ui
                        .selectable_label(app.project_tree_open, tree_label)
                        .clicked()
                    {
                        toggle_tree = true;
                    }
                });
            });

            if toggle_tree {
                app.handle_event(EditorEvent::ToggleProjectTree);
            }

            if let Some(name) = selected_syntax_theme {
                if app.syntax.set_theme_by_name(&name) {
                    for doc in &mut app.documents {
                        doc.syntax.invalidate();
                    }
                    app.status_message = Some(format!("Syntax theme: {name}"));
                }
            }

            if let Some(name) = selected_ui_theme {
                app.set_ui_theme_by_name(&name);
                app.status_message = Some(format!("UI theme: {name}"));
            }
        });

    // Find bar — appears above the status bar when open.
    if app.search.bar_open {
        egui::TopBottomPanel::bottom("find_bar")
            .frame(
                egui::Frame::none()
                    .fill(theme.panel_bg)
                    .inner_margin(6.0)
                    .stroke(egui::Stroke::new(1.0, theme.border)),
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
                    if regex_btn.clicked() {
                        app.handle_event(EditorEvent::ToggleFindRegex);
                    }
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

    // Go-to-line bar — appears above the status bar when open.
    if app.go_to_line_dialog.is_some() {
        egui::TopBottomPanel::bottom("go_to_line_bar")
            .frame(
                egui::Frame::none()
                    .fill(theme.panel_bg)
                    .inner_margin(6.0)
                    .stroke(egui::Stroke::new(1.0, theme.border)),
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

    // Project-tree sidebar. Lives on the left, collapsible via F2.
    if app.project_tree_open {
        render_project_tree_sidebar(ctx, app);
    }

    egui::CentralPanel::default()
        .frame(egui::Frame::none().fill(theme.editor_bg))
        .show(ctx, |ui| {
            render_text(ui, app, &theme);
        });

    // Modal prompts render on top. Close-confirm is a centred dialog;
    // the open-file dialog is also centred. The order matters only if
    // both were up (they shouldn't be — opening one drops the other)
    // and we want close-confirm on top because it's tied to a
    // destructive action.
    if app.close_confirm.is_some() {
        render_close_confirm_window(ctx, app);
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

            egui::ScrollArea::vertical()
                .id_salt("project_tree_scroll")
                .auto_shrink([false; 2])
                .show_rows(
                    ui,
                    ui.text_style_height(&egui::TextStyle::Body),
                    rows.len(),
                    |ui, row_range| {
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
                    },
                );
        });
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
    ui.button(text)
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
                        if ui.button("Replace").clicked() {
                            app.handle_event(EditorEvent::ProjectSearchReplaceAllConfirm);
                        }
                        if ui.button("Cancel").clicked() {
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
            let item_count = app.command_palette.items.len();
            egui::ScrollArea::vertical()
                .id_salt("command_palette_items")
                .auto_shrink([false; 2])
                .show_rows(
                    ui,
                    ui.text_style_height(&egui::TextStyle::Body),
                    item_count,
                    |ui, row_range| {
                        for i in row_range {
                            let command = &app.command_palette.items[i];
                            let label = if command.keybinding.is_empty() {
                                command.label.to_string()
                            } else {
                                format!("{}  ({})", command.label, command.keybinding)
                            };
                            let is_selected = i == selected;
                            let text = egui::RichText::new(label).monospace().size(14.0);
                            let response = if is_selected {
                                ui.selectable_label(true, text.strong())
                            } else {
                                ui.selectable_label(false, text)
                            };
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
    let (cursor_line, cursor_byte_col) = app
        .active_buffer()
        .pos_to_linecol(cursor_pos)
        .unwrap_or((0, 0));

    let selection = app.active_buffer().selection();
    let sel_range: Option<std::ops::Range<usize>> = if selection.is_collapsed() {
        None
    } else {
        Some(selection.range())
    };

    // Find-match highlight colors come from the active theme. Two
    // intensities mirror VSCode / Sublime: bright for the current
    // match, dimmer for the rest.
    let query_nonempty = !app.search.query.is_empty();
    let current_match_start = app.search.current_match();
    let current_match_color = theme.match_current;
    let other_match_color = theme.match_other;

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
        // Snap to the target line once we're within a pixel so the
        // caret doesn't hover slightly above/below the line forever.
        if (app.caret_anim_y - target_caret_y).abs() < 1.0 {
            app.caret_anim_y = target_caret_y;
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
        // Create one highlighter for this render pass and reuse it for
        // every visible line. This avoids per-line setup cost without
        // leaking memory in `SyntaxEngine`.
        let path = app.active_doc().path_buf();
        let syntax = app.syntax.syntax_for_path(path.as_deref());
        let mut highlighter = syntax.map(|s| app.syntax.highlighter_for(s));

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
            // Round y to integer pixels so glyphs align cleanly with
            // the selection rectangle.
            let y = (rect.top() + line_idx as f32 * line_height).round();

            let line_text = app
                .active_buffer()
                .line_text(line_idx)
                .map(|c| c.into_owned())
                .unwrap_or_default();

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

            // Git gutter bar / deletion marker.
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
                        egui::pos2(rect.left() + 2.0, y + 3.0),
                        egui::vec2(4.0, line_height - 6.0),
                    );
                    painter.rect_filled(bar_rect, 2.0, color);
                }
                let removed = app.active_doc().git_gutter.removed_blocks_before(line_idx);
                if !removed.is_empty() {
                    let total = removed.iter().map(|b| b.count).sum::<usize>();
                    let marker = if total > 1 {
                        format!("▼{total}")
                    } else {
                        "▼".to_string()
                    };
                    painter.text(
                        egui::pos2(rect.left() + 4.0, y - line_height / 2.0),
                        egui::Align2::CENTER_CENTER,
                        marker,
                        egui::FontId::proportional(8.0),
                        theme.git_deleted,
                    );
                }
            }

            // Gutter (line number + separator).
            let gutter = format!("{:>width$} \u{2502} ", line_idx + 1, width = gutter_width);
            let gutter_color = if line_idx == cursor_line {
                theme.line_number_active
            } else {
                theme.gutter_text
            };
            painter.text(
                egui::pos2((rect.left()).round(), y),
                egui::Align2::LEFT_TOP,
                gutter,
                font_id.clone(),
                gutter_color,
            );

            let text_x = (rect.left() + prefix_chars as f32 * char_width
                - app.active_doc().view.scroll_x_cols as f32 * char_width)
                .round();

            // Compute selection-in-this-line once. If there's no
            // selection, `seg` stays at the default (None) and we
            // draw the entire line as one piece. `line_byte_range`
            // is bound in the outer scope so the match-highlights
            // block below can read it without re-querying.
            let line_byte_range = app
                .active_buffer()
                .line_byte_range(line_idx)
                .unwrap_or(0..0);
            let sel_in_line: Option<(usize, usize)> = sel_range.as_ref().and_then(|sr| {
                let intersect = selection_in_line(line_byte_range.clone(), sr.clone())?;
                let start = line_byte_range.start;
                let total_chars = line_text.chars().count();
                let take_lo =
                    byte_to_char_col(&line_text, intersect.start - start).min(total_chars);
                let take_hi = byte_to_char_col(&line_text, intersect.end - start).min(total_chars);
                if take_hi > take_lo {
                    Some((take_lo, take_hi))
                } else {
                    None
                }
            });

            // Compute match highlights for this line. One entry per match that
            // overlaps this line, each tagged with the colour
            // (bright for current, dim for the rest). Same shape
            // as `sel_in_line` (a char-col range) but it's a Vec
            // because there can be many matches on a line. Skipped
            // when a selection is present so the user can see
            // their selection without matches painting over it.
            //
            // Matches are clipped to the line's byte range so
            // multi-line regex matches still show their visible
            // portion.
            let mut match_highlights: Vec<(usize, usize, egui::Color32)> = Vec::new();
            if sel_in_line.is_none() && query_nonempty {
                let mut idx = app
                    .search
                    .matches
                    .partition_point(|&(s, _)| s < line_byte_range.start);
                if idx > 0 {
                    // A match may start on the previous line but extend
                    // into this one; include it if it overlaps.
                    idx -= 1;
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
                        byte_to_char_col(&line_text, intersect.start - start).min(total_chars);
                    let take_hi =
                        byte_to_char_col(&line_text, intersect.end - start).min(total_chars);
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

            if let Some((take_lo, take_hi)) = sel_in_line {
                // Selection rendering: three segments
                // (before / selected / after). Each is drawn
                // exactly once at an integer-rounded x to avoid
                // sub-pixel ghosting on the selection rectangle.
                let before: String = line_text.chars().take(take_lo).collect();
                let selected: String = line_text
                    .chars()
                    .skip(take_lo)
                    .take(take_hi - take_lo)
                    .collect();
                let after: String = line_text.chars().skip(take_hi).collect();

                let sel_x = (text_x + width_of(&before)).round();
                let sel_w = width_of(&selected).round();

                if !before.is_empty() {
                    painter.text(
                        egui::pos2(text_x, y),
                        egui::Align2::LEFT_TOP,
                        before,
                        font_id.clone(),
                        theme.text,
                    );
                }

                painter.rect_filled(
                    egui::Rect::from_min_size(egui::pos2(sel_x, y), egui::vec2(sel_w, line_height)),
                    0.0,
                    theme.selection_bg,
                );
                painter.text(
                    egui::pos2(sel_x, y),
                    egui::Align2::LEFT_TOP,
                    selected,
                    font_id.clone(),
                    theme.text,
                );

                if !after.is_empty() {
                    painter.text(
                        egui::pos2((sel_x + sel_w).round(), y),
                        egui::Align2::LEFT_TOP,
                        after,
                        font_id.clone(),
                        theme.text,
                    );
                }
            } else if match_highlights.is_empty() {
                // Plain line — check for syntax highlighting.
                // Tokens come from the per-document SyntaxCache,
                // lazily populated. Only lines without selection
                // and without match highlights get syntax colors
                // (precedence: selection > matches > syntax).
                let doc = &mut app.documents[app.active];
                let segments = get_syntax_segments(
                    &app.syntax,
                    &mut doc.syntax,
                    &mut highlighter,
                    line_idx,
                    &line_text,
                );
                if segments.is_empty() {
                    // No syntax (unknown extension, too large, or
                    // passthrough) — draw as before.
                    painter.text(
                        egui::pos2(text_x, y),
                        egui::Align2::LEFT_TOP,
                        &line_text,
                        font_id.clone(),
                        theme.text,
                    );
                } else {
                    // Walk segments left-to-right, drawing each
                    // in its syntect theme color. Since segments
                    // cover the entire line (no gaps), we just
                    // draw each one sequentially.
                    let mut char_cursor = 0usize;
                    for seg in &segments {
                        let seg_lo = byte_to_char_col(&line_text, seg.range.start);
                        let seg_hi = byte_to_char_col(&line_text, seg.range.end);
                        // Draw gap before this segment (shouldn't
                        // happen — segments cover the full line —
                        // but kept as a safety net).
                        if seg_lo > char_cursor {
                            let gap: String = line_text
                                .chars()
                                .skip(char_cursor)
                                .take(seg_lo - char_cursor)
                                .collect();
                            if !gap.is_empty() {
                                let gap_x = (text_x
                                    + width_of(
                                        &line_text.chars().take(char_cursor).collect::<String>(),
                                    ))
                                .round();
                                painter.text(
                                    egui::pos2(gap_x, y),
                                    egui::Align2::LEFT_TOP,
                                    gap,
                                    font_id.clone(),
                                    theme.text,
                                );
                            }
                        }
                        // Draw the colored segment.
                        let text: String = line_text
                            .chars()
                            .skip(seg_lo)
                            .take(seg_hi - seg_lo)
                            .collect();
                        if !text.is_empty() {
                            let seg_x = (text_x
                                + width_of(&line_text.chars().take(seg_lo).collect::<String>()))
                            .round();
                            let c = seg.color;
                            painter.text(
                                egui::pos2(seg_x, y),
                                egui::Align2::LEFT_TOP,
                                text,
                                font_id.clone(),
                                egui::Color32::from_rgb(c.r, c.g, c.b),
                            );
                        }
                        char_cursor = seg_hi;
                    }
                    // Trailing gap after last segment.
                    let total_chars = line_text.chars().count();
                    if char_cursor < total_chars {
                        let tail: String = line_text.chars().skip(char_cursor).collect();
                        if !tail.is_empty() {
                            let tail_x = (text_x
                                + width_of(
                                    &line_text.chars().take(char_cursor).collect::<String>(),
                                ))
                            .round();
                            painter.text(
                                egui::pos2(tail_x, y),
                                egui::Align2::LEFT_TOP,
                                tail,
                                font_id.clone(),
                                theme.text,
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
                        let plain: String =
                            line_text.chars().skip(cursor).take(lo - cursor).collect();
                        if !plain.is_empty() {
                            painter.text(
                                egui::pos2(text_x, y),
                                egui::Align2::LEFT_TOP,
                                plain,
                                font_id.clone(),
                                theme.text,
                            );
                        }
                    }
                    let matched: String = line_text.chars().skip(lo).take(hi - lo).collect();
                    if !matched.is_empty() {
                        // Width must be measured from `text_x` so
                        // we account for the chars before this
                        // segment too — `width_of(&matched)` alone
                        // would be wrong if a previous segment
                        // included tabs (tab advance ≠ 1 char).
                        let matched_x = (text_x
                            + width_of(&line_text.chars().take(lo).collect::<String>()))
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
                        let match_text_color = if color == current_match_color {
                            theme.match_current_text
                        } else {
                            theme.match_other_text
                        };
                        painter.text(
                            egui::pos2(matched_x, y),
                            egui::Align2::LEFT_TOP,
                            matched,
                            font_id.clone(),
                            match_text_color,
                        );
                    }
                    cursor = hi;
                }
                if cursor < total_chars {
                    let tail: String = line_text.chars().skip(cursor).collect();
                    if !tail.is_empty() {
                        painter.text(
                            egui::pos2(text_x, y),
                            egui::Align2::LEFT_TOP,
                            tail,
                            font_id.clone(),
                            theme.text,
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
            let caret_line_text = app
                .active_buffer()
                .line_text(cursor_line)
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
                theme.caret,
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
            // Clicking or dragging in the editor gives the editor focus.
            app.project_tree_focused = false;
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
        let content_width = prefix_chars as f32 * char_width
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

/// Get syntax tokens for a line, using the per-document cache.
/// Tokenizes on cache miss (lazy population). Returns an empty Vec
/// Get syntax color segments for a line, using the per-document
/// cache and the global `SyntaxEngine`. Tokenizes on cache miss
/// (lazy population). Returns an empty Vec when syntax highlighting
/// is disabled (file too large, unknown extension, or no path).
///
/// `highlighter` is created once per render pass and reused for each
/// visible line; the caller owns it so we don't need a self-referential
/// `SyntaxEngine`.
fn get_syntax_segments(
    syntax_engine: &core::SyntaxEngine,
    cache: &mut core::SyntaxCache,
    highlighter: &mut Option<core::HighlightLines<'_>>,
    line_idx: usize,
    line_text: &str,
) -> Vec<core::ColorSegment> {
    if !cache.dirty {
        if let Some(segs) = cache.lines.get(&line_idx) {
            return segs.clone();
        }
    }

    let segments = if let Some(ref mut h) = highlighter {
        syntax_engine.highlight_line_with(h, line_text)
    } else {
        Vec::new()
    };

    if cache.dirty {
        cache.lines.clear();
        cache.dirty = false;
    }
    cache.lines.insert(line_idx, segments.clone());
    segments
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

    let line_text = app
        .active_buffer()
        .line_text(line_offset)
        .map(|c| c.into_owned())
        .unwrap_or_default();
    let total_chars = line_text.chars().count();
    let char_col = char_col.min(total_chars);
    let byte_col = core::char_col_to_byte_col(&line_text, char_col);

    let line_byte_start = app.active_buffer().line_byte_range(line_offset)?.start;
    Some(line_byte_start + byte_col)
}
