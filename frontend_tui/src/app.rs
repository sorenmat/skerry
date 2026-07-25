//! The `App` — owns the `Buffer`, view state, and event handling.
//!
//! Split into its own module so `main.rs` stays a thin entry point and
//! so we can unit-test the event-handling logic without spinning up a
//! real terminal.

use std::time::{Duration, Instant};

use core::{Buffer, BytePos, Document, EditorEvent, Movement, Search, Selection, SyntaxEngine};
use crossterm::event::{self as cxevent, Event};
use ratatui::Terminal;

/// The application. One `App` per running frontend instance.
pub struct App {
    /// All open documents. `active` indexes into this Vec; all
    /// events flow through `active_doc_mut()`.
    pub documents: Vec<Document>,
    /// Index of the currently focused document into `documents`.
    pub active: usize,
    pub should_quit: bool,
    pub status_message: Option<String>,
    /// Last rendered viewport height in lines (set during `render`).
    /// Window-global — comes from the terminal size.
    pub viewport_height: u16,
    /// Find state: query, match list, current match, bar visibility.
    /// Operates on the active document's buffer.
    pub search: Search,
    /// Close-on-dirty prompt. `Some` while the prompt is up — the
    /// renderer draws the dialog overlay and the input loop intercepts
    /// keys instead of forwarding them to `handle_event`.
    pub close_confirm: Option<CloseConfirm>,
    /// Go-to-line dialog. `Some` while the prompt is up. The user types
    /// a 1-based line number; Enter jumps, Esc cancels.
    pub go_to_line_dialog: Option<GoToLineDialog>,
    pub rename_dialog: Option<RenameDialog>,
    /// Global syntax highlighting engine.
    pub syntax: SyntaxEngine,
    /// Whether the project file-tree sidebar is visible.
    pub project_tree_open: bool,
    /// The active document's project tree, including expansion state.
    pub project_tree: Option<core::ProjectTree>,
    /// Index of the selected row in the visible (flattened) project tree.
    pub project_tree_selected: usize,
    /// Last rendered width of the project-tree sidebar in terminal
    /// columns. Used to offset mouse clicks in the editor content area.
    pub tree_width: u16,
    /// Project-wide search dialog state.
    pub project_search: ProjectSearch,
    /// Fuzzy file finder state.
    pub fuzzy_finder: FuzzyFinder,
    /// Command palette state.
    pub command_palette: CommandPalette,
    /// LSP completion popup state.
    pub lsp_completion: LspCompletionState,
    /// LSP hover state.
    pub lsp_hover: LspHoverState,
    /// LSP go-to-definition request state.
    pub lsp_definition: LspDefinitionState,
    pub pending_format_save: bool,
    /// Persistent user configuration / session state.
    pub config: core::Config,
    /// Time of the most recent buffer-modifying edit. Used by auto-save
    /// to decide when the user has been idle long enough to save.
    pub last_edit_time: Instant,
    /// File-system watcher for externally changed files. `None` if the
    /// watcher could not be initialized on this platform.
    pub file_watcher: Option<core::FileWatcher>,
    /// LSP manager shared with the GUI frontend.
    pub lsp_manager: core::lsp::LspManager,
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
    /// Whether the replace field is focused (Tab toggles).
    pub replace_focused: bool,
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

/// State for the LSP completion popup.
#[derive(Debug, Clone, Default)]
pub struct LspCompletionState {
    /// Whether the completion popup is visible.
    pub open: bool,
    /// Completion items from the language server.
    pub items: Vec<lsp_types::CompletionItem>,
    /// Index of the selected item.
    pub selected: usize,
    /// True when a request has been fired but no response has arrived.
    pub pending: bool,
}

/// State for an LSP hover response shown in the status bar.
#[derive(Debug, Clone, Default)]
pub struct LspHoverState {
    /// True when a request has been fired but no response has arrived.
    pub pending: bool,
    /// Cursor position the hover request was issued for.
    pub request_pos: Option<lsp_types::Position>,
}

/// State for an in-flight LSP go-to-definition request.
#[derive(Debug, Clone, Default)]
pub struct LspDefinitionState {
    /// True when a request has been fired but no response has arrived.
    pub pending: bool,
    /// Cursor position the definition request was issued for.
    pub request_pos: Option<lsp_types::Position>,
}

/// The three choices offered when closing a dirty document. Stored on
/// [`CloseConfirm::choice`]; Tab / Shift+Tab / Left / Right cycle the
/// focused option, and Enter / `y` activates it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseChoice {
    /// Save the buffer first, then close. No-op if the buffer has no
    /// path (treated as cancel + status message).
    Save,
    /// Discard unsaved edits and close.
    Discard,
    /// Cancel — leave the document open, drop the prompt.
    Cancel,
}

/// State for the close-on-dirty dialog. Captured at prompt-open time so
/// the close target doesn't shift if the user changes the active doc
/// while the prompt is up (defensive — they can't via normal key flow,
/// but this guarantees the close happens on the right doc).
#[allow(dead_code)] // `doc_index` reserved for future prompt+tab-switch
                    // interleave; current modal intercepts prevent active
                    // changes while the prompt is up.
pub struct CloseConfirm {
    /// Index of the document the close was requested against.
    pub doc_index: usize,
    /// Currently focused choice. Cycles Save → Discard → Cancel → Save.
    pub choice: CloseChoice,
}

/// State for the go-to-line text-input dialog. The user types a
/// 1-based line number into `query`; Enter jumps via
/// [`App::submit_go_to_line_dialog`].
pub struct GoToLineDialog {
    pub query: String,
}

/// Rename-symbol prompt state. Pre-filled with the word under the cursor.
/// The TUI doesn't render an interactive rename widget yet (the GUI does);
/// the dialog struct exists so the event plumbing is in place.
#[allow(dead_code)]
pub struct RenameDialog {
    pub new_name: String,
}

impl App {
    /// Create an `App` around a single buffer. Wraps the buffer in a
    /// one-element document list. Test convenience — production code
    /// uses `new_with_documents` so the multi-file CLI path stays
    /// explicit.
    #[allow(dead_code)]
    pub fn new(buffer: Box<dyn Buffer>) -> Self {
        Self::new_with_documents(vec![Document::new(buffer)], core::Config::default())
    }

    /// Create an `App` around a pre-built list of documents.
    /// The first document becomes active.
    pub fn new_with_documents(mut documents: Vec<Document>, config: core::Config) -> Self {
        assert!(!documents.is_empty(), "App needs at least one document");
        for doc in &mut documents {
            config.apply_document_defaults(&mut doc.view);
        }
        let mut app = Self {
            documents,
            active: 0,
            should_quit: false,
            status_message: None,
            viewport_height: 0,
            search: Search::new(),
            close_confirm: None,
            go_to_line_dialog: None,
            rename_dialog: None,
            syntax: SyntaxEngine::default_dark(),
            project_tree_open: config.project_tree_open.unwrap_or(true),
            project_tree: None,
            project_tree_selected: 0,
            tree_width: 0,
            project_search: ProjectSearch::default(),
            fuzzy_finder: FuzzyFinder::default(),
            command_palette: CommandPalette::default(),
            lsp_completion: LspCompletionState::default(),
            lsp_hover: LspHoverState::default(),
            lsp_definition: LspDefinitionState::default(),
            pending_format_save: false,
            config,
            last_edit_time: Instant::now(),
            file_watcher: core::FileWatcher::new().ok(),
            lsp_manager: core::lsp::LspManager::new(),
        };
        if let Some(theme) = app.config.theme.clone() {
            app.syntax.set_theme_by_name(&theme);
        }
        app.refresh_project_tree();
        app.sync_watcher();
        app
    }

    /// Index of the currently focused document.
    #[allow(dead_code)] // API surface for upcoming multi-buffer events; not yet bound in TUI.
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

    /// Notify the LSP manager that the active document was opened.
    pub fn lsp_open_active(&mut self) {
        let doc = &self.documents[self.active];
        let Some(uri) = doc.uri() else { return };
        let Some(lang) = doc.language_id() else {
            return;
        };
        if !core::lsp::LspManager::is_language_supported(lang) {
            return;
        }
        let root = doc.lsp_root_uri().unwrap_or_else(|| uri.clone());
        let text = doc.text();
        self.lsp_manager
            .open_document(uri, lang.to_string(), root, text);
    }

    /// Queue an LSP change notification for the active document.
    pub fn lsp_change_active(&mut self) {
        let Some(uri) = self.active_doc().uri() else {
            return;
        };
        let text = self.active_doc().text();
        self.lsp_manager.change_document(uri, text);
    }

    /// Notify the LSP manager that the active document was saved.
    pub fn lsp_save_active(&mut self) {
        let Some(uri) = self.active_doc().uri() else {
            return;
        };
        self.lsp_manager.save_document(&uri);
    }

    /// Notify the LSP manager that the active document was closed.
    pub fn lsp_close_active(&mut self) {
        let Some(uri) = self.active_doc().uri() else {
            return;
        };
        self.lsp_manager.close_document(&uri);
    }

    /// Open or focus the file containing an LSP definition target and
    /// move the cursor to the target position.
    fn jump_to_lsp_location(&mut self, target_uri: url::Url, target_pos: lsp_types::Position) {
        let active_uri = self.active_doc().uri();
        if active_uri.as_ref() == Some(&target_uri) {
            self.set_cursor_lsp_position(target_pos);
            return;
        }
        if let Ok(path) = target_uri.to_file_path() {
            if let Some(idx) = self
                .documents
                .iter()
                .position(|d| d.uri().as_ref() == Some(&target_uri))
            {
                self.active = idx;
                self.set_cursor_lsp_position(target_pos);
            } else {
                self.open_or_switch_to_path(&path);
                if self.active_doc().uri().as_ref() == Some(&target_uri) {
                    self.set_cursor_lsp_position(target_pos);
                } else {
                    self.status_message = Some("LSP: could not open definition file.".to_string());
                }
            }
        } else {
            self.status_message = Some("LSP: definition target is not a local file.".to_string());
        }
    }

    /// Move the cursor to an LSP position in the active document and
    /// scroll the viewport so the cursor stays visible.
    fn set_cursor_lsp_position(&mut self, pos: lsp_types::Position) {
        let line = pos.line as usize;
        let col = pos.character as usize;
        let byte_pos = core::clamped_line_charcol_to_pos(self.active_buffer(), line, col);
        self.active_buffer_mut().set_cursor(byte_pos);
        self.active_buffer_mut()
            .set_selection(core::Selection::collapsed(byte_pos));
        self.adjust_viewport(self.viewport_height);
    }

    /// Insert the selected LSP completion item at the cursor and close
    /// the completion popup.
    pub fn apply_lsp_completion(&mut self) {
        if !self.lsp_completion.open || self.lsp_completion.items.is_empty() {
            self.lsp_completion.open = false;
            return;
        }
        let item = self.lsp_completion.items[self.lsp_completion.selected].clone();
        let text = item.insert_text.as_ref().unwrap_or(&item.label).clone();
        let selection = self.active_buffer().selection().range();
        let pos = selection.start;
        match self.active_buffer_mut().replace(selection, &text) {
            Ok(_) => {
                let new_pos = pos + text.len();
                self.active_buffer_mut().set_cursor(new_pos);
                self.active_buffer_mut()
                    .set_selection(core::Selection::collapsed(new_pos));
                self.status_message = Some(format!("Completed: {}", item.label));
            }
            Err(e) => {
                self.status_message = Some(format!("Completion insert error: {e}"));
            }
        }
        self.lsp_completion.open = false;
        self.lsp_completion.pending = false;
    }

    /// Shortcut for `self.active_doc().buffer` as `&dyn Buffer`.
    pub fn active_buffer(&self) -> &dyn Buffer {
        &*self.documents[self.active].buffer
    }

    /// Shortcut for `self.active_doc_mut().buffer` as `&mut dyn Buffer`.
    pub fn active_buffer_mut(&mut self) -> &mut dyn Buffer {
        &mut *self.documents[self.active].buffer
    }

    /// Return the active document URI and current cursor position in
    /// LSP coordinates, if both are available.
    fn lsp_cursor_position(&self) -> Option<(url::Url, lsp_types::Position)> {
        let uri = self.active_doc().uri()?;
        let (line, col) = self
            .active_buffer()
            .pos_to_linecol(self.active_buffer().cursor())?;
        let pos = lsp_types::Position {
            line: line as u32,
            character: col as u32,
        };
        Some((uri, pos))
    }

    /// The identifier-like word under (or just before) the cursor.
    fn word_at_cursor(&self) -> Option<String> {
        let pos = self.active_buffer().cursor();
        let (line, byte_col) = self.active_buffer().pos_to_linecol(pos).unwrap_or((0, 0));
        let text = self.active_buffer().line_text(line)?.into_owned();
        let chars: Vec<char> = text.chars().collect();
        if chars.is_empty() {
            return None;
        }
        let is_id = |c: char| c.is_alphanumeric() || c == '_';
        let mut col = byte_col.min(chars.len());
        if col > 0 && col <= chars.len() && is_id(chars[col - 1]) {
            col -= 1;
        }
        if col >= chars.len() || !is_id(chars[col]) {
            return None;
        }
        let mut start = col;
        while start > 0 && is_id(chars[start - 1]) {
            start -= 1;
        }
        let mut end = col;
        while end < chars.len() && is_id(chars[end]) {
            end += 1;
        }
        Some(chars[start..end].iter().collect())
    }

    /// If a rename result has landed for the active doc, apply it.
    pub fn apply_pending_rename(&mut self) {
        let Some(uri) = self.active_doc().uri() else {
            return;
        };
        let Some(edit) = self.lsp_manager.take_rename_result(&uri) else {
            return;
        };
        let count = self.apply_workspace_edit(&edit);
        self.status_message = Some(format!("Renamed ({count} edits)."));
    }

    fn apply_workspace_edit(&mut self, edit: &lsp_types::WorkspaceEdit) -> usize {
        let Some(changes) = &edit.changes else {
            return 0;
        };
        let mut total = 0;
        for (uri, edits) in changes {
            if self.active_doc().uri().as_ref() != Some(uri) {
                continue;
            }
            let mut byte_edits: Vec<(std::ops::Range<usize>, String)> = Vec::new();
            for te in edits {
                if let Some(range) = self.lsp_range_to_byte_range(&te.range) {
                    byte_edits.push((range, te.new_text.clone()));
                }
            }
            byte_edits.sort_by_key(|e| std::cmp::Reverse(e.0.start));
            for (range, new_text) in byte_edits {
                if self.active_buffer_mut().replace(range, &new_text).is_ok() {
                    total += 1;
                }
            }
            self.active_doc_mut().syntax.invalidate();
        }
        total
    }

    fn lsp_range_to_byte_range(&self, range: &lsp_types::Range) -> Option<std::ops::Range<usize>> {
        let buf = self.active_buffer();
        let start_line = range.start.line as usize;
        let end_line = range.end.line as usize;
        let start = buf.line_byte_range(start_line)?;
        let end = buf.line_byte_range(end_line)?;
        let start_text = buf.line_text(start_line)?;
        let end_text = buf.line_text(end_line)?;
        let start_byte = start.start + core::char_col_to_byte_col(&start_text, range.start.character as usize);
        let end_byte = end.start + core::char_col_to_byte_col(&end_text, range.end.character as usize);
        Some(start_byte..end_byte)
    }

    /// Select the next occurrence of the word under the primary cursor
    /// (or the selected text). Adds a new selection at the next match.
    fn select_next_occurrence(&mut self) {
        let buf = self.active_buffer();
        let primary = buf.selection();
        let needle = if primary.is_collapsed() {
            self.word_at_cursor()
        } else {
            buf.slice(primary.range())
        };
        let Some(needle) = needle else {
            self.status_message = Some("No word to search.".to_string());
            return;
        };
        if needle.is_empty() {
            return;
        }
        let needle_bytes = needle.as_bytes();
        let total = buf.len();
        let last_head = buf.selections().last().map(|s| s.head).unwrap_or(0);
        let search_start = last_head + needle.len();
        let existing: Vec<std::ops::Range<usize>> =
            buf.selections().iter().map(|s| s.range()).collect();
        let content = String::from_utf8_lossy(&buf.to_bytes()).to_string();
        let search = |from: usize| -> Option<usize> {
            if from >= content.len() {
                return None;
            }
            content[from..].find(&needle).map(|p| p + from)
        };
        let mut found = search(search_start);
        if found.is_none() && search_start > 0 {
            found = search(0);
        }
        while let Some(pos) = found {
            let candidate = pos..(pos + needle_bytes.len());
            let overlaps = existing.iter().any(|r| {
                candidate.start < r.end && candidate.end > r.start
            });
            if !overlaps && pos != last_head {
                let mut sels: Vec<Selection> =
                    self.active_buffer().selections().to_vec();
                sels.push(Selection {
                    anchor: candidate.start,
                    head: candidate.end,
                });
                sels.sort_by_key(|s| s.head);
                self.active_buffer_mut().set_selections(sels);
                return;
            }
            found = search(pos + needle_bytes.len());
            if pos + needle_bytes.len() >= total {
                break;
            }
        }
        self.status_message = Some("No more occurrences.".to_string());
    }

