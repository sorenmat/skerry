//! The `App` — owns the `Buffer`, view state, and event handling.
//!
//! Split into its own module so `main.rs` stays a thin entry point and
//! so we can unit-test the event-handling logic without spinning up a
//! real terminal.

use std::path::PathBuf;
use std::time::Duration;

use core::{Buffer, BytePos, Document, EditorEvent, Movement, Search, Selection};
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
    /// Open-file dialog. `Some` while the prompt is up. The user types
    /// a path; Enter loads it, Esc cancels. Same intercept pattern as
    /// `close_confirm` and the find bar.
    pub open_file_dialog: Option<OpenFileDialog>,
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

/// State for the open-file text-input dialog. The user types a path
/// into `query`; Enter resolves it via [`App::submit_open_file_dialog`].
pub struct OpenFileDialog {
    pub query: String,
}

impl App {
    /// Create an `App` around a single buffer. Wraps the buffer in a
    /// one-element document list. Test convenience — production code
    /// uses `new_with_documents` so the multi-file CLI path stays
    /// explicit.
    #[allow(dead_code)]
    pub fn new(buffer: Box<dyn Buffer>) -> Self {
        Self::new_with_documents(vec![Document::new(buffer)])
    }

    /// Create an `App` around a pre-built list of documents.
    /// The first document becomes active.
    pub fn new_with_documents(documents: Vec<Document>) -> Self {
        assert!(
            !documents.is_empty(),
            "App needs at least one document"
        );
        Self {
            documents,
            active: 0,
            should_quit: false,
            status_message: None,
            viewport_height: 0,
            search: Search::new(),
            close_confirm: None,
            open_file_dialog: None,
        }
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

    /// Shortcut for `self.active_doc().buffer` as `&dyn Buffer`.
    pub fn active_buffer(&self) -> &dyn Buffer {
        &*self.documents[self.active].buffer
    }

    /// Shortcut for `self.active_doc_mut().buffer` as `&mut dyn Buffer`.
    pub fn active_buffer_mut(&mut self) -> &mut dyn Buffer {
        &mut *self.documents[self.active].buffer
    }

    /// Number of currently open documents.
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
                self.status_message =
                    Some(format!("clipboard unavailable: {e}"));
                None
            }
        };

        loop {
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
                        // Clipboard shortcuts are intercepted before
                        // generic key translation because they need
                        // direct OS access.
                        if let Some(action) =
                            crate::event::classify_clipboard_key(key, self.active_buffer())
                        {
                            self.apply_clipboard_action(&mut clipboard, action);
                            continue;
                        }
                        if let Some(editor_event) =
                            crate::event::translate_key(key, Some(self))
                        {
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
    /// Two prompts today:
    /// - **close_confirm**: Tab/Shift+Tab/Left/Right cycle choice,
    ///   Enter confirms the focused choice, `y` confirms as Discard,
    ///   `n` and Esc cancel.
    /// - **open_file_dialog**: printable chars append, Backspace pops,
    ///   Enter submits, Esc cancels.
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

        if self.open_file_dialog.is_some() {
            match key.code {
                KeyCode::Esc => self.cancel_open_file_dialog(),
                KeyCode::Enter => self.submit_open_file_dialog(),
                KeyCode::Backspace => self.pop_open_file_query(),
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL)
                        && !key.modifiers.contains(KeyModifiers::ALT) =>
                {
                    // Plain printable only — filter out ctrl/alt so
                    // we don't pollute the path with control chars.
                    self.push_open_file_query(c);
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
                        self.status_message =
                            Some(format!("clipboard copy failed: {e}"));
                    }
                } else {
                    self.status_message =
                        Some("clipboard unavailable; copy skipped".into());
                }
            }
            crate::event::ClipboardAction::Cut(text) => {
                if let Some(cb) = clipboard.as_mut() {
                    if let Err(e) = cb.set_text(text) {
                        self.status_message =
                            Some(format!("clipboard cut failed: {e}"));
                    }
                }
                // The buffer side of cut still applies even if the
                // clipboard write failed — the user intended to remove
                // the selected text.
                self.handle_event(EditorEvent::DeleteSelection);
            }
        }
    }

    /// Apply an `EditorEvent` to the buffer / app state.
    pub fn handle_event(&mut self, event: EditorEvent) {
        match event {
            EditorEvent::Insert(ch) => {
                // Selection-aware: a non-collapsed selection is replaced
                // by the inserted character (matches every editor since
                // 1995).
                self.insert_text(&ch.to_string());
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
            EditorEvent::DeleteLeft => {
                // Selection-aware: delete the selection first, fall back
                // to a one-char backspace if nothing is selected.
                if self.delete_selection_if_any() {
                    return;
                }
                let pos = self.active_buffer().cursor();
                if pos > 0 {
                    match self.active_buffer_mut().delete((pos - 1)..pos) {
                        Ok(new_pos) => {
                            self.active_buffer_mut().set_cursor(new_pos);
                            self.active_buffer_mut().set_selection(Selection::collapsed(new_pos));
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
                            self.active_buffer_mut().set_selection(Selection::collapsed(new_pos));
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
                        self.active_buffer_mut().set_selection(Selection::collapsed(new_pos));
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
                        self.active_buffer_mut().set_selection(Selection::collapsed(new_pos));
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
                self.active_doc_mut().view.scroll_x_cols = self.active_doc().view.scroll_x_cols.saturating_sub(1);
            }
            EditorEvent::ScrollRight => {
                self.active_doc_mut().view.scroll_x_cols = self.active_doc().view.scroll_x_cols.saturating_add(1);
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
                    self.active_buffer_mut().set_selection(Selection::collapsed(pos));
                }
            }
            EditorEvent::FindNext => {
                if let Some(pos) = self.search.next_after(self.active_buffer().cursor()) {
                    self.active_buffer_mut().set_cursor(pos);
                    self.active_buffer_mut().set_selection(Selection::collapsed(pos));
                }
            }
            EditorEvent::FindPrev => {
                if let Some(pos) = self.search.prev_before(self.active_buffer().cursor()) {
                    self.active_buffer_mut().set_cursor(pos);
                    self.active_buffer_mut().set_selection(Selection::collapsed(pos));
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
            EditorEvent::SetIndentMode { use_spaces, tab_width } => {
                self.set_indent_mode(use_spaces, tab_width);
            }
            EditorEvent::CycleIndentMode => {
                self.cycle_indent_mode();
            }
            EditorEvent::ToggleSoftWrap => {
                self.toggle_soft_wrap();
            }
            EditorEvent::Move(movement) => {
                let new_pos = self.compute_target(movement);
                self.active_buffer_mut().set_cursor(new_pos);
                self.active_buffer_mut().set_selection(Selection::collapsed(new_pos));
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
                self.active_buffer_mut().set_selection(Selection::collapsed(clamped));
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
                Ok(()) => self.status_message = Some("Saved.".to_string()),
                Err(e) => self.status_message = Some(format!("Save error: {e}")),
            },
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
                self.documents.push(Document::empty());
                self.active = self.documents.len() - 1;
                self.status_message = Some("New document.".to_string());
            }
            EditorEvent::CloseDoc => {
                self.request_close_active();
            }
            EditorEvent::NextDoc => {
                if !self.documents.is_empty() {
                    self.active = (self.active + 1) % self.documents.len();
                }
            }
            EditorEvent::PrevDoc => {
                if !self.documents.is_empty() {
                    self.active =
                        (self.active + self.documents.len() - 1) % self.documents.len();
                }
            }
            EditorEvent::OpenFile(maybe_path) => match maybe_path {
                Some(path) => self.open_path(&path),
                None => self.open_file_dialog = Some(OpenFileDialog { query: String::new() }),
            },
            EditorEvent::Quit => {
                self.should_quit = true;
            }
        }
    }

    /// Shared insertion path used by `Insert(char)` and `InsertTab`.
    /// Selection-aware: a non-collapsed selection is replaced by the
    /// inserted text (matches every editor since 1995). Centralised
    /// here so `Paste` and `InsertTab` don't drift in their error /
    /// cursor-update behaviour.
    fn insert_text(&mut self, text: &str) {
        self.delete_selection_if_any();
        let pos = self.active_buffer().cursor();
        match self.active_buffer_mut().insert(pos, text) {
            Ok(new_pos) => {
                self.active_buffer_mut().set_cursor(new_pos);
                self.active_buffer_mut().set_selection(Selection::collapsed(new_pos));
                self.status_message = None;
            }
            Err(e) => self.status_message = Some(format!("insert error: {e}")),
        }
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
                self.active_buffer_mut().set_selection(Selection::collapsed(new_pos));
                true
            }
            Err(e) => {
                self.status_message = Some(format!("delete error: {e}"));
                false
            }
        }
    }

    /// Compute the byte position a movement should land on.
    fn compute_target(&self, movement: Movement) -> usize {
        let pos = self.active_buffer().cursor();
        let len = self.active_buffer().len();
        match movement {
            Movement::Left => pos.saturating_sub(1),
            Movement::Right => {
                if pos < len {
                    pos + 1
                } else {
                    pos
                }
            }
            Movement::Up => {
                let (line, col) = self.active_buffer().pos_to_linecol(pos)
                    .unwrap_or((0, 0));
                if line == 0 {
                    0
                } else {
                    self.clamped_linecol_to_pos(line - 1, col)
                }
            }
            Movement::Down => {
                let (line, col) = self.active_buffer().pos_to_linecol(pos)
                    .unwrap_or((0, 0));
                if line + 1 >= self.active_buffer().line_count() {
                    pos
                } else {
                    self.clamped_linecol_to_pos(line + 1, col)
                }
            }
            Movement::PageUp => {
                let (line, col) = self.active_buffer().pos_to_linecol(pos)
                    .unwrap_or((0, 0));
                let page = self.viewport_lines();
                if line == 0 {
                    0
                } else {
                    let target = line.saturating_sub(page);
                    self.clamped_linecol_to_pos(target, col)
                }
            }
            Movement::PageDown => {
                let (line, col) = self.active_buffer().pos_to_linecol(pos)
                    .unwrap_or((0, 0));
                let page = self.viewport_lines();
                let last = self.active_buffer().line_count().saturating_sub(1);
                let target = (line + page).min(last);
                self.clamped_linecol_to_pos(target, col)
            }
            Movement::WordLeft => self.skip_word_left_from(pos),
            Movement::WordRight => self.skip_word_right_from(pos),
            Movement::LineStart => {
                let (line, _) = self.active_buffer().pos_to_linecol(pos).unwrap_or((0, 0));
                self.active_buffer().linecol_to_pos(line, 0).unwrap_or(0)
            }
            Movement::LineEnd => {
                let (line, _) = self.active_buffer().pos_to_linecol(pos).unwrap_or((0, 0));
                // line_byte_range gives the [start, end) byte range for the
                // line; its end is the cursor's LineEnd target.
                self.active_buffer().line_byte_range(line)
                    .map(|r| r.end)
                    .unwrap_or(len)
            }
            Movement::DocumentStart => 0,
            Movement::DocumentEnd => len,
        }
    }

    /// Resolve `(line, col)` to a byte position, clamping `col` to the
    /// target line's actual byte length when it exceeds the line.
    /// Without this clamp, `linecol_to_pos` returns `None` for a col
    /// past the end of a line, and the vertical-movement handlers
    /// would silently fail to move the cursor when the target line
    /// is shorter than the current column (e.g. cursor at column 30
    /// on a long line, Up to a 5-char line above — should land at
    /// the end of that 5-char line, not stay put).
    fn clamped_linecol_to_pos(&self, line: usize, col: usize) -> usize {
        let Some(range) = self.active_buffer().line_byte_range(line) else {
            // Line is out of range — fall back to buffer end. This
            // shouldn't happen in normal Up/Down/PageUp/PageDown
            // because the caller bounds-checks, but we don't want a
            // panic if it ever does.
            return self.active_buffer().len();
        };
        let line_byte_len = range.end - range.start;
        let clamped_col = col.min(line_byte_len);
        self.active_buffer()
            .linecol_to_pos(line, clamped_col)
            .unwrap_or(range.end)
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
        let (line, _) = self.active_buffer().pos_to_linecol(pos).unwrap_or((0, 0));
        let line_text = match self.active_buffer().line_text(line) {
            Some(cow) => cow.into_owned(),
            None => return pos.saturating_sub(1),
        };
        // We work in chars; convert pos to char-col, scan, then back.
        let char_col = core::byte_to_char_col(&line_text, pos.saturating_sub(0));
        let chars: Vec<char> = line_text.chars().collect();
        let mut i = char_col.min(chars.len());
        // If we're sitting on a word char, eat back through word chars
        // until we hit a non-word char.
        if i == 0 {
            return 0;
        }
        if is_word_char(chars[i - 1]) {
            while i > 0 && is_word_char(chars[i - 1]) {
                i -= 1;
            }
        } else {
            // Skip non-word chars.
            while i > 0 && !is_word_char(chars[i - 1]) {
                i -= 1;
            }
            // Then skip word chars if we landed on one.
            while i > 0 && is_word_char(chars[i - 1]) {
                i -= 1;
            }
        }
        core::char_col_to_byte_col(&line_text, i)
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
        let line_byte_start = self.active_buffer().line_byte_range(line)
            .map(|r| r.start)
            .unwrap_or(0);
        let line_byte_end = self.active_buffer().line_byte_range(line)
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
            return self.active_buffer().line_byte_range(line + 1)
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
        let Some(pos) = self.search.current_match() else {
            self.status_message = Some("Replace: no current match.".to_string());
            return;
        };
        let end = pos + self.search.query.len();
        let replacement = self.search.replace_query.clone();
        if let Err(e) = self.active_buffer_mut().replace(pos..end, &replacement) {
            self.status_message = Some(format!("Replace error: {e}"));
            return;
        }
        // Refresh matches (positions shifted) — keep cursor at the
        // replacement start so the user can see what changed.
        self.search.refresh(&self.active_buffer().to_bytes());
        self.active_buffer_mut().set_cursor(pos);
        self.active_buffer_mut().set_selection(Selection::collapsed(pos));
        // Advance to next match.
        if let Some(next) = self.search.next_after(pos) {
            self.active_buffer_mut().set_cursor(next);
            self.active_buffer_mut().set_selection(Selection::collapsed(next));
            self.status_message =
                Some(format!("Replaced 1; advanced to match {}/{}.",
                             self.search.current.unwrap_or(0) + 1,
                             self.search.matches.len()));
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
        let matches: Vec<usize> = self.search.matches.clone();
        if matches.is_empty() {
            self.status_message = Some("Replace all: no matches.".to_string());
            return;
        }
        let count = matches.len();
        let query_len = self.search.query.len();
        let replacement = self.search.replace_query.clone();
        // Wrap in one edit group so the whole batch is a single undo.
        self.active_buffer_mut().begin_edit_group();
        let mut err = None;
        for &pos in matches.iter().rev() {
            if let Err(e) = self.active_buffer_mut().replace(pos..pos + query_len, &replacement) {
                err = Some(e);
                break;
            }
        }
        self.active_buffer_mut().end_edit_group();
        if let Some(e) = err {
            self.status_message = Some(format!("Replace error: {e}"));
            return;
        }
        // Refresh matches (positions shifted, count may be different
        // if replacement contains the query — recursive replace
        // semantics are deliberately NOT implemented here; we
        // snapshot matches before any replace so the loop is bounded).
        self.search.refresh(&self.active_buffer().to_bytes());
        self.active_buffer_mut().set_cursor(0);
        self.active_buffer_mut().set_selection(Selection::collapsed(0));
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
        let cursor_line = self.active_buffer().pos_to_linecol(cursor_pos)
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
        let (line, _) = self.active_buffer().pos_to_linecol(cursor_pos)
            .unwrap_or((0, 0));
        let Some(line_range) = self.active_buffer().line_byte_range(line) else {
            return;
        };
        let line_count = self.active_buffer().line_count();
        if line + 1 < line_count {
            // Not the last line — eat the trailing newline so the next
            // line shifts up.
            let next_line_start = self.active_buffer().line_byte_range(line + 1)
                .map(|r| r.start)
                .unwrap_or(line_range.end);
            match self.active_buffer_mut().delete(line_range.start..next_line_start) {
                Ok(np) => {
                    self.active_buffer_mut().set_cursor(np);
                    self.active_buffer_mut().set_selection(Selection::collapsed(np));
                }
                Err(e) => self.status_message = Some(format!("delete error: {e}")),
            }
        } else if line > 0 {
            // Last line, no trailing newline — eat the preceding
            // newline so the buffer gets shorter.
            let prev_end = self.active_buffer().line_byte_range(line - 1)
                .map(|r| r.end)
                .unwrap_or(line_range.start);
            match self.active_buffer_mut().delete(prev_end..line_range.end) {
                Ok(np) => {
                    self.active_buffer_mut().set_cursor(np);
                    self.active_buffer_mut().set_selection(Selection::collapsed(np));
                }
                Err(e) => self.status_message = Some(format!("delete error: {e}")),
            }
        } else {
            // Only line in buffer, no newline. Just clear it.
            match self.active_buffer_mut().delete(0..line_range.end) {
                Ok(np) => {
                    self.active_buffer_mut().set_cursor(np);
                    self.active_buffer_mut().set_selection(Selection::collapsed(np));
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
        let (line, _) = self.active_buffer().pos_to_linecol(cursor_pos)
            .unwrap_or((0, 0));
        let Some(line_range) = self.active_buffer().line_byte_range(line) else {
            return;
        };
        let line_count = self.active_buffer().line_count();
        let line_text = self.active_buffer().slice(line_range.clone())
            .unwrap_or_default();
        let line_ends_with_newline = line_text.ends_with('\n');
        if line + 1 < line_count {
            // Insert just before the next line, with a newline if the
            // current line doesn't end in one.
            let insert_pos = self.active_buffer().line_byte_range(line + 1)
                .map(|r| r.start)
                .unwrap_or(line_range.end);
            let to_insert = if line_ends_with_newline {
                line_text.clone()
            } else {
                format!("{line_text}\n")
            };
            match self.active_buffer_mut().insert(insert_pos, &to_insert) {
                Ok(np) => {
                    let new_line_start = np - to_insert.len()
                        + if line_ends_with_newline { 1 } else { 0 };
                    self.active_buffer_mut().set_cursor(new_line_start);
                    self.active_buffer_mut().set_selection(Selection::collapsed(new_line_start));
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
                    self.active_buffer_mut().set_selection(Selection::collapsed(new_line_start));
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
        let (line, _) = self.active_buffer().pos_to_linecol(cursor_pos)
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
        // Compute byte ranges that INCLUDE the trailing newline so the
        // swap preserves line structure.
        let line_with_nl = |excl: std::ops::Range<usize>, l: usize| -> std::ops::Range<usize> {
            let start = excl.start;
            let end = if l + 1 < line_count {
                self.active_buffer().line_byte_range(l + 1)
                    .map(|r| r.start)
                    .unwrap_or(excl.end)
            } else {
                self.active_buffer().len()
            };
            start..end
        };
        let my_range = line_with_nl(my_range_excl, line);
        let other_range = line_with_nl(other_range_excl, target_line);
        let my_text = self.active_buffer().slice(my_range.clone()).unwrap_or_default();
        let other_text = self.active_buffer().slice(other_range.clone())
            .unwrap_or_default();
        // Adjacent lines — delete their union in one shot. Then
        // reinsert in swapped order at delete_start. The text that
        // ends up at the LOWER position goes at delete_start first;
        // the higher-position text goes after it.
        let delete_start = my_range.start.min(other_range.start);
        let delete_end = my_range.end.max(other_range.end);
        let _ = self.active_buffer_mut().delete_silent(delete_start..delete_end);
        let (lower_text, higher_text) = if delta < 0 {
            // Move up: my line lands at the lower slot.
            (my_text.as_str(), other_text.as_str())
        } else {
            // Move down: other line stays at the lower slot.
            (other_text.as_str(), my_text.as_str())
        };
        let _ = self.active_buffer_mut().insert_silent(delete_start, lower_text);
        let _ = self.active_buffer_mut().insert_silent(delete_start + lower_text.len(), higher_text);
        let new_pos = self.active_buffer().linecol_to_pos(line, 0)
            .unwrap_or(cursor_pos);
        self.active_buffer_mut().set_cursor(new_pos);
        self.active_buffer_mut().set_selection(Selection::collapsed(new_pos));
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

        let gutter_width = total_lines.to_string().len().max(2);
        let prefix_text = format!("{:>width$} │ ", 1, width = gutter_width);
        let prefix_chars = prefix_text.chars().count() as u16;

        let line_byte_start = self.active_buffer().line_byte_range(doc_line)?.start;

        if col < prefix_chars {
            // Click in the gutter — position at the start of the line.
            return Some(line_byte_start);
        }

        let line_text = self.active_buffer().line_text(doc_line)?.into_owned();
        // Account for horizontal scroll: the rendered text starts at
        // char `scroll_x` of the full line.
        let scroll_x = self.active_doc().view.scroll_x_cols;
        let text_col = (col - prefix_chars) as usize;
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
            // opening the open-file dialog over an existing one would
            // be confusing, so we let close-confirm win.
            self.open_file_dialog = None;
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
        self.documents.remove(self.active);
        if self.active >= self.documents.len() {
            self.active = self.documents.len() - 1;
        }
        self.status_message = Some("Closed document.".to_string());
    }

    /// Append a character to the open-file dialog's text input.
    /// No-op when the dialog isn't open.
    pub fn push_open_file_query(&mut self, ch: char) {
        if let Some(d) = self.open_file_dialog.as_mut() {
            d.query.push(ch);
        }
    }

    /// Remove the last character from the open-file dialog's text
    /// input. No-op when the dialog isn't open or the query is empty.
    pub fn pop_open_file_query(&mut self) {
        if let Some(d) = self.open_file_dialog.as_mut() {
            d.query.pop();
        }
    }

    /// Cancel the open-file dialog. No-op when it isn't open.
    pub fn cancel_open_file_dialog(&mut self) {
        if self.open_file_dialog.take().is_some() {
            self.status_message = Some("Open cancelled.".to_string());
        }
    }

    /// Submit the open-file dialog's current query as a path to load.
    /// On success the active document's buffer is replaced with the
    /// file's contents; the dialog drops. On error (file exists but
    /// can't be read) the dialog drops and the error lands in the
    /// status bar.
    ///
    /// `path` is interpreted as a filesystem path. An empty query
    /// is treated as Cancel — pressing Enter on an empty prompt
    /// shouldn't blow up with a confusing I/O error.
    pub fn submit_open_file_dialog(&mut self) {
        let Some(dialog) = self.open_file_dialog.take() else {
            return;
        };
        if dialog.query.is_empty() {
            self.status_message = Some("Open cancelled.".to_string());
            return;
        }
        let path = PathBuf::from(dialog.query);
        self.open_path(&path);
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
        self.documents[self.active] = Document::new(buffer);
        self.status_message = Some(format!(
            "Opened {}",
            path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("<path>")
        ));
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
        app.active_buffer_mut().set_selection(Selection::collapsed(2));
        app.handle_event(EditorEvent::SelectExtend(Movement::Right));
        let sel = app.active_buffer().selection();
        assert_eq!(sel.anchor, 2);
        assert_eq!(sel.head, 3);
    }

    #[test]
    fn set_cursor_event_collapses_selection() {
        let mut app = app_with("hello world");
        app.active_buffer_mut().set_selection(Selection {
            anchor: 0,
            head: 5,
        });
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
        app.active_buffer_mut().set_selection(Selection::collapsed(2));
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
        app.active_buffer_mut().set_selection(Selection::collapsed(6));
        app.handle_event(EditorEvent::Paste("beautiful ".to_string()));
        assert_eq!(app.active_buffer().to_bytes(), b"hello beautiful world".to_vec());
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
        assert_eq!(app.search.matches, vec![10]);
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
        let (line, col) = app.active_buffer().pos_to_linecol(app.active_buffer().cursor()).unwrap();
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
        let (line, _) = app.active_buffer().pos_to_linecol(app.active_buffer().cursor()).unwrap();
        assert_eq!(line, 3);
    }

    #[test]
    fn page_down_clamps_to_last_line() {
        let mut app = make_multi_line_app();
        app.viewport_height = 100;
        let pos = app.active_buffer().linecol_to_pos(0, 0).unwrap();
        app.active_buffer_mut().set_cursor(pos);
        app.handle_event(EditorEvent::Move(Movement::PageDown));
        let (line, _) = app.active_buffer().pos_to_linecol(app.active_buffer().cursor()).unwrap();
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
        let buf: Box<dyn Buffer> =
            Box::new(PieceTableBuffer::from_bytes(s.into_bytes()));
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
        app.active_buffer_mut().set_selection(Selection { anchor: 6, head: 11 });
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
        assert_eq!(app.active_buffer().to_bytes(), b"alpha\nbeta\nbeta\ngamma".to_vec());
    }

    #[test]
    fn move_line_up_swaps_with_previous() {
        let mut app = app_with("alpha\nbeta\ngamma");
        app.active_buffer_mut().set_cursor(8); // 'b' in "beta"
        app.handle_event(EditorEvent::MoveLineUp);
        assert_eq!(app.active_buffer().to_bytes(), b"beta\nalpha\ngamma".to_vec());
    }

    #[test]
    fn move_line_down_swaps_with_next() {
        let mut app = app_with("alpha\nbeta\ngamma");
        app.active_buffer_mut().set_cursor(2); // somewhere in "alpha"
        app.handle_event(EditorEvent::MoveLineDown);
        assert_eq!(app.active_buffer().to_bytes(), b"beta\nalpha\ngamma".to_vec());
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
        App::new_with_documents(docs)
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
            app.documents[0].view.scroll_top_line,
            top_doc0,
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
        let mut app = app_with(
            "l0\nl1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nl9\nl10\nl11\nl12\nl13\nl14",
        );
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
        assert_eq!(app.close_confirm.as_ref().unwrap().choice, CloseChoice::Save);
        app.cycle_close_choice(1);
        assert_eq!(app.close_confirm.as_ref().unwrap().choice, CloseChoice::Discard);
        app.cycle_close_choice(1);
        assert_eq!(app.close_confirm.as_ref().unwrap().choice, CloseChoice::Cancel);
        app.cycle_close_choice(1);
        assert_eq!(app.close_confirm.as_ref().unwrap().choice, CloseChoice::Save);
        // Backward wraps too.
        app.cycle_close_choice(-1);
        assert_eq!(app.close_confirm.as_ref().unwrap().choice, CloseChoice::Cancel);
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
        assert_eq!(app.close_confirm.as_ref().unwrap().choice, CloseChoice::Cancel);
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

        assert!(app.close_confirm.is_none(), "prompt dropped on save failure");
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
    fn open_file_dialog_opens_on_openfile_none() {
        let mut app = app_with("hello");
        assert!(app.open_file_dialog.is_none());
        app.handle_event(EditorEvent::OpenFile(None));
        assert!(app.open_file_dialog.is_some());
        assert_eq!(app.open_file_dialog.as_ref().unwrap().query, "");
    }

    #[test]
    fn open_file_dialog_push_pop_query() {
        let mut app = app_with("hello");
        app.handle_event(EditorEvent::OpenFile(None));
        app.push_open_file_query('/');
        app.push_open_file_query('t');
        app.push_open_file_query('m');
        app.push_open_file_query('p');
        assert_eq!(app.open_file_dialog.as_ref().unwrap().query, "/tmp");
        app.pop_open_file_query();
        assert_eq!(app.open_file_dialog.as_ref().unwrap().query, "/tm");
        // Popping more than the query just empties (String::pop).
        app.pop_open_file_query();
        app.pop_open_file_query();
        app.pop_open_file_query();
        app.pop_open_file_query();
        assert_eq!(app.open_file_dialog.as_ref().unwrap().query, "");
    }

    #[test]
    fn open_file_dialog_cancel_drops_dialog() {
        let mut app = app_with("hello");
        app.handle_event(EditorEvent::OpenFile(None));
        app.push_open_file_query('/');
        app.cancel_open_file_dialog();
        assert!(app.open_file_dialog.is_none());
    }

    #[test]
    fn open_file_dialog_submit_empty_cancels() {
        let mut app = app_with("hello");
        app.handle_event(EditorEvent::OpenFile(None));
        app.submit_open_file_dialog();
        assert!(app.open_file_dialog.is_none(), "empty submit = cancel");
        // Original buffer untouched.
        assert_eq!(app.active_buffer().to_bytes(), b"hello".to_vec());
    }

    #[test]
    fn open_file_dialog_submit_loads_existing_file() {
        // Write a temp file with known contents.
        let dir = std::env::temp_dir();
        let path = dir.join(format!("the_editor_open_existing_{}.txt", std::process::id()));
        std::fs::write(&path, b"from disk").unwrap();

        let mut app = app_with("buffer");
        app.handle_event(EditorEvent::OpenFile(None));
        app.push_open_file_query(path.to_string_lossy().chars().next().unwrap());
        for c in path.to_string_lossy().chars().skip(1) {
            app.push_open_file_query(c);
        }
        app.submit_open_file_dialog();

        assert!(app.open_file_dialog.is_none());
        assert_eq!(app.active_buffer().to_bytes(), b"from disk".to_vec());
        assert_eq!(app.active_doc().path(), Some(path.as_path()));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn open_file_event_with_some_path_loads_directly() {
        // OpenFile(Some(p)) should bypass the dialog and load directly.
        let dir = std::env::temp_dir();
        let path = dir.join(format!("the_editor_open_some_{}.txt", std::process::id()));
        std::fs::write(&path, b"hi").unwrap();

        let mut app = app_with("buffer");
        app.handle_event(EditorEvent::OpenFile(Some(path.clone())));
        assert!(app.open_file_dialog.is_none(), "Some path skips dialog");
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

    #[test]
    fn close_confirm_drops_open_file_dialog_when_opened() {
        // Defensive: opening the close-confirm should drop any
        // open-file dialog so the user isn't asked to navigate
        // two prompts at once.
        let mut app = app_with("hello");
        app.handle_event(EditorEvent::Insert('!'));
        app.handle_event(EditorEvent::OpenFile(None));
        assert!(app.open_file_dialog.is_some());
        app.handle_event(EditorEvent::CloseDoc);
        assert!(app.close_confirm.is_some());
        assert!(app.open_file_dialog.is_none());
    }

    // ----- modal key interception -----

    fn key(code: crossterm::event::KeyCode, mods: crossterm::event::KeyModifiers) -> crossterm::event::KeyEvent {
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
        let path = dir.join(format!("the_editor_dispatch_save_{}.txt", std::process::id()));
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
        assert_eq!(app.close_confirm.as_ref().unwrap().choice, CloseChoice::Save);
        app.dispatch_modal_key(key(
            crossterm::event::KeyCode::Tab,
            crossterm::event::KeyModifiers::NONE,
        ));
        assert_eq!(app.close_confirm.as_ref().unwrap().choice, CloseChoice::Discard);
    }

    #[test]
    fn open_file_dialog_dispatch_char_appends() {
        let mut app = app_with("hello");
        app.handle_event(EditorEvent::OpenFile(None));
        app.dispatch_modal_key(key(
            crossterm::event::KeyCode::Char('a'),
            crossterm::event::KeyModifiers::NONE,
        ));
        app.dispatch_modal_key(key(
            crossterm::event::KeyCode::Char('b'),
            crossterm::event::KeyModifiers::NONE,
        ));
        app.dispatch_modal_key(key(
            crossterm::event::KeyCode::Char('c'),
            crossterm::event::KeyModifiers::NONE,
        ));
        assert_eq!(app.open_file_dialog.as_ref().unwrap().query, "abc");
    }

    #[test]
    fn open_file_dialog_dispatch_backspace_pops() {
        let mut app = app_with("hello");
        app.handle_event(EditorEvent::OpenFile(None));
        app.push_open_file_query('a');
        app.push_open_file_query('b');
        app.push_open_file_query('c');
        app.dispatch_modal_key(key(
            crossterm::event::KeyCode::Backspace,
            crossterm::event::KeyModifiers::NONE,
        ));
        assert_eq!(app.open_file_dialog.as_ref().unwrap().query, "ab");
    }

    #[test]
    fn open_file_dialog_dispatch_esc_cancels() {
        let mut app = app_with("hello");
        app.handle_event(EditorEvent::OpenFile(None));
        app.dispatch_modal_key(key(
            crossterm::event::KeyCode::Esc,
            crossterm::event::KeyModifiers::NONE,
        ));
        assert!(app.open_file_dialog.is_none());
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
        assert!(!app.search.replace_bar_open, "coupled: closing find closes replace");
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
        app.active_buffer_mut().set_selection(Selection {
            anchor: 0,
            head: 5,
        });
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
}