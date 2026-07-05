//! `EditorApp` — owns the `Buffer`, view state, and event handling.
//!
//! Mirrors `frontend_tui::App` so the event-handling logic stays in
//! lockstep across frontends (ADR 0005).

use std::time::Instant;

use core::{Buffer, Document, EditorEvent, Movement, Search, Selection, SyntaxEngine};
use eframe::egui;
use eframe::egui::Context;
use eframe::App;

use crate::theme::GuiTheme;

/// The GUI editor application. eframe calls `update()` each frame.
pub struct EditorApp {
    /// All open documents. `active` indexes into this Vec; all
    /// events flow through `active_doc_mut()`.
    pub documents: Vec<Document>,
    /// Index of the currently focused document into `documents`.
    pub active: usize,
    pub should_quit: bool,
    pub status_message: Option<String>,
    /// Number of lines that fit in the current viewport. Updated each
    /// frame by the renderer; used by PageUp/PageDown to compute a
    /// reasonable page size when there's no other source of truth.
    /// Window-global, not per-document — it's a property of the
    /// window's height, not of any single doc.
    pub viewport_lines: usize,
    /// Find state: query, match list, current match, bar visibility.
    /// Operates on the active document's buffer.
    pub search: Search,
    /// Close-on-dirty prompt. `Some` while the prompt is up — the
    /// renderer draws the dialog window and the input loop intercepts
    /// keys instead of forwarding them to `handle_event`. Mirrors the
    /// TUI's `close_confirm`.
    pub close_confirm: Option<CloseConfirm>,
    /// Go-to-line dialog. `Some` while the prompt is up. The user types
    /// a 1-based line number; Enter jumps, Esc cancels.
    pub go_to_line_dialog: Option<GoToLineDialog>,
    /// Animated caret vertical position in content-space pixels
    /// (`cursor_line * line_height`). Lerps toward the target each
    /// frame so the caret slides smoothly between lines instead of
    /// teleporting. Snaps (no lerp) when the view scrolls (edge-stick
    /// already handles smoothness) or when switching tabs. `NaN`
    /// means "not initialized" — the first render snaps to the
    /// cursor's actual position.
    pub caret_anim_y: f32,
    /// Previous `active` document index. Used to detect tab switches
    /// so the caret animation snaps instead of sliding from the old
    /// tab's caret position to the new one.
    pub prev_active: usize,
    /// Global syntax highlighting engine. Holds all language
    /// definitions and the active theme. Shared across all documents.
    pub syntax: SyntaxEngine,
    /// Whether the project file-tree sidebar is visible.
    pub project_tree_open: bool,
    /// The active document's project tree, including expansion state.
    pub project_tree: Option<core::ProjectTree>,
    /// Index of the selected row in the visible (flattened) project tree.
    pub project_tree_selected: usize,
    /// Whether the project tree sidebar currently has keyboard focus.
    pub project_tree_focused: bool,
    /// Project-wide search dialog state.
    pub project_search: ProjectSearch,
    /// Whether the keybindings help window is open.
    pub keybindings_help_open: bool,
    /// Fuzzy file finder state.
    pub fuzzy_finder: FuzzyFinder,
    /// Command palette state.
    pub command_palette: CommandPalette,
    /// Persistent user configuration / session state.
    pub config: core::Config,
    /// Active GUI chrome theme. Applied to egui's global visuals each
    /// frame so every widget and panel uses the palette.
    pub theme: GuiTheme,
    /// Time of the most recent buffer-modifying edit. Used by auto-save
    /// to decide when the user has been idle long enough to save.
    pub last_edit_time: Instant,
    /// Whether the window currently has focus. Used to detect focus loss
    /// for auto-save-on-focus-change.
    pub window_focused: bool,
    /// File-system watcher for externally changed files. `None` if the
    /// watcher could not be initialized on this platform.
    pub file_watcher: Option<core::FileWatcher>,
}

/// State for the project-wide search / replace dialog.
#[derive(Debug, Clone, Default)]
pub struct ProjectSearch {
    /// Whether the search dialog is open.
    pub open: bool,
    /// Current search query string.
    pub query: String,
    /// Current replacement query string.
    pub replace_query: String,
    /// Search results.
    pub results: Vec<core::ProjectSearchResult>,
    /// Replace preview results.
    pub replace_previews: Vec<core::ReplacePreview>,
    /// Index of the selected result.
    pub selected: usize,
    /// Whether the replace-all confirmation prompt is currently shown.
    pub confirm_replace: bool,
}

/// One candidate shown in the fuzzy file finder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuzzyCandidate {
    /// Display text shown in the finder (typically a relative path).
    pub display: String,
    /// Absolute path to open when this candidate is selected.
    pub path: std::path::PathBuf,
}

/// State for the fuzzy file finder.
#[derive(Debug, Clone, Default)]
pub struct FuzzyFinder {
    /// Whether the finder is open.
    pub open: bool,
    /// Current query string.
    pub query: String,
    /// All available candidates.
    pub items: Vec<FuzzyCandidate>,
    /// Filtered and ranked candidate indices + match metadata.
    pub filtered: Vec<(usize, core::FuzzyMatch)>,
    /// Index into `filtered` of the currently selected candidate.
    pub selected: usize,
}

/// State for the command palette.
#[derive(Debug, Clone, Default)]
pub struct CommandPalette {
    /// Whether the palette is open.
    pub open: bool,
    /// Current filter query.
    pub query: String,
    /// Filtered command list.
    pub items: Vec<&'static core::Command>,
    /// Index of the selected command.
    pub selected: usize,
}

/// The three choices offered when closing a dirty document. Mirrors
/// `frontend_tui::app::CloseChoice` exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseChoice {
    Save,
    Discard,
    Cancel,
}

/// State for the close-on-dirty dialog. Captured at prompt-open time
/// so the close target doesn't shift if the user changes the active
/// doc while the prompt is up.
#[allow(dead_code)] // `doc_index` reserved for future prompt+tab-switch interleave.
pub struct CloseConfirm {
    pub doc_index: usize,
    pub choice: CloseChoice,
}

/// State for the go-to-line text-input dialog.
pub struct GoToLineDialog {
    pub query: String,
}

impl EditorApp {
    /// Create an `EditorApp` around a single buffer. Wraps the buffer
    /// in a one-element document list.
    pub fn new(buffer: Box<dyn Buffer>) -> Self {
        Self::new_with_documents(vec![Document::new(buffer)], core::Config::default())
    }

    /// Create an `EditorApp` around a pre-built list of documents.
    /// The first document becomes active.
    pub fn new_with_documents(mut documents: Vec<Document>, config: core::Config) -> Self {
        assert!(
            !documents.is_empty(),
            "EditorApp needs at least one document"
        );
        for doc in &mut documents {
            config.apply_document_defaults(&mut doc.view);
        }
        let theme = config
            .ui_theme
            .as_deref()
            .and_then(GuiTheme::by_name)
            .copied()
            .unwrap_or_else(|| *GuiTheme::default_dark());
        let mut app = Self {
            documents,
            active: 0,
            should_quit: false,
            status_message: None,
            viewport_lines: 20,
            search: Search::new(),
            close_confirm: None,
            go_to_line_dialog: None,
            caret_anim_y: f32::NAN,
            prev_active: 0,
            syntax: SyntaxEngine::default_dark(),
            project_tree_open: config.project_tree_open.unwrap_or(true),
            project_tree: None,
            project_tree_selected: 0,
            project_tree_focused: false,
            project_search: ProjectSearch::default(),
            keybindings_help_open: false,
            fuzzy_finder: FuzzyFinder::default(),
            command_palette: CommandPalette::default(),
            config,
            theme,
            last_edit_time: Instant::now(),
            window_focused: true,
            file_watcher: core::FileWatcher::new().ok(),
        };
        if let Some(theme) = app.config.theme.clone() {
            app.syntax.set_theme_by_name(&theme);
        }
        app.refresh_project_tree();
        app.sync_watcher();
        app
    }

    /// Switch the GUI chrome theme by name. Returns true if the name
    /// matched a built-in theme.
    pub fn set_ui_theme_by_name(&mut self, name: &str) -> bool {
        if let Some(theme) = GuiTheme::by_name(name) {
            self.theme = *theme;
            self.config.ui_theme = Some(theme.name.to_string());
            true
        } else {
            false
        }
    }

    /// Toggle smooth vertical caret animation in the GUI.
    pub fn toggle_caret_animation(&mut self) {
        self.config.caret_animation = !self.config.caret_animation;
        self.snap_caret_animation();
        self.status_message = Some(if self.config.caret_animation {
            "Caret animation: on".to_string()
        } else {
            "Caret animation: off".to_string()
        });
    }

    /// Index of the currently focused document.
    pub fn active(&self) -> usize {
        self.active
    }

    /// Reference to the active document.
    pub fn active_doc(&self) -> &Document {
        &self.documents[self.active]
    }

    /// Mutable reference to the active document. Use this when you
    /// need to change per-document state (view, search, etc.).
    pub fn active_doc_mut(&mut self) -> &mut Document {
        &mut self.documents[self.active]
    }

    /// Shortcut for `self.active_doc().buffer` as `&dyn Buffer`.
    pub fn active_buffer(&self) -> &dyn Buffer {
        &*self.documents[self.active].buffer
    }

    /// Shortcut for `self.active_doc_mut().buffer` as `&mut dyn Buffer`.
    pub fn active_buffer_mut(&mut self) -> &mut dyn Buffer {
        &mut *self.documents[self.active].buffer
    }

    /// Number of currently open documents.
    pub fn doc_count(&self) -> usize {
        self.documents.len()
    }

    /// Whether a modal with a text-edit query is currently open. When
    /// true, clipboard cut/copy and raw text/key events should be routed
    /// to the modal's TextEdit, not the buffer.
    fn text_modal_open(&self) -> bool {
        self.fuzzy_finder.open
            || self.command_palette.open
            || self.project_search.open
            || self.search.bar_open
            || self.search.replace_bar_open
    }

    /// Pull input events from egui and apply them to the buffer.
    ///
    /// Clipboard shortcuts (Cmd/Ctrl+C, X, V) are intercepted here instead
    /// of going through `translate_event` because they need OS clipboard
    /// access that doesn't fit into a single `EditorEvent`. We still go
    /// through `handle_event` for the buffer side (delete-selection on cut,
    /// paste-text on paste) — only the clipboard read/write is local to
    /// the frontend.
    ///
    /// While the find bar is open, Enter / Shift+Enter / Esc are handled
    /// here so they go to the bar instead of triggering the buffer's
    /// default (newline insert / extend-selection / quit).
    ///
    /// The same intercept pattern handles the close-confirm prompt and
    /// the open-file dialog (both set state on `self` and intercept
    /// keys before they reach `translate_event`).
    pub fn handle_input(&mut self, ctx: &Context) {
        let mut clipboard_events: Vec<crate::event::ClipboardAction> = Vec::new();
        ctx.input(|i| {
            for event in &i.events {
                let text_modal_open = self.text_modal_open();
                if !text_modal_open {
                    if let Some(clip) =
                        crate::event::classify_clipboard_event(event, self.active_buffer())
                    {
                        clipboard_events.push(clip);
                        continue;
                    }
                }
                if self.search.bar_open {
                    if let Some(bar_event) = find_bar_translate(event) {
                        self.handle_event(bar_event);
                        continue;
                    }
                    // Swallow text events so they update the find bar
                    // TextEdit but don't insert into the buffer.
                    if matches!(event, eframe::egui::Event::Text(_)) {
                        continue;
                    }
                }
                // Fuzzy file finder intercepts navigation keys while it's
                // open. Text input is handled by its TextEdit widget.
                if self.fuzzy_finder.open {
                    if let Some(fuzzy_event) = crate::event::fuzzy_finder_translate(event) {
                        self.handle_event(fuzzy_event);
                        continue;
                    }
                }
                // Command palette intercepts navigation keys while it's
                // open. Text input is handled by its TextEdit widget.
                if self.command_palette.open {
                    if let Some(palette_event) = crate::event::command_palette_translate(event) {
                        self.handle_event(palette_event);
                        continue;
                    }
                }
                // Project-search dialog intercepts navigation keys and
                // printable input while it's open.
                if self.project_search.open {
                    let query = self.project_search.query.clone();
                    let confirm = self.project_search.confirm_replace;
                    if let Some(search_event) =
                        crate::event::project_search_translate(event, &query, confirm)
                    {
                        self.handle_event(search_event);
                        continue;
                    }
                    // Suppress remaining key/text events so the query
                    // TextEdit doesn't also write through to the buffer.
                    if matches!(
                        event,
                        eframe::egui::Event::Key { .. } | eframe::egui::Event::Text(_)
                    ) {
                        continue;
                    }
                }
                // Fuzzy finder and command palette use egui TextEdit
                // widgets for their queries. After handling navigation
                // keys, swallow all remaining key/text events so they
                // don't reach the buffer.
                if self.fuzzy_finder.open || self.command_palette.open {
                    if matches!(
                        event,
                        eframe::egui::Event::Key { .. } | eframe::egui::Event::Text(_)
                    ) {
                        continue;
                    }
                }
                // Project-tree sidebar intercepts navigation keys only
                // when it has keyboard focus. Mouse clicks update focus
                // in the UI renderers.
                if self.project_tree_open && self.project_tree_focused {
                    if let Some(tree_event) = project_tree_translate(event) {
                        self.handle_event(tree_event);
                        continue;
                    }
                }
                // Modal prompts intercept keys before translate_event
                // so e.g. Ctrl+W inside the open-file dialog doesn't
                // bounce back into a close-confirm prompt.
                if self.dispatch_modal_event(event) {
                    continue;
                }
                // The keybindings help window closes on Esc.
                if self.keybindings_help_open {
                    if let eframe::egui::Event::Key {
                        key: eframe::egui::Key::Escape,
                        pressed: true,
                        ..
                    } = event
                    {
                        self.keybindings_help_open = false;
                        continue;
                    }
                }
                if let Some(editor_event) = crate::event::translate_event(event) {
                    self.handle_event(editor_event);
                }
            }
        });
        for action in clipboard_events {
            self.apply_clipboard_action(ctx, action);
        }
    }

    /// Intercept an egui event when a modal prompt is open. Mirrors
    /// `frontend_tui::App::dispatch_modal_key` for parity.
    ///
    /// - close_confirm: Tab/Shift+Tab cycle the focused choice, Enter
    ///   confirms, `y` confirms as Discard, Esc/`n` cancel.
    /// - go_to_line_dialog: printable chars / Backspace edit the line
    ///   number, Enter submits, Esc cancels.
    ///
    /// Returns `true` when the event was consumed.
    fn dispatch_modal_event(&mut self, event: &eframe::egui::Event) -> bool {
        use eframe::egui::{Event, Key};

        if self.close_confirm.is_some() {
            if let Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } = event
            {
                match *key {
                    Key::Escape => {
                        self.close_confirm = None;
                        self.status_message = Some("Close cancelled.".to_string());
                    }
                    Key::N if !modifiers.command && !modifiers.ctrl => {
                        self.close_confirm = None;
                        self.status_message = Some("Close cancelled.".to_string());
                    }
                    Key::Tab if !modifiers.shift => self.cycle_close_choice(1),
                    Key::Tab => self.cycle_close_choice(-1), // Shift+Tab
                    Key::ArrowRight => self.cycle_close_choice(1),
                    Key::ArrowLeft => self.cycle_close_choice(-1),
                    Key::Enter => self.confirm_close_choice(),
                    Key::Y => {
                        // One-key Discard shortcut.
                        self.close_confirm = None;
                        self.perform_close_active();
                    }
                    _ => {} // Eat everything else.
                }
            }
            return true;
        }

        if self.go_to_line_dialog.is_some() {
            if let Event::Key {
                key, pressed: true, ..
            } = event
            {
                match *key {
                    Key::Escape => self.cancel_go_to_line_dialog(),
                    Key::Enter => self.submit_go_to_line_dialog(),
                    Key::Backspace => self.pop_go_to_line_query(),
                    _ => {}
                }
            }
            if let Event::Text(text) = event {
                if !text.is_empty() {
                    for c in text.chars() {
                        self.push_go_to_line_query(c);
                    }
                }
                return true;
            }
            return true;
        }