    pub fn format_active_document(&mut self) {
        let Some(uri) = self.active_doc().uri() else {
            return;
        };
        if self.lsp_manager.supports_formatting(&uri) {
            self.lsp_manager.request_formatting(&uri);
            self.status_message = Some("Formatting...".to_string());
        } else {
            self.try_external_format();
        }
    }

    /// Run the configured external formatter for the active document.
    fn try_external_format(&mut self) -> bool {
        let Some(lang) = self.active_doc().language_id() else {
            self.status_message = Some("Formatting not available.".to_string());
            return false;
        };
        let cmd = match core::formatter_for_language(&self.config, lang) {
            Some(c) => c,
            None => {
                self.status_message =
                    Some(format!("No formatter configured for {lang}."));
                return false;
            }
        };
        let input = String::from_utf8_lossy(&self.active_buffer().to_bytes()).to_string();
        let Some(formatted) = core::run_external_formatter(cmd, &input) else {
            self.status_message = Some("Formatter failed or no changes.".to_string());
            return false;
        };
        // Save cursor as line+col, restore after the replace.
        let (cursor_line, cursor_col) = self
            .active_buffer()
            .pos_to_linecol(self.active_buffer().cursor())
            .unwrap_or((0, 0));
        let len = self.active_buffer().len();
        let _ = self.active_buffer_mut().replace(0..len, &formatted);
        let new_pos = self
            .active_buffer()
            .linecol_to_pos(cursor_line, cursor_col)
            .unwrap_or_else(|| self.active_buffer().len());
        self.active_buffer_mut().set_cursor(new_pos);
        self.active_doc_mut().syntax.invalidate();
        self.status_message = Some("Formatted.".to_string());
        true
    }

    pub fn apply_pending_format(&mut self) {
        let Some(uri) = self.active_doc().uri() else {
            return;
        };
        let Some(edits) = self.lsp_manager.take_formatting_result(&uri) else {
            return;
        };
        if edits.is_empty() {
            self.pending_format_save = false;
            return;
        }
        let mut byte_edits: Vec<(std::ops::Range<usize>, String)> = Vec::new();
        for te in &edits {
            if let Some(range) = self.lsp_range_to_byte_range(&te.range) {
                byte_edits.push((range, te.new_text.clone()));
            }
        }
        byte_edits.sort_by_key(|e| std::cmp::Reverse(e.0.start));
        for (range, new_text) in byte_edits {
            let _ = self.active_buffer_mut().replace(range, &new_text);
        }
        self.active_doc_mut().syntax.invalidate();
        if self.pending_format_save {
            self.pending_format_save = false;
            if self.active_buffer_mut().save().is_ok() {
                self.active_doc_mut().refresh_git_gutter();
                self.lsp_save_active();
                self.status_message = Some("Saved + formatted.".to_string());
            } else {
                self.status_message = Some("Formatted (re-save failed).".to_string());
            }
        } else {
            self.status_message = Some("Formatted.".to_string());
        }
    }
    #[allow(dead_code)] // API surface for upcoming multi-buffer events; not yet bound in TUI.
    pub fn doc_count(&self) -> usize {
        self.documents.len()
    }

    /// Run the event loop until `should_quit` is set.
    ///
    /// Clipboard I/O lives in this loop, not in `handle_event`, because
    /// it needs OS-level access (`arboard`). When we see a clipboard
    /// shortcut (Ctrl+C / Ctrl+X), we copy the selection text to the
    /// system clipboard; Ctrl+X additionally fires
    /// [`EditorEvent::DeleteSelection`] through `handle_event` so the
    /// buffer stays consistent. Ctrl+V is handled the same way:
    /// read clipboard, then fire [`EditorEvent::Paste`] with the text.
    pub fn run<B: ratatui::backend::Backend>(
        &mut self,
        terminal: &mut Terminal<B>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Create the clipboard handle once. arboard initialises a
        // platform-specific backend (X11/Wayland/Cocoa/Win32) on
        // construction; doing it lazily here means we don't pay the
        // cost (or fail) when the user never invokes copy/paste.
        let mut clipboard = match arboard::Clipboard::new() {
            Ok(cb) => Some(cb),
            Err(e) => {
                self.status_message = Some(format!("clipboard unavailable: {e}"));
                None
            }
        };

        loop {
            self.lsp_manager.poll();
            self.lsp_open_active();
            self.lsp_change_active();
            if let Some(status) = self.lsp_manager.take_status() {
                self.status_message = Some(status);
            }

            // Update the LSP completion popup if a response just arrived.
            if self.lsp_completion.pending {
                if let Some(uri) = self.active_doc().uri() {
                    if let Some(list) = self.lsp_manager.completion_result(&uri) {
                        self.lsp_completion.items = list.items.clone();
                        self.lsp_completion.pending = false;
                    }
                } else {
                    self.lsp_completion.pending = false;
                }
            }

            // Apply rename / format results if they just arrived.
            self.apply_pending_rename();
            self.apply_pending_format();

            // Update the LSP hover status if a response just arrived.
            if self.lsp_hover.pending {
                if let Some((uri, _)) = self.lsp_cursor_position() {
                    if let Some(request_pos) = self.lsp_hover.request_pos {
                        if let Some(hover) = self.lsp_manager.hover_result(&uri, request_pos) {
                            let text = core::lsp::hover_text(hover);
                            self.status_message =
                                Some(text.lines().next().unwrap_or("").to_string());
                            self.lsp_hover.pending = false;
                            self.lsp_hover.request_pos = None;
                        }
                    }
                } else {
                    self.lsp_hover.pending = false;
                    self.lsp_hover.request_pos = None;
                }
            }

            // Apply a go-to-definition response if one just arrived.
            if self.lsp_definition.pending {
                if let Some((uri, current_pos)) = self.lsp_cursor_position() {
                    if let Some(request_pos) = self.lsp_definition.request_pos {
                        if let Some(resp) = self.lsp_manager.definition_result(&uri, request_pos) {
                            self.lsp_definition.pending = false;
                            self.lsp_definition.request_pos = None;
                            if request_pos != current_pos {
                                self.status_message =
                                    Some("LSP: cursor moved; definition ignored.".to_string());
                            } else if let Some((target_uri, target_pos)) =
                                core::lsp::LspManager::definition_target(resp)
                            {
                                self.jump_to_lsp_location(target_uri, target_pos);
                            } else {
                                self.status_message = Some("LSP: no definition found.".to_string());
                            }
                        }
                    }
                } else {
                    self.lsp_definition.pending = false;
                    self.lsp_definition.request_pos = None;
                }
            }

            // Render.
            terminal.draw(|frame| crate::ui::render(frame, self))?;

            // Poll for events.
            if cxevent::poll(Duration::from_millis(100))? {
                match cxevent::read()? {
                    Event::Key(key) => {
                        // Modal prompts (close-confirm, open-file dialog)
                        // intercept keys before they reach translate_key.
                        // We don't want a Ctrl+W inside the open-file
                        // dialog to bounce back into close-confirm, and we
                        // don't want printable chars inside the dialog to
                        // land in the buffer.
                        if self.dispatch_modal_key(key) {
                            continue;
                        }
                        // LSP completion popup intercepts navigation and
                        // insertion keys while it's open.
                        if self.lsp_completion.open {
                            use crossterm::event::KeyCode;
                            match key.code {
                                KeyCode::Esc => {
                                    self.lsp_completion.open = false;
                                    continue;
                                }
                                KeyCode::Up => {
                                    if !self.lsp_completion.items.is_empty() {
                                        self.lsp_completion.selected =
                                            self.lsp_completion.selected.saturating_sub(1);
                                    }
                                    continue;
                                }
                                KeyCode::Down => {
                                    if !self.lsp_completion.items.is_empty() {
                                        self.lsp_completion.selected =
                                            (self.lsp_completion.selected + 1)
                                                .min(self.lsp_completion.items.len() - 1);
                                    }
                                    continue;
                                }
                                KeyCode::Enter | KeyCode::Tab => {
                                    self.apply_lsp_completion();
                                    continue;
                                }
                                _ => {}
                            }
                            // Swallow printable keys so they don't leak
                            // into the buffer while the popup is open.
                            if matches!(key.code, KeyCode::Char(_)) {
                                continue;
                            }
                        }
                        // Clipboard shortcuts are intercepted before
                        // generic key translation because they need
                        // direct OS access.
                        if let Some(action) =
                            crate::event::classify_clipboard_key(key, self.active_buffer())
                        {
                            self.apply_clipboard_action(&mut clipboard, action);
                            continue;
                        }
                        if let Some(editor_event) = crate::event::translate_key(key, Some(self)) {
                            self.handle_event(editor_event);
                        }
                    }
                    Event::Mouse(mouse) => {
                        if let Some(editor_event) = crate::event::translate_mouse(mouse, self) {
                            self.handle_event(editor_event);
                        }
                    }
                    Event::Resize(_, _) => {
                        // Terminal resize is handled implicitly on the next
                        // render — ratatui recomputes layout. Nothing to do.
                    }
                    _ => {}
                }
            }

            // Auto-save idle dirty buffers each loop iteration. The poll
            // timeout keeps this check running ~10 times a second.
            self.auto_save();
            self.maybe_refresh_git_gutter();
            self.maybe_refresh_git_blame();

            // Check for files that changed externally.
            self.handle_external_changes();

            if self.should_quit {
                break;
            }
        }
        Ok(())
    }

    /// Perform an OS-clipboard action produced by
    /// [`crate::event::classify_clipboard_key`]. `clipboard` is an
    /// `Option` because clipboard initialisation can fail (e.g. on a
    /// headless Linux without a display server); when it's `None`,
    /// clipboard actions become no-ops but the buffer state for
    /// `Cut` is still updated (the user can still delete the
    /// selection, they just can't put it on the system clipboard).
    /// Intercept a key event when a modal prompt is open. Returns `true`
    /// when the event was consumed (caller should NOT forward it to
    /// `translate_key` / `handle_event`).
    ///
    /// Prompts today:
    /// - **close_confirm**: Tab/Shift+Tab/Left/Right cycle choice,
    ///   Enter confirms the focused choice, `y` confirms as Discard,
    ///   `n` and Esc cancel.
    /// - **go_to_line_dialog**: digits append, Backspace pops, Enter
    ///   jumps, Esc cancels.
    ///
    /// If both were somehow up (they shouldn't be — opening a dialog
    /// drops the other), close_confirm wins because it's tied to an
    /// irreversible action.
    fn dispatch_modal_key(&mut self, key: cxevent::KeyEvent) -> bool {
        use cxevent::{KeyCode, KeyModifiers};

        if self.close_confirm.is_some() {
            match (key.code, key.modifiers) {
                (KeyCode::Esc, _) | (KeyCode::Char('n'), _) | (KeyCode::Char('N'), _) => {
                    // Esc / n = cancel.
                    self.close_confirm = None;
                    self.status_message = Some("Close cancelled.".to_string());
                }
                (KeyCode::Tab, KeyModifiers::NONE)
                | (KeyCode::Right, _)
                | (KeyCode::Char('l'), KeyModifiers::NONE) => {
                    self.cycle_close_choice(1);
                }
                (KeyCode::BackTab, _)
                | (KeyCode::Left, _)
                | (KeyCode::Char('h'), KeyModifiers::NONE) => {
                    self.cycle_close_choice(-1);
                }
                (KeyCode::Enter, _) => {
                    self.confirm_close_choice();
                }
                (KeyCode::Char('y'), _) | (KeyCode::Char('Y'), _) => {
                    // One-key "yes, close it" — Discard.
                    self.close_confirm = None;
                    self.perform_close_active();
                }
                _ => {
                    // Eat everything else while the prompt is open so
                    // the buffer doesn't receive stray keystrokes.
                }
            }
            return true;
        }

        if self.go_to_line_dialog.is_some() {
            match key.code {
                KeyCode::Esc => self.cancel_go_to_line_dialog(),
                KeyCode::Enter => self.submit_go_to_line_dialog(),
                KeyCode::Backspace => self.pop_go_to_line_query(),
                KeyCode::Char(c)
                    if !key.modifiers.contains(KeyModifiers::CONTROL)
                        && !key.modifiers.contains(KeyModifiers::ALT) =>
                {
                    // Only digits make sense for a line number.
                    self.push_go_to_line_query(c);
                }
                _ => {}
            }
            return true;
        }

        false
    }

    fn apply_clipboard_action(
        &mut self,
        clipboard: &mut Option<arboard::Clipboard>,
        action: crate::event::ClipboardAction,
    ) {
        match action {
            crate::event::ClipboardAction::Copy(text) => {
                if let Some(cb) = clipboard.as_mut() {
                    if let Err(e) = cb.set_text(text) {
                        self.status_message = Some(format!("clipboard copy failed: {e}"));
                    }
                } else {
                    self.status_message = Some("clipboard unavailable; copy skipped".into());
                }
            }
            crate::event::ClipboardAction::Cut(text) => {
                if let Some(cb) = clipboard.as_mut() {
                    if let Err(e) = cb.set_text(text) {
                        self.status_message = Some(format!("clipboard cut failed: {e}"));
                    }
                }
                // The buffer side of cut still applies even if the
                // clipboard write failed — the user intended to remove
                // the selected text.
                self.handle_event(EditorEvent::DeleteSelection);
            }
            crate::event::ClipboardAction::Paste => {
                if let Some(cb) = clipboard.as_mut() {
                    match cb.get_text() {
                        Ok(text) => {
                            if !text.is_empty() {
                                self.handle_event(EditorEvent::Paste(text));
                            }
                        }
                        Err(e) => {
                            self.status_message = Some(format!("clipboard paste failed: {e}"));
                        }
                    }
                } else {
                    self.status_message = Some("clipboard unavailable; paste skipped".into());
                }
            }
        }
    }