        false
    }

    /// Execute a clipboard action that was classified earlier (during
    /// `handle_input`). At this point egui's input lock is released so
    /// we can call `ctx.output_mut`.
    fn apply_clipboard_action(&mut self, ctx: &Context, action: crate::event::ClipboardAction) {
        match action {
            crate::event::ClipboardAction::Copy(text) => {
                ctx.copy_text(text);
            }
            crate::event::ClipboardAction::Cut(text) => {
                ctx.copy_text(text);
                self.handle_event(EditorEvent::DeleteSelection);
            }
        }
    }

    /// Apply an `EditorEvent` to the buffer / app state. Same logic as
    /// `frontend_tui::App::handle_event` so both frontends behave
    /// identically.
    pub fn handle_event(&mut self, event: EditorEvent) {
        // Check whether this event modifies the buffer text — used to
        // invalidate the syntax cache after the match.
        let modifies_buffer = matches!(
            &event,
            EditorEvent::Insert(_)
                | EditorEvent::InsertTab
                | EditorEvent::DeleteLeft
                | EditorEvent::DeleteRight
                | EditorEvent::DeleteSelection
                | EditorEvent::DeleteWordLeft
                | EditorEvent::DeleteWordRight
                | EditorEvent::DeleteLine
                | EditorEvent::DuplicateLine
                | EditorEvent::MoveLineUp
                | EditorEvent::MoveLineDown
                | EditorEvent::Paste(_)
                | EditorEvent::Undo
                | EditorEvent::Redo
                | EditorEvent::ReplaceOne
                | EditorEvent::ReplaceAll
        );
        match event {
            EditorEvent::Insert(ch) => {
                // Selection-aware: a non-collapsed selection is replaced
                // by the inserted character (matches every editor since
                // 1995).
                self.insert_text(&ch.to_string());
            }
            EditorEvent::DeleteLeft => {
                if self.delete_selection_if_any() {
                    return;
                }
                let pos = self.active_buffer().cursor();
                if pos > 0 {
                    match self.active_buffer_mut().delete((pos - 1)..pos) {
                        Ok(new_pos) => {
                            self.active_buffer_mut().set_cursor(new_pos);
                            self.active_buffer_mut()
                                .set_selection(Selection::collapsed(new_pos));
                        }
                        Err(e) => self.status_message = Some(format!("delete error: {e}")),
                    }
                }
            }
            EditorEvent::DeleteRight => {
                if self.delete_selection_if_any() {
                    return;
                }
                let pos = self.active_buffer().cursor();
                if pos < self.active_buffer().len() {
                    match self.active_buffer_mut().delete(pos..(pos + 1)) {
                        Ok(new_pos) => {
                            self.active_buffer_mut().set_cursor(new_pos);
                            self.active_buffer_mut()
                                .set_selection(Selection::collapsed(new_pos));
                        }
                        Err(e) => self.status_message = Some(format!("delete error: {e}")),
                    }
                }
            }
            EditorEvent::DeleteSelection => {
                self.delete_selection_if_any();
            }
            EditorEvent::DeleteWordLeft => {
                if self.delete_selection_if_any() {
                    return;
                }
                let pos = self.active_buffer().cursor();
                if pos == 0 {
                    return;
                }
                let target = skip_word_left(self.active_buffer(), pos);
                if target == pos {
                    return;
                }
                match self.active_buffer_mut().delete(target..pos) {
                    Ok(new_pos) => {
                        self.active_buffer_mut().set_cursor(new_pos);
                        self.active_buffer_mut()
                            .set_selection(Selection::collapsed(new_pos));
                    }
                    Err(e) => self.status_message = Some(format!("delete error: {e}")),
                }
            }
            EditorEvent::DeleteWordRight => {
                if self.delete_selection_if_any() {
                    return;
                }
                let pos = self.active_buffer().cursor();
                let len = self.active_buffer().len();
                if pos >= len {
                    return;
                }
                let target = skip_word_right(self.active_buffer(), pos);
                if target == pos {
                    return;
                }
                match self.active_buffer_mut().delete(pos..target) {
                    Ok(new_pos) => {
                        self.active_buffer_mut().set_cursor(new_pos);
                        self.active_buffer_mut()
                            .set_selection(Selection::collapsed(new_pos));
                    }
                    Err(e) => self.status_message = Some(format!("delete error: {e}")),
                }
            }
            EditorEvent::DeleteLine => {
                self.delete_current_line();
            }
            EditorEvent::DuplicateLine => {
                self.duplicate_current_line();
            }
            EditorEvent::MoveLineUp => {
                self.move_current_line(-1);
            }
            EditorEvent::MoveLineDown => {
                self.move_current_line(1);
            }
            EditorEvent::ScrollLeft => {
                let new = self.active_doc().view.scroll_x_cols.saturating_sub(1);
                self.active_doc_mut().view.scroll_x_cols = new;
            }
            EditorEvent::ScrollRight => {
                let new = self.active_doc().view.scroll_x_cols.saturating_add(1);
                self.active_doc_mut().view.scroll_x_cols = new;
            }
            EditorEvent::FindOpen => {
                self.search.bar_open = true;
            }
            EditorEvent::FindClose => {
                self.search.bar_open = false;
                // Coupled with replace — closing find also closes replace.
                self.search.replace_bar_open = false;
            }
            EditorEvent::FindQueryChanged(q) => {
                self.search.query = q;
                self.search.refresh(&self.active_buffer().to_bytes());
                if let Some(pos) = self.search.current_match() {
                    self.active_buffer_mut().set_cursor(pos);
                    self.active_buffer_mut()
                        .set_selection(Selection::collapsed(pos));
                }
            }
            EditorEvent::FindNext => {
                if let Some(pos) = self.search.next_after(self.active_buffer().cursor()) {
                    self.active_buffer_mut().set_cursor(pos);
                    self.active_buffer_mut()
                        .set_selection(Selection::collapsed(pos));
                    // Auto-scroll on next render: the per-doc
                    // `last_seen_cursor` still holds the old value, so
                    // the render path detects motion and scrolls into
                    // view.
                }
            }
            EditorEvent::FindPrev => {
                if let Some(pos) = self.search.prev_before(self.active_buffer().cursor()) {
                    self.active_buffer_mut().set_cursor(pos);
                    self.active_buffer_mut()
                        .set_selection(Selection::collapsed(pos));
                    // See FindNext.
                }
            }
            EditorEvent::ToggleFindRegex => {
                self.search.regex_mode = !self.search.regex_mode;
                self.search.refresh(&self.active_buffer().to_bytes());
                if let Some(pos) = self.search.current_match() {
                    self.active_buffer_mut().set_cursor(pos);
                    self.active_buffer_mut()
                        .set_selection(Selection::collapsed(pos));
                }
            }
            EditorEvent::ReplaceOpen => {
                // Require the find bar to be open — no point editing the
                // replacement without an active search visible.
                if self.search.bar_open {
                    self.search.replace_bar_open = true;
                } else {
                    self.search.bar_open = true;
                    self.search.replace_bar_open = true;
                }
            }
            EditorEvent::ReplaceClose => {
                self.search.replace_bar_open = false;
            }
            EditorEvent::ReplaceQueryChanged(q) => {
                self.search.replace_query = q;
            }
            EditorEvent::ReplaceOne => {
                self.replace_one();
            }
            EditorEvent::ReplaceAll => {
                self.replace_all();
            }
            EditorEvent::SetIndentMode {
                use_spaces,
                tab_width,
            } => {
                self.set_indent_mode(use_spaces, tab_width);
                self.sync_config();
            }
            EditorEvent::CycleIndentMode => {
                self.cycle_indent_mode();
                self.sync_config();
            }
            EditorEvent::ToggleSoftWrap => {
                self.toggle_soft_wrap();
                self.sync_config();
            }
            EditorEvent::CycleTheme => {
                self.cycle_theme();
                self.sync_config();
            }
            EditorEvent::InsertTab => {
                // Indent insertion respects the active doc's indent
                // mode: spaces (count = tab_width) or a literal tab.
                // Reuses the same selection-aware path as Insert so
                // selecting some text and pressing Tab replaces it
                // with the indent — matches Sublime / VSCode / IntelliJ.
                let (use_spaces, tab_width) = {
                    let v = &self.active_doc().view;
                    (v.use_spaces, v.tab_width)
                };
                let text = if use_spaces {
                    " ".repeat(tab_width)
                } else {
                    "\t".to_string()
                };
                self.insert_text(&text);
            }
            EditorEvent::Move(movement) => {
                let new_pos = self.compute_target(movement);
                self.active_buffer_mut().set_cursor(new_pos);
                self.active_buffer_mut()
                    .set_selection(Selection::collapsed(new_pos));
            }
            EditorEvent::SelectExtend(movement) => {
                let new_pos = self.compute_target(movement);
                let anchor = self.active_buffer().selection().anchor;
                self.active_buffer_mut().set_cursor(new_pos);
                self.active_buffer_mut().set_selection(Selection {
                    anchor,
                    head: new_pos,
                });
            }
            EditorEvent::SetCursor { pos } => {
                let clamped = pos.min(self.active_buffer().len());
                self.active_buffer_mut().set_cursor(clamped);
                self.active_buffer_mut()
                    .set_selection(Selection::collapsed(clamped));
                // Mouse clicks should place the caret instantly — the
                // smooth animation is for arrow-key movement, not clicks.
                self.snap_caret_animation();
            }
            EditorEvent::SelectExtendTo { pos } => {
                let clamped = pos.min(self.active_buffer().len());
                let anchor = self.active_buffer().selection().anchor;
                self.active_buffer_mut().set_cursor(clamped);
                self.active_buffer_mut().set_selection(Selection {
                    anchor,
                    head: clamped,
                });
                // Mouse drag selections also snap the caret to the new
                // line instead of sliding it across the viewport.
                self.snap_caret_animation();
            }
            EditorEvent::Paste(text) => {
                // Selection-aware paste: replace the selection if any,
                // otherwise insert at cursor. Goes through the shared
                // insert_text path so error / cursor-update behaviour
                // matches Insert and InsertTab.
                self.insert_text(&text);
                if let Some(msg) = self.status_message.as_deref() {
                    if msg.contains("insert error") {
                        self.status_message = Some(msg.replace("insert error", "paste error"));
                    }
                }
            }
            EditorEvent::Save => match self.active_buffer_mut().save() {
                Ok(()) => {
                    self.status_message = Some("Saved.".to_string());
                    self.active_doc_mut().refresh_git_gutter();
                    self.sync_config();
                }
                Err(e) => self.status_message = Some(format!("Save error: {e}")),
            },
            EditorEvent::SaveAs(maybe_path) => match maybe_path {
                Some(path) => {
                    self.active_buffer_mut().set_source_path(path);
                    self.handle_event(EditorEvent::Save);
                    self.sync_watcher();
                }
                None => {
                    if let Some(path) = rfd::FileDialog::new().save_file() {
                        self.handle_event(EditorEvent::SaveAs(Some(path)));
                    }
                }
            },
            EditorEvent::ReloadFile => {
                if let Some(path) = self.active_doc().path().map(|p| p.to_path_buf()) {
                    self.reload_document_at_path(&path);
                } else {
                    self.status_message = Some("No file to reload.".to_string());
                }
            }
            EditorEvent::Undo => {
                if self.active_buffer_mut().undo() {
                    self.status_message = Some("Undid.".to_string());
                }
            }
            EditorEvent::Redo => {
                if self.active_buffer_mut().redo() {
                    self.status_message = Some("Redid.".to_string());
                }
            }
            EditorEvent::NewDoc => {
                self.documents.push(Document::new_with_config(
                    Box::new(core::PieceTableBuffer::new()),
                    &self.config,
                ));
                self.active = self.documents.len() - 1;
                self.status_message = Some("New document.".to_string());
                self.sync_config();
                self.sync_watcher();
            }
            EditorEvent::CloseDoc => {
                self.request_close_active();
                self.sync_config();
                self.sync_watcher();
            }
            EditorEvent::NextDoc => {
                if !self.documents.is_empty() {
                    self.active = (self.active + 1) % self.documents.len();
                }
            }
            EditorEvent::PrevDoc => {
                if !self.documents.is_empty() {
                    self.active = (self.active + self.documents.len() - 1) % self.documents.len();
                }
            }
            EditorEvent::OpenFile(maybe_path) => match maybe_path {
                Some(path) => {
                    self.open_path(&path);
                    self.sync_config();
                }
                None => {
                    if let Some(path) = rfd::FileDialog::new().pick_file() {
                        self.open_path(&path);
                        self.sync_config();
                    }
                }
            },
            EditorEvent::GoToLine(maybe_line) => match maybe_line {
                Some(line) => self.go_to_line(line),
                None => {
                    self.go_to_line_dialog = Some(GoToLineDialog {
                        query: String::new(),
                    })
                }
            },
            EditorEvent::ToggleProjectTree => {
                self.toggle_project_tree();
                self.sync_config();
            }
            EditorEvent::ProjectTreeMove { delta } => {
                self.move_project_tree_selection(delta);
            }
            EditorEvent::ProjectTreeOpen => {
                self.open_or_toggle_selected_project_tree_node();
            }
            EditorEvent::ProjectSearch(query) => {
                self.project_search.open = true;
                if let Some(q) = query {
                    self.project_search.query = q;
                }
                self.refresh_project_search();
            }
            EditorEvent::ProjectSearchQueryChanged(q) => {
                self.project_search.query = q;
                self.refresh_project_search();
            }
            EditorEvent::ProjectSearchMove { delta } => {
                self.move_project_search_selection(delta);
            }
            EditorEvent::ProjectSearchOpenResult => {
                self.open_selected_project_search_result();
            }
            EditorEvent::ProjectSearchClose => {
                self.project_search.open = false;
            }
            EditorEvent::ProjectSearchReplaceQueryChanged(q) => {
                self.project_search.replace_query = q;
                self.refresh_project_search();
            }
            EditorEvent::ProjectSearchToggleFocus => {
                // egui manages focus between TextEdit widgets itself;
                // this event just consumes the Tab key.
            }
            EditorEvent::ProjectSearchReplaceAll => {
                self.apply_project_replace_all();
            }
            EditorEvent::ProjectSearchReplaceAllConfirm => {
                self.confirm_project_replace_all(true);
            }
            EditorEvent::ProjectSearchReplaceAllCancel => {
                self.confirm_project_replace_all(false);
            }
            EditorEvent::FuzzyFinder(query) => {
                self.open_fuzzy_finder(query);
            }
            EditorEvent::FuzzyFinderQueryChanged(q) => {
                self.fuzzy_finder.query = q;
                self.refresh_fuzzy_finder();
            }
            EditorEvent::FuzzyFinderMove { delta } => {
                self.move_fuzzy_finder_selection(delta);
            }
            EditorEvent::FuzzyFinderExecute => {
                self.execute_fuzzy_finder();
            }
            EditorEvent::FuzzyFinderClose => {
                self.fuzzy_finder.open = false;
            }
            EditorEvent::ToggleKeybindingsHelp => {
                self.keybindings_help_open = !self.keybindings_help_open;
            }
            EditorEvent::ToggleGitGutter => {
                let enabled = !self.active_doc().view.git_gutter_enabled;
                self.active_doc_mut().view.git_gutter_enabled = enabled;
                self.status_message = Some(format!(
                    "Git gutter {}.",
                    if enabled { "enabled" } else { "disabled" }
                ));
                if enabled {
                    self.active_doc_mut().refresh_git_gutter();
                }
            }
            EditorEvent::RefreshGitGutter => {
                self.active_doc_mut().refresh_git_gutter();
            }
            EditorEvent::NextHunk => {
                self.jump_hunk(1);
            }
            EditorEvent::PrevHunk => {
                self.jump_hunk(-1);
            }
            EditorEvent::CommandPalette(query) => {
                self.open_command_palette(query);
            }
            EditorEvent::CommandPaletteQueryChanged(q) => {
                self.command_palette.query = q;
                self.refresh_command_palette();
            }
            EditorEvent::CommandPaletteMove { delta } => {
                self.move_command_palette_selection(delta);
            }
            EditorEvent::CommandPaletteExecute => {
                self.execute_selected_command();
            }
            EditorEvent::CommandPaletteClose => {
                self.command_palette.open = false;
            }
            EditorEvent::Quit => {
                self.sync_config();
                self.should_quit = true;
            }
        }

        if modifies_buffer {
            self.active_doc_mut().syntax.invalidate();
            self.active_doc_mut().git_gutter.mark_dirty();
            self.last_edit_time = Instant::now();
        }
    }

    /// Shared insertion path used by `Insert(char)`, `InsertTab`, and
    /// `Paste(String)`. Selection-aware: a non-collapsed selection
    /// is replaced by the inserted text. Centralised here so all
    /// three events share error / cursor-update behaviour.
    fn insert_text(&mut self, text: &str) {
        self.delete_selection_if_any();
        let pos = self.active_buffer().cursor();
        match self.active_buffer_mut().insert(pos, text) {
            Ok(new_pos) => {
                self.active_buffer_mut().set_cursor(new_pos);
                self.active_buffer_mut()
                    .set_selection(Selection::collapsed(new_pos));
                self.status_message = None;
            }
            Err(e) => self.status_message = Some(format!("insert error: {e}")),
        }
    }

    /// Set the indent mode for the active document. Indent mode
    /// controls what the Tab key inserts (spaces vs tab character)
    /// and how many spaces per indent level. Per-document so opening
    /// a file with different conventions doesn't fight the user's
    /// preferred mode. Mirrors `frontend_tui::App::set_indent_mode`.
    pub fn set_indent_mode(&mut self, use_spaces: bool, tab_width: usize) {
        let tab_width = tab_width.clamp(1, 16);
        self.active_doc_mut().view.use_spaces = use_spaces;
        self.active_doc_mut().view.tab_width = tab_width;
        let mode = if use_spaces {
            format!("spaces:{tab_width}")
        } else {
            format!("tabs (width {tab_width})")
        };
        self.status_message = Some(format!("Indent: {mode}"));
    }

    /// Cycle through the four common indent presets. See the TUI
    /// counterpart for the rationale and presets list.
    pub fn cycle_indent_mode(&mut self) {
        let v = &self.active_doc().view;
        let next = match (v.use_spaces, v.tab_width) {
            (true, 2) => (true, 4),
            (true, 4) => (true, 8),
            (true, 8) => (false, 4),
            (false, _) => (true, 2),
            _ => (true, 2),
        };
        self.set_indent_mode(next.0, next.1);
    }

    /// Toggle soft-wrap on the active document. The GUI frontend
    /// honours this in its renderer — long lines wrap on multiple
    /// visual rows. Mirrors `frontend_tui::App::toggle_soft_wrap`.
    pub fn toggle_soft_wrap(&mut self) {
        let new_value = !self.active_doc().view.soft_wrap;
        self.active_doc_mut().view.soft_wrap = new_value;
        self.status_message = Some(if new_value {
            "Soft-wrap: on".to_string()
        } else {
            "Soft-wrap: off (horizontal scroll)".to_string()
        });
    }

    /// Cycle to the next syntect theme and invalidate the syntax cache
    /// for every open document so the new colors appear immediately.
    /// Mirrors `frontend_tui::App::cycle_theme`.
    pub fn cycle_theme(&mut self) {
        let name = self.syntax.cycle_theme().to_string();
        for doc in &mut self.documents {
            doc.syntax.invalidate();
        }
        self.status_message = Some(format!("Theme: {name}"));
    }

    /// Reset the caret vertical animation so the next render snaps the
    /// caret to the cursor's actual line. Used for mouse clicks / drags
    /// where a sliding caret feels wrong.
    pub fn snap_caret_animation(&mut self) {
        self.caret_anim_y = f32::NAN;
    }

    /// If the selection is non-empty, delete it and collapse the cursor
    /// to the start of the deleted range. Returns `true` when a deletion
    /// actually happened.
    fn delete_selection_if_any(&mut self) -> bool {
        let sel = self.active_buffer().selection();
        if sel.is_collapsed() {
            return false;
        }
        let range = sel.range();
        match self.active_buffer_mut().delete(range) {
            Ok(new_pos) => {
                self.active_buffer_mut().set_cursor(new_pos);
                self.active_buffer_mut()
                    .set_selection(Selection::collapsed(new_pos));
                true
            }
            Err(e) => {
                self.status_message = Some(format!("delete error: {e}"));
                false
            }
        }
    }

    /// Replace the currently-active find match with the replace query,
    /// then advance to the next match. Mirrors
    /// `frontend_tui::App::replace_one`.
    pub fn replace_one(&mut self) {
        if self.search.query.is_empty() {
            self.status_message = Some("Replace: nothing to find.".to_string());
            return;
        }
        if self.search.replace_query.is_empty() {
            self.status_message =
                Some("Replace: replacement is empty — type something first.".to_string());
            return;
        }
        if self.search.regex_mode && self.search.regex_error.is_some() {
            self.status_message = Some("Replace: invalid regex.".to_string());
            return;
        }
        let text = match String::from_utf8(self.active_buffer().to_bytes()) {
            Ok(text) => text,
            Err(_) => {
                self.status_message = Some("Replace: buffer is not valid UTF-8.".to_string());
                return;
            }
        };
        let Some((pos, end, replacement)) = self.search.current_replacement(&text) else {
            self.status_message = Some("Replace: no current match.".to_string());
            return;
        };
        if let Err(e) = self.active_buffer_mut().replace(pos..end, &replacement) {
            self.status_message = Some(format!("Replace error: {e}"));
            return;
        }
        self.search.refresh(&self.active_buffer().to_bytes());
        self.active_buffer_mut().set_cursor(pos);
        self.active_buffer_mut()
            .set_selection(Selection::collapsed(pos));
        if let Some(next) = self.search.next_after(pos) {
            self.active_buffer_mut().set_cursor(next);
            self.active_buffer_mut()
                .set_selection(Selection::collapsed(next));
            self.status_message = Some(format!(
                "Replaced 1; advanced to match {}/{}.",
                self.search.current.unwrap_or(0) + 1,
                self.search.matches.len()
            ));
        } else {
            self.status_message = Some("Replaced 1; no more matches.".to_string());
        }
    }

    /// Replace every find match with the replace query, as a single
    /// undo entry. Mirrors `frontend_tui::App::replace_all`.
    pub fn replace_all(&mut self) {
        if self.search.query.is_empty() {
            self.status_message = Some("Replace all: nothing to find.".to_string());
            return;
        }
        if self.search.replace_query.is_empty() {
            self.status_message =
                Some("Replace all: replacement is empty — type something first.".to_string());
            return;
        }
        if self.search.regex_mode && self.search.regex_error.is_some() {
            self.status_message = Some("Replace all: invalid regex.".to_string());
            return;
        }
        if self.search.matches.is_empty() {
            self.status_message = Some("Replace all: no matches.".to_string());
            return;
        }
        let count = self.search.matches.len();
        let text = match String::from_utf8(self.active_buffer().to_bytes()) {
            Ok(text) => text,
            Err(_) => {
                self.status_message = Some("Replace all: buffer is not valid UTF-8.".to_string());
                return;
            }
        };
        let Some(new_text) = self.search.replace_all_text(&text) else {
            self.status_message = Some("Replace all: invalid regex.".to_string());
            return;
        };
        let len = self.active_buffer().len();
        self.active_buffer_mut().begin_edit_group();
        let result = self.active_buffer_mut().replace(0..len, &new_text);
        self.active_buffer_mut().end_edit_group();
        if let Err(e) = result {
            self.status_message = Some(format!("Replace error: {e}"));
            return;
        }
        self.search.refresh(&self.active_buffer().to_bytes());
        self.active_buffer_mut().set_cursor(0);
        self.active_buffer_mut()
            .set_selection(Selection::collapsed(0));
        self.status_message = Some(format!("Replaced {count} occurrences."));
    }

    /// Compute the byte position a movement should land on. Identical
    /// to `frontend_tui::App::compute_target`.
    fn compute_target(&self, movement: Movement) -> usize {
        let pos = self.active_buffer().cursor();
        let len = self.active_buffer().len();
        match movement {
            Movement::Left => core::move_left_by_char(self.active_buffer(), pos),
            Movement::Right => core::move_right_by_char(self.active_buffer(), pos),
            Movement::Up => {
                let (line, col) = core::cursor_char_linecol(self.active_buffer(), pos);
                if line == 0 {
                    0
                } else {
                    core::clamped_line_charcol_to_pos(self.active_buffer(), line - 1, col)
                }
            }
            Movement::Down => {
                let (line, col) = core::cursor_char_linecol(self.active_buffer(), pos);
                if line + 1 >= self.active_buffer().line_count() {
                    pos
                } else {
                    core::clamped_line_charcol_to_pos(self.active_buffer(), line + 1, col)
                }
            }
            Movement::PageUp => {
                let (line, col) = core::cursor_char_linecol(self.active_buffer(), pos);
                let page = self.viewport_lines.max(1);
                let target = line.saturating_sub(page);
                core::clamped_line_charcol_to_pos(self.active_buffer(), target, col)
            }
            Movement::PageDown => {
                let (line, col) = core::cursor_char_linecol(self.active_buffer(), pos);
                let page = self.viewport_lines.max(1);
                let last = self.active_buffer().line_count().saturating_sub(1);
                let target = (line + page).min(last);
                core::clamped_line_charcol_to_pos(self.active_buffer(), target, col)
            }
            Movement::WordLeft => skip_word_left(self.active_buffer(), pos),
            Movement::WordRight => skip_word_right(self.active_buffer(), pos),
            Movement::LineStart => {
                let (line, _) = self.active_buffer().pos_to_linecol(pos).unwrap_or((0, 0));
                self.active_buffer().linecol_to_pos(line, 0).unwrap_or(0)
            }
            Movement::LineEnd => {
                let (line, _) = self.active_buffer().pos_to_linecol(pos).unwrap_or((0, 0));
                self.active_buffer()
                    .line_byte_range(line)
                    .map(|r| r.end)
                    .unwrap_or(len)
            }
            Movement::DocumentStart => 0,
            Movement::DocumentEnd => len,
        }
    }

    pub fn is_dirty(&self) -> bool {
        self.active_buffer().is_dirty()
    }

    /// Delete the entire line under the cursor, including its trailing
    /// newline (so the next line collapses up).
    fn delete_current_line(&mut self) {
        let cursor_pos = self.active_buffer().cursor();
        let (line, _) = self
            .active_buffer()
            .pos_to_linecol(cursor_pos)
            .unwrap_or((0, 0));
        let Some(line_range) = self.active_buffer().line_byte_range(line) else {
            return;
        };
        let line_count = self.active_buffer().line_count();
        if line + 1 < line_count {
            let next_line_start = self
                .active_buffer()
                .line_byte_range(line + 1)
                .map(|r| r.start)
                .unwrap_or(line_range.end);
            match self
                .active_buffer_mut()
                .delete(line_range.start..next_line_start)
            {
                Ok(np) => {
                    self.active_buffer_mut().set_cursor(np);
                    self.active_buffer_mut()
                        .set_selection(Selection::collapsed(np));
                }
                Err(e) => self.status_message = Some(format!("delete error: {e}")),
            }
        } else if line > 0 {
            let prev_end = self
                .active_buffer()
                .line_byte_range(line - 1)
                .map(|r| r.end)
                .unwrap_or(line_range.start);
            match self.active_buffer_mut().delete(prev_end..line_range.end) {
                Ok(np) => {
                    self.active_buffer_mut().set_cursor(np);
                    self.active_buffer_mut()
                        .set_selection(Selection::collapsed(np));
                }
                Err(e) => self.status_message = Some(format!("delete error: {e}")),
            }
        } else if let Err(e) = self.active_buffer_mut().delete(0..line_range.end) {
            self.status_message = Some(format!("delete error: {e}"));
        } else {
            self.active_buffer_mut().set_cursor(0);
            self.active_buffer_mut()
                .set_selection(Selection::collapsed(0));
        }
        let _ = line_count;
    }

    fn duplicate_current_line(&mut self) {
        let cursor_pos = self.active_buffer().cursor();
        let (line, _) = self
            .active_buffer()
            .pos_to_linecol(cursor_pos)
            .unwrap_or((0, 0));
        let Some(line_range) = self.active_buffer().line_byte_range(line) else {
            return;
        };
        let line_count = self.active_buffer().line_count();
        let line_text = self
            .active_buffer()
            .slice(line_range.clone())
            .unwrap_or_default();
        let line_ends_with_newline = line_text.ends_with('\n');
        if line + 1 < line_count {
            let insert_pos = self
                .active_buffer()
                .line_byte_range(line + 1)
                .map(|r| r.start)
                .unwrap_or(line_range.end);
            let to_insert = if line_ends_with_newline {
                line_text.clone()
            } else {
                format!("{line_text}\n")
            };
            match self.active_buffer_mut().insert(insert_pos, &to_insert) {
                Ok(np) => {
                    let new_line_start =
                        np - to_insert.len() + if line_ends_with_newline { 1 } else { 0 };
                    self.active_buffer_mut().set_cursor(new_line_start);
                    self.active_buffer_mut()
                        .set_selection(Selection::collapsed(new_line_start));
                }
                Err(e) => self.status_message = Some(format!("insert error: {e}")),
            }
        } else {
            let len = self.active_buffer().len();
            let to_insert = if line_ends_with_newline {
                line_text.clone()
            } else {
                format!("\n{line_text}")
            };
            match self.active_buffer_mut().insert(len, &to_insert) {
                Ok(np) => {
                    let new_line_start = if line_ends_with_newline {
                        np - line_text.len()
                    } else {
                        np + 1
                    };
                    self.active_buffer_mut().set_cursor(new_line_start);
                    self.active_buffer_mut()
                        .set_selection(Selection::collapsed(new_line_start));
                }
                Err(e) => self.status_message = Some(format!("insert error: {e}")),
            }
        }
        let _ = line_count;
    }

    fn move_current_line(&mut self, delta: i32) {
        let cursor_pos = self.active_buffer().cursor();
        let (line, _) = self
            .active_buffer()
            .pos_to_linecol(cursor_pos)
            .unwrap_or((0, 0));
        let line_count = self.active_buffer().line_count();
        let target_line = if delta < 0 {
            if line == 0 {
                return;
            }
            line - 1
        } else {
            if line + 1 >= line_count {
                return;
            }
            line + 1
        };
        let Some(my_range_excl_nl) = self.active_buffer().line_byte_range(line) else {
            return;
        };
        let Some(other_range_excl_nl) = self.active_buffer().line_byte_range(target_line) else {
            return;
        };
        // Group the delete + two inserts so a single undo reverts the
        // whole line swap.
        self.active_buffer_mut().begin_edit_group();
        // Compute byte ranges that INCLUDE the trailing newline so the
        // swap preserves line structure.
        let line_with_nl = |excl: std::ops::Range<usize>, l: usize| -> std::ops::Range<usize> {
            let start = excl.start;
            let end = if l + 1 < line_count {
                self.active_buffer()
                    .line_byte_range(l + 1)
                    .map(|r| r.start)
                    .unwrap_or(excl.end)
            } else {
                self.active_buffer().len()
            };
            start..end
        };
        let my_range = line_with_nl(my_range_excl_nl, line);
        let other_range = line_with_nl(other_range_excl_nl, target_line);
        let my_text = self
            .active_buffer()
            .slice(my_range.clone())
            .unwrap_or_default();
        let other_text = self
            .active_buffer()
            .slice(other_range.clone())
            .unwrap_or_default();
        // Adjacent lines — delete their union in one shot (the
        // newline between them is part of one of the ranges, so no
        // content between them is lost). Then reinsert in swapped
        // order at delete_start. The text that ends up at the LOWER
        // position goes at delete_start first; the higher-position
        // text goes after it.
        let delete_start = my_range.start.min(other_range.start);
        let delete_end = my_range.end.max(other_range.end);
        let _ = self
            .active_buffer_mut()
            .delete_silent(delete_start..delete_end);
        let (lower_text, higher_text) = if delta < 0 {
            // Move up: my line lands at the lower slot.
            (my_text.as_str(), other_text.as_str())
        } else {
            // Move down: other line stays at the lower slot.
            (other_text.as_str(), my_text.as_str())
        };
        let _ = self
            .active_buffer_mut()
            .insert_silent(delete_start, lower_text);
        let _ = self
            .active_buffer_mut()
            .insert_silent(delete_start + lower_text.len(), higher_text);
        self.active_buffer_mut().end_edit_group();
        // After the swap the moved line sits at `target_line`, so put
        // the cursor at the start of that line.
        let new_pos = self
            .active_buffer()
            .linecol_to_pos(target_line, 0)
            .unwrap_or(cursor_pos);
        self.active_buffer_mut().set_cursor(new_pos);
        self.active_buffer_mut()
            .set_selection(Selection::collapsed(new_pos));
        let _ = line_count;
    }

    /// Helper: ask the OS to close the window. eframe 0.30 has no
    /// `Frame::close()`; we send a viewport command instead.
    pub fn request_close(&self, ctx: &Context) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    /// Begin closing the active document. If the buffer has unsaved
    /// edits, open the close-confirm prompt instead of closing —
    /// actual close happens once the user picks Save / Discard /
    /// Cancel via [`EditorApp::cycle_close_choice`] and
    /// [`EditorApp::confirm_close_choice`]. If the buffer is clean
    /// (or it is the only document), close immediately.
    pub fn request_close_active(&mut self) {
        self.request_close_doc(self.active);
    }

    /// Begin closing a document by index. Mirrors
    /// [`request_close_active`] but works for any open tab.
    pub fn request_close_doc(&mut self, idx: usize) {
        if idx >= self.documents.len() {
            return;
        }
        if self.documents[idx].is_dirty() {
            self.active = idx;
            self.go_to_line_dialog = None;
            self.close_confirm = Some(CloseConfirm {
                doc_index: idx,
                choice: CloseChoice::Save,
            });
            self.status_message = None;
            return;
        }
        let old_active = self.active;
        self.active = idx;
        self.perform_close_active();
        // Restore the active index if we closed an inactive tab, but
        // keep the new index if it was already the last document.
        if old_active != idx && self.documents.len() > 1 {
            if old_active > idx {
                self.active = old_active - 1;
            } else {
                self.active = old_active;
            }
        }
    }

    /// Cycle the focused choice on the close-confirm prompt. `delta`
    /// moves forward (+1) or backward (-1) through Save → Discard →
    /// Cancel. No-op when no prompt is up.
    pub fn cycle_close_choice(&mut self, delta: i32) {
        let Some(confirm) = self.close_confirm.as_mut() else {
            return;
        };
        confirm.choice = match (confirm.choice, delta) {
            (CloseChoice::Save, d) if d > 0 => CloseChoice::Discard,
            (CloseChoice::Discard, d) if d > 0 => CloseChoice::Cancel,
            (CloseChoice::Cancel, d) if d > 0 => CloseChoice::Save,
            (CloseChoice::Save, _) => CloseChoice::Cancel,
            (CloseChoice::Discard, _) => CloseChoice::Save,
            (CloseChoice::Cancel, _) => CloseChoice::Discard,
        };
    }

    /// Activate the currently-focused choice on the close-confirm
    /// prompt. Save saves then closes (failure drops the prompt and
    /// surfaces the error). Discard closes. Cancel drops the prompt.
    pub fn confirm_close_choice(&mut self) {
        let Some(confirm) = self.close_confirm.take() else {
            return;
        };
        match confirm.choice {
            CloseChoice::Save => match self.active_buffer_mut().save() {
                Ok(()) => {
                    self.status_message = Some("Saved.".to_string());
                    self.perform_close_active();
                }
                Err(e) => {
                    self.status_message = Some(format!("Save error: {e}"));
                }
            },
            CloseChoice::Discard => {
                self.perform_close_active();
            }
            CloseChoice::Cancel => {
                self.status_message = Some("Close cancelled.".to_string());
            }
        }
    }

    /// The "perform" half of a close. Pulled out so the prompt
    /// handlers and the UI module (button callbacks) share the same
    /// code path. Public within the crate so `ui.rs` can call it after
    /// the user clicks a button.
    pub fn perform_close_active(&mut self) {
        if self.documents.len() == 1 {
            self.should_quit = true;
            self.status_message = Some("Closed last document — quitting.".to_string());
            return;
        }
        self.documents.remove(self.active);
        if self.active >= self.documents.len() {
            self.active = self.documents.len() - 1;
        }
        self.status_message = Some("Closed document.".to_string());
    }

    /// Load `path` into the active document. Existing files replace
    /// the buffer; non-existent paths become empty buffers with the
    /// path remembered for the next Save. Errors land in the status
    /// bar and leave the buffer untouched.
    pub fn open_path(&mut self, path: &std::path::Path) {
        use core::PieceTableBuffer;
        let buffer: Box<dyn Buffer> = if path.exists() {
            match PieceTableBuffer::from_path(path.to_path_buf()) {
                Ok(buf) => Box::new(buf),
                Err(e) => {
                    self.status_message = Some(format!("Open error: {e}"));
                    return;
                }
            }
        } else {
            Box::new(PieceTableBuffer::from_bytes_with_path(
                Vec::new(),
                path.to_path_buf(),
            ))
        };
        self.documents[self.active] = Document::new_with_config(buffer, &self.config);
        self.status_message = Some(format!(
            "Opened {}",
            path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("<path>")
        ));
        self.sync_watcher();
    }

    /// Open `path` in a document, switching to an existing document
    /// if the path is already open. Used by the project tree so
    /// clicking an already-open file just focuses it.
    pub fn open_or_switch_to_path(&mut self, path: &std::path::Path) {
        if let Some(idx) = self.documents.iter().position(|d| d.path() == Some(path)) {
            self.active = idx;
            self.status_message = Some(format!(
                "Switched to {}",
                path.file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("<path>")
            ));
            return;
        }
        // Open as a new document at the end of the list.
        self.documents.push(Document::new_with_config(
            Box::new(core::PieceTableBuffer::new()),
            &self.config,
        ));
        self.active = self.documents.len() - 1;
        self.open_path(path);
        self.sync_config();
        self.sync_watcher();
    }

    /// Flatten the project tree into visible `(depth, node)` rows
    /// based on the current expansion state.
    pub fn project_tree_rows(&self) -> Vec<(usize, &core::FsNode)> {
        self.project_tree
            .as_ref()
            .map(|t| t.visible_rows())
            .unwrap_or_default()
    }

    /// Refresh the project tree from the active document's project.
    /// Called when the tree is toggled or the active doc changes.
    pub fn refresh_project_tree(&mut self) {
        if let Some(project) = self.active_doc().project.clone() {
            self.project_tree = project.tree(10_000).map(core::ProjectTree::new);
            self.project_tree_selected = self
                .project_tree_selected
                .min(self.project_tree_rows().len().saturating_sub(1));
        } else {
            self.project_tree = None;
            self.project_tree_selected = 0;
        }
    }

    /// Toggle the project-tree sidebar and refresh its contents.
    pub fn toggle_project_tree(&mut self) {
        self.project_tree_open = !self.project_tree_open;
        if self.project_tree_open {
            self.refresh_project_tree();
            self.project_tree_focused = true;
        } else {
            self.project_tree_focused = false;
        }
    }

    /// Move the project-tree selection by `delta` rows, wrapping at
    /// the ends.
    pub fn move_project_tree_selection(&mut self, delta: isize) {
        let len = self.project_tree_rows().len();
        if len == 0 {
            return;
        }
        let current = self.project_tree_selected as isize;
        let next = (current + delta).rem_euclid(len as isize);
        self.project_tree_selected = next as usize;
    }

    /// Toggle expansion of the directory at the selected tree row.
    /// If the selected row is a file, open it instead.
    pub fn open_or_toggle_selected_project_tree_node(&mut self) {
        let rows = self.project_tree_rows();
        let Some((_, node)) = rows.get(self.project_tree_selected).copied() else {
            return;
        };
        let rel_path = node.rel_path().to_path_buf();
        if node.is_dir() {
            if let Some(tree) = self.project_tree.as_mut() {
                tree.toggle(&rel_path);
            }
        } else if let Some(project) = self.active_doc().project.clone() {
            let path = project.root.join(rel_path);
            self.open_or_switch_to_path(&path);
        }
    }

    /// Re-run the project search and replace preview with the current
    /// queries. Reset the replace-all confirmation flag because the
    /// preview changed.
    pub fn refresh_project_search(&mut self) {
        self.project_search.confirm_replace = false;
        if let Some(project) = self.active_doc().project.clone() {
            self.project_search.results = project.search(&self.project_search.query, 1_000);
            self.project_search.replace_previews = project.replace_preview(
                &self.project_search.query,
                &self.project_search.replace_query,
                1_000,
            );
            self.project_search.selected = self
                .project_search
                .selected
                .min(self.project_search.results.len().saturating_sub(1));
        } else {
            self.project_search.results.clear();
            self.project_search.replace_previews.clear();
            self.project_search.selected = 0;
        }
    }

    /// Start the project-wide replace flow. Guards against empty queries
    /// and, when the preview is non-empty, flips to the confirmation
    /// prompt instead of replacing immediately.
    pub fn apply_project_replace_all(&mut self) {
        if self.project_search.query.is_empty() {
            self.status_message = Some("Project replace: nothing to find.".to_string());
            return;
        }
        if self.project_search.replace_query.is_empty() {
            self.status_message =
                Some("Project replace: replacement is empty — type something first.".to_string());
            return;
        }
        if self.project_search.replace_previews.is_empty() {
            self.status_message = Some("No occurrences to replace.".to_string());
            return;
        }
        self.project_search.confirm_replace = true;
    }

    /// Confirm or cancel the project-wide replace-all after the user has
    /// reviewed the preview. When confirmed, write files, then reload any
    /// open documents that were modified (dirty ones are marked stale
    /// instead).
    pub fn confirm_project_replace_all(&mut self, confirmed: bool) {
        self.project_search.confirm_replace = false;
        if !confirmed {
            return;
        }
        let Some(project) = self.active_doc().project.clone() else {
            self.status_message = Some("No project detected.".to_string());
            return;
        };
        match project.replace_all(
            &self.project_search.query,
            &self.project_search.replace_query,
        ) {
            Ok(report) if report.total == 0 => {
                self.status_message = Some("No occurrences replaced.".to_string());
            }
            Ok(report) => {
                let mut stale = 0usize;
                for path in &report.changed_files {
                    if let Some(idx) = self
                        .documents
                        .iter()
                        .position(|d| d.path().map(|p| p == path).unwrap_or(false))
                    {
                        if self.documents[idx].is_dirty() {
                            self.documents[idx].external_change = true;
                            stale += 1;
                        } else {
                            self.reload_document_at_path(path);
                        }
                    }
                }
                let mut msg = format!(
                    "Replaced {} occurrences in {} files.",
                    report.total,
                    report.changed_files.len()
                );
                if stale > 0 {
                    msg.push_str(&format!(
                        " {stale} open file(s) had unsaved changes and were marked stale."
                    ));
                }
                self.status_message = Some(msg);
                self.project_search.open = false;
            }
            Err(e) => {
                self.status_message = Some(format!(
                    "Replace error in {}: {}",
                    e.rel_path.to_string_lossy(),
                    e.message
                ));
            }
        }
    }

    /// Open the command palette with an optional initial query.
    pub fn open_command_palette(&mut self, query: Option<String>) {
        self.command_palette.open = true;
        if let Some(q) = query {
            self.command_palette.query = q;
        }
        self.refresh_command_palette();
    }

    /// Re-filter the command palette list from the current query.
    pub fn refresh_command_palette(&mut self) {
        self.command_palette.items = core::filter_commands(&self.command_palette.query);
        self.command_palette.selected = self
            .command_palette
            .selected
            .min(self.command_palette.items.len().saturating_sub(1));
    }

    /// Move the command-palette selection by `delta` rows, wrapping at
    /// the ends.
    pub fn move_command_palette_selection(&mut self, delta: isize) {
        let len = self.command_palette.items.len();
        if len == 0 {
            return;
        }
        let current = self.command_palette.selected as isize;
        let next = (current + delta).rem_euclid(len as isize);
        self.command_palette.selected = next as usize;
    }

    /// Execute the selected command and close the palette.
    pub fn execute_selected_command(&mut self) {
        let Some(command) = self
            .command_palette
            .items
            .get(self.command_palette.selected)
        else {
            return;
        };
        let event = command.event.clone();
        self.command_palette.open = false;
        self.handle_event(event);
    }

    // ----- fuzzy file finder -----

    /// Open the fuzzy file finder. If `query` is `Some`, seed the query
    /// string with it. The candidate list is built from project files
    /// plus recently-opened files.
    pub fn open_fuzzy_finder(&mut self, query: Option<String>) {
        self.fuzzy_finder.open = true;
        self.fuzzy_finder.query = query.unwrap_or_default();
        self.fuzzy_finder.items.clear();

        let mut seen = std::collections::HashSet::new();

        if let Some(project) = self.active_doc().project.clone() {
            for rel_path in project.all_files(10_000) {
                let display = rel_path.to_string_lossy().to_string();
                let abs_path = project.root.join(&rel_path);
                seen.insert(abs_path.clone());
                self.fuzzy_finder.items.push(FuzzyCandidate {
                    display,
                    path: abs_path,
                });
            }
        }

        for recent in &self.config.recent_files {
            if seen.insert(recent.clone()) {
                self.fuzzy_finder.items.push(FuzzyCandidate {
                    display: recent.to_string_lossy().to_string(),
                    path: recent.clone(),
                });
            }
        }

        self.refresh_fuzzy_finder();
    }

    /// Re-filter and re-rank candidates from the current query.
    pub fn refresh_fuzzy_finder(&mut self) {
        let candidates: Vec<String> = self
            .fuzzy_finder
            .items
            .iter()
            .map(|c| c.display.clone())
            .collect();
        self.fuzzy_finder.filtered = core::filter_and_rank(&self.fuzzy_finder.query, &candidates);
        self.fuzzy_finder.selected = self
            .fuzzy_finder
            .selected
            .min(self.fuzzy_finder.filtered.len().saturating_sub(1));
    }

    /// Move the fuzzy finder selection by `delta` rows, wrapping at the
    /// ends of the filtered list.
    pub fn move_fuzzy_finder_selection(&mut self, delta: isize) {
        let len = self.fuzzy_finder.filtered.len();
        if len == 0 {
            return;
        }
        let current = self.fuzzy_finder.selected as isize;
        let next = (current + delta).rem_euclid(len as isize);
        self.fuzzy_finder.selected = next as usize;
    }

    /// Open the selected fuzzy-finder candidate and close the finder.
    pub fn execute_fuzzy_finder(&mut self) {
        let Some((idx, _)) = self.fuzzy_finder.filtered.get(self.fuzzy_finder.selected) else {
            return;
        };
        let path = self.fuzzy_finder.items[*idx].path.clone();
        self.fuzzy_finder.open = false;
        self.open_or_switch_to_path(&path);
    }

    /// Move the project-search selection by `delta` rows, wrapping at
    /// the ends.
    pub fn move_project_search_selection(&mut self, delta: isize) {
        let len = self.project_search.results.len();
        if len == 0 {
            return;
        }
        let current = self.project_search.selected as isize;
        let next = (current + delta).rem_euclid(len as isize);
        self.project_search.selected = next as usize;
    }

    /// Open the selected project-search result, switching to the file if
    /// already open and jumping the cursor to the match.
    pub fn open_selected_project_search_result(&mut self) {
        let Some(result) = self
            .project_search
            .results
            .get(self.project_search.selected)
            .cloned()
        else {
            return;
        };
        let Some(project) = self.active_doc().project.clone() else {
            return;
        };
        let path = project.root.join(&result.rel_path);
        self.open_or_switch_to_path(&path);
        self.go_to_line(result.line);
        let line = result.line.saturating_sub(1);
        let col = result.col.saturating_sub(1);
        if let Some(pos) = self.active_buffer().linecol_to_pos(line, col) {
            self.active_buffer_mut().set_cursor(pos);
            self.active_buffer_mut()
                .set_selection(Selection::collapsed(pos));
        }
        self.project_search.open = false;
    }

    /// Write the current session state (open files, theme, sidebar state,
    /// per-document defaults, window geometry) back to the config file.
    pub fn sync_config(&mut self) {
        self.config.theme = Some(self.syntax.theme_name().to_string());
        self.config.project_tree_open = Some(self.project_tree_open);
        self.config.recent_files = self
            .documents
            .iter()
            .filter_map(|d| d.path().map(|p| p.to_path_buf()))
            .collect();
        if let Some(doc) = self.documents.get(self.active) {
            self.config.capture_document_defaults(&doc.view);
        }
        self.config.save();
    }

    /// Update the file watcher to watch the source paths of all open
    /// documents.
    pub fn sync_watcher(&mut self) {
        let paths: Vec<std::path::PathBuf> = self
            .documents
            .iter()
            .filter_map(|d| d.path().map(|p| p.to_path_buf()))
            .collect();
        if let Some(watcher) = self.file_watcher.as_mut() {
            watcher.sync_watch_list(&paths);
        }
    }

    /// Poll the file watcher and process any external changes. Clean
    /// buffers are reloaded automatically; dirty buffers are flagged so
    /// the user can decide whether to reload.
    pub fn handle_external_changes(&mut self) {
        let changes: Vec<core::FileChange> = self
            .file_watcher
            .as_ref()
            .map(|w| w.poll_changes())
            .unwrap_or_default();
        for change in changes {
            for doc in &mut self.documents {
                if doc.path() == Some(change.path.as_path()) {
                    if doc.buffer.is_dirty() {
                        doc.external_change = true;
                        self.status_message = Some(format!(
                            "{} changed externally. Reload with Ctrl+Shift+R.",
                            change
                                .path
                                .file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or("<file>")
                        ));
                    } else {
                        self.reload_document_at_path(&change.path);
                    }
                    break;
                }
            }
        }
    }

    /// Reload the document whose path matches `path` from disk. Resets
    /// the `external_change` flag and view state.
    pub fn reload_document_at_path(&mut self, path: &std::path::Path) {
        use core::PieceTableBuffer;
        if let Some(idx) = self.documents.iter().position(|d| d.path() == Some(path)) {
            let buffer: Box<dyn Buffer> = if path.exists() {
                match PieceTableBuffer::from_path(path.to_path_buf()) {
                    Ok(buf) => Box::new(buf),
                    Err(e) => {
                        self.status_message = Some(format!("Reload error: {e}"));
                        return;
                    }
                }
            } else {
                Box::new(PieceTableBuffer::from_bytes_with_path(
                    Vec::new(),
                    path.to_path_buf(),
                ))
            };
            self.documents[idx] = Document::new_with_config(buffer, &self.config);
            self.status_message = Some(format!(
                "Reloaded {}.",
                path.file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("<file>")
            ));
        }
    }

    /// Save every dirty buffer that has a source path. Used by focus-loss
    /// auto-save and by the explicit auto-save idle check.
    pub fn save_all_dirty(&mut self) {
        let mut saved_any = false;
        for doc in &mut self.documents {
            if doc.buffer.source_path().is_none() || !doc.buffer.is_dirty() {
                continue;
            }
            match doc.buffer.save() {
                Ok(()) => saved_any = true,
                Err(e) => {
                    self.status_message = Some(format!("Auto-save error: {e}"));
                    return;
                }
            }
        }
        if saved_any {
            self.status_message = Some("Auto-saved.".to_string());
        }
    }

    /// Auto-save any dirty buffers that have a source path and have been
    /// idle for longer than `config.auto_save_delay_ms`. Called each
    /// frame from the GUI update loop.
    pub fn auto_save(&mut self) {
        if !self.config.auto_save {
            return;
        }
        let delay = std::time::Duration::from_millis(self.config.auto_save_delay_ms);
        if self.last_edit_time.elapsed() < delay {
            return;
        }
        self.save_all_dirty();
    }

    /// Refresh the active document's git gutter if it is enabled, stale,
    /// and the user has been idle for a short moment.
    pub fn maybe_refresh_git_gutter(&mut self) {
        if !self.active_doc().view.git_gutter_enabled {
            return;
        }
        if !self.active_doc().git_gutter.dirty() {
            return;
        }
        const DELAY: std::time::Duration = std::time::Duration::from_millis(500);
        if self.last_edit_time.elapsed() < DELAY {
            return;
        }
        self.active_doc_mut().refresh_git_gutter();
    }

    /// Jump the cursor to the start of the given 1-based line.
    /// Out-of-range values are clamped to the document bounds.
    pub fn go_to_line(&mut self, line: usize) {
        if line == 0 {
            self.status_message = Some("Line numbers start at 1.".to_string());
            return;
        }
        let line_idx = line.saturating_sub(1);
        let total_lines = self.active_buffer().line_count();
        let target_idx = line_idx.min(total_lines.saturating_sub(1));
        let target_pos = self
            .active_buffer()
            .line_byte_range(target_idx)
            .map(|r| r.start)
            .unwrap_or_else(|| self.active_buffer().len());
        self.active_buffer_mut().set_cursor(target_pos);
        self.active_buffer_mut()
            .set_selection(Selection::collapsed(target_pos));
        self.snap_caret_animation();
        self.status_message = Some(format!("Go to line {line}"));
    }

    /// Jump the cursor to the next or previous git hunk. `delta` should
    /// be `1` for next or `-1` for previous.
    pub fn jump_hunk(&mut self, delta: isize) {
        let doc = self.active_doc();
        if !doc.view.git_gutter_enabled || !doc.git_gutter.enabled() {
            self.status_message = Some("Git gutter not available.".to_string());
            return;
        }
        let hunks = doc.git_gutter.hunks().to_vec();
        if hunks.is_empty() {
            self.status_message = Some("No git hunks.".to_string());
            return;
        }
        let (_, cursor_line) = self
            .active_buffer()
            .pos_to_linecol(self.active_buffer().cursor())
            .unwrap_or((0, 0));
        let target = if delta > 0 {
            hunks
                .iter()
                .find(|h| h.start_line > cursor_line)
                .or_else(|| hunks.first())
        } else {
            hunks
                .iter()
                .rev()
                .find(|h| h.start_line < cursor_line)
                .or_else(|| hunks.last())
        };
        if let Some(hunk) = target {
            self.go_to_line(hunk.start_line + 1);
            let direction = if delta > 0 { "Next" } else { "Previous" };
            self.status_message = Some(format!("{} hunk.", direction));
        }
    }

    /// Append a character to the go-to-line dialog's text input.
    /// No-op when the dialog isn't open.
    pub fn push_go_to_line_query(&mut self, ch: char) {
        if ch.is_ascii_digit() && !ch.is_whitespace() {
            if let Some(d) = self.go_to_line_dialog.as_mut() {
                d.query.push(ch);
            }
        }
    }

    /// Remove the last character from the go-to-line dialog's text
    /// input. No-op when the dialog isn't open or the query is empty.
    pub fn pop_go_to_line_query(&mut self) {
        if let Some(d) = self.go_to_line_dialog.as_mut() {
            d.query.pop();
        }
    }

    /// Cancel the go-to-line dialog. No-op when it isn't open.
    pub fn cancel_go_to_line_dialog(&mut self) {
        if self.go_to_line_dialog.take().is_some() {
            self.status_message = Some("Go-to-line cancelled.".to_string());
        }
    }

    /// Submit the go-to-line dialog's current query as a line number.
    /// Empty or invalid input cancels the dialog.
    pub fn submit_go_to_line_dialog(&mut self) {
        let Some(dialog) = self.go_to_line_dialog.take() else {
            return;
        };
        if dialog.query.is_empty() {
            self.status_message = Some("Go-to-line cancelled.".to_string());
            return;
        }
        match dialog.query.parse::<usize>() {
            Ok(line) => self.go_to_line(line),
            Err(_) => {
                self.status_message =
                    Some(format!("'{}' is not a valid line number.", dialog.query));
            }
        }
    }
}

impl App for EditorApp {
    fn update(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        // Apply the active GUI chrome theme before any widgets are drawn.
        self.theme.apply(ctx);

        // 1. Pull input events from egui and apply them to the buffer.
        self.handle_input(ctx);

        // 2. Refresh the project tree when switching documents.
        if self.project_tree_open && self.active != self.prev_active {
            self.refresh_project_tree();
        }

        // 3. Auto-save dirty buffers when idle, and immediately on focus
        // loss if configured.
        let focused = ctx.input(|i| i.focused);
        if self.window_focused && !focused && self.config.auto_save_on_focus_change {
            self.save_all_dirty();
        }
        self.window_focused = focused;
        self.auto_save();

        // 4. Check for files that changed externally.
        self.handle_external_changes();

        // 5. Refresh the git gutter after the user has been idle briefly.
        self.maybe_refresh_git_gutter();

        // 6. Capture the current window size for persistence. Only saved
        // to disk on quit / explicit sync_config, so this is cheap.
        if let Some(rect) = ctx.input(|i| i.viewport().inner_rect) {
            self.config.window_width = Some(rect.width().round() as u32);
            self.config.window_height = Some(rect.height().round() as u32);
        }

        // 6. Render the frame.
        crate::ui::render(ctx, self);

        // 7. Close the window if the user requested quit (Ctrl+Q / Esc).
        if self.should_quit {
            self.request_close(ctx);
        }
    }
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Translate an egui event to a find-bar action. Returns `None` for
/// events we don't intercept — the caller then falls through to the
/// regular key translation.
fn find_bar_translate(event: &eframe::egui::Event) -> Option<EditorEvent> {
    use eframe::egui::{Event, Key};
    match event {
        Event::Key {
            key: Key::Escape,
            pressed: true,
            ..
        } => Some(EditorEvent::FindClose),
        Event::Key {
            key: Key::Enter,
            pressed: true,
            modifiers,
            ..
        } => {
            if modifiers.shift {
                Some(EditorEvent::FindPrev)
            } else {
                Some(EditorEvent::FindNext)
            }
        }
        Event::Key {
            key: Key::R,
            pressed: true,
            modifiers,
            ..
        } if modifiers.alt => Some(EditorEvent::ToggleFindRegex),
        _ => None,
    }
}

/// Translate an egui event to a project-tree action while the sidebar
/// is open. Returns `None` for events that should fall through to the
/// normal editor bindings.
fn project_tree_translate(event: &eframe::egui::Event) -> Option<EditorEvent> {
    use eframe::egui::{Event, Key};
    match event {
        Event::Key {
            key: Key::Escape,
            pressed: true,
            ..
        } => Some(EditorEvent::ToggleProjectTree),
        Event::Key {
            key: Key::ArrowUp,
            pressed: true,
            ..
        } => Some(EditorEvent::ProjectTreeMove { delta: -1 }),
        Event::Key {
            key: Key::ArrowDown,
            pressed: true,
            ..
        } => Some(EditorEvent::ProjectTreeMove { delta: 1 }),
        Event::Key {
            key: Key::Enter,
            pressed: true,
            ..
        } => Some(EditorEvent::ProjectTreeOpen),
        _ => None,
    }
}

/// Move `pos` left to the start of the previous word (or beginning of
/// the line if no word boundary exists to the left).
fn skip_word_left(buffer: &dyn Buffer, pos: usize) -> usize {
    if pos == 0 {
        return 0;
    }
    let (line, _) = buffer.pos_to_linecol(pos).unwrap_or((0, 0));
    let line_text = match buffer.line_text(line) {
        Some(cow) => cow.into_owned(),
        None => return pos.saturating_sub(1),
    };
    let line_byte_start = buffer.line_byte_range(line).map(|r| r.start).unwrap_or(0);
    let char_col = core::byte_to_char_col(&line_text, pos - line_byte_start);
    let chars: Vec<char> = line_text.chars().collect();
    let mut i = char_col.min(chars.len());
    if i == 0 {
        return line_byte_start;
    }
    if is_word_char(chars[i - 1]) {
        while i > 0 && is_word_char(chars[i - 1]) {
            i -= 1;
        }
    } else {
        while i > 0 && !is_word_char(chars[i - 1]) {
            i -= 1;
        }
        while i > 0 && is_word_char(chars[i - 1]) {
            i -= 1;
        }
    }
    line_byte_start + core::char_col_to_byte_col(&line_text, i)
}

/// Move `pos` right to the next word boundary. Matches Ctrl+Right in
/// most editors: if currently in a word, advance to end of it; if
/// currently in whitespace, advance to start of the next word. Word
/// boundaries are char-class transitions between word chars
/// (alphanumeric + `_`) and non-word chars. Crosses line boundaries.
fn skip_word_right(buffer: &dyn Buffer, pos: usize) -> usize {
    let len = buffer.len();
    if pos >= len {
        return len;
    }
    let (line, _) = buffer.pos_to_linecol(pos).unwrap_or((0, 0));
    let line_text = match buffer.line_text(line) {
        Some(cow) => cow.into_owned(),
        None => return (pos + 1).min(len),
    };
    let line_byte_start = buffer.line_byte_range(line).map(|r| r.start).unwrap_or(0);
    let line_byte_end = buffer.line_byte_range(line).map(|r| r.end).unwrap_or(len);
    let char_col = core::byte_to_char_col(&line_text, pos - line_byte_start);
    let chars: Vec<char> = line_text.chars().collect();
    let mut i = char_col.min(chars.len());

    // In-word: eat to end of word. In-whitespace: skip ws to start of
    // next word.
    if i < chars.len() && is_word_char(chars[i]) {
        while i < chars.len() && is_word_char(chars[i]) {
            i += 1;
        }
    } else {
        while i < chars.len() && !is_word_char(chars[i]) {
            i += 1;
        }
    }

    let new_byte = line_byte_start + core::char_col_to_byte_col(&line_text, i);
    if new_byte >= line_byte_end && line + 1 < buffer.line_count() {
        return buffer
            .line_byte_range(line + 1)
            .map(|r| r.start)
            .unwrap_or(len);
    }
    new_byte.min(len)
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::PieceTableBuffer;

    fn app_with(content: &str) -> EditorApp {
        let buf: Box<dyn Buffer> =
            Box::new(PieceTableBuffer::from_bytes(content.as_bytes().to_vec()));
        EditorApp::new(buf)
    }

    fn arrow_down_event() -> eframe::egui::Event {
        eframe::egui::Event::Key {
            key: eframe::egui::Key::ArrowDown,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: eframe::egui::Modifiers::default(),
        }
    }

    #[test]
    fn project_tree_focus_routes_arrow_keys() {
        let mut app = app_with("a\nb\n");
        app.project_tree_open = true;
        app.project_tree_focused = true;

        let ctx = eframe::egui::Context::default();
        ctx.input_mut(|i| i.events.push(arrow_down_event()));
        app.handle_input(&ctx);
        let (line, _) = app
            .active_buffer()
            .pos_to_linecol(app.active_buffer().cursor())
            .unwrap();
        // Project tree consumed the event; cursor stayed put.
        assert_eq!(line, 0);

        app.project_tree_focused = false;
        let ctx2 = eframe::egui::Context::default();
        ctx2.input_mut(|i| i.events.push(arrow_down_event()));
        app.handle_input(&ctx2);
        let (line, _) = app
            .active_buffer()
            .pos_to_linecol(app.active_buffer().cursor())
            .unwrap();
        assert_eq!(line, 1);
    }

    #[test]
    fn insert_advances_cursor_and_marks_dirty() {
        let mut app = app_with("");
        app.handle_event(EditorEvent::Insert('h'));
        app.handle_event(EditorEvent::Insert('i'));
        assert_eq!(app.active_buffer().to_bytes(), b"hi".to_vec());
        assert_eq!(app.active_buffer().cursor(), 2);
        assert!(app.is_dirty());
    }

    #[test]
    fn backspace_at_start_is_noop() {
        let mut app = app_with("abc");
        app.active_buffer_mut().set_cursor(0);
        app.handle_event(EditorEvent::DeleteLeft);
        assert_eq!(app.active_buffer().to_bytes(), b"abc".to_vec());
        assert_eq!(app.active_buffer().cursor(), 0);
    }

    #[test]
    fn delete_right_at_end_is_noop() {
        let mut app = app_with("abc");
        app.active_buffer_mut().set_cursor(3);
        app.handle_event(EditorEvent::DeleteRight);
        assert_eq!(app.active_buffer().to_bytes(), b"abc".to_vec());
    }

    #[test]
    fn arrow_left_right_moves_cursor() {
        let mut app = app_with("abc");
        app.active_buffer_mut().set_cursor(1);
        app.handle_event(EditorEvent::Move(Movement::Left));
        assert_eq!(app.active_buffer().cursor(), 0);
        app.handle_event(EditorEvent::Move(Movement::Right));
        assert_eq!(app.active_buffer().cursor(), 1);
    }

    #[test]
    fn arrow_left_right_skip_over_multibyte_characters() {
        let mut app = app_with("héllo");
        app.active_buffer_mut().set_cursor(3);
        app.handle_event(EditorEvent::Move(Movement::Left));
        assert_eq!(app.active_buffer().cursor(), 1);
        app.handle_event(EditorEvent::Move(Movement::Left));
        assert_eq!(app.active_buffer().cursor(), 0);
        app.handle_event(EditorEvent::Move(Movement::Right));
        assert_eq!(app.active_buffer().cursor(), 1);
        app.handle_event(EditorEvent::Move(Movement::Right));
        assert_eq!(app.active_buffer().cursor(), 3);
    }

    #[test]
    fn down_arrow_preserves_visual_column_on_multibyte_target_line() {
        let mut app = app_with("ab\néx");
        app.active_buffer_mut().set_cursor(2);
        app.handle_event(EditorEvent::Move(Movement::Down));
        assert_eq!(app.active_buffer().cursor(), 6);
        assert_eq!(
            app.active_buffer()
                .pos_to_linecol(app.active_buffer().cursor()),
            Some((1, 3))
        );
    }

    #[test]
    fn home_and_end_via_movement() {
        let mut app = app_with("hello\nworld");
        app.active_buffer_mut().set_cursor(8);
        app.handle_event(EditorEvent::Move(Movement::LineStart));
        assert_eq!(app.active_buffer().cursor(), 6);
        app.handle_event(EditorEvent::Move(Movement::LineEnd));
        assert_eq!(app.active_buffer().cursor(), 11);
    }

    #[test]
    fn document_start_and_end() {
        let mut app = app_with("hello\nworld");
        app.active_buffer_mut().set_cursor(5);
        app.handle_event(EditorEvent::Move(Movement::DocumentStart));
        assert_eq!(app.active_buffer().cursor(), 0);
        app.handle_event(EditorEvent::Move(Movement::DocumentEnd));
        assert_eq!(app.active_buffer().cursor(), 11);
    }

    #[test]
    fn quit_sets_flag() {
        let mut app = app_with("");
        assert!(!app.should_quit);
        app.handle_event(EditorEvent::Quit);
        assert!(app.should_quit);
    }

    #[test]
    fn save_clears_dirty_when_path_set() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("the_editor_gui_save_{}.txt", std::process::id()));
        let _ = std::fs::remove_file(&path);