    /// Apply an `EditorEvent` to the buffer / app state.
    pub fn handle_event(&mut self, event: EditorEvent) {
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
                | EditorEvent::RenameApply { .. }
                | EditorEvent::FormatDocument
        );
        // Capture the cursor BEFORE the edit runs so we know which line
        // was touched. Used to invalidate only that line's cached syntax
        // segments (and those below) instead of nuking the whole cache.
        let edit_start_line = if modifies_buffer {
            self.active_buffer()
                .pos_to_linecol(self.active_buffer().cursor())
                .map(|(l, _)| l)
        } else {
            None
        };
        // Undo/Redo/Replace can move/delete content across arbitrary line
        // ranges, so they get the conservative full-cache wipe. Localized
        // edits (inserts, deletes, paste, move/duplicate line) only affect
        // the edit line downwards — those get surgical invalidation.
        let full_invalidate = matches!(
            &event,
            EditorEvent::Undo | EditorEvent::Redo | EditorEvent::ReplaceAll
        );
        // Snapshot the buffer length before a localized edit so we can
        // derive the tree-sitter InputEdit delta afterward. Only needed
        // for localized edits — Undo/Redo/Replace re-parse fully.
        let ts_pre_edit = if modifies_buffer && !full_invalidate {
            let pos = self.active_buffer().cursor();
            let (line, col) = self.active_buffer().pos_to_linecol(pos).unwrap_or((0, 0));
            Some((pos, self.active_buffer().len(), line, col))
        } else {
            None
        };
        match event {
            EditorEvent::Insert(ch) => {
                // Auto-pairing: only for single-cursor. With multi-cursor,
                // skip auto-pairing and insert plainly at each.
                let single_cursor = self.active_buffer().selections().len() == 1
                    && self.active_buffer().selection().is_collapsed();
                if single_cursor {
                    let pos = self.active_buffer().cursor();
                    if let Some(_open) = core::matching_open(ch) {
                        if core::char_after(self.active_buffer(), pos) == Some(ch) {
                            let new_pos = core::move_right_by_char(self.active_buffer(), pos);
                            self.active_buffer_mut().set_cursor(new_pos);
                            self.active_buffer_mut()
                                .set_selection(Selection::collapsed(new_pos));
                            return;
                        }
                    }
                    if let Some(close) = core::matching_close(ch) {
                        self.insert_paired(ch, close);
                        return;
                    }
                }
                // Auto-indent: pressing Enter copies the current line's
                // leading whitespace to the new line, adding one indent
                // level if the line ends with {, (, [, or =>.
                if ch == '\n' && single_cursor {
                    let pos = self.active_buffer().cursor();
                    let (line, _) = self.active_buffer().pos_to_linecol(pos).unwrap_or((0, 0));
                    let v = &self.active_doc().view;
                    let indent = core::auto_indent(
                        self.active_buffer(),
                        line,
                        v.use_spaces,
                        v.tab_width,
                    );
                    self.insert_text(&format!("\n{indent}"));
                    return;
                }
                self.insert_text(&ch.to_string());
            }
            EditorEvent::InsertTab => {
                // First try snippet expansion.
                if self.active_buffer().selection().is_collapsed()
                    && self.active_buffer().selections().len() == 1
                {
                    let pos = self.active_buffer().cursor();
                    let (line, byte_col) = self
                        .active_buffer()
                        .pos_to_linecol(pos)
                        .unwrap_or((0, 0));
                    if let Some(line_text) = self.active_buffer().line_text(line) {
                        let line_text = line_text.into_owned();
                        if let Some((trigger, range)) = core::snippet_trigger_at_cursor(&line_text, byte_col) {
                            if let Some(body) = self.config.snippets.get(&trigger) {
                                let (expanded, cursor_offset) = core::expand_snippet(body);
                                let line_start = self.active_buffer().line_byte_range(line).unwrap_or(0..0).start;
                                let trigger_start = line_start + range.start;
                                let trigger_end = line_start + range.end;
                                if self.active_buffer_mut().replace(trigger_start..trigger_end, &expanded).is_ok() {
                                    let new_cursor = trigger_start + cursor_offset;
                                    self.active_buffer_mut().set_cursor(new_cursor);
                                    self.active_doc_mut().syntax.invalidate();
                                    return;
                                }
                            }
                        }
                    }
                }
                // Normal tab: insert indent.
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
            EditorEvent::DeleteLeft => {
                // Selection-aware: delete the selection first, fall back
                // to a one-char backspace if nothing is selected.
                if self.delete_selection_if_any() {
                    return;
                }
                let pos = self.active_buffer().cursor();
                if pos > 0 {
                    // Auto-pair backspace: if the chars before and after
                    // the cursor form a matching pair, delete both.
                    let before = core::char_before(self.active_buffer(), pos);
                    let after = core::char_after(self.active_buffer(), pos);
                    if let (Some(open), Some(close)) = (before, after) {
                        if core::matching_close(open) == Some(close) {
                            let left = core::move_left_by_char(self.active_buffer(), pos);
                            match self.active_buffer_mut().delete(left..(pos + 1)) {
                                Ok(new_pos) => {
                                    self.active_buffer_mut().set_cursor(new_pos);
                                    self.active_buffer_mut()
                                        .set_selection(Selection::collapsed(new_pos));
                                }
                                Err(e) => self.status_message =
                                    Some(format!("delete error: {e}")),
                            }
                            return;
                        }
                    }
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
                // Selection-aware: if anything is selected, just delete it.
                if self.delete_selection_if_any() {
                    return;
                }
                let pos = self.active_buffer().cursor();
                if pos == 0 {
                    return;
                }
                let target = self.skip_word_left_from(pos);
                if target == pos {
                    return; // already at word boundary, nothing to do
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
                let target = self.skip_word_right_from(pos);
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
                self.active_doc_mut().view.scroll_x_cols =
                    self.active_doc().view.scroll_x_cols.saturating_sub(1);
            }
            EditorEvent::ScrollRight => {
                self.active_doc_mut().view.scroll_x_cols =
                    self.active_doc().view.scroll_x_cols.saturating_add(1);
            }
            EditorEvent::FindOpen => {
                self.search.bar_open = true;
                let sel = self.active_buffer().selection();
                if !sel.is_collapsed() {
                    let (start_line, _) = self
                        .active_buffer()
                        .pos_to_linecol(sel.anchor.min(sel.head))
                        .unwrap_or((0, 0));
                    let (end_line, _) = self
                        .active_buffer()
                        .pos_to_linecol(sel.anchor.max(sel.head))
                        .unwrap_or((0, 0));
                    if start_line != end_line {
                        let r = sel.range(); self.search.selection_range = Some((r.start, r.end));
                    } else {
                        self.search.selection_range = None;
                    }
                } else {
                    self.search.selection_range = None;
                }
            }
            EditorEvent::FindClose => {
                self.search.bar_open = false;
                self.search.selection_range = None;
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
                }
            }
            EditorEvent::FindPrev => {
                if let Some(pos) = self.search.prev_before(self.active_buffer().cursor()) {
                    self.active_buffer_mut().set_cursor(pos);
                    self.active_buffer_mut()
                        .set_selection(Selection::collapsed(pos));
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
            EditorEvent::ToggleFindCaseSensitive => {
                self.search.case_sensitive = !self.search.case_sensitive;
                self.search.refresh(&self.active_buffer().to_bytes());
                if let Some(pos) = self.search.current_match() {
                    self.active_buffer_mut().set_cursor(pos);
                    self.active_buffer_mut()
                        .set_selection(Selection::collapsed(pos));
                }
            }
            EditorEvent::ToggleFindWholeWord => {
                self.search.whole_word = !self.search.whole_word;
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
                    // Auto-open find too so the user has the full
                    // find/replace experience.
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
            EditorEvent::Move(movement) => {
                let new_sels: Vec<Selection> = self
                    .active_buffer()
                    .selections()
                    .iter()
                    .map(|s| {
                        let new_pos = self.compute_target_from(movement, s.head);
                        Selection::collapsed(new_pos)
                    })
                    .collect();
                self.active_buffer_mut().set_selections(new_sels);
            }
            EditorEvent::SelectExtend(movement) => {
                let new_sels: Vec<Selection> = self
                    .active_buffer()
                    .selections()
                    .iter()
                    .map(|s| {
                        let new_pos = self.compute_target_from(movement, s.head);
                        Selection {
                            anchor: s.anchor,
                            head: new_pos,
                        }
                    })
                    .collect();
                self.active_buffer_mut().set_selections(new_sels);
            }
            EditorEvent::SetCursor { pos } => {
                let clamped = pos.min(self.active_buffer().len());
                self.active_buffer_mut().set_cursor(clamped);
                self.active_buffer_mut()
                    .set_selection(Selection::collapsed(clamped));
            }
            EditorEvent::SelectExtendTo { pos } => {
                let clamped = pos.min(self.active_buffer().len());
                let anchor = self.active_buffer().selection().anchor;
                self.active_buffer_mut().set_cursor(clamped);
                self.active_buffer_mut().set_selection(Selection {
                    anchor,
                    head: clamped,
                });
            }
            EditorEvent::AddCursor { pos } => {
                let clamped = pos.min(self.active_buffer().len());
                let mut sels: Vec<Selection> =
                    self.active_buffer().selections().to_vec();
                if !sels.iter().any(|s| s.head == clamped && s.is_collapsed()) {
                    sels.push(Selection::collapsed(clamped));
                    sels.sort_by_key(|s| s.head);
                    self.active_buffer_mut().set_selections(sels);
                }
            }
            EditorEvent::ColumnSelect { from_line, from_col, to_line, to_col } => {
                let sels = core::column_selections(
                    self.active_buffer(),
                    from_line,
                    from_col,
                    to_line,
                    to_col,
                );
                if !sels.is_empty() {
                    self.active_buffer_mut().set_selections(sels);
                }
            }
            EditorEvent::SelectNextOccurrence => {
                self.select_next_occurrence();
            }
            EditorEvent::SelectAll => {
                let len = self.active_buffer().len();
                self.active_buffer_mut()
                    .set_selections(vec![Selection { anchor: 0, head: len }]);
            }
            EditorEvent::ToggleComment => {
                let Some(lang) = self.active_doc().language_id() else {
                    return;
                };
                let Some(prefix) = core::line_comment_prefix(lang) else {
                    self.status_message = Some("No comment syntax for this language.".to_string());
                    return;
                };
                let sel = self.active_buffer().selection();
                let (start_line, _) = self
                    .active_buffer()
                    .pos_to_linecol(sel.anchor.min(sel.head))
                    .unwrap_or((0, 0));
                let (end_line, _) = self
                    .active_buffer()
                    .pos_to_linecol(sel.anchor.max(sel.head))
                    .unwrap_or((0, 0));
                let total = self.active_buffer().line_count();
                let end_line = (end_line + 1).min(total);
                let edits = core::compute_comment_toggles(
                    self.active_buffer(),
                    start_line,
                    end_line,
                    prefix,
                );
                let mut edits = edits;
                edits.sort_by_key(|(pos, _, _)| std::cmp::Reverse(*pos));
                for (_pos, insert, del_range) in edits {
                    let _ = self.active_buffer_mut().replace(del_range, &insert);
                }
                self.active_doc_mut().syntax.invalidate();
            }
            EditorEvent::CollapseCursors => {
                if self.active_buffer().selections().len() > 1 {
                    let primary = self.active_buffer().selections()[0];
                    self.active_buffer_mut()
                        .set_selections(vec![primary]);
                } else {
                    self.handle_event(EditorEvent::CloseDoc);
                }
            }
            EditorEvent::Paste(text) => {
                // Selection-aware paste: replace the selection if any,
                // otherwise insert at cursor. Goes through the shared
                // insert_text path so error / cursor-update behaviour
                // matches Insert and InsertTab.
                self.insert_text(&text);
                // insert_text reports generic "insert error" on
                // failure; for paste the conventional label is
                // "paste error" so swap it. We only swap when the
                // insert actually failed.
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
                    self.lsp_save_active();
                    self.sync_config();
                    if let Some(uri) = self.active_doc().uri() {
                        if self.lsp_manager.supports_formatting(&uri) {
                            self.lsp_manager.request_formatting(&uri);
                            self.pending_format_save = true;
                        } else if self.try_external_format() {
                            let saved = self.active_buffer_mut().save().is_ok();
                            if saved {
                                self.active_doc_mut().refresh_git_gutter();
                                self.lsp_save_active();
                                self.status_message =
                                    Some("Saved + formatted.".to_string());
                            }
                        }
                    }
                }
                Err(e) => self.status_message = Some(format!("Save error: {e}")),
            },
            EditorEvent::SaveAs(maybe_path) => match maybe_path {
                Some(path) => {
                    self.active_buffer_mut().set_source_path(path);
                    self.handle_event(EditorEvent::Save);
                    self.sync_watcher();
                }
                None => self.status_message = Some("Save As requires the GUI.".to_string()),
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
            EditorEvent::GoToSymbol => {
                // TUI: request symbols and show them in the status bar
                // for now. A full interactive picker is a follow-up.
                if let Some(uri) = self.active_doc().uri() {
                    self.lsp_manager.request_document_symbols(&uri);
                    self.status_message = Some("Requesting symbols...".to_string());
                }
            }
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
                self.project_search.replace_focused = !self.project_search.replace_focused;
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
                // GUI-only help window; ignored in the TUI for now.
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
            EditorEvent::ToggleGitBlame => {
                let enabled = !self.active_doc().view.git_blame_enabled;
                self.active_doc_mut().view.git_blame_enabled = enabled;
                self.sync_config();
                self.status_message = Some(format!(
                    "Git blame {}.",
                    if enabled { "enabled" } else { "disabled" }
                ));
                if enabled {
                    self.active_doc_mut().git_blame.mark_dirty();
                }
            }
            EditorEvent::ToggleFold { line } => {
                let doc = &self.documents[self.active];
                let foldable = doc
                    .ts_tree
                    .as_ref()
                    .and_then(|t| t.tree())
                    .map(core::fold::foldable_ranges_pub)
                    .unwrap_or_default();
                let doc = &mut self.documents[self.active];
                doc.folds.set_foldable(foldable);
                doc.folds.toggle(line);
                let total = doc.buffer.line_count();
                doc.folds.rebuild(total);
            }
            EditorEvent::UnfoldAll => {
                self.active_doc_mut().folds.unfold_all();
                let total = self.active_buffer().line_count();
                self.active_doc_mut().folds.rebuild(total);
                self.status_message = Some("All folds unfolded.".to_string());
            }
            EditorEvent::LspCompletion => {
                let Some(uri) = self.active_doc().uri() else {
                    self.status_message = Some("LSP: no file URI.".to_string());
                    return;
                };
                let Some((line, col)) = self
                    .active_buffer()
                    .pos_to_linecol(self.active_buffer().cursor())
                else {
                    self.status_message = Some("LSP: cursor position unknown.".to_string());
                    return;
                };
                let pos = lsp_types::Position {
                    line: line as u32,
                    character: col as u32,
                };
                self.lsp_completion.open = true;
                self.lsp_completion.pending = true;
                self.lsp_completion.selected = 0;
                self.lsp_completion.items.clear();
                self.lsp_manager.request_completion(&uri, pos);
                self.status_message = Some("LSP: requesting completions...".to_string());
            }
            EditorEvent::LspHover => {
                if let Some((uri, pos)) = self.lsp_cursor_position() {
                    self.lsp_hover.pending = true;
                    self.lsp_hover.request_pos = Some(pos);
                    self.lsp_manager.request_hover(&uri, pos);
                    self.status_message = Some("LSP: requesting hover...".to_string());
                } else {
                    self.status_message = Some("LSP: hover not available.".to_string());
                }
            }
            EditorEvent::LspGoToDefinition => {
                if let Some((uri, pos)) = self.lsp_cursor_position() {
                    self.lsp_definition.pending = true;
                    self.lsp_definition.request_pos = Some(pos);
                    self.lsp_manager.request_definition(&uri, pos);
                    self.status_message = Some("LSP: requesting definition...".to_string());
                } else {
                    self.status_message = Some("LSP: definition not available.".to_string());
                }
            }
            EditorEvent::RenameSymbol => {
                // TUI rename: read the new name via a status-bar prompt
                // (no popup widget like the GUI). The TUI's GoToLine
                // pattern is the model. For now, request directly with a
                // placeholder; a full TUI rename prompt is a follow-up.
                if let Some((uri, _pos)) = self.lsp_cursor_position() {
                    if self.lsp_manager.supports_rename(&uri) {
                        let word = self.word_at_cursor().unwrap_or_default();
                        self.rename_dialog = Some(RenameDialog { new_name: word });
                    } else {
                        self.status_message =
                            Some("LSP: rename not supported.".to_string());
                    }
                }
            }
            EditorEvent::RenameApply { new_name } => {
                self.rename_dialog = None;
                if let Some((uri, pos)) = self.lsp_cursor_position() {
                    self.lsp_manager.request_rename(&uri, pos, &new_name);
                    self.status_message = Some("LSP: requesting rename...".to_string());
                }
            }
            EditorEvent::FormatDocument => {
                self.format_active_document();
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
            // Invalidate syntax cache. For localized edits, drop only the
            // edited line and below so a keystroke doesn't re-tokenize the
            // whole viewport — critical for large files where each visible
            // line costs ~0.3ms+ in release. Undo/Redo/Replace can touch
            // arbitrary regions, so they wipe everything.
            if full_invalidate {
                self.active_doc_mut().syntax.invalidate();
            } else if let Some(line) = edit_start_line {
                self.active_doc_mut().syntax.invalidate_from(line);
            } else {
                self.active_doc_mut().syntax.invalidate();
            }
            // Keep the tree-sitter parse tree current. Localized edits get
            // an incremental re-parse via InputEdit; Undo/Redo/Replace
            // re-parse fully. No-op when the doc has no tree.
            if full_invalidate {
                let bytes = self.active_doc_mut().buffer.to_bytes();
                if let Some(tree) = self.active_doc_mut().ts_tree.as_mut() {
                    tree.parse(&bytes);
                }
            } else if let Some((start_byte, old_len, line, col)) = ts_pre_edit {
                let new_len = self.active_buffer().len();
                let len_diff = new_len as i64 - old_len as i64;
                let delta = core::ts::EditDelta::single_line(line, col, start_byte, len_diff as i32);
                self.active_doc_mut().apply_ts_edit(delta);
            }
            self.active_doc_mut().git_gutter.mark_dirty();
            self.active_doc_mut().git_blame.mark_dirty();
            self.last_edit_time = Instant::now();
        }
    }

    /// Shared insertion path used by `Insert(char)`, `InsertTab`, and
    /// `Paste`. Selection-aware: non-collapsed selections are replaced,
    /// and the text is inserted at every cursor (multi-cursor). Inserts
    /// are applied right-to-left so earlier positions aren't shifted.
    fn insert_text(&mut self, text: &str) {
        self.delete_selection_if_any();
        let mut positions: Vec<core::BytePos> = self
            .active_buffer()
            .selections()
            .iter()
            .map(|s| s.head)
            .collect();
        positions.sort();
        positions.dedup();
        positions.reverse();
        let mut new_positions: Vec<core::BytePos> = Vec::new();
        for pos in &positions {
            match self.active_buffer_mut().insert(*pos, text) {
                Ok(new_pos) => new_positions.push(new_pos),
                Err(e) => {
                    self.status_message = Some(format!("insert error: {e}"));
                    return;
                }
            }
        }
        // Adjust positions for the cumulative shift of right-to-left insertion.
        let text_len = text.len();
        new_positions.reverse();
        for (i, p) in new_positions.iter_mut().enumerate() {
            *p += i * text_len;
        }
        new_positions.sort();
        let new_sels: Vec<Selection> = new_positions
            .iter()
            .map(|&p| Selection::collapsed(p))
            .collect();
        if !new_sels.is_empty() {
            self.active_buffer_mut().set_selections(new_sels);
        }
        self.status_message = None;
    }

    /// Insert an auto-paired open/close (e.g. `()`), leaving the cursor
    /// between the two chars. Assumes the caller has already checked that
    /// the selection is collapsed.
    fn insert_paired(&mut self, open: char, close: char) {
        let pos = self.active_buffer().cursor();
        let pair: String = format!("{open}{close}");
        if self.active_buffer_mut().insert(pos, &pair).is_ok() {
            let between = core::move_left_by_char(self.active_buffer(), self.active_buffer().cursor());
            self.active_buffer_mut().set_cursor(between);
            self.active_buffer_mut()
                .set_selection(Selection::collapsed(between));
            self.status_message = None;
        }
    }

    /// Delete all non-collapsed selections (right-to-left). Returns
    /// `true` when any deletion happened.
    fn delete_selection_if_any(&mut self) -> bool {
        let sels: Vec<Selection> = self
            .active_buffer()
            .selections()
            .iter()
            .filter(|s| !s.is_collapsed())
            .copied()
            .collect();
        if sels.is_empty() {
            return false;
        }
        let mut new_cursors: Vec<core::BytePos> = Vec::new();
        let mut any_deleted = false;
        let mut sels = sels;
        sels.sort_by_key(|s| std::cmp::Reverse(s.range().start));
        for sel in &sels {
            let range = sel.range();
            match self.active_buffer_mut().delete(range.clone()) {
                Ok(new_pos) => {
                    new_cursors.push(new_pos);
                    any_deleted = true;
                }
                Err(e) => {
                    self.status_message = Some(format!("delete error: {e}"));
                }
            }
        }
        if any_deleted {
            let mut all_cursors: Vec<core::BytePos> = new_cursors;
            let collapsed: Vec<core::BytePos> = self
                .active_buffer()
                .selections()
                .iter()
                .filter(|s| s.is_collapsed())
                .map(|s| s.head)
                .collect();
            all_cursors.extend(collapsed);
            all_cursors.sort();
            all_cursors.dedup();
            let new_sels: Vec<Selection> =
                all_cursors.iter().map(|&p| Selection::collapsed(p)).collect();
            self.active_buffer_mut().set_selections(new_sels);
        }
        any_deleted
    }

    /// Compute the byte position a movement should land on.
    #[allow(dead_code)]
    fn compute_target(&self, movement: Movement) -> usize {
        let pos = self.active_buffer().cursor();
        self.compute_target_from(movement, pos)
    }

    /// Compute the movement target from an explicit starting position.
    fn compute_target_from(&self, movement: Movement, pos: usize) -> usize {
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
                let page = self.viewport_lines();
                if line == 0 {
                    0
                } else {
                    let target = line.saturating_sub(page);
                    core::clamped_line_charcol_to_pos(self.active_buffer(), target, col)
                }
            }
            Movement::PageDown => {
                let (line, col) = core::cursor_char_linecol(self.active_buffer(), pos);
                let page = self.viewport_lines();
                let last = self.active_buffer().line_count().saturating_sub(1);
                let target = (line + page).min(last);
                core::clamped_line_charcol_to_pos(self.active_buffer(), target, col)
            }
            Movement::WordLeft => self.skip_word_left_from(pos),
            Movement::WordRight => self.skip_word_right_from(pos),
            Movement::LineStart => {
                let (line, col) = self.active_buffer().pos_to_linecol(pos).unwrap_or((0, 0));
                // Smart Home: toggle between column 0 and the first
                // non-whitespace char.
                let first_nws = self
                    .active_buffer()
                    .line_text(line)
                    .map(|t| {
                        core::byte_to_char_col(&t, t.chars().take_while(|c| *c == ' ' || *c == '\t').count())
                    })
                    .unwrap_or(0);
                if col == 0 {
                    self.active_buffer().linecol_to_pos(line, first_nws).unwrap_or(0)
                } else {
                    self.active_buffer().linecol_to_pos(line, 0).unwrap_or(0)
                }
            }
            Movement::LineEnd => {
                let (line, _) = self.active_buffer().pos_to_linecol(pos).unwrap_or((0, 0));
                // line_byte_range gives the [start, end) byte range for the
                // line; its end is the cursor's LineEnd target.
                self.active_buffer()
                    .line_byte_range(line)
                    .map(|r| r.end)
                    .unwrap_or(len)
            }
            Movement::DocumentStart => 0,
            Movement::DocumentEnd => len,
        }
    }

    /// Number of lines that fit in the current viewport. Falls back to
    /// a sensible default of 20 lines if the renderer hasn't measured
    /// the terminal yet.
    fn viewport_lines(&self) -> usize {
        let h = self.viewport_height as usize;
        if h == 0 {
            20
        } else {
            h
        }
    }

    /// Move `pos` left to the start of the previous word. Word boundary
    /// = transition between word-char and non-word-char, or line
    /// boundary, whichever comes first.
    fn skip_word_left_from(&self, pos: usize) -> usize {
        if pos == 0 {
            return 0;
        }
        let (line, _) = self.active_buffer().pos_to_linecol(pos).unwrap_or((0, 0));
        let line_text = match self.active_buffer().line_text(line) {
            Some(cow) => cow.into_owned(),
            None => return pos.saturating_sub(1),
        };
        let line_byte_start = self
            .active_buffer()
            .line_byte_range(line)
            .map(|r| r.start)
            .unwrap_or(0);
        // Work in line-relative byte offsets: convert the absolute pos
        // to a column within this line, scan, then convert back.
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
    fn skip_word_right_from(&self, pos: usize) -> usize {
        let len = self.active_buffer().len();
        if pos >= len {
            return len;
        }
        let (line, _) = self.active_buffer().pos_to_linecol(pos).unwrap_or((0, 0));
        let line_text = match self.active_buffer().line_text(line) {
            Some(cow) => cow.into_owned(),
            None => return (pos + 1).min(len),
        };
        let line_byte_start = self
            .active_buffer()
            .line_byte_range(line)
            .map(|r| r.start)
            .unwrap_or(0);
        let line_byte_end = self
            .active_buffer()
            .line_byte_range(line)
            .map(|r| r.end)
            .unwrap_or(len);
        let char_col = core::byte_to_char_col(&line_text, pos - line_byte_start);
        let chars: Vec<char> = line_text.chars().collect();
        let mut i = char_col.min(chars.len());

        // In-word: eat to end of word. In-whitespace: skip ws to start
        // of next word. Pick based on the char we're sitting on.
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

        // If we landed at end-of-line and there's a next line, wrap to
        // the start of the next line so consecutive WordRights keep
        // moving past line boundaries.
        if new_byte >= line_byte_end && line + 1 < self.active_buffer().line_count() {
            return self
                .active_buffer()
                .line_byte_range(line + 1)
                .map(|r| r.start)
                .unwrap_or(len);
        }
        new_byte.min(len)
    }

    /// Set the indent mode for the active document and report the
    /// change in the status bar. Indent mode controls what the Tab
    /// key inserts (spaces vs tab character) and how many spaces per
    /// indent level. Per-document so opening a file with different
    /// conventions doesn't fight the user's preferred mode.
    pub fn set_indent_mode(&mut self, use_spaces: bool, tab_width: usize) {
        // Clamp tab_width to a sensible range so a stray config
        // value can't break the renderer (and the status line
        // formatting). 1..=16 covers every indent style in the wild.
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

    /// Cycle through the four common indent presets in order:
    /// spaces:2 → spaces:4 → spaces:8 → tabs (width 4) → spaces:2.
    /// Wired to Cmd/Ctrl+I in both frontends so the user can flip
    /// modes without remembering the magic keybinding.
    pub fn cycle_indent_mode(&mut self) {
        let v = &self.active_doc().view;
        let next = match (v.use_spaces, v.tab_width) {
            (true, 2) => (true, 4),
            (true, 4) => (true, 8),
            (true, 8) => (false, 4),
            (false, _) => (true, 2),
            // Other widths (e.g. from a future config) collapse to the
            // first preset so the cycle always lands somewhere sane.
            _ => (true, 2),
        };
        self.set_indent_mode(next.0, next.1);
    }

    /// Toggle soft-wrap on the active document and report the
    /// change. The TUI frontend doesn't yet implement visual line
    /// wrapping (the GUI does); the toggle still flips the state so
    /// the setting travels with the document and per-doc configs
    /// stay consistent across frontends. The TUI shows the new
    /// state in the status bar so the user knows the toggle worked.
    pub fn toggle_soft_wrap(&mut self) {
        let new_value = !self.active_doc().view.soft_wrap;
        self.active_doc_mut().view.soft_wrap = new_value;
        self.status_message = Some(if new_value {
            "Soft-wrap: on".to_string()
        } else {
            "Soft-wrap: off (horizontal scroll)".to_string()
        });
    }

    /// Cycle to the next theme and invalidate the syntax cache
    /// for every open document so the new colors appear immediately.
    /// Mirrors `frontend_gui::EditorApp::cycle_theme`.
    pub fn cycle_theme(&mut self) {
        let name = self.syntax.cycle_theme().to_string();
        for doc in &mut self.documents {
            doc.syntax.invalidate();
        }
        self.status_message = Some(format!("Theme: {name}"));
    }

    /// Replace the currently-active find match with the replace
    /// query, then advance to the next match. No-op (with status
    /// message) when:
    /// - The find query is empty.
    /// - The replace query is empty (refusing to silently delete).
    /// - There's no current match.
    ///
    /// The replace happens via `Buffer::replace` which is one atomic
    /// edit — single undo entry. After the replace, the search is
    /// refreshed (match positions shifted) and the cursor is moved
    /// to the next match via `Search::next_after`. If the replacement
    /// was the last match, the cursor lands at the replacement's
    /// start position (you see what was changed) and the status bar
    /// reports "no more matches".
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
        // Refresh matches (positions shifted) — keep cursor at the
        // replacement start so the user can see what changed.
        self.search.refresh(&self.active_buffer().to_bytes());
        self.active_buffer_mut().set_cursor(pos);
        self.active_buffer_mut()
            .set_selection(Selection::collapsed(pos));
        // Advance to next match.
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
    /// undo entry. Iterates in reverse so earlier byte offsets stay
    /// valid as later ones are replaced. No-op (with status message)
    /// when the queries are empty or there are no matches.
    ///
    /// v1: no confirmation prompt. Undo if you regret it.
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
        // Wrap in one edit group so the whole batch is a single undo.
        self.active_buffer_mut().begin_edit_group();
        let result = self.active_buffer_mut().replace(0..len, &new_text);
        self.active_buffer_mut().end_edit_group();
        if let Err(e) = result {
            self.status_message = Some(format!("Replace error: {e}"));
            return;
        }
        // Refresh matches (positions shifted, count may be different
        // if replacement contains the query — recursive replace
        // semantics are deliberately NOT implemented here; we
        // snapshot matches before any replace so the loop is bounded).
        self.search.refresh(&self.active_buffer().to_bytes());
        self.active_buffer_mut().set_cursor(0);
        self.active_buffer_mut()
            .set_selection(Selection::collapsed(0));
        self.status_message = Some(format!("Replaced {count} occurrences."));
    }

    /// Adjust the active document's `scroll_top_line` so the cursor
    /// stays within the configured `scroll-margin` of the viewport's
    /// top and bottom rows. Called by the renderer after it
    /// determines the viewport height. Each document owns its own
    /// scroll offset, so switching tabs preserves where you were
    /// scrolled to in each one — the cursor-following clamp only
    /// fires for the doc you're currently looking at.
    pub fn adjust_viewport(&mut self, viewport_height: u16) {
        self.viewport_height = viewport_height;
        let cursor_pos = self.active_buffer().cursor();
        let cursor_line = self
            .active_buffer()
            .pos_to_linecol(cursor_pos)
            .map(|(l, _)| l)
            .unwrap_or(0);
        let vh = viewport_height as usize;
        if vh == 0 {
            return;
        }
        // Margin (Emacs `scroll-margin`): the cursor triggers a
        // scroll when it's within this many rows of the viewport's
        // edge. After scrolling, `margin` rows of buffer are kept
        // visible above/below the cursor, so the user lands the
        // cursor near an edge and continues moving without the
        // view jumping at the very last row. Reads from the doc's
        // `ViewState::scroll_margin_lines` (defaults to 3 in
        // `core::Document`).
        //
        // **Edge case**: when `2 * margin + 1 > vh` the safe zone
        // (rows [margin, vh - 1 - margin]) collapses to nothing and
        // every cursor movement trips a scroll — the user sees the
        // view "scroll on every cursor move near the middle" with a
        // small window. Fall back to legacy `margin=0`
        // (scroll only on actual viewport exit) when the requested
        // margin can't fit. Same fallback the GUI uses.
        let requested_margin = self.active_doc().view.scroll_margin_lines;
        let margin = if vh > 2 * requested_margin + 1 {
            requested_margin
        } else {
            0
        };
        let top = self.active_doc().view.scroll_top_line;
        let new_top = if cursor_line < top.saturating_add(margin) {
            // Cursor within `margin` of (or before) the top row.
            // Scroll up so the cursor lands at row `margin`.
            cursor_line.saturating_sub(margin)
        } else if cursor_line >= top + vh.saturating_sub(margin) {
            // Cursor within `margin` of (or past) the bottom row.
            // Scroll down so the cursor lands at row
            // `vh - margin - 1`. Solving
            //   cursor_line = (new_top + vh - margin - 1)
            // for `new_top` gives the line below.
            cursor_line + margin + 1 - vh
        } else {
            // Cursor inside the safe zone — no scroll. Manual wheel
            // scrolling away from the cursor is preserved across
            // presses.
            top
        };
        self.active_doc_mut().view.scroll_top_line = new_top;
    }

    /// Whether the buffer has unsaved edits. Convenience wrapper around
    /// the trait method so render code doesn't need to import the trait.
    pub fn is_dirty(&self) -> bool {
        self.active_buffer().is_dirty()
    }

    /// Delete the entire line under the cursor, including its trailing
    /// newline (so the next line collapses up). Cursor lands at the
    /// start of the deleted line's position.
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
            // Not the last line — eat the trailing newline so the next
            // line shifts up.
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
            // Last line, no trailing newline — eat the preceding
            // newline so the buffer gets shorter.
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
        } else {
            // Only line in buffer, no newline. Just clear it.
            match self.active_buffer_mut().delete(0..line_range.end) {
                Ok(np) => {
                    self.active_buffer_mut().set_cursor(np);
                    self.active_buffer_mut()
                        .set_selection(Selection::collapsed(np));
                }
                Err(e) => self.status_message = Some(format!("delete error: {e}")),
            }
        }
        let _ = line_count;
    }

    /// Duplicate the line under the cursor. The new copy is inserted
    /// below; cursor moves to the start of the new copy.
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
            // Insert just before the next line, with a newline if the
            // current line doesn't end in one.
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
            // Last line. Append a newline + the line text so the copy
            // is on its own line.
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

    /// Move the current line up (`delta = -1`) or down (`delta = +1`)
    /// by swapping with the adjacent line.
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
        let Some(my_range_excl) = self.active_buffer().line_byte_range(line) else {
            return;
        };
        let Some(other_range_excl) = self.active_buffer().line_byte_range(target_line) else {
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
        let my_range = line_with_nl(my_range_excl, line);
        let other_range = line_with_nl(other_range_excl, target_line);
        let my_text = self
            .active_buffer()
            .slice(my_range.clone())
            .unwrap_or_default();
        let other_text = self
            .active_buffer()
            .slice(other_range.clone())
            .unwrap_or_default();
        // Adjacent lines — delete their union in one shot. Then
        // reinsert in swapped order at delete_start. The text that
        // ends up at the LOWER position goes at delete_start first;
        // the higher-position text goes after it.
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

    /// Convert a mouse click in terminal cells to a byte position in the
    /// buffer. Returns `None` for clicks outside the content area, above
    /// the viewport top, or past the last line.
    ///
    /// `col` and `row` are absolute terminal coordinates. Layout:
    /// row 0 = header, rows 1..(1+viewport_height) = content,
    /// row (1+viewport_height) = status bar.
    pub fn click_to_byte_pos(&self, col: u16, row: u16) -> Option<BytePos> {
        // Header occupies row 0.
        if row == 0 {
            return None;
        }
        let content_row = (row - 1) as usize;
        // Clicks past the bottom of the content area (status bar) are ignored.
        if content_row >= self.viewport_height as usize {
            return None;
        }
        let doc_line = self.active_doc().view.scroll_top_line + content_row;
        let total_lines = self.active_buffer().line_count();
        if doc_line >= total_lines {
            // Click past last line — clamp to end of last line.
            return Some(self.active_buffer().len());
        }

        // When the project tree sidebar is open, the editor content is
        // shifted right by `tree_width` columns.
        let content_col = col.saturating_sub(self.tree_width);

        let gutter_width = total_lines.to_string().len().max(2);
        let prefix_text = format!("{:>width$} │ ", 1, width = gutter_width);
        let prefix_chars = prefix_text.chars().count() as u16;

        let line_byte_start = self.active_buffer().line_byte_range(doc_line)?.start;

        if content_col < prefix_chars {
            // Click in the gutter — position at the start of the line.
            return Some(line_byte_start);
        }

        let line_text = self.active_buffer().line_text(doc_line)?.into_owned();
        // Account for horizontal scroll: the rendered text starts at
        // char `scroll_x` of the full line.
        let scroll_x = self.active_doc().view.scroll_x_cols;
        let text_col = (content_col - prefix_chars) as usize;
        let char_col = (text_col + scroll_x).min(line_text.chars().count());
        let byte_col = core::char_col_to_byte_col(&line_text, char_col);

        Some(line_byte_start + byte_col)
    }

    /// Begin closing the active document. If the buffer has unsaved
    /// edits, open the close-confirm prompt instead of closing —
    /// actual close happens once the user picks Save / Discard /
    /// Cancel via [`App::cycle_close_choice`] and
    /// [`App::confirm_close_choice`]. If the buffer is clean (or it
    /// is the only document), close immediately.
    ///
    /// Splitting the "begin" step from the "perform" step is what
    /// makes the prompt possible: the input loop shows the dialog and
    /// intercepts key events; once the user decides, we call the
    /// perform step.
    pub fn request_close_active(&mut self) {
        if self.active_doc().is_dirty() {
            // Only one prompt at a time. If the close-confirm is
            // already up, refresh it to point at the (still same) doc;
            // opening other dialogs over an existing one would be
            // confusing, so we let close-confirm win.
            self.go_to_line_dialog = None;
            self.close_confirm = Some(CloseConfirm {
                doc_index: self.active,
                choice: CloseChoice::Save,
            });
            self.status_message = None;
            return;
        }
        self.perform_close_active();
    }

    /// Cycle the focused choice on the close-confirm prompt. `delta`
    /// moves forward (+1) or backward (-1) through the Save → Discard
    /// → Cancel cycle. No-op when no prompt is up.
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
    /// prompt. Save saves then closes (failure keeps the prompt
    /// closed but reports the error in the status bar — the user
    /// can Ctrl+S manually then Ctrl+W again). Discard closes.
    /// Cancel just drops the prompt.
    ///
    /// `y` is treated as Discard (a one-key "yes, throw it away" for
    /// power users); `n` and Esc are equivalent to Cancel.
    pub fn confirm_close_choice(&mut self) {
        // Copy out the choice + target, then drop the prompt. Doing
        // the drop first lets `perform_close_active` borrow `self`
        // non-overlapping with the prompt.
        let Some(confirm) = self.close_confirm.take() else {
            return;
        };
        match confirm.choice {
            CloseChoice::Save => {
                // Save first; if it fails, drop the close (treat as
                // Cancel) and surface the error.
                match self.active_buffer_mut().save() {
                    Ok(()) => {
                        self.status_message = Some("Saved.".to_string());
                        self.perform_close_active();
                    }
                    Err(e) => {
                        self.status_message = Some(format!("Save error: {e}"));
                    }
                }
            }
            CloseChoice::Discard => {
                self.perform_close_active();
            }
            CloseChoice::Cancel => {
                self.status_message = Some("Close cancelled.".to_string());
            }
        }
    }

    /// The "perform" half of a close. Captures the v1 logic — quit
    /// when the last document goes, else remove + neighbour-pick.
    /// Pulled out so [`request_close_active`] and
    /// [`confirm_close_choice`] share it.
    fn perform_close_active(&mut self) {
        if self.documents.len() == 1 {
            self.should_quit = true;
            self.status_message = Some("Closed last document — quitting.".to_string());
            return;
        }
        self.lsp_close_active();
        self.documents.remove(self.active);
        if self.active >= self.documents.len() {
            self.active = self.documents.len() - 1;
        }
        self.status_message = Some("Closed document.".to_string());
    }

    /// Load `path` into the active document. If the file exists, its
    /// bytes replace the buffer and `source_path` is updated. If the
    /// path doesn't exist yet, the buffer becomes empty but the path
    /// is remembered — the next Save will create the file. Errors
    /// (file exists but unreadable, etc.) land in `status_message`
    /// and leave the buffer untouched.
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
        // Replace the active document's buffer in-place. View state
        // resets — a freshly-opened file shouldn't inherit the scroll
        // position of whatever was there before.
        self.documents[self.active] = Document::new_with_config(buffer, &self.config);
        self.status_message = Some(format!(
            "Opened {}",
            path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("<path>")
        ));
        self.sync_watcher();
        self.lsp_open_active();
    }

    /// Open `path` in a document, switching to an existing document if
    /// the path is already open. Used by the project tree.
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

    /// Write the current session state (open files, theme, sidebar state,
    /// per-document defaults) back to the config file.
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

    /// Save every dirty buffer that has a source path.
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
    /// idle for longer than `config.auto_save_delay_ms`.
    pub fn auto_save(&mut self) {
        if !self.config.auto_save {
            return;
        }
        let delay = Duration::from_millis(self.config.auto_save_delay_ms);
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
        const DELAY: Duration = Duration::from_millis(500);
        if self.last_edit_time.elapsed() < DELAY {
            return;
        }
        self.active_doc_mut().refresh_git_gutter();
    }

    /// Debounced git blame refresh.
    pub fn maybe_refresh_git_blame(&mut self) {
        if !self.active_doc().view.git_blame_enabled {
            return;
        }
        if !self.active_doc().git_blame.dirty() {
            return;
        }
        const DELAY: Duration = Duration::from_millis(500);
        if self.last_edit_time.elapsed() < DELAY {
            return;
        }
        let path = self.active_doc().path_buf();
        let line_count = self.active_buffer().line_count();
        let buf_len = self.active_buffer().len();
        self.active_doc_mut()
            .git_blame
            .refresh(path.as_deref(), buf_len, line_count);
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
        // Move cursor to the match column.
        let line = result.line.saturating_sub(1);
        let col = result.col.saturating_sub(1);
        if let Some(pos) = self.active_buffer().linecol_to_pos(line, col) {
            self.active_buffer_mut().set_cursor(pos);
            self.active_buffer_mut()
                .set_selection(Selection::collapsed(pos));
        }
        self.project_search.open = false;
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
        self.status_message = Some(format!("Go to line {line}"));
    }

    /// Append a digit to the go-to-line dialog's text input.
    /// No-op when the dialog isn't open or the character isn't a digit.
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

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::PieceTableBuffer;

    fn app_with(content: &str) -> App {
        let buf: Box<dyn Buffer> =
            Box::new(PieceTableBuffer::from_bytes(content.as_bytes().to_vec()));
        App::new(buf)
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
        let path = dir.join(format!("the_editor_app_save_{}.txt", std::process::id()));
        let _ = std::fs::remove_file(&path);

        let buf: Box<dyn Buffer> = Box::new(PieceTableBuffer::from_bytes_with_path(
            b"".to_vec(),
            path.clone(),
        ));
        let mut app = App::new(buf);
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
    fn set_cursor_event_collapses_selection() {
        let mut app = app_with("hello world");
        app.active_buffer_mut()
            .set_selection(Selection { anchor: 0, head: 5 });
        app.handle_event(EditorEvent::SetCursor { pos: 8 });
        assert_eq!(app.active_buffer().cursor(), 8);
        assert!(app.active_buffer().selection().is_collapsed());
    }

    #[test]
    fn select_extend_to_preserves_anchor() {
        let mut app = app_with("hello world");
        // First click sets anchor=5.
        app.handle_event(EditorEvent::SetCursor { pos: 5 });
        // Then drag to position 9.
        app.handle_event(EditorEvent::SelectExtendTo { pos: 9 });
        let sel = app.active_buffer().selection();
        assert_eq!(sel.anchor, 5);
        assert_eq!(sel.head, 9);
    }

    // ----- click_to_byte_pos: terminal coords → byte offset -----

    #[test]
    fn click_in_gutter_snaps_to_line_start() {
        let mut app = app_with("hello\nworld");
        app.active_doc_mut().view.scroll_top_line = 0;
        app.viewport_height = 10;
        // Click on row 2 (the "world" line), col 0 (gutter).
        // Row 0 = header, rows 1..N = content.
        let pos = app.click_to_byte_pos(0, 2).unwrap();
        assert_eq!(pos, 6, "should snap to start of line 1 ('world')");
    }

    #[test]
    fn click_in_text_positions_at_char() {
        let mut app = app_with("hello\nworld");
        app.active_doc_mut().view.scroll_top_line = 0;
        app.viewport_height = 10;
        // Row 2 = "world" (after header on row 0). Gutter is 4 chars wide.
        // Click on col 5 = "w" of "world" (col 5 in the text).
        let gutter_width = 4;
        let pos = app.click_to_byte_pos(gutter_width as u16, 2).unwrap();
        assert_eq!(pos, 6, "start of 'world'");
    }

    #[test]
    fn click_in_header_returns_none() {
        let mut app = app_with("hello");
        app.viewport_height = 5;
        let pos = app.click_to_byte_pos(0, 0);
        assert!(pos.is_none(), "header is not part of content");
    }

    #[test]
    fn click_in_status_returns_none() {
        let mut app = app_with("hello");
        app.active_doc_mut().view.scroll_top_line = 0;
        app.viewport_height = 5;
        // Row 6 = status (rows 0=header, 1..5=content, 6=status)
        let pos = app.click_to_byte_pos(0, 6);
        assert!(pos.is_none(), "status bar is not part of content");
    }

    #[test]
    fn click_past_last_line_clamps_to_end() {
        let mut app = app_with("hello");
        app.active_doc_mut().view.scroll_top_line = 0;
        app.viewport_height = 10;
        // Row 5 = past the end of the buffer (only 1 line at row 1).
        let pos = app.click_to_byte_pos(0, 5).unwrap();
        assert_eq!(pos, 5, "clamped to buffer length");
    }

    #[test]
    fn click_with_viewport_scrolled() {
        let mut app = app_with("aaa\nbbb\nccc\nddd");
        app.active_doc_mut().view.scroll_top_line = 2; // viewport shows lines 2,3 (ccc, ddd)
        app.viewport_height = 5;
        // Row 1 (first content row) = scroll_top_line + 0 = line 2 ("ccc").
        // Click col 0 (gutter).
        let pos = app.click_to_byte_pos(0, 1).unwrap();
        assert_eq!(pos, 8, "should snap to start of line 2 ('ccc')");
    }

    // ----- selection-aware editing -----

    #[test]
    fn insert_replaces_selection() {
        let mut app = app_with("hello world");
        app.active_buffer_mut().set_selection(Selection {
            anchor: 6,
            head: 11,
        });
        app.handle_event(EditorEvent::Insert('!'));
        assert_eq!(app.active_buffer().to_bytes(), b"hello !".to_vec());
        assert_eq!(app.active_buffer().cursor(), 7);
    }

    #[test]
    fn delete_left_with_selection_deletes_selection() {
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

    // ----- Find -----

    #[test]
    fn find_open_sets_bar_open() {
        let mut app = app_with("hello world");
        assert!(!app.search.bar_open);
        app.handle_event(EditorEvent::FindOpen);
        assert!(app.search.bar_open);
        app.handle_event(EditorEvent::FindClose);
        assert!(!app.search.bar_open);
    }

    #[test]
    fn find_query_changed_runs_search() {
        let mut app = app_with("the quick brown fox");
        app.handle_event(EditorEvent::FindQueryChanged("brown".to_string()));
        assert_eq!(app.search.matches, vec![(10, 15)]);
        assert_eq!(app.search.current, Some(0));
        assert_eq!(app.active_buffer().cursor(), 10);
    }

    #[test]
    fn find_next_moves_cursor_through_matches() {
        let mut app = app_with("abc abc abc");
        app.handle_event(EditorEvent::FindQueryChanged("abc".to_string()));
        assert_eq!(app.active_buffer().cursor(), 0);
        app.handle_event(EditorEvent::FindNext);
        assert_eq!(app.active_buffer().cursor(), 4);
        app.handle_event(EditorEvent::FindNext);
        assert_eq!(app.active_buffer().cursor(), 8);
        app.handle_event(EditorEvent::FindNext);
        // Wraps to first.
        assert_eq!(app.active_buffer().cursor(), 0);
    }

    #[test]
    fn find_prev_moves_backwards() {
        let mut app = app_with("abc abc abc");
        app.handle_event(EditorEvent::FindQueryChanged("abc".to_string()));
        app.handle_event(EditorEvent::FindPrev);
        // Wraps to last match.
        assert_eq!(app.active_buffer().cursor(), 8);
        app.handle_event(EditorEvent::FindPrev);
        assert_eq!(app.active_buffer().cursor(), 4);
    }

    #[test]
    fn find_with_no_matches_leaves_current_none() {
        let mut app = app_with("hello world");
        app.handle_event(EditorEvent::FindQueryChanged("xyz".to_string()));
        assert!(app.search.matches.is_empty());
        assert!(app.search.current.is_none());
    }

    // ----- Up/Down over short lines -----

    #[test]
    fn up_arrow_clamps_to_end_of_shorter_line_above() {
        // Buffer with one long line (30 chars) and a 5-char line above
        // it. Cursor at col 25 on the long line; pressing Up should
        // land at end of the 5-char line (col 5), not stay at col 25
        // and not jump to col 0. This was the bug: `linecol_to_pos`
        // returned None when col exceeded the target line's length,
        // and the movement code fell back to the current position.
        let mut app = app_with("hello\nthis is a much longer line");
        // Move to line 1, column 25 (well past "hello" length of 5).
        let pos = app.active_buffer().linecol_to_pos(1, 25).unwrap();
        app.active_buffer_mut().set_cursor(pos);
        // Up should land at line 0 column 5 (end of "hello").
        app.handle_event(EditorEvent::Move(Movement::Up));
        let (line, col) = app
            .active_buffer()
            .pos_to_linecol(app.active_buffer().cursor())
            .unwrap();
        assert_eq!(line, 0);
        assert_eq!(col, 5, "should clamp to end of short line above");
    }

    #[test]
    fn down_arrow_clamps_to_end_of_shorter_line_below() {
        // Mirror of the up-arrow test: short line above, long line
        // below. Cursor at col 4 on "hello"; Down should land at
        // col 4 on the long line below (column 4 fits there).
        let mut app = app_with("hello\nthis is a much longer line");
        let pos = app.active_buffer().linecol_to_pos(0, 4).unwrap();
        app.active_buffer_mut().set_cursor(pos);
        app.handle_event(EditorEvent::Move(Movement::Down));
        let (line, col) = app
            .active_buffer()
            .pos_to_linecol(app.active_buffer().cursor())
            .unwrap();
        assert_eq!(line, 1);
        assert_eq!(col, 4);
    }

    #[test]
    fn down_arrow_clamps_when_target_line_shorter() {
        // Long line above, short line below. Cursor at col 25 on the
        // long line; Down should land at col 5 (end of "hello").
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
    fn page_up_clamps_when_target_line_shorter() {
        // PageUp moves multiple lines at once — clamping must still
        // apply. Build a buffer with a long last line and several
        // short lines above. PageUp from line 5 col 25 should land
        // at line 0 col 1 (end of "a"), not stay at col 25.
        let buf: Box<dyn Buffer> = Box::new(PieceTableBuffer::from_bytes(
            b"a\nb\nc\nd\ne\nthis is a long final line".to_vec(),
        ));
        let mut app = App::new(buf);
        app.viewport_height = 5;
        let pos = app.active_buffer().linecol_to_pos(5, 25).unwrap();
        app.active_buffer_mut().set_cursor(pos);
        // PageUp with viewport 5 → target line 0.
        app.handle_event(EditorEvent::Move(Movement::PageUp));
        let (line, col) = app
            .active_buffer()
            .pos_to_linecol(app.active_buffer().cursor())
            .unwrap();
        assert_eq!(line, 0);
        assert_eq!(col, 1, "should clamp to end of 'a' (length 1)");
    }

    #[test]
    fn page_down_clamps_when_target_line_shorter() {
        let buf: Box<dyn Buffer> = Box::new(PieceTableBuffer::from_bytes(
            b"a\nb\nc\nd\nthis is a long final line".to_vec(),
        ));
        let mut app = App::new(buf);
        app.viewport_height = 5;
        let pos = app.active_buffer().linecol_to_pos(0, 1).unwrap();
        app.active_buffer_mut().set_cursor(pos);
        // PageDown with viewport 5 → target line min(0+5, 4) = 4.
        app.handle_event(EditorEvent::Move(Movement::PageDown));
        let (line, col) = app
            .active_buffer()
            .pos_to_linecol(app.active_buffer().cursor())
            .unwrap();
        assert_eq!(line, 4);
        assert_eq!(col, 1, "col 1 fits on the long last line");
    }

    // ----- PageUp / PageDown -----

    #[test]
    fn page_up_moves_cursor_up_one_viewport() {
        let mut app = make_multi_line_app();
        app.viewport_height = 5;
        // Start at line 9 col 2, page up by 5 → line 4 col 2.
        let pos = app.active_buffer().linecol_to_pos(9, 2).unwrap();
        app.active_buffer_mut().set_cursor(pos);
        app.handle_event(EditorEvent::Move(Movement::PageUp));
        let (line, col) = app
            .active_buffer()
            .pos_to_linecol(app.active_buffer().cursor())
            .unwrap();
        assert_eq!((line, col), (4, 2));
    }

    #[test]
    fn page_up_clamps_to_top() {
        let mut app = make_multi_line_app();
        app.viewport_height = 10;
        let pos = app.active_buffer().linecol_to_pos(2, 0).unwrap();
        app.active_buffer_mut().set_cursor(pos);
        app.handle_event(EditorEvent::Move(Movement::PageUp));
        assert_eq!(app.active_buffer().cursor(), 0);
    }

    #[test]
    fn page_down_moves_cursor_down_one_viewport() {
        let mut app = make_multi_line_app();
        app.viewport_height = 3;
        let pos = app.active_buffer().linecol_to_pos(0, 0).unwrap();
        app.active_buffer_mut().set_cursor(pos);
        app.handle_event(EditorEvent::Move(Movement::PageDown));
        let (line, _) = app
            .active_buffer()
            .pos_to_linecol(app.active_buffer().cursor())
            .unwrap();
        assert_eq!(line, 3);
    }

    #[test]
    fn page_down_clamps_to_last_line() {
        let mut app = make_multi_line_app();
        app.viewport_height = 100;
        let pos = app.active_buffer().linecol_to_pos(0, 0).unwrap();
        app.active_buffer_mut().set_cursor(pos);
        app.handle_event(EditorEvent::Move(Movement::PageDown));
        let (line, _) = app
            .active_buffer()
            .pos_to_linecol(app.active_buffer().cursor())
            .unwrap();
        assert_eq!(line, 9); // last line of 10-line buffer
    }

    // ----- Word movement -----

    #[test]
    fn word_right_alternates_end_of_word_and_start_of_next() {
        // Standard Ctrl+Right: in-word → end of word; in-whitespace →
        // start of next word. So 0 → 5 → 8 → 13 → 14 → 17.
        let mut app = app_with("hello   world foo");
        app.active_buffer_mut().set_cursor(0);
        app.handle_event(EditorEvent::Move(Movement::WordRight));
        assert_eq!(app.active_buffer().cursor(), 5, "end of 'hello'");
        app.handle_event(EditorEvent::Move(Movement::WordRight));
        assert_eq!(app.active_buffer().cursor(), 8, "start of 'world'");
        app.handle_event(EditorEvent::Move(Movement::WordRight));
        assert_eq!(app.active_buffer().cursor(), 13, "end of 'world'");
        app.handle_event(EditorEvent::Move(Movement::WordRight));
        assert_eq!(app.active_buffer().cursor(), 14, "start of 'foo'");
        app.handle_event(EditorEvent::Move(Movement::WordRight));
        assert_eq!(app.active_buffer().cursor(), 17, "end of 'foo'");
    }

    #[test]
    fn word_left_skips_word_back() {
        let mut app = app_with("hello world");
        app.active_buffer_mut().set_cursor(11);
        app.handle_event(EditorEvent::Move(Movement::WordLeft));
        assert_eq!(app.active_buffer().cursor(), 6, "start of 'world'");
        app.handle_event(EditorEvent::Move(Movement::WordLeft));
        assert_eq!(app.active_buffer().cursor(), 0, "start of 'hello'");
    }

    #[test]
    fn word_left_on_second_line_does_not_jump_to_line_zero() {
        // Regression: skip_word_left_from was missing line_byte_start,
        // so Ctrl+Left on line 2+ computed a position relative to
        // byte 0 instead of the line start — causing wild jumps and
        // even data loss on Ctrl+Backspace.
        let mut app = app_with("hello\nworld");
        // cursor at 'r' in "world" (byte 8)
        app.active_buffer_mut().set_cursor(8);
        app.handle_event(EditorEvent::Move(Movement::WordLeft));
        // Should land at start of "world" (byte 6), NOT byte 0
        assert_eq!(
            app.active_buffer().cursor(),
            6,
            "WordLeft on line 2 must stay within line 2"
        );
    }

    #[test]
    fn delete_word_left_on_second_line_does_not_wipe_buffer() {
        // Regression: the same missing offset made Ctrl+Backspace on
        // line 2 delete from byte 0, wiping earlier lines.
        let mut app = app_with("hello\nworld");
        app.active_buffer_mut().set_cursor(8); // at 'r'
        app.handle_event(EditorEvent::DeleteWordLeft);
        let bytes = app.active_buffer().to_bytes();
        let result = String::from_utf8_lossy(&bytes);
        assert!(
            result.starts_with("hello\n"),
            "line 1 must survive Ctrl+Backspace on line 2, got: {result:?}"
        );
    }

    #[test]
    fn word_right_from_mid_word_jumps_to_end() {
        let mut app = app_with("hello world");
        app.active_buffer_mut().set_cursor(2); // inside "hello"
        app.handle_event(EditorEvent::Move(Movement::WordRight));
        assert_eq!(app.active_buffer().cursor(), 5, "end of 'hello'");
    }

    fn make_multi_line_app() -> App {
        // 10 lines: "line0\nline1\n...\nline9"
        let content: Vec<String> = (0..10).map(|i| format!("line{i}\n")).collect();
        let s: String = content.into_iter().collect();
        // strip the trailing newline after line9 so line_count() == 10
        let s = s.trim_end_matches('\n').to_string();
        let buf: Box<dyn Buffer> = Box::new(PieceTableBuffer::from_bytes(s.into_bytes()));
        App::new(buf)
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

    #[test]
    fn delete_word_left_selection_aware() {
        let mut app = app_with("hello world");
        app.active_buffer_mut().set_selection(Selection {
            anchor: 6,
            head: 11,
        });
        app.handle_event(EditorEvent::DeleteWordLeft);
        assert_eq!(app.active_buffer().to_bytes(), b"hello ".to_vec());
    }

    // ----- Line ops -----

    #[test]
    fn delete_line_removes_entire_line_and_newline() {
        let mut app = app_with("alpha\nbeta\ngamma");
        // Cursor on "beta" (line 1).
        app.active_buffer_mut().set_cursor(8); // 'b' in "beta"
        app.handle_event(EditorEvent::DeleteLine);
        assert_eq!(app.active_buffer().to_bytes(), b"alpha\ngamma".to_vec());
    }

    #[test]
    fn duplicate_line_inserts_copy_below() {
        let mut app = app_with("alpha\nbeta\ngamma");
        app.active_buffer_mut().set_cursor(8); // 'b' in "beta"
        app.handle_event(EditorEvent::DuplicateLine);
        assert_eq!(
            app.active_buffer().to_bytes(),
            b"alpha\nbeta\nbeta\ngamma".to_vec()
        );
    }

    #[test]
    fn move_line_up_swaps_with_previous() {
        let mut app = app_with("alpha\nbeta\ngamma");
        app.active_buffer_mut().set_cursor(8); // 'b' in "beta"
        app.handle_event(EditorEvent::MoveLineUp);
        assert_eq!(
            app.active_buffer().to_bytes(),
            b"beta\nalpha\ngamma".to_vec()
        );
    }

    #[test]
    fn move_line_down_swaps_with_next() {
        let mut app = app_with("alpha\nbeta\ngamma");
        app.active_buffer_mut().set_cursor(2); // somewhere in "alpha"
        app.handle_event(EditorEvent::MoveLineDown);
        assert_eq!(
            app.active_buffer().to_bytes(),
            b"beta\nalpha\ngamma".to_vec()
        );
    }

    #[test]
    fn move_line_up_at_first_line_is_noop() {
        let mut app = app_with("alpha\nbeta");
        app.active_buffer_mut().set_cursor(0);
        app.handle_event(EditorEvent::MoveLineUp);
        assert_eq!(app.active_buffer().to_bytes(), b"alpha\nbeta".to_vec());
    }

    // ----- multi-buffer / tabs -----

    /// Helper: build an `App` from a list of buffer contents. Each
    /// entry becomes one document, in order.
    fn app_with_docs(contents: &[&str]) -> App {
        let docs: Vec<Document> = contents
            .iter()
            .map(|c| {
                let buf: Box<dyn core::Buffer> =
                    Box::new(core::PieceTableBuffer::from_bytes(c.as_bytes().to_vec()));
                Document::new(buf)
            })
            .collect();
        App::new_with_documents(docs, core::Config::default())
    }

    #[test]
    fn new_doc_appends_empty_document_and_activates_it() {
        let mut app = app_with_docs(&["alpha", "beta"]);
        assert_eq!(app.doc_count(), 2);
        assert_eq!(app.active(), 0);
        app.handle_event(EditorEvent::NewDoc);
        assert_eq!(app.doc_count(), 3);
        assert_eq!(app.active(), 2);
        assert_eq!(app.active_buffer().to_bytes(), b"".to_vec());
    }

    #[test]
    fn close_doc_with_multiple_docs_removes_active_and_picks_neighbour() {
        let mut app = app_with_docs(&["alpha", "beta", "gamma"]);
        assert_eq!(app.active(), 0);
        app.handle_event(EditorEvent::CloseDoc);
        assert_eq!(app.doc_count(), 2);
        assert_eq!(app.active(), 0);
        assert_eq!(app.active_buffer().to_bytes(), b"beta".to_vec());
    }

    #[test]
    fn close_doc_at_tail_moves_active_to_new_last() {
        let mut app = app_with_docs(&["alpha", "beta", "gamma"]);
        app.active = 2;
        app.handle_event(EditorEvent::CloseDoc);
        assert_eq!(app.doc_count(), 2);
        assert_eq!(app.active(), 1);
        assert_eq!(app.active_buffer().to_bytes(), b"beta".to_vec());
    }

    #[test]
    fn close_last_doc_quits_editor() {
        let mut app = app_with_docs(&["only"]);
        assert!(!app.should_quit);
        app.handle_event(EditorEvent::CloseDoc);
        assert!(app.should_quit);
        assert_eq!(app.doc_count(), 1);
    }

    #[test]
    fn next_doc_wraps_around() {
        let mut app = app_with_docs(&["a", "b", "c"]);
        assert_eq!(app.active(), 0);
        app.handle_event(EditorEvent::NextDoc);
        assert_eq!(app.active(), 1);
        app.handle_event(EditorEvent::NextDoc);
        assert_eq!(app.active(), 2);
        app.handle_event(EditorEvent::NextDoc);
        assert_eq!(app.active(), 0);
    }

    #[test]
    fn prev_doc_wraps_around() {
        let mut app = app_with_docs(&["a", "b", "c"]);
        assert_eq!(app.active(), 0);
        app.handle_event(EditorEvent::PrevDoc);
        assert_eq!(app.active(), 2);
        app.handle_event(EditorEvent::PrevDoc);
        assert_eq!(app.active(), 1);
        app.handle_event(EditorEvent::PrevDoc);
        assert_eq!(app.active(), 0);
    }

    #[test]
    fn switching_docs_preserves_buffer_state() {
        let mut app = app_with_docs(&["hello", "world"]);
        app.active_buffer_mut().insert(5, "!").unwrap();
        assert_eq!(app.active_buffer().to_bytes(), b"hello!".to_vec());
        app.handle_event(EditorEvent::NextDoc);
        assert_eq!(app.active_buffer().to_bytes(), b"world".to_vec());
        app.handle_event(EditorEvent::PrevDoc);
        assert_eq!(app.active_buffer().to_bytes(), b"hello!".to_vec());
    }

    // ----- per-doc vertical scroll -----

    #[test]
    fn adjust_viewport_writes_to_active_doc_only() {
        // The cursor-following clamp in `adjust_viewport` must only
        // touch the active document's scroll_top_line. Other docs'
        // offsets are untouched so they keep their scroll position
        // when the user tabs back.
        let mut app = app_with_docs(&["alpha\nbeta\ngamma\ndelta", "x"]);
        app.viewport_height = 2;

        // Move cursor near the bottom of doc 0 and adjust — only doc 0
        // scrolls.
        let pos = app.active_buffer().linecol_to_pos(3, 0).unwrap();
        app.active_buffer_mut().set_cursor(pos);
        app.adjust_viewport(2);
        let top_doc0 = app.documents[0].view.scroll_top_line;
        let top_doc1 = app.documents[1].view.scroll_top_line;
        assert!(top_doc0 > 0, "doc 0 should have scrolled: top={top_doc0}");
        assert_eq!(top_doc1, 0, "doc 1 untouched: top={top_doc1}");

        // Switch to doc 1 and adjust — doc 0's offset stays where it was.
        app.active = 1;
        app.adjust_viewport(2);
        assert_eq!(
            app.documents[0].view.scroll_top_line, top_doc0,
            "doc 0 scroll preserved across tab switch"
        );
    }

    #[test]
    fn adjust_viewport_uses_scroll_margin_for_cursor_following() {
        // Emacs `scroll-margin`: the view should pre-emptively
        // scroll when the cursor is within `scroll_margin_lines`
        // rows of the viewport edge. With margin=2 and vh=10
        // (large enough to fit multiple safe rows), the safe zone
        // is rows [2..7] (= 5 rows). Cursor moves within that
        // band don't trigger a scroll.
        let mut app = app_with("l0\nl1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nl9\nl10\nl11\nl12\nl13\nl14");
        app.viewport_height = 10;
        app.documents[0].view.scroll_margin_lines = 2;

        // Park the cursor at line 5 (row 5 with top=0). Move it
        // through the safe zone without a scroll triggering.
        let pos = app.active_buffer().linecol_to_pos(5, 0).unwrap();
        app.active_buffer_mut().set_cursor(pos);
        app.adjust_viewport(10);
        assert_eq!(app.documents[0].view.scroll_top_line, 0);
        // Walk through safe zone (rows 2..7). Each move from safe
        // row to safe row stays in safe zone — no scroll.
        for line in [4usize, 5, 6, 7] {
            let pos = app.active_buffer().linecol_to_pos(line, 0).unwrap();
            app.active_buffer_mut().set_cursor(pos);
            app.adjust_viewport(10);
            assert_eq!(
                app.documents[0].view.scroll_top_line, 0,
                "cursor at line {line} (inside safe zone rows 2..7 with margin=2) should NOT scroll; got top={}",
                app.documents[0].view.scroll_top_line
            );
        }

        // Step ONCE past the bottom of the safe zone (line 8 with
        // top=0 → row 8 = vh - margin - 1 + 1). Triggers scroll,
        // pinning the cursor at row vh - 1 - margin = 7.
        let pos = app.active_buffer().linecol_to_pos(8, 0).unwrap();
        app.active_buffer_mut().set_cursor(pos);
        app.adjust_viewport(10);
        // new_top = cursor_line - (vh - 1 - margin) = 8 - 7 = 1.
        assert_eq!(
            app.documents[0].view.scroll_top_line, 1,
            "margin=2, vh=10, cursor at line 8: should pin at row 7, new_top = 1"
        );

        // Next cursor move (line 9) should keep the cursor at
        // row 7 — new_top = 9 - 7 = 2. Each subsequent press
        // advances the view by exactly 1 line.
        let pos = app.active_buffer().linecol_to_pos(9, 0).unwrap();
        app.active_buffer_mut().set_cursor(pos);
        app.adjust_viewport(10);
        assert_eq!(app.documents[0].view.scroll_top_line, 2);

        // And for the top edge: cursor way up at line 0 should
        // pull the view to top with new_top = 0 - margin = -2 →
        // saturating to 0.
        let pos = app.active_buffer().linecol_to_pos(0, 0).unwrap();
        app.active_buffer_mut().set_cursor(pos);
        app.adjust_viewport(10);
        assert_eq!(
            app.documents[0].view.scroll_top_line, 0,
            "margin=2, cursor at line 0: new_top saturates to 0"
        );

        // Margin=0 (legacy): the cursor only triggers scroll when
        // it actually leaves the viewport, no pre-scroll.
        let mut app = app_with("a\nb\nc\nd\ne\nf");
        app.viewport_height = 3;
        app.documents[0].view.scroll_margin_lines = 0;
        let pos = app.active_buffer().linecol_to_pos(2, 0).unwrap();
        app.active_buffer_mut().set_cursor(pos);
        app.adjust_viewport(3);
        // No margin: cursor at line 2 (= row 2 inside viewport 0..2).
        assert_eq!(app.documents[0].view.scroll_top_line, 0);
        let pos = app.active_buffer().linecol_to_pos(3, 0).unwrap();
        app.active_buffer_mut().set_cursor(pos);
        app.adjust_viewport(3);
        // Cursor at line 3 (one past last visible row): trigger.
        // new_top = cursor - vh + 1 = 3 - 3 + 1 = 1.
        assert_eq!(app.documents[0].view.scroll_top_line, 1);
    }

    #[test]
    fn switch_tabs_preserves_each_docs_scroll_top() {
        // Scrolling in doc A, then switching to doc B and scrolling
        // there, then back to doc A — doc A's scroll offset is exactly
        // where we left it. This is the "per-doc vertical scroll"
        // parity with horizontal scroll.
        let mut app = app_with_docs(&[
            "l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nl9\nl10",
            "l1\nl2\nl3\nl4\nl5",
        ]);
        app.viewport_height = 3;

        // Scroll doc 0 to the bottom.
        let pos = app.active_buffer().linecol_to_pos(9, 0).unwrap();
        app.active_buffer_mut().set_cursor(pos);
        app.adjust_viewport(3);
        let doc0_top = app.documents[0].view.scroll_top_line;
        assert!(doc0_top > 0, "doc 0 scrolled down: top={doc0_top}");

        // Switch to doc 1, scroll it down too.
        app.handle_event(EditorEvent::NextDoc);
        let pos = app.active_buffer().linecol_to_pos(4, 0).unwrap();
        app.active_buffer_mut().set_cursor(pos);
        app.adjust_viewport(3);
        let doc1_top = app.documents[1].view.scroll_top_line;
        assert!(doc1_top > 0, "doc 1 scrolled down: top={doc1_top}");

        // Back to doc 0 — its offset is preserved.
        app.handle_event(EditorEvent::PrevDoc);
        assert_eq!(
            app.documents[0].view.scroll_top_line, doc0_top,
            "doc 0 scroll preserved"
        );
        assert_eq!(
            app.documents[1].view.scroll_top_line, doc1_top,
            "doc 1 scroll preserved"
        );
    }

    // ----- close-on-dirty prompt -----

    #[test]
    fn close_doc_on_clean_buffer_removes_immediately() {
        // A fresh from_bytes buffer is clean — close happens directly,
        // no prompt.
        let mut app = app_with("alpha");
        assert!(!app.active_doc().is_dirty());
        assert!(app.close_confirm.is_none());
        app.handle_event(EditorEvent::CloseDoc);
        assert!(app.close_confirm.is_none(), "no prompt on clean close");
        assert_eq!(app.doc_count(), 1, "single doc → quit, doc stays");
        assert!(app.should_quit);
    }

    #[test]
    fn close_doc_on_dirty_buffer_opens_prompt() {
        // Edit the buffer so it becomes dirty, then hit CloseDoc.
        let mut app = app_with("hello");
        app.handle_event(EditorEvent::Insert('!'));
        assert!(app.active_doc().is_dirty());
        app.handle_event(EditorEvent::CloseDoc);
        let confirm = app.close_confirm.as_ref().expect("prompt should be open");
        assert_eq!(confirm.doc_index, 0);
        assert_eq!(confirm.choice, CloseChoice::Save, "Save is the default");
        // The document is still there.
        assert_eq!(app.doc_count(), 1);
        assert!(!app.should_quit);
    }

    #[test]
    fn cycle_close_choice_walks_save_discard_cancel() {
        let mut app = app_with("hello");
        app.handle_event(EditorEvent::Insert('!'));
        app.handle_event(EditorEvent::CloseDoc);
        assert_eq!(
            app.close_confirm.as_ref().unwrap().choice,
            CloseChoice::Save
        );
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
        // Backward wraps too.
        app.cycle_close_choice(-1);
        assert_eq!(
            app.close_confirm.as_ref().unwrap().choice,
            CloseChoice::Cancel
        );
    }

    #[test]
    fn confirm_close_choice_discard_closes_without_saving() {
        let mut app = app_with("hello");
        app.handle_event(EditorEvent::Insert('!'));
        app.handle_event(EditorEvent::CloseDoc);
        // Cycle to Discard, confirm.
        app.cycle_close_choice(1);
        app.confirm_close_choice();
        assert!(app.close_confirm.is_none());
        assert_eq!(app.doc_count(), 1);
        assert!(app.should_quit);
        // Buffer was dirty-but-discarded, so the save path was NOT
        // taken — we just exited.
    }

    #[test]
    fn confirm_close_choice_cancel_drops_prompt_only() {
        let mut app = app_with("hello");
        app.handle_event(EditorEvent::Insert('!'));
        app.handle_event(EditorEvent::CloseDoc);
        // Cycle Save → Discard (delta=1), then Discard → Cancel
        // (delta=1). cycle(2) doesn't skip past Cancel — the cycle
        // implementation only handles unit steps; that's deliberate
        // because the delta is +1/-1 in all real callers (Tab/Shift+Tab).
        app.cycle_close_choice(1);
        app.cycle_close_choice(1);
        assert_eq!(
            app.close_confirm.as_ref().unwrap().choice,
            CloseChoice::Cancel
        );
        app.confirm_close_choice();
        assert!(app.close_confirm.is_none());
        assert_eq!(app.doc_count(), 1, "doc still here");
        assert!(!app.should_quit, "still running");
        // Buffer is still dirty.
        assert!(app.active_doc().is_dirty());
    }

    #[test]
    fn confirm_close_choice_save_saves_then_closes() {
        // Build an app with a pathed buffer and unsaved edits. Insert
        // is at cursor position (default 0), so the '!' lands at the
        // start: "!hello".
        let dir = std::env::temp_dir();
        let path = dir.join(format!("the_editor_close_save_{}.txt", std::process::id()));
        let _ = std::fs::remove_file(&path);

        let buf: Box<dyn Buffer> = Box::new(PieceTableBuffer::from_bytes_with_path(
            b"hello".to_vec(),
            path.clone(),
        ));
        let mut app = App::new(buf);
        app.handle_event(EditorEvent::Insert('!'));
        assert!(app.active_doc().is_dirty());

        app.handle_event(EditorEvent::CloseDoc);
        // Save is the default — confirm directly.
        app.confirm_close_choice();

        // Buffer should be saved → not dirty. Single doc → quit.
        assert!(app.close_confirm.is_none());
        assert!(app.should_quit);

        // File should exist on disk.
        let contents = std::fs::read_to_string(&path).unwrap();
        assert_eq!(contents, "!hello");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn confirm_close_choice_save_without_path_reports_error() {
        // A dirty buffer with no source_path can't be saved. The
        // prompt should drop and the error should land in the status
        // message.
        let mut app = app_with("hello");
        app.handle_event(EditorEvent::Insert('!'));
        assert!(app.active_doc().is_dirty());
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
        // Single doc → didn't quit because the close was aborted.
        assert!(!app.should_quit);
        assert_eq!(app.doc_count(), 1);
    }

    #[test]
    fn open_file_event_with_some_path_loads_directly() {
        // OpenFile(Some(p)) should bypass the native picker and load directly.
        let dir = std::env::temp_dir();
        let path = dir.join(format!("the_editor_open_some_{}.txt", std::process::id()));
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
        let path = dir.join(format!("the_editor_open_new_{}.txt", std::process::id()));
        let _ = std::fs::remove_file(&path);

        let mut app = app_with("buffer");
        app.handle_event(EditorEvent::OpenFile(Some(path.clone())));
        // Buffer is empty (file didn't exist) but path is remembered.
        assert_eq!(app.active_buffer().to_bytes(), b"".to_vec());
        assert_eq!(app.active_doc().path(), Some(path.as_path()));
    }

    // ----- modal key interception -----

    fn key(
        code: crossterm::event::KeyCode,
        mods: crossterm::event::KeyModifiers,
    ) -> crossterm::event::KeyEvent {
        crossterm::event::KeyEvent::new(code, mods)
    }

    #[test]
    fn close_confirm_dispatch_esc_cancels() {
        let mut app = app_with("hello");
        app.handle_event(EditorEvent::Insert('!'));
        app.handle_event(EditorEvent::CloseDoc);
        let consumed = app.dispatch_modal_key(key(
            crossterm::event::KeyCode::Esc,
            crossterm::event::KeyModifiers::NONE,
        ));
        assert!(consumed);
        assert!(app.close_confirm.is_none());
    }

    #[test]
    fn close_confirm_dispatch_enter_confirms_save() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "the_editor_dispatch_save_{}.txt",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let buf: Box<dyn Buffer> = Box::new(PieceTableBuffer::from_bytes_with_path(
            b"hello".to_vec(),
            path.clone(),
        ));
        let mut app = App::new(buf);
        app.handle_event(EditorEvent::Insert('!'));
        app.handle_event(EditorEvent::CloseDoc);
        // Enter on Save (default).
        app.dispatch_modal_key(key(
            crossterm::event::KeyCode::Enter,
            crossterm::event::KeyModifiers::NONE,
        ));
        assert!(app.close_confirm.is_none());
        assert!(app.should_quit, "single doc close = quit");
        let contents = std::fs::read_to_string(&path).unwrap();
        // Insert('!') at default cursor 0 → "!hello".
        assert_eq!(contents, "!hello");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn close_confirm_dispatch_y_drops_with_discard() {
        // Single doc → discard still triggers quit, not removal
        // (consistent with perform_close_active's behaviour). Use
        // multi-doc to verify the discard path actually removes.
        let mut app = app_with_docs(&["alpha", "beta"]);
        app.handle_event(EditorEvent::Insert('!'));
        // Active is 0 (alpha). Make sure it's dirty.
        assert!(app.active_doc().is_dirty());
        app.handle_event(EditorEvent::CloseDoc);
        app.dispatch_modal_key(key(
            crossterm::event::KeyCode::Char('y'),
            crossterm::event::KeyModifiers::NONE,
        ));
        assert!(app.close_confirm.is_none());
        assert_eq!(app.doc_count(), 1, "multi-doc: discard + close");
        // The remaining doc should be "beta" (active slid to index 0).
        assert_eq!(app.active_buffer().to_bytes(), b"beta".to_vec());
    }

    #[test]
    fn close_confirm_dispatch_tab_cycles_choice() {
        let mut app = app_with("hello");
        app.handle_event(EditorEvent::Insert('!'));
        app.handle_event(EditorEvent::CloseDoc);
        assert_eq!(
            app.close_confirm.as_ref().unwrap().choice,
            CloseChoice::Save
        );
        app.dispatch_modal_key(key(
            crossterm::event::KeyCode::Tab,
            crossterm::event::KeyModifiers::NONE,
        ));
        assert_eq!(
            app.close_confirm.as_ref().unwrap().choice,
            CloseChoice::Discard
        );
    }

    // ----- find + replace -----

    #[test]
    fn replace_one_with_empty_query_is_noop() {
        let mut app = app_with("hello world");
        app.handle_event(EditorEvent::FindOpen);
        app.handle_event(EditorEvent::ReplaceOpen);
        app.search.replace_query = "earth".to_string();
        // No find query, so replace should refuse.
        app.handle_event(EditorEvent::ReplaceOne);
        assert_eq!(app.active_buffer().to_bytes(), b"hello world".to_vec());
        let status = app.status_message.as_deref().unwrap_or("");
        assert!(status.contains("nothing to find"), "status: {status}");
    }

    #[test]
    fn replace_one_with_empty_replace_query_is_noop() {
        let mut app = app_with("hello world");
        app.handle_event(EditorEvent::FindOpen);
        app.handle_event(EditorEvent::FindQueryChanged("world".to_string()));
        // Replace query is empty (default).
        assert!(app.search.replace_query.is_empty());
        app.handle_event(EditorEvent::ReplaceOne);
        assert_eq!(app.active_buffer().to_bytes(), b"hello world".to_vec());
        let status = app.status_message.as_deref().unwrap_or("");
        assert!(status.contains("replacement is empty"), "status: {status}");
    }

    #[test]
    fn replace_one_replaces_current_match_and_advances() {
        let mut app = app_with("foo bar foo baz");
        app.handle_event(EditorEvent::FindOpen);
        app.handle_event(EditorEvent::ReplaceQueryChanged("FOO".to_string()));
        app.handle_event(EditorEvent::FindQueryChanged("foo".to_string()));
        // Current match is at byte 0.
        assert_eq!(app.search.current_match(), Some(0));
        app.handle_event(EditorEvent::ReplaceOne);
        // First "foo" became "FOO"; remaining matches shift: was 8, now 8.
        assert_eq!(app.active_buffer().to_bytes(), b"FOO bar foo baz".to_vec());
        // Cursor should advance to next match.
        assert_eq!(app.search.current_match(), Some(8));
    }

    #[test]
    fn replace_one_with_shorter_replacement_collapses_match_list() {
        let mut app = app_with("aaaa bbbb aaaa");
        app.handle_event(EditorEvent::FindOpen);
        app.handle_event(EditorEvent::ReplaceQueryChanged("X".to_string()));
        app.handle_event(EditorEvent::FindQueryChanged("aaaa".to_string()));
        assert_eq!(app.search.matches.len(), 2);
        app.handle_event(EditorEvent::ReplaceOne);
        // First "aaaa" (4 bytes) became "X" (1 byte). The second
        // match's offset shifts from 10 to 7.
        assert_eq!(app.active_buffer().to_bytes(), b"X bbbb aaaa".to_vec());
        assert_eq!(app.search.current_match(), Some(7));
    }

    #[test]
    fn replace_one_with_longer_replacement_shifts_matches() {
        let mut app = app_with("ab ab cd");
        app.handle_event(EditorEvent::FindOpen);
        app.handle_event(EditorEvent::ReplaceQueryChanged("XYZ".to_string()));
        app.handle_event(EditorEvent::FindQueryChanged("ab".to_string()));
        app.handle_event(EditorEvent::ReplaceOne);
        // First "ab" → "XYZ". Second match shifts 3 bytes right.
        assert_eq!(app.active_buffer().to_bytes(), b"XYZ ab cd".to_vec());
        assert_eq!(app.search.current_match(), Some(4));
    }

    #[test]
    fn replace_all_replaces_every_match() {
        let mut app = app_with("foo bar foo baz foo");
        app.handle_event(EditorEvent::FindOpen);
        app.handle_event(EditorEvent::ReplaceQueryChanged("X".to_string()));
        app.handle_event(EditorEvent::FindQueryChanged("foo".to_string()));
        assert_eq!(app.search.matches.len(), 3);
        app.handle_event(EditorEvent::ReplaceAll);
        assert_eq!(app.active_buffer().to_bytes(), b"X bar X baz X".to_vec());
        // No matches remain.
        assert!(app.search.matches.is_empty());
    }

    #[test]
    fn replace_all_with_no_matches_is_noop() {
        let mut app = app_with("hello world");
        app.handle_event(EditorEvent::FindOpen);
        app.handle_event(EditorEvent::ReplaceQueryChanged("X".to_string()));
        app.handle_event(EditorEvent::FindQueryChanged("xyz".to_string()));
        app.handle_event(EditorEvent::ReplaceAll);
        assert_eq!(app.active_buffer().to_bytes(), b"hello world".to_vec());
    }

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
    fn replace_all_invalid_regex_reports_error() {
        let mut app = app_with("hello world");
        app.handle_event(EditorEvent::FindOpen);
        app.handle_event(EditorEvent::FindQueryChanged("(".to_string()));
        app.handle_event(EditorEvent::ToggleFindRegex);
        app.handle_event(EditorEvent::ReplaceOpen);
        app.handle_event(EditorEvent::ReplaceQueryChanged("x".to_string()));
        app.handle_event(EditorEvent::ReplaceAll);
        assert_eq!(app.active_buffer().to_bytes(), b"hello world".to_vec());
        let status = app.status_message.as_deref().unwrap_or("");
        assert!(status.contains("invalid regex"), "status: {status}");
    }

    #[test]
    fn replace_open_also_opens_find_bar_if_closed() {
        let mut app = app_with("hello");
        // Both bars closed.
        assert!(!app.search.bar_open);
        assert!(!app.search.replace_bar_open);
        app.handle_event(EditorEvent::ReplaceOpen);
        assert!(app.search.bar_open, "find bar should auto-open");
        assert!(app.search.replace_bar_open);
    }

    #[test]
    fn find_close_drops_replace_bar() {
        let mut app = app_with("hello");
        app.handle_event(EditorEvent::FindOpen);
        app.handle_event(EditorEvent::ReplaceOpen);
        assert!(app.search.replace_bar_open);
        app.handle_event(EditorEvent::FindClose);
        assert!(
            !app.search.replace_bar_open,
            "coupled: closing find closes replace"
        );
    }

    #[test]
    fn ctrl_r_translates_to_replace_open() {
        // Default binding for ReplaceOpen is Ctrl+R.
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let ev = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL);
        assert_eq!(
            crate::event::translate_key(ev, None),
            Some(EditorEvent::ReplaceOpen)
        );
    }

    // ----- indent settings -----

    #[test]
    fn default_indent_is_spaces_4() {
        let app = app_with("hello");
        assert!(app.active_doc().view.use_spaces);
        assert_eq!(app.active_doc().view.tab_width, 4);
    }

    #[test]
    fn tab_with_default_settings_inserts_4_spaces() {
        let mut app = app_with("");
        app.handle_event(EditorEvent::InsertTab);
        assert_eq!(app.active_buffer().to_bytes(), b"    ".to_vec());
        assert_eq!(app.active_buffer().cursor(), 4);
    }

    #[test]
    fn tab_with_use_spaces_8_inserts_8_spaces() {
        let mut app = app_with("");
        app.active_doc_mut().view.use_spaces = true;
        app.active_doc_mut().view.tab_width = 8;
        app.handle_event(EditorEvent::InsertTab);
        assert_eq!(app.active_buffer().to_bytes(), b"        ".to_vec());
    }

    #[test]
    fn tab_with_use_spaces_false_inserts_tab_char() {
        let mut app = app_with("");
        app.active_doc_mut().view.use_spaces = false;
        app.active_doc_mut().view.tab_width = 4;
        app.handle_event(EditorEvent::InsertTab);
        assert_eq!(app.active_buffer().to_bytes(), b"\t".to_vec());
    }

    #[test]
    fn tab_replaces_selection_with_indent() {
        let mut app = app_with("hello world");
        app.active_buffer_mut()
            .set_selection(Selection { anchor: 0, head: 5 });
        app.handle_event(EditorEvent::InsertTab);
        // "hello" replaced with 4 spaces.
        assert_eq!(app.active_buffer().to_bytes(), b"     world".to_vec());
    }

    #[test]
    fn set_indent_mode_updates_view_and_status() {
        let mut app = app_with("");
        app.handle_event(EditorEvent::SetIndentMode {
            use_spaces: false,
            tab_width: 4,
        });
        assert!(!app.active_doc().view.use_spaces);
        assert_eq!(app.active_doc().view.tab_width, 4);
        let status = app.status_message.as_deref().unwrap_or("");
        assert!(status.contains("tabs"), "status: {status}");
    }

    #[test]
    fn set_indent_mode_clamps_tab_width() {
        // Stray / pathological widths should not break the renderer.
        let mut app = app_with("");
        app.handle_event(EditorEvent::SetIndentMode {
            use_spaces: true,
            tab_width: 0,
        });
        assert_eq!(app.active_doc().view.tab_width, 1, "clamp to >= 1");
        app.handle_event(EditorEvent::SetIndentMode {
            use_spaces: true,
            tab_width: 1000,
        });
        assert_eq!(app.active_doc().view.tab_width, 16, "clamp to <= 16");
    }

    #[test]
    fn cycle_indent_mode_walks_presets() {
        let mut app = app_with("");
        // Default: spaces:4. Cycle: spaces:4 -> spaces:8.
        app.handle_event(EditorEvent::CycleIndentMode);
        let v = &app.active_doc().view;
        assert_eq!((v.use_spaces, v.tab_width), (true, 8));
        // spaces:8 -> tabs (width 4).
        app.handle_event(EditorEvent::CycleIndentMode);
        let v = &app.active_doc().view;
        assert_eq!((v.use_spaces, v.tab_width), (false, 4));
        // tabs -> spaces:2.
        app.handle_event(EditorEvent::CycleIndentMode);
        let v = &app.active_doc().view;
        assert_eq!((v.use_spaces, v.tab_width), (true, 2));
        // spaces:2 -> spaces:4 (back to default).
        app.handle_event(EditorEvent::CycleIndentMode);
        let v = &app.active_doc().view;
        assert_eq!((v.use_spaces, v.tab_width), (true, 4));
    }

    #[test]
    fn indent_settings_are_per_document() {
        // Different docs can have different indent modes — useful
        // when editing a mix of Makefile (tabs) and Python (4
        // spaces) in one session.
        let mut app = app_with_docs(&["x", "y"]);
        app.active = 0;
        app.handle_event(EditorEvent::SetIndentMode {
            use_spaces: false,
            tab_width: 4,
        });
        // Switch to doc 1 — its mode should still be the default.
        app.handle_event(EditorEvent::NextDoc);
        assert!(app.active_doc().view.use_spaces);
        assert_eq!(app.active_doc().view.tab_width, 4);
        // Switch back — doc 0 still has tabs mode.
        app.handle_event(EditorEvent::PrevDoc);
        assert!(!app.active_doc().view.use_spaces);
    }

    #[test]
    fn new_document_uses_config_defaults() {
        let config = core::Config {
            use_spaces: Some(false),
            tab_width: Some(8),
            soft_wrap: Some(true),
            scroll_margin_lines: Some(5),
            ..core::Config::default()
        };
        let buf: Box<dyn core::Buffer> = Box::new(core::PieceTableBuffer::new());
        let app = App::new_with_documents(vec![core::Document::new(buf)], config);
        let v = &app.active_doc().view;
        assert!(!v.use_spaces);
        assert_eq!(v.tab_width, 8);
        assert!(v.soft_wrap);
        assert_eq!(v.scroll_margin_lines, 5);
    }

    #[test]
    fn sync_config_captures_document_defaults() {
        let mut app = app_with("");
        app.handle_event(EditorEvent::SetIndentMode {
            use_spaces: false,
            tab_width: 2,
        });
        app.handle_event(EditorEvent::ToggleSoftWrap);
        app.sync_config();

        assert_eq!(app.config.use_spaces, Some(false));
        assert_eq!(app.config.tab_width, Some(2));
        assert_eq!(app.config.soft_wrap, Some(true));
    }

    // ----- soft-wrap toggle -----

    #[test]
    fn default_soft_wrap_is_off() {
        let app = app_with("hello");
        assert!(!app.active_doc().view.soft_wrap);
    }

    #[test]
    fn toggle_soft_wrap_flips_state_and_status() {
        let mut app = app_with("hello");
        app.handle_event(EditorEvent::ToggleSoftWrap);
        assert!(app.active_doc().view.soft_wrap);
        let status = app.status_message.as_deref().unwrap_or("");
        assert!(status.contains("on"), "status: {status}");
        // Toggle again — back to off.
        app.handle_event(EditorEvent::ToggleSoftWrap);
        assert!(!app.active_doc().view.soft_wrap);
        let status = app.status_message.as_deref().unwrap_or("");
        assert!(status.contains("off"), "status: {status}");
    }

    #[test]
    fn soft_wrap_is_per_document() {
        let mut app = app_with_docs(&["x", "y"]);
        app.active = 0;
        app.handle_event(EditorEvent::ToggleSoftWrap);
        assert!(app.active_doc().view.soft_wrap);
        app.handle_event(EditorEvent::NextDoc);
        assert!(!app.active_doc().view.soft_wrap);
        app.handle_event(EditorEvent::PrevDoc);
        assert!(app.active_doc().view.soft_wrap);
    }

    #[test]
    fn ctrl_shift_w_translates_to_toggle_soft_wrap() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let ev = KeyEvent::new(
            KeyCode::Char('W'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        );
        assert_eq!(
            crate::event::translate_key(ev, None),
            Some(EditorEvent::ToggleSoftWrap)
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
            std::env::temp_dir().join(format!("the_editor_tui_proj_tree_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Cargo.toml"), "[package]").unwrap();
        std::fs::write(dir.join("main.rs"), "fn main() {}").unwrap();

        let path = dir.join("main.rs");
        let buf: Box<dyn Buffer> =
            Box::new(PieceTableBuffer::from_bytes_with_path(b"".to_vec(), path));
        let mut app = App::new(buf);

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
            std::env::temp_dir().join(format!("the_editor_tui_proj_open_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Cargo.toml"), "[package]").unwrap();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/lib.rs"), "pub fn lib() {}").unwrap();

        let path = dir.join("main.rs");
        let buf: Box<dyn Buffer> =
            Box::new(PieceTableBuffer::from_bytes_with_path(b"".to_vec(), path));
        let mut app = App::new(buf);

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

    fn app_with_path(content: &str, path: std::path::PathBuf) -> App {
        let buf: Box<dyn Buffer> = Box::new(core::PieceTableBuffer::from_bytes_with_path(
            content.as_bytes().to_vec(),
            path,
        ));
        App::new_with_documents(vec![Document::new(buf)], core::Config::default())
    }

    #[test]
    fn auto_save_writes_idle_dirty_buffer() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "the_editor_tui_autosave_{}.txt",
            std::process::id()
        ));
        let mut app = app_with_path("hello", path.clone());
        app.handle_event(EditorEvent::Insert('!'));
        assert!(app.active_doc().is_dirty());
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

    // ----- project-wide search / replace -----

    fn temp_project_with_file_tui(
        name: &str,
        contents: &str,
    ) -> (std::path::PathBuf, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "the_editor_tui_proj_{}_{}",
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
        let (_dir, path) = temp_project_with_file_tui("confirm", "foo foo");
        let buf: Box<dyn Buffer> =
            Box::new(core::PieceTableBuffer::from_path(path.clone()).unwrap());
        let mut app = App::new_with_documents(vec![Document::new(buf)], core::Config::default());

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
        let (_dir, path) = temp_project_with_file_tui("reload", "foo foo");
        let buf: Box<dyn Buffer> =
            Box::new(core::PieceTableBuffer::from_path(path.clone()).unwrap());
        let mut app = App::new_with_documents(vec![Document::new(buf)], core::Config::default());

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
        let (_dir, path) = temp_project_with_file_tui("stale", "foo foo");
        let buf: Box<dyn Buffer> =
            Box::new(core::PieceTableBuffer::from_path(path.clone()).unwrap());
        let mut app = App::new_with_documents(vec![Document::new(buf)], core::Config::default());
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

    fn temp_project_for_fuzzy_tui(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "the_editor_tui_fuzzy_{}_{}",
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
        let dir = temp_project_for_fuzzy_tui("list");
        let buf: Box<dyn Buffer> = Box::new(PieceTableBuffer::from_bytes(b"x".to_vec()));
        let mut app = App::new_with_documents(vec![Document::new(buf)], core::Config::default());
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
        let dir = temp_project_for_fuzzy_tui("filter");
        let buf: Box<dyn Buffer> = Box::new(PieceTableBuffer::from_bytes(b"x".to_vec()));
        let mut app = App::new_with_documents(vec![Document::new(buf)], core::Config::default());
        app.documents[0].project = core::Project::from_path(&dir);

        app.handle_event(EditorEvent::FuzzyFinder(Some("lib".to_string())));
        assert_eq!(app.fuzzy_finder.filtered.len(), 1);
        let (idx, _) = app.fuzzy_finder.filtered[0];
        assert!(app.fuzzy_finder.items[idx].display.contains("lib.rs"));
    }

    #[test]
    fn fuzzy_finder_opens_selected_file() {
        let dir = temp_project_for_fuzzy_tui("open");
        let buf: Box<dyn Buffer> = Box::new(PieceTableBuffer::from_bytes(b"x".to_vec()));
        let mut app = App::new_with_documents(vec![Document::new(buf)], core::Config::default());
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
        let dir = temp_project_for_fuzzy_tui("close");
        let buf: Box<dyn Buffer> = Box::new(PieceTableBuffer::from_bytes(b"x".to_vec()));
        let mut app = App::new_with_documents(vec![Document::new(buf)], core::Config::default());
        app.documents[0].project = core::Project::from_path(&dir);

        app.handle_event(EditorEvent::FuzzyFinder(None));
        assert!(app.fuzzy_finder.open);
        app.handle_event(EditorEvent::FuzzyFinderClose);
        assert!(!app.fuzzy_finder.open);
    }

    // ----- git gutter -----

    fn temp_git_repo_tui(
        name: &str,
        file_name: &str,
        content: &str,
    ) -> (std::path::PathBuf, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "the_editor_tui_git_{}_{}",
            std::process::id(),
            name
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        run_git_tui(&dir, &["init", "-q"]);
        run_git_tui(&dir, &["config", "user.email", "test@example.com"]);
        run_git_tui(&dir, &["config", "user.name", "Test"]);
        let path = dir.join(file_name);
        std::fs::write(&path, content).unwrap();
        run_git_tui(&dir, &["add", file_name]);
        run_git_tui(&dir, &["commit", "-q", "-m", "initial"]);
        (dir, path)
    }

    fn run_git_tui(dir: &std::path::Path, args: &[&str]) {
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
        let (_dir, path) = temp_git_repo_tui("unchanged", "a.txt", "line1\nline2\n");
        let buf: Box<dyn Buffer> = Box::new(PieceTableBuffer::from_path(path.clone()).unwrap());
        let app = App::new_with_documents(
            vec![Document::new_with_config(buf, &core::Config::default())],
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
        let (_dir, path) = temp_git_repo_tui("edit", "a.txt", "line1\nline2\n");
        let buf: Box<dyn Buffer> = Box::new(PieceTableBuffer::from_path(path.clone()).unwrap());
        let mut app = App::new_with_documents(
            vec![Document::new_with_config(buf, &core::Config::default())],
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
        let (_dir, path) = temp_git_repo_tui("hunk", "a.txt", "aaa\nbbb\nccc\n");
        let buf: Box<dyn Buffer> = Box::new(PieceTableBuffer::from_path(path.clone()).unwrap());
        let mut app = App::new_with_documents(
            vec![Document::new_with_config(buf, &core::Config::default())],
            core::Config::default(),
        );
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