        let buf: Box<dyn Buffer> = Box::new(PieceTableBuffer::from_bytes_with_path(
            b"".to_vec(),
            path.clone(),
        ));
        let mut app = EditorApp::new(buf);
        app.handle_event(EditorEvent::Insert('x'));
        assert!(app.is_dirty());
        app.handle_event(EditorEvent::Save);
        assert!(!app.is_dirty());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn select_extend_moves_head_keeps_anchor() {
        let mut app = app_with("hello world");
        app.active_buffer_mut().set_cursor(2);
        app.active_buffer_mut()
            .set_selection(Selection::collapsed(2));
        app.handle_event(EditorEvent::SelectExtend(Movement::Right));
        let sel = app.active_buffer().selection();
        assert_eq!(sel.anchor, 2);
        assert_eq!(sel.head, 3);
    }

    #[test]
    fn gui_event_handling_matches_tui() {
        // The contract: the same sequence of EditorEvents produces the
        // same buffer state on both frontends. We can't directly compare
        // here, but we can verify that all shared event types are
        // handled. If you add an event variant to `core::input` but
        // forget to handle it here, this test acts as a checklist.
        let mut app = app_with("");
        for ev in [
            EditorEvent::Insert('a'),
            EditorEvent::Insert('b'),
            EditorEvent::Move(Movement::Right),
            EditorEvent::DeleteLeft,
            EditorEvent::Save,
            EditorEvent::Quit,
        ] {
            app.handle_event(ev);
        }
        assert_eq!(app.active_buffer().to_bytes(), b"a".to_vec());
        assert!(app.should_quit);
    }

    // ----- selection-aware editing -----

    #[test]
    fn insert_replaces_selection() {
        // Selecting "world" then typing '!' should produce "hello!"
        let mut app = app_with("hello world");
        app.active_buffer_mut().set_selection(Selection {
            anchor: 6,
            head: 11,
        });
        app.handle_event(EditorEvent::Insert('!'));
        assert_eq!(app.active_buffer().to_bytes(), b"hello !".to_vec());
        assert_eq!(app.active_buffer().cursor(), 7);
        assert!(app.active_buffer().selection().is_collapsed());
    }

    #[test]
    fn delete_left_with_selection_deletes_selection() {
        // Selecting "world" then pressing Backspace should delete "world".
        let mut app = app_with("hello world");
        app.active_buffer_mut().set_selection(Selection {
            anchor: 6,
            head: 11,
        });
        app.handle_event(EditorEvent::DeleteLeft);
        assert_eq!(app.active_buffer().to_bytes(), b"hello ".to_vec());
        assert_eq!(app.active_buffer().cursor(), 6);
    }

    #[test]
    fn delete_right_with_selection_deletes_selection() {
        let mut app = app_with("hello world");
        app.active_buffer_mut().set_selection(Selection {
            anchor: 6,
            head: 11,
        });
        app.handle_event(EditorEvent::DeleteRight);
        assert_eq!(app.active_buffer().to_bytes(), b"hello ".to_vec());
        assert_eq!(app.active_buffer().cursor(), 6);
    }

    #[test]
    fn delete_selection_noop_when_collapsed() {
        let mut app = app_with("hello");
        app.active_buffer_mut().set_cursor(2);
        app.active_buffer_mut()
            .set_selection(Selection::collapsed(2));
        app.handle_event(EditorEvent::DeleteSelection);
        assert_eq!(app.active_buffer().to_bytes(), b"hello".to_vec());
        assert_eq!(app.active_buffer().cursor(), 2);
    }

    #[test]
    fn paste_replaces_selection() {
        let mut app = app_with("hello world");
        app.active_buffer_mut().set_selection(Selection {
            anchor: 6,
            head: 11,
        });
        app.handle_event(EditorEvent::Paste("Rust".to_string()));
        assert_eq!(app.active_buffer().to_bytes(), b"hello Rust".to_vec());
        assert_eq!(app.active_buffer().cursor(), 10);
    }

    #[test]
    fn paste_with_no_selection_inserts_at_cursor() {
        let mut app = app_with("hello world");
        app.active_buffer_mut().set_cursor(6);
        app.active_buffer_mut()
            .set_selection(Selection::collapsed(6));
        app.handle_event(EditorEvent::Paste("beautiful ".to_string()));
        assert_eq!(
            app.active_buffer().to_bytes(),
            b"hello beautiful world".to_vec()
        );
    }

    // ----- Up/Down over short lines (parity with TUI) -----

    #[test]
    fn up_arrow_clamps_to_end_of_shorter_line_above() {
        let mut app = app_with("hello\nthis is a much longer line");
        let pos = app.active_buffer().linecol_to_pos(1, 25).unwrap();
        app.active_buffer_mut().set_cursor(pos);
        app.handle_event(EditorEvent::Move(Movement::Up));
        let (line, col) = app
            .active_buffer()
            .pos_to_linecol(app.active_buffer().cursor())
            .unwrap();
        assert_eq!(line, 0);
        assert_eq!(col, 5);
    }

    #[test]
    fn down_arrow_clamps_to_end_of_shorter_line_below() {
        let mut app = app_with("this is a much longer line\nhello");
        let pos = app.active_buffer().linecol_to_pos(0, 25).unwrap();
        app.active_buffer_mut().set_cursor(pos);
        app.handle_event(EditorEvent::Move(Movement::Down));
        let (line, col) = app
            .active_buffer()
            .pos_to_linecol(app.active_buffer().cursor())
            .unwrap();
        assert_eq!(line, 1);
        assert_eq!(col, 5);
    }

    #[test]
    fn page_up_clamps_to_target_line_length() {
        let buf: Box<dyn Buffer> = Box::new(PieceTableBuffer::from_bytes(
            b"a\nb\nc\nd\ne\nthis is a long final line".to_vec(),
        ));
        let mut app = EditorApp::new(buf);
        app.viewport_lines = 5;
        let pos = app.active_buffer().linecol_to_pos(5, 25).unwrap();
        app.active_buffer_mut().set_cursor(pos);
        app.handle_event(EditorEvent::Move(Movement::PageUp));
        let (line, col) = app
            .active_buffer()
            .pos_to_linecol(app.active_buffer().cursor())
            .unwrap();
        assert_eq!(line, 0);
        assert_eq!(col, 1);
    }

    // ----- PageUp / PageDown / Word movement -----

    fn make_multi_line_app() -> EditorApp {
        let content: String = (0..10).map(|i| format!("line{i}\n")).collect();
        let content = content.trim_end_matches('\n').to_string();
        let buf: Box<dyn Buffer> = Box::new(PieceTableBuffer::from_bytes(content.into_bytes()));
        EditorApp::new(buf)
    }

    #[test]
    fn page_up_and_down_move_cursor_by_viewport() {
        let mut app = make_multi_line_app();
        app.viewport_lines = 5;
        let pos = app.active_buffer().linecol_to_pos(9, 2).unwrap();
        app.active_buffer_mut().set_cursor(pos);
        app.handle_event(EditorEvent::Move(Movement::PageUp));
        assert_eq!(
            app.active_buffer()
                .pos_to_linecol(app.active_buffer().cursor()),
            Some((4, 2))
        );
        app.handle_event(EditorEvent::Move(Movement::PageDown));
        app.handle_event(EditorEvent::Move(Movement::PageDown));
        assert_eq!(
            app.active_buffer()
                .pos_to_linecol(app.active_buffer().cursor()),
            Some((9, 2))
        );
    }

    #[test]
    fn word_right_walks_word_boundaries() {
        let mut app = app_with("hello   world foo");
        app.active_buffer_mut().set_cursor(0);
        app.handle_event(EditorEvent::Move(Movement::WordRight));
        assert_eq!(app.active_buffer().cursor(), 5);
        app.handle_event(EditorEvent::Move(Movement::WordRight));
        assert_eq!(app.active_buffer().cursor(), 8);
        app.handle_event(EditorEvent::Move(Movement::WordRight));
        assert_eq!(app.active_buffer().cursor(), 13);
        app.handle_event(EditorEvent::Move(Movement::WordRight));
        assert_eq!(app.active_buffer().cursor(), 14);
    }

    #[test]
    fn word_left_walks_word_boundaries() {
        let mut app = app_with("hello   world");
        app.active_buffer_mut().set_cursor(13);
        app.handle_event(EditorEvent::Move(Movement::WordLeft));
        assert_eq!(app.active_buffer().cursor(), 8);
        app.handle_event(EditorEvent::Move(Movement::WordLeft));
        assert_eq!(app.active_buffer().cursor(), 0);
    }

    // ----- Word delete -----

    #[test]
    fn delete_word_left_removes_word() {
        let mut app = app_with("hello world");
        app.active_buffer_mut().set_cursor(11);
        app.handle_event(EditorEvent::DeleteWordLeft);
        assert_eq!(app.active_buffer().to_bytes(), b"hello ".to_vec());
    }

    #[test]
    fn delete_word_right_removes_word() {
        let mut app = app_with("hello world");
        app.active_buffer_mut().set_cursor(6);
        app.handle_event(EditorEvent::DeleteWordRight);
        assert_eq!(app.active_buffer().to_bytes(), b"hello ".to_vec());
    }

    // ----- Line ops -----

    #[test]
    fn delete_line_removes_entire_line_and_newline() {
        let mut app = app_with("alpha\nbeta\ngamma");
        app.active_buffer_mut().set_cursor(8);
        app.handle_event(EditorEvent::DeleteLine);
        assert_eq!(app.active_buffer().to_bytes(), b"alpha\ngamma".to_vec());
    }

    #[test]
    fn duplicate_line_inserts_copy_below() {
        let mut app = app_with("alpha\nbeta\ngamma");
        app.active_buffer_mut().set_cursor(8);
        app.handle_event(EditorEvent::DuplicateLine);
        assert_eq!(
            app.active_buffer().to_bytes(),
            b"alpha\nbeta\nbeta\ngamma".to_vec()
        );
    }

    #[test]
    fn move_line_down_swaps_with_next() {
        let mut app = app_with("alpha\nbeta\ngamma");
        app.active_buffer_mut().set_cursor(2);
        app.handle_event(EditorEvent::MoveLineDown);
        assert_eq!(
            app.active_buffer().to_bytes(),
            b"beta\nalpha\ngamma".to_vec()
        );
    }

    // ----- multi-buffer / tabs -----

    /// Helper: build an `EditorApp` from a list of buffer contents.
    /// Each entry becomes one document, in order.
    fn app_with_docs(contents: &[&str]) -> EditorApp {
        let docs: Vec<Document> = contents
            .iter()
            .map(|c| {
                let buf: Box<dyn Buffer> =
                    Box::new(PieceTableBuffer::from_bytes(c.as_bytes().to_vec()));
                Document::new(buf)
            })
            .collect();
        EditorApp::new_with_documents(docs, core::Config::default())
    }

    #[test]
    fn new_doc_appends_empty_document_and_activates_it() {
        let mut app = app_with_docs(&["alpha", "beta"]);
        assert_eq!(app.doc_count(), 2);
        assert_eq!(app.active(), 0);
        app.handle_event(EditorEvent::NewDoc);
        assert_eq!(app.doc_count(), 3);
        assert_eq!(app.active(), 2);
        // The new doc is empty and unsaved.
        assert_eq!(app.active_buffer().to_bytes(), b"".to_vec());
        assert!(!app.active_buffer().is_dirty()); // empty buffer is not dirty
    }

    #[test]
    fn close_doc_with_multiple_docs_removes_active_and_picks_neighbour() {
        let mut app = app_with_docs(&["alpha", "beta", "gamma"]);
        assert_eq!(app.active(), 0);
        app.handle_event(EditorEvent::CloseDoc);
        assert_eq!(app.doc_count(), 2);
        // Active stays at index 0 (the doc after the removed one slid down).
        assert_eq!(app.active(), 0);
        assert_eq!(app.active_buffer().to_bytes(), b"beta".to_vec());
    }

    #[test]
    fn close_doc_at_tail_moves_active_to_new_last() {
        let mut app = app_with_docs(&["alpha", "beta", "gamma"]);
        app.active = 2;
        app.handle_event(EditorEvent::CloseDoc);
        assert_eq!(app.doc_count(), 2);
        assert_eq!(app.active(), 1, "tail close should move active back");
        assert_eq!(app.active_buffer().to_bytes(), b"beta".to_vec());
    }

    #[test]
    fn close_last_doc_quits_editor() {
        let mut app = app_with_docs(&["only"]);
        assert!(!app.should_quit);
        app.handle_event(EditorEvent::CloseDoc);
        assert!(app.should_quit);
        // The single document is NOT removed (the editor is quitting,
        // not closing). This keeps the close path free of any
        // post-close indexing surprises.
        assert_eq!(app.doc_count(), 1);
    }

    #[test]
    fn close_inactive_tab_keeps_active_selection() {
        let mut app = app_with_docs(&["alpha", "beta", "gamma"]);
        app.active = 1; // beta is active
        app.request_close_doc(0); // close alpha
        assert_eq!(app.doc_count(), 2);
        assert_eq!(app.active(), 0, "active should shift left with closed tab");
        assert_eq!(app.active_buffer().to_bytes(), b"beta".to_vec());
    }

    #[test]
    fn close_inactive_dirty_tab_opens_prompt_for_that_doc() {
        let mut app = app_with_docs(&["alpha", "beta"]);
        app.active = 1;
        // Make the inactive doc dirty.
        app.documents[0].buffer.insert(0, "!").unwrap();
        assert!(app.documents[0].is_dirty());
        app.request_close_doc(0);
        assert!(app.close_confirm.is_some());
        assert_eq!(app.close_confirm.as_ref().unwrap().doc_index, 0);
        assert_eq!(app.active(), 0, "prompt focuses the doc being closed");
    }

    #[test]
    fn next_doc_wraps_around() {
        let mut app = app_with_docs(&["a", "b", "c"]);
        assert_eq!(app.active(), 0);
        app.handle_event(EditorEvent::NextDoc);
        assert_eq!(app.active(), 1);
        app.handle_event(EditorEvent::NextDoc);
        assert_eq!(app.active(), 2);
        // Wraps.
        app.handle_event(EditorEvent::NextDoc);
        assert_eq!(app.active(), 0);
    }

    #[test]
    fn prev_doc_wraps_around() {
        let mut app = app_with_docs(&["a", "b", "c"]);
        assert_eq!(app.active(), 0);
        // Wraps to last.
        app.handle_event(EditorEvent::PrevDoc);
        assert_eq!(app.active(), 2);
        app.handle_event(EditorEvent::PrevDoc);
        assert_eq!(app.active(), 1);
        app.handle_event(EditorEvent::PrevDoc);
        assert_eq!(app.active(), 0);
    }

    #[test]
    fn switching_docs_preserves_buffer_state() {
        // Editing doc 0 must not touch doc 1.
        let mut app = app_with_docs(&["hello", "world"]);
        app.active_buffer_mut().insert(5, "!").unwrap();
        assert_eq!(app.active_buffer().to_bytes(), b"hello!".to_vec());
        app.handle_event(EditorEvent::NextDoc);
        assert_eq!(app.active_buffer().to_bytes(), b"world".to_vec());
        app.handle_event(EditorEvent::PrevDoc);
        assert_eq!(app.active_buffer().to_bytes(), b"hello!".to_vec());
    }

    // ----- GUI auto-scroll trigger -----

    #[test]
    fn last_seen_cursor_starts_at_zero() {
        let app = app_with("hello");
        assert_eq!(app.active_doc().view.last_seen_cursor, 0);
    }

    #[test]
    fn handle_event_moves_cursor_without_touching_last_seen_cursor() {
        // `last_seen_cursor` is the renderer's record of the LAST
        // observed cursor position. It must NOT be updated by
        // `handle_event` — otherwise we'd lose the ability to detect
        // motion between frames (the renderer is the only writer).
        let mut app = app_with("hello");
        assert_eq!(app.active_doc().view.last_seen_cursor, 0);
        app.handle_event(EditorEvent::Insert('x'));
        assert_eq!(app.active_buffer().cursor(), 1);
        assert_eq!(
            app.active_doc().view.last_seen_cursor,
            0,
            "handle_event must not update last_seen_cursor; renderer does"
        );
    }

    #[test]
    fn last_seen_cursor_is_per_document() {
        // Each doc carries its own last_seen_cursor. Switching tabs
        // doesn't leak the previous tab's value into the new active
        // doc's "did the cursor move?" check — that's what makes
        // per-doc scroll preservation across tab switches work.
        let mut app = app_with_docs(&["alpha", "beta"]);
        app.active_doc_mut().view.last_seen_cursor = 42;
        app.handle_event(EditorEvent::NextDoc);
        assert_eq!(
            app.active_doc().view.last_seen_cursor,
            0,
            "doc 1's last_seen_cursor starts at 0"
        );
        // Switching back: doc 0's value is intact.
        app.handle_event(EditorEvent::PrevDoc);
        assert_eq!(app.active_doc().view.last_seen_cursor, 42);
    }

    // ----- close-on-dirty prompt (parity with TUI) -----

    #[test]
    fn close_doc_on_clean_buffer_quits_immediately() {
        let mut app = app_with("alpha");
        assert!(!app.active_doc().is_dirty());
        app.handle_event(EditorEvent::CloseDoc);
        assert!(app.close_confirm.is_none(), "no prompt on clean close");
        assert!(app.should_quit);
    }

    #[test]
    fn close_doc_on_dirty_buffer_opens_prompt() {
        let mut app = app_with("hello");
        app.handle_event(EditorEvent::Insert('!'));
        assert!(app.active_doc().is_dirty());
        app.handle_event(EditorEvent::CloseDoc);
        let confirm = app.close_confirm.as_ref().expect("prompt should be open");
        assert_eq!(confirm.doc_index, 0);
        assert_eq!(confirm.choice, CloseChoice::Save, "Save is the default");
        assert!(!app.should_quit);
        assert_eq!(app.doc_count(), 1);
    }

    #[test]
    fn cycle_close_choice_walks_save_discard_cancel() {
        let mut app = app_with("hello");
        app.handle_event(EditorEvent::Insert('!'));
        app.handle_event(EditorEvent::CloseDoc);
        app.cycle_close_choice(1);
        assert_eq!(
            app.close_confirm.as_ref().unwrap().choice,
            CloseChoice::Discard
        );
        app.cycle_close_choice(1);
        assert_eq!(
            app.close_confirm.as_ref().unwrap().choice,
            CloseChoice::Cancel
        );
        app.cycle_close_choice(1);
        assert_eq!(
            app.close_confirm.as_ref().unwrap().choice,
            CloseChoice::Save
        );
        app.cycle_close_choice(-1);
        assert_eq!(
            app.close_confirm.as_ref().unwrap().choice,
            CloseChoice::Cancel
        );
    }

    #[test]
    fn confirm_close_choice_discard_closes_without_saving() {
        let mut app = app_with_docs(&["alpha", "beta"]);
        app.handle_event(EditorEvent::Insert('!'));
        app.handle_event(EditorEvent::CloseDoc);
        app.cycle_close_choice(1); // Save → Discard
        app.confirm_close_choice();
        assert!(app.close_confirm.is_none());
        assert_eq!(app.doc_count(), 1);
        assert!(!app.should_quit);
    }

    #[test]
    fn confirm_close_choice_cancel_drops_prompt_only() {
        let mut app = app_with("hello");
        app.handle_event(EditorEvent::Insert('!'));
        app.handle_event(EditorEvent::CloseDoc);
        app.cycle_close_choice(1); // Save → Discard
        app.cycle_close_choice(1); // Discard → Cancel
        app.confirm_close_choice();
        assert!(app.close_confirm.is_none());
        assert!(!app.should_quit);
        assert!(app.active_doc().is_dirty(), "buffer untouched");
    }

    #[test]
    fn confirm_close_choice_save_saves_then_closes() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "the_editor_gui_close_save_{}.txt",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let buf: Box<dyn Buffer> = Box::new(PieceTableBuffer::from_bytes_with_path(
            b"hello".to_vec(),
            path.clone(),
        ));
        let mut app = EditorApp::new(buf);
        app.handle_event(EditorEvent::Insert('!'));
        app.handle_event(EditorEvent::CloseDoc);
        app.confirm_close_choice();
        assert!(app.close_confirm.is_none());
        assert!(app.should_quit);
        let contents = std::fs::read_to_string(&path).unwrap();
        // Insert('!') at cursor 0 → "!hello".
        assert_eq!(contents, "!hello");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn confirm_close_choice_save_without_path_reports_error() {
        let mut app = app_with("hello");
        app.handle_event(EditorEvent::Insert('!'));
        assert!(app.active_doc().path().is_none());
        app.handle_event(EditorEvent::CloseDoc);
        app.confirm_close_choice();
        assert!(
            app.close_confirm.is_none(),
            "prompt dropped on save failure"
        );
        let status = app.status_message.as_deref().unwrap_or("");
        assert!(
            status.contains("Save error"),
            "expected save error in status: {status}"
        );
        assert!(!app.should_quit);
    }

    // ----- open-file via direct path -----

    #[test]
    fn open_file_event_with_some_path_loads_directly() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "the_editor_gui_open_some_{}.txt",
            std::process::id()
        ));
        std::fs::write(&path, b"hi").unwrap();
        let mut app = app_with("buffer");
        app.handle_event(EditorEvent::OpenFile(Some(path.clone())));
        assert_eq!(app.active_buffer().to_bytes(), b"hi".to_vec());
        assert_eq!(app.active_doc().path(), Some(path.as_path()));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn open_file_with_nonexistent_path_creates_empty_buffer_with_path() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "the_editor_gui_open_new_{}.txt",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let mut app = app_with("buffer");
        app.handle_event(EditorEvent::OpenFile(Some(path.clone())));
        assert_eq!(app.active_buffer().to_bytes(), b"".to_vec());
        assert_eq!(app.active_doc().path(), Some(path.as_path()));
    }

    #[test]
    fn close_confirm_drops_go_to_line_dialog_when_opened() {
        let mut app = app_with("hello");
        app.handle_event(EditorEvent::Insert('!'));
        app.handle_event(EditorEvent::GoToLine(None));
        assert!(app.go_to_line_dialog.is_some());
        app.handle_event(EditorEvent::CloseDoc);
        assert!(app.close_confirm.is_some());
        assert!(app.go_to_line_dialog.is_none());
    }

    // ----- GUI event-translation parity -----

    use eframe::egui::{Event, Key, Modifiers};

    fn primary_mods() -> Modifiers {
        Modifiers {
            ctrl: true,
            command: true,
            ..Default::default()
        }
    }

    fn key_event(key: Key, pressed: bool, modifiers: Modifiers) -> Event {
        Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers,
        }
    }

    #[test]
    fn primary_o_opens_file_dialog() {
        let ev = key_event(Key::O, true, primary_mods());
        assert_eq!(
            crate::event::translate_event(&ev),
            Some(EditorEvent::OpenFile(None))
        );
    }

    // ----- theme cycling -----

    #[test]
    fn cycle_theme_changes_theme_and_invalidates_cache() {
        let mut app = app_with("let x = 42;");
        // Pre-warm the cache.
        app.active_doc_mut().syntax.lines.insert(0, Vec::new());
        app.active_doc_mut().syntax.dirty = false;
        assert!(!app.active_doc().syntax.lines.is_empty());

        let before = app.syntax.theme_name().to_string();
        app.handle_event(EditorEvent::CycleTheme);
        let status = app.status_message.as_deref().unwrap_or("");
        assert!(status.contains("Theme:"), "status: {status}");

        // Every document's syntax cache should be invalidated.
        for doc in &app.documents {
            assert!(doc.syntax.dirty);
            assert!(doc.syntax.lines.is_empty());
        }

        // Theme actually changed (unless there's only one bundled theme).
        if app.syntax.theme_names().len() > 1 {
            assert_ne!(app.syntax.theme_name(), before);
        }
    }

    // ----- go-to-line -----

    #[test]
    fn go_to_line_jumps_to_start_of_target_line() {
        let mut app = app_with("alpha\nbeta\ngamma");
        app.handle_event(EditorEvent::GoToLine(Some(2)));
        // Line 2 starts at byte 6 ("alpha\n" = 6 bytes).
        assert_eq!(app.active_buffer().cursor(), 6);
        assert!(app
            .status_message
            .as_deref()
            .unwrap_or("")
            .contains("Go to line 2"));
    }

    #[test]
    fn go_to_line_clamps_past_end() {
        let mut app = app_with("alpha\nbeta");
        app.handle_event(EditorEvent::GoToLine(Some(100)));
        // Document has 2 lines; clamp to line 2 start.
        assert_eq!(app.active_buffer().cursor(), 6);
    }

    #[test]
    fn go_to_line_zero_rejected() {
        let mut app = app_with("alpha\nbeta");
        app.handle_event(EditorEvent::GoToLine(Some(0)));
        assert_eq!(app.active_buffer().cursor(), 0);
        assert!(app
            .status_message
            .as_deref()
            .unwrap_or("")
            .contains("start at 1"));
    }

    #[test]
    fn go_to_line_dialog_opens_and_accepts_input() {
        let mut app = app_with("a\nb\nc");
        app.handle_event(EditorEvent::GoToLine(None));
        assert!(app.go_to_line_dialog.is_some());
        app.push_go_to_line_query('3');
        app.submit_go_to_line_dialog();
        assert!(app.go_to_line_dialog.is_none());
        // Line 3 starts at byte 4 ("a\nb\n" = 4 bytes).
        assert_eq!(app.active_buffer().cursor(), 4);
    }

    #[test]
    fn go_to_line_dialog_rejects_non_digit_input() {
        let mut app = app_with("a\nb");
        app.handle_event(EditorEvent::GoToLine(None));
        app.push_go_to_line_query('a');
        assert_eq!(app.go_to_line_dialog.as_ref().unwrap().query, "");
    }

    #[test]
    fn go_to_line_dialog_invalid_input_shows_error() {
        let mut app = app_with("a\nb");
        app.handle_event(EditorEvent::GoToLine(None));
        // Inject an invalid query directly — user input is filtered to
        // digits, but parse errors are still handled defensively.
        app.go_to_line_dialog.as_mut().unwrap().query = "abc".to_string();
        app.submit_go_to_line_dialog();
        assert!(app
            .status_message
            .as_deref()
            .unwrap_or("")
            .contains("not a valid line number"));
    }

    // ----- project tree -----

    #[test]
    fn project_tree_shows_by_default_and_toggles() {
        let dir =
            std::env::temp_dir().join(format!("the_editor_gui_proj_tree_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Cargo.toml"), "[package]").unwrap();
        std::fs::write(dir.join("main.rs"), "fn main() {}").unwrap();

        let path = dir.join("main.rs");
        let buf: Box<dyn Buffer> =
            Box::new(PieceTableBuffer::from_bytes_with_path(b"".to_vec(), path));
        let mut app = EditorApp::new(buf);

        assert!(app.project_tree_open);
        let rows: Vec<String> = app
            .project_tree_rows()
            .into_iter()
            .map(|(_, n)| n.name().to_string())
            .collect();
        assert!(rows.contains(&"Cargo.toml".to_string()));
        assert!(rows.contains(&"main.rs".to_string()));

        app.handle_event(EditorEvent::ToggleProjectTree);
        assert!(!app.project_tree_open);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn project_tree_collapses_and_opens_file() {
        let dir =
            std::env::temp_dir().join(format!("the_editor_gui_proj_open_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Cargo.toml"), "[package]").unwrap();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/lib.rs"), "pub fn lib() {}").unwrap();

        let path = dir.join("main.rs");
        let buf: Box<dyn Buffer> =
            Box::new(PieceTableBuffer::from_bytes_with_path(b"".to_vec(), path));
        let mut app = EditorApp::new(buf);

        // Collapse the src directory so lib.rs disappears from the rows.
        let src_idx = app
            .project_tree_rows()
            .into_iter()
            .position(|(_, n)| n.name() == "src")
            .unwrap();
        app.project_tree_selected = src_idx;
        app.handle_event(EditorEvent::ProjectTreeOpen);
        assert!(!app
            .project_tree_rows()
            .iter()
            .any(|(_, n)| n.name() == "lib.rs"));

        // Re-expand src and open lib.rs.
        app.handle_event(EditorEvent::ProjectTreeOpen);
        let lib_idx = app
            .project_tree_rows()
            .into_iter()
            .position(|(_, n)| n.name() == "lib.rs")
            .unwrap();
        app.project_tree_selected = lib_idx;
        app.handle_event(EditorEvent::ProjectTreeOpen);

        assert_eq!(
            app.active_doc().path(),
            Some(dir.join("src/lib.rs").as_path())
        );
        assert_eq!(app.active_buffer().to_bytes(), b"pub fn lib() {}".to_vec());

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ----- auto-save -----

    fn app_with_path(content: &str, path: std::path::PathBuf) -> EditorApp {
        let buf: Box<dyn Buffer> = Box::new(PieceTableBuffer::from_bytes_with_path(
            content.as_bytes().to_vec(),
            path,
        ));
        EditorApp::new_with_documents(vec![Document::new(buf)], core::Config::default())
    }

    #[test]
    fn auto_save_writes_idle_dirty_buffer() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("the_editor_autosave_{}.txt", std::process::id()));
        let mut app = app_with_path("hello", path.clone());
        app.handle_event(EditorEvent::Insert('!'));
        assert!(app.active_doc().is_dirty());
        // Pretend the edit happened long ago.
        app.last_edit_time = Instant::now() - std::time::Duration::from_secs(10);
        app.auto_save();
        assert!(!app.active_doc().is_dirty());
        let saved = std::fs::read_to_string(&path).unwrap();
        assert_eq!(saved, "!hello");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn auto_save_skips_unnamed_buffers() {
        let mut app = app_with("hello");
        app.handle_event(EditorEvent::Insert('!'));
        app.last_edit_time = Instant::now() - std::time::Duration::from_secs(10);
        app.auto_save();
        assert!(app.active_doc().is_dirty());
    }

    #[test]
    fn auto_save_respects_config_toggle() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "the_editor_autosave_off_{}.txt",
            std::process::id()
        ));
        let mut app = app_with_path("hello", path.clone());
        app.config.auto_save = false;
        app.handle_event(EditorEvent::Insert('!'));
        app.last_edit_time = Instant::now() - std::time::Duration::from_secs(10);
        app.auto_save();
        assert!(app.active_doc().is_dirty());
        let _ = std::fs::remove_file(&path);
    }

    // ----- external reload -----

    #[test]
    fn reload_document_at_path_loads_new_content() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("the_editor_reload_{}.txt", std::process::id()));
        std::fs::write(&path, "original").unwrap();

        let mut app = app_with_path("original", path.clone());
        assert_eq!(app.active_buffer().to_bytes(), b"original".to_vec());

        std::fs::write(&path, "updated").unwrap();
        app.reload_document_at_path(&path);
        assert_eq!(app.active_buffer().to_bytes(), b"updated".to_vec());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn reload_file_event_reloads_active_document() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "the_editor_reload_event_{}.txt",
            std::process::id()
        ));
        std::fs::write(&path, "v1").unwrap();

        let mut app = app_with_path("v1", path.clone());
        std::fs::write(&path, "v2").unwrap();
        app.handle_event(EditorEvent::ReloadFile);
        assert_eq!(app.active_buffer().to_bytes(), b"v2".to_vec());

        let _ = std::fs::remove_file(&path);
    }

    // ----- regex find / replace -----

    #[test]
    fn toggle_find_regex_switches_mode_and_finds_variable_length_matches() {
        let mut app = app_with("foo bar baz");
        app.handle_event(EditorEvent::FindOpen);
        app.handle_event(EditorEvent::FindQueryChanged("b\\w+".to_string()));
        assert!(app.search.matches.is_empty());
        app.handle_event(EditorEvent::ToggleFindRegex);
        assert!(app.search.regex_mode);
        assert_eq!(app.search.matches, vec![(4, 7), (8, 11)]);
        assert_eq!(app.search.current_match(), Some(4));
    }

    #[test]
    fn replace_one_in_regex_mode_expands_capture_groups() {
        let mut app = app_with("hello world");
        app.handle_event(EditorEvent::FindOpen);
        app.handle_event(EditorEvent::FindQueryChanged("(\\w+) (\\w+)".to_string()));
        app.handle_event(EditorEvent::ToggleFindRegex);
        app.handle_event(EditorEvent::ReplaceOpen);
        app.handle_event(EditorEvent::ReplaceQueryChanged("$2 $1".to_string()));
        app.handle_event(EditorEvent::ReplaceOne);
        assert_eq!(app.active_buffer().to_bytes(), b"world hello".to_vec());
    }

    #[test]
    fn replace_all_in_regex_mode_expands_capture_groups() {
        let mut app = app_with("hello world\nfoo bar");
        app.handle_event(EditorEvent::FindOpen);
        app.handle_event(EditorEvent::FindQueryChanged("(\\w+) (\\w+)".to_string()));
        app.handle_event(EditorEvent::ToggleFindRegex);
        app.handle_event(EditorEvent::ReplaceOpen);
        app.handle_event(EditorEvent::ReplaceQueryChanged("$2 $1".to_string()));
        app.handle_event(EditorEvent::ReplaceAll);
        assert_eq!(
            app.active_buffer().to_bytes(),
            b"world hello\nbar foo".to_vec()
        );
    }

    #[test]
    fn default_ui_theme_is_dark() {
        let app = app_with("hello");
        assert_eq!(app.theme.name, "Dark");
        assert_eq!(app.config.ui_theme, None);
    }

    #[test]
    fn set_ui_theme_by_name_switches_theme_and_config() {
        let mut app = app_with("hello");
        assert!(app.set_ui_theme_by_name("Light"));
        assert_eq!(app.theme.name, "Light");
        assert_eq!(app.config.ui_theme, Some("Light".to_string()));

        assert!(app.set_ui_theme_by_name("Dark"));
        assert_eq!(app.theme.name, "Dark");
        assert_eq!(app.config.ui_theme, Some("Dark".to_string()));
    }

    #[test]
    fn set_ui_theme_by_name_unknown_is_noop() {
        let mut app = app_with("hello");
        assert!(!app.set_ui_theme_by_name("Neon"));
        assert_eq!(app.theme.name, "Dark");
        assert_eq!(app.config.ui_theme, None);
    }

    #[test]
    fn app_loads_ui_theme_from_config() {
        let config = core::Config {
            ui_theme: Some("Light".to_string()),
            ..core::Config::default()
        };
        let buf: Box<dyn Buffer> = Box::new(core::PieceTableBuffer::from_bytes(b"hi".to_vec()));
        let app = EditorApp::new_with_documents(vec![core::Document::new(buf)], config);
        assert_eq!(app.theme.name, "Light");
    }

    #[test]
    fn caret_animation_defaults_off() {
        let app = app_with("hello");
        assert!(!app.config.caret_animation);
    }

    #[test]
    fn toggle_caret_animation_updates_config_and_status() {
        let mut app = app_with("hello");
        app.caret_anim_y = 12.0;

        app.toggle_caret_animation();
        assert!(app.config.caret_animation);
        assert!(app.caret_anim_y.is_nan());
        assert_eq!(app.status_message, Some("Caret animation: on".to_string()));

        app.toggle_caret_animation();
        assert!(!app.config.caret_animation);
        assert!(app.caret_anim_y.is_nan());
        assert_eq!(app.status_message, Some("Caret animation: off".to_string()));
    }

    // ----- project-wide search / replace -----

    fn temp_project_with_file(
        name: &str,
        contents: &str,
    ) -> (std::path::PathBuf, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "the_editor_gui_proj_{}_{}",
            std::process::id(),
            name
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("Cargo.toml"), "[package]").unwrap();
        let file_path = dir.join("src").join("a.rs");
        std::fs::write(&file_path, contents).unwrap();
        (dir, file_path)
    }

    #[test]
    fn project_replace_all_prompts_for_confirmation() {
        let (_dir, path) = temp_project_with_file("confirm", "foo foo");
        let buf: Box<dyn Buffer> = Box::new(PieceTableBuffer::from_path(path.clone()).unwrap());
        let mut app =
            EditorApp::new_with_documents(vec![core::Document::new(buf)], core::Config::default());

        app.handle_event(EditorEvent::ProjectSearch(Some("foo".to_string())));
        app.handle_event(EditorEvent::ProjectSearchReplaceQueryChanged(
            "bar".to_string(),
        ));
        app.handle_event(EditorEvent::ProjectSearchReplaceAll);

        assert!(app.project_search.confirm_replace);
        assert_eq!(app.project_search.replace_previews.len(), 1);
        assert_eq!(app.project_search.replace_previews[0].occurrence_count, 2);
    }

    #[test]
    fn project_replace_all_reloads_open_file() {
        let (_dir, path) = temp_project_with_file("reload", "foo foo");
        let buf: Box<dyn Buffer> = Box::new(PieceTableBuffer::from_path(path.clone()).unwrap());
        let mut app =
            EditorApp::new_with_documents(vec![core::Document::new(buf)], core::Config::default());

        app.handle_event(EditorEvent::ProjectSearch(Some("foo".to_string())));
        app.handle_event(EditorEvent::ProjectSearchReplaceQueryChanged(
            "bar".to_string(),
        ));
        app.handle_event(EditorEvent::ProjectSearchReplaceAll);
        app.handle_event(EditorEvent::ProjectSearchReplaceAllConfirm);

        assert!(!app.project_search.open);
        assert_eq!(app.active_buffer().to_bytes(), b"bar bar".to_vec());
        assert!(!app.active_doc().external_change);
    }

    #[test]
    fn project_replace_all_marks_dirty_open_file_stale() {
        let (_dir, path) = temp_project_with_file("stale", "foo foo");
        let buf: Box<dyn Buffer> = Box::new(PieceTableBuffer::from_path(path.clone()).unwrap());
        let mut app =
            EditorApp::new_with_documents(vec![core::Document::new(buf)], core::Config::default());
        app.handle_event(EditorEvent::Insert('!'));

        app.handle_event(EditorEvent::ProjectSearch(Some("foo".to_string())));
        app.handle_event(EditorEvent::ProjectSearchReplaceQueryChanged(
            "bar".to_string(),
        ));
        app.handle_event(EditorEvent::ProjectSearchReplaceAll);
        app.handle_event(EditorEvent::ProjectSearchReplaceAllConfirm);

        assert!(!app.project_search.open);
        assert!(app.active_doc().external_change);
    }

    // ----- fuzzy file finder -----

    fn temp_project_for_fuzzy(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "the_editor_gui_fuzzy_{}_{}",
            std::process::id(),
            name
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join("tests")).unwrap();
        std::fs::write(dir.join("Cargo.toml"), "[package]").unwrap();
        std::fs::write(dir.join("src").join("main.rs"), "fn main() {}").unwrap();
        std::fs::write(dir.join("src").join("lib.rs"), "pub fn lib() {}").unwrap();
        std::fs::write(dir.join("tests").join("it.rs"), "#[test] fn it() {}").unwrap();
        dir
    }

    #[test]
    fn fuzzy_finder_lists_project_files() {
        let dir = temp_project_for_fuzzy("list");
        let buf: Box<dyn Buffer> = Box::new(core::PieceTableBuffer::from_bytes(b"x".to_vec()));
        let mut app =
            EditorApp::new_with_documents(vec![core::Document::new(buf)], core::Config::default());
        app.documents[0].project = core::Project::from_path(&dir);

        app.handle_event(EditorEvent::FuzzyFinder(None));

        assert!(app.fuzzy_finder.open);
        let displays: Vec<&str> = app
            .fuzzy_finder
            .items
            .iter()
            .map(|c| c.display.as_str())
            .collect();
        assert!(displays.contains(&"src/main.rs"));
        assert!(displays.contains(&"src/lib.rs"));
        assert!(displays.contains(&"tests/it.rs"));
    }

    #[test]
    fn fuzzy_finder_filters_by_query() {
        let dir = temp_project_for_fuzzy("filter");
        let buf: Box<dyn Buffer> = Box::new(core::PieceTableBuffer::from_bytes(b"x".to_vec()));
        let mut app =
            EditorApp::new_with_documents(vec![core::Document::new(buf)], core::Config::default());
        app.documents[0].project = core::Project::from_path(&dir);

        app.handle_event(EditorEvent::FuzzyFinder(Some("lib".to_string())));
        assert_eq!(app.fuzzy_finder.filtered.len(), 1);
        let (idx, _) = app.fuzzy_finder.filtered[0];
        assert!(app.fuzzy_finder.items[idx].display.contains("lib.rs"));
    }

    #[test]
    fn fuzzy_finder_opens_selected_file() {
        let dir = temp_project_for_fuzzy("open");
        let buf: Box<dyn Buffer> = Box::new(core::PieceTableBuffer::from_bytes(b"x".to_vec()));
        let mut app =
            EditorApp::new_with_documents(vec![core::Document::new(buf)], core::Config::default());
        app.documents[0].project = core::Project::from_path(&dir);

        app.handle_event(EditorEvent::FuzzyFinder(Some("main".to_string())));
        assert_eq!(app.documents.len(), 1);
        app.handle_event(EditorEvent::FuzzyFinderExecute);

        assert!(!app.fuzzy_finder.open);
        assert_eq!(app.documents.len(), 2);
        assert_eq!(
            app.active_doc()
                .path()
                .as_ref()
                .unwrap()
                .file_name()
                .unwrap(),
            "main.rs"
        );
    }

    #[test]
    fn fuzzy_finder_closes_on_esc() {
        let dir = temp_project_for_fuzzy("close");
        let buf: Box<dyn Buffer> = Box::new(core::PieceTableBuffer::from_bytes(b"x".to_vec()));
        let mut app =
            EditorApp::new_with_documents(vec![core::Document::new(buf)], core::Config::default());
        app.documents[0].project = core::Project::from_path(&dir);

        app.handle_event(EditorEvent::FuzzyFinder(None));
        assert!(app.fuzzy_finder.open);
        app.handle_event(EditorEvent::FuzzyFinderClose);
        assert!(!app.fuzzy_finder.open);
    }

    #[test]
    fn fuzzy_finder_swallows_typed_keys() {
        let buf: Box<dyn Buffer> = Box::new(core::PieceTableBuffer::from_bytes(b"hello".to_vec()));
        let mut app =
            EditorApp::new_with_documents(vec![core::Document::new(buf)], core::Config::default());
        app.handle_event(EditorEvent::FuzzyFinder(None));
        assert!(app.fuzzy_finder.open);

        let ctx = egui::Context::default();
        ctx.input_mut(|i| i.events.push(egui::Event::Text("a".to_string())));
        app.handle_input(&ctx);
        assert_eq!(app.active_buffer().to_bytes(), b"hello");
    }

    #[test]
    fn command_palette_swallows_typed_keys() {
        let buf: Box<dyn Buffer> = Box::new(core::PieceTableBuffer::from_bytes(b"hello".to_vec()));
        let mut app =
            EditorApp::new_with_documents(vec![core::Document::new(buf)], core::Config::default());
        app.handle_event(EditorEvent::CommandPalette(None));
        assert!(app.command_palette.open);

        let ctx = egui::Context::default();
        ctx.input_mut(|i| i.events.push(egui::Event::Text("a".to_string())));
        app.handle_input(&ctx);
        assert_eq!(app.active_buffer().to_bytes(), b"hello");
    }

    // ----- git gutter -----

    fn temp_git_repo(
        name: &str,
        file_name: &str,
        content: &str,
    ) -> (std::path::PathBuf, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "the_editor_gui_git_{}_{}",
            std::process::id(),
            name
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        run_git(&dir, &["init", "-q"]);
        run_git(&dir, &["config", "user.email", "test@example.com"]);
        run_git(&dir, &["config", "user.name", "Test"]);
        let path = dir.join(file_name);
        std::fs::write(&path, content).unwrap();
        run_git(&dir, &["add", file_name]);
        run_git(&dir, &["commit", "-q", "-m", "initial"]);
        (dir, path)
    }

    fn run_git(dir: &std::path::Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .expect("git should be installed");
        assert!(status.success());
    }

    #[test]
    fn git_gutter_initializes_unchanged() {
        let (_dir, path) = temp_git_repo("unchanged", "a.txt", "line1\nline2\n");
        let buf: Box<dyn Buffer> =
            Box::new(core::PieceTableBuffer::from_path(path.clone()).unwrap());
        let app = EditorApp::new_with_documents(
            vec![core::Document::new_with_config(
                buf,
                &core::Config::default(),
            )],
            core::Config::default(),
        );
        assert!(app.active_doc().git_gutter.enabled());
        assert_eq!(
            app.active_doc().git_gutter.status(0),
            core::LineStatus::Unchanged
        );
        assert_eq!(
            app.active_doc().git_gutter.status(1),
            core::LineStatus::Unchanged
        );
    }

    #[test]
    fn git_gutter_reflects_edits_after_refresh() {
        let (_dir, path) = temp_git_repo("edit", "a.txt", "line1\nline2\n");
        let buf: Box<dyn Buffer> =
            Box::new(core::PieceTableBuffer::from_path(path.clone()).unwrap());
        let mut app = EditorApp::new_with_documents(
            vec![core::Document::new_with_config(
                buf,
                &core::Config::default(),
            )],
            core::Config::default(),
        );
        app.handle_event(EditorEvent::Move(core::Movement::LineEnd));
        app.handle_event(EditorEvent::Insert('\n'));
        app.handle_event(EditorEvent::Insert('n'));
        app.handle_event(EditorEvent::Insert('e'));
        app.handle_event(EditorEvent::Insert('w'));
        app.handle_event(EditorEvent::RefreshGitGutter);
        assert_eq!(
            app.active_doc().git_gutter.status(1),
            core::LineStatus::Added
        );
    }

    #[test]
    fn git_gutter_hunk_navigation_jumps_cursor() {
        let (_dir, path) = temp_git_repo("hunk", "a.txt", "aaa\nbbb\nccc\n");
        let buf: Box<dyn Buffer> =
            Box::new(core::PieceTableBuffer::from_path(path.clone()).unwrap());
        let mut app = EditorApp::new_with_documents(
            vec![core::Document::new_with_config(
                buf,
                &core::Config::default(),
            )],
            core::Config::default(),
        );
        // Modify the first line, then move the cursor down.
        app.handle_event(EditorEvent::Insert('z'));
        app.handle_event(EditorEvent::Move(core::Movement::Down));
        app.handle_event(EditorEvent::RefreshGitGutter);
        app.handle_event(EditorEvent::PrevHunk);
        let (_, line) = app
            .active_buffer()
            .pos_to_linecol(app.active_buffer().cursor())
            .unwrap();
        assert_eq!(line, 0);
    }
}
