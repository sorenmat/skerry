//! `EditorApp` — owns the `Buffer`, view state, and event handling.
//!
//! Mirrors `frontend_tui::App` so the event-handling logic stays in
//! lockstep across frontends (ADR 0005).

use std::path::PathBuf;

use core::{Buffer, Document, EditorEvent, Movement, Search, Selection};
use eframe::egui;
use eframe::egui::Context;
use eframe::App;

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
    /// Open-file dialog. `Some` while the prompt is up. The user types
    /// a path; Enter loads it, Esc cancels. Mirrors the TUI's
    /// `open_file_dialog`.
    pub open_file_dialog: Option<OpenFileDialog>,
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

/// State for the open-file text-input dialog.
pub struct OpenFileDialog {
    pub query: String,
}

impl EditorApp {
    /// Create an `EditorApp` around a single buffer. Wraps the buffer
    /// in a one-element document list.
    pub fn new(buffer: Box<dyn Buffer>) -> Self {
        Self::new_with_documents(vec![Document::new(buffer)])
    }

    /// Create an `EditorApp` around a pre-built list of documents.
    /// The first document becomes active.
    pub fn new_with_documents(documents: Vec<Document>) -> Self {
        assert!(
            !documents.is_empty(),
            "EditorApp needs at least one document"
        );
        Self {
            documents,
            active: 0,
            should_quit: false,
            status_message: None,
            viewport_lines: 20,
            search: Search::new(),
            close_confirm: None,
            open_file_dialog: None,
        }
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
                if let Some(clip) =
                    crate::event::classify_clipboard_event(event, self.active_buffer())
                {
                    clipboard_events.push(clip);
                    continue;
                }
                if self.search.bar_open {
                    if let Some(bar_event) = find_bar_translate(event) {
                        self.handle_event(bar_event);
                        continue;
                    }
                }
                // Modal prompts intercept keys before translate_event
                // so e.g. Ctrl+W inside the open-file dialog doesn't
                // bounce back into a close-confirm prompt.
                if self.dispatch_modal_event(event) {
                    continue;
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
    /// - open_file_dialog: printable chars / Backspace edit the path,
    ///   Enter submits, Esc cancels.
    ///
    /// Returns `true` when the event was consumed.
    fn dispatch_modal_event(&mut self, event: &eframe::egui::Event) -> bool {
        use eframe::egui::{Event, Key};

        if self.close_confirm.is_some() {
            if let Event::Key { key, pressed: true, modifiers, .. } = event {
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

        if self.open_file_dialog.is_some() {
            if let Event::Key { key, pressed: true, .. } = event {
                match *key {
                    Key::Escape => self.cancel_open_file_dialog(),
                    Key::Enter => self.submit_open_file_dialog(),
                    Key::Backspace => self.pop_open_file_query(),
                    _ => {}
                }
            }
            if let Event::Text(text) = event {
                if !text.is_empty() {
                    for c in text.chars() {
                        self.push_open_file_query(c);
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
    fn apply_clipboard_action(
        &mut self,
        ctx: &Context,
        action: crate::event::ClipboardAction,
    ) {
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
        match event {
            EditorEvent::Insert(ch) => {
                // Selection-aware: a non-collapsed selection is replaced
                // by the inserted character (matches every editor since
                // 1995).
                self.delete_selection_if_any();
                let pos = self.active_buffer().cursor();
                let s = ch.to_string();
                match self.active_buffer_mut().insert(pos, &s) {
                    Ok(new_pos) => {
                        self.active_buffer_mut().set_cursor(new_pos);
                        self.active_buffer_mut().set_selection(Selection::collapsed(new_pos));
                        self.status_message = None;
                    }
                    Err(e) => self.status_message = Some(format!("insert error: {e}")),
                }
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
                let target = skip_word_right(self.active_buffer(), pos);
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
                    // Auto-scroll on next render: the per-doc
                    // `last_seen_cursor` still holds the old value, so
                    // the render path detects motion and scrolls into
                    // view.
                }
            }
            EditorEvent::FindPrev => {
                if let Some(pos) = self.search.prev_before(self.active_buffer().cursor()) {
                    self.active_buffer_mut().set_cursor(pos);
                    self.active_buffer_mut().set_selection(Selection::collapsed(pos));
                    // See FindNext.
                }
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
                // otherwise insert at cursor.
                self.delete_selection_if_any();
                let pos = self.active_buffer().cursor();
                match self.active_buffer_mut().insert(pos, &text) {
                    Ok(new_pos) => {
                        self.active_buffer_mut().set_cursor(new_pos);
                        self.active_buffer_mut().set_selection(Selection::collapsed(new_pos));
                        self.status_message = None;
                    }
                    Err(e) => self.status_message = Some(format!("paste error: {e}")),
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

    /// Compute the byte position a movement should land on. Identical
    /// to `frontend_tui::App::compute_target`.
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
                let (line, col) = self.active_buffer().pos_to_linecol(pos).unwrap_or((0, 0));
                if line == 0 {
                    0
                } else {
                    self.active_buffer().linecol_to_pos(line - 1, col)
                        .unwrap_or(pos)
                }
            }
            Movement::Down => {
                let (line, col) = self.active_buffer().pos_to_linecol(pos).unwrap_or((0, 0));
                if line + 1 >= self.active_buffer().line_count() {
                    pos
                } else {
                    self.active_buffer().linecol_to_pos(line + 1, col)
                        .unwrap_or(pos)
                }
            }
            Movement::PageUp => {
                let (line, col) = self.active_buffer().pos_to_linecol(pos).unwrap_or((0, 0));
                let page = self.viewport_lines.max(1);
                let target = line.saturating_sub(page);
                self.active_buffer().linecol_to_pos(target, col)
                    .unwrap_or(pos)
            }
            Movement::PageDown => {
                let (line, col) = self.active_buffer().pos_to_linecol(pos).unwrap_or((0, 0));
                let page = self.viewport_lines.max(1);
                let last = self.active_buffer().line_count().saturating_sub(1);
                let target = (line + page).min(last);
                self.active_buffer().linecol_to_pos(target, col)
                    .unwrap_or(pos)
            }
            Movement::WordLeft => skip_word_left(self.active_buffer(), pos),
            Movement::WordRight => skip_word_right(self.active_buffer(), pos),
            Movement::LineStart => {
                let (line, _) = self.active_buffer().pos_to_linecol(pos).unwrap_or((0, 0));
                self.active_buffer().linecol_to_pos(line, 0).unwrap_or(0)
            }
            Movement::LineEnd => {
                let (line, _) = self.active_buffer().pos_to_linecol(pos).unwrap_or((0, 0));
                self.active_buffer().line_byte_range(line)
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
        let (line, _) = self.active_buffer().pos_to_linecol(cursor_pos)
            .unwrap_or((0, 0));
        let Some(line_range) = self.active_buffer().line_byte_range(line) else {
            return;
        };
        let line_count = self.active_buffer().line_count();
        if line + 1 < line_count {
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
        } else if let Err(e) = self.active_buffer_mut().delete(0..line_range.end) {
            self.status_message = Some(format!("delete error: {e}"));
        } else {
            self.active_buffer_mut().set_cursor(0);
            self.active_buffer_mut().set_selection(Selection::collapsed(0));
        }
        let _ = line_count;
    }

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
        let Some(my_range_excl_nl) = self.active_buffer().line_byte_range(line) else {
            return;
        };
        let Some(other_range_excl_nl) = self.active_buffer().line_byte_range(target_line) else {
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
        let my_range = line_with_nl(my_range_excl_nl, line);
        let other_range = line_with_nl(other_range_excl_nl, target_line);
        let my_text = self.active_buffer().slice(my_range.clone()).unwrap_or_default();
        let other_text = self.active_buffer().slice(other_range.clone())
            .unwrap_or_default();
        // Adjacent lines — delete their union in one shot (the
        // newline between them is part of one of the ranges, so no
        // content between them is lost). Then reinsert in swapped
        // order at delete_start. The text that ends up at the LOWER
        // position goes at delete_start first; the higher-position
        // text goes after it.
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
        if self.active_doc().is_dirty() {
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
            CloseChoice::Save => {
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
    /// On success the active document's buffer is replaced; on error
    /// the dialog drops and the error lands in the status bar.
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
        self.documents[self.active] = Document::new(buffer);
        self.status_message = Some(format!(
            "Opened {}",
            path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("<path>")
        ));
    }
}

impl App for EditorApp {
    fn update(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        // 1. Pull input events from egui and apply them to the buffer.
        self.handle_input(ctx);

        // 2. Render the frame.
        crate::ui::render(ctx, self);

        // 3. Close the window if the user requested quit (Ctrl+Q / Esc).
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
    let line_byte_end = buffer
        .line_byte_range(line)
        .map(|r| r.end)
        .unwrap_or(len);
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
        app.active_buffer_mut().set_selection(Selection::collapsed(2));
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

    // ----- PageUp / PageDown / Word movement -----

    fn make_multi_line_app() -> EditorApp {
        let content: String = (0..10).map(|i| format!("line{i}\n")).collect();
        let content = content.trim_end_matches('\n').to_string();
        let buf: Box<dyn Buffer> =
            Box::new(PieceTableBuffer::from_bytes(content.into_bytes()));
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
            app.active_buffer().pos_to_linecol(app.active_buffer().cursor()),
            Some((4, 2))
        );
        app.handle_event(EditorEvent::Move(Movement::PageDown));
        app.handle_event(EditorEvent::Move(Movement::PageDown));
        assert_eq!(
            app.active_buffer().pos_to_linecol(app.active_buffer().cursor()),
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
        assert_eq!(app.active_buffer().to_bytes(), b"alpha\nbeta\nbeta\ngamma".to_vec());
    }

    #[test]
    fn move_line_down_swaps_with_next() {
        let mut app = app_with("alpha\nbeta\ngamma");
        app.active_buffer_mut().set_cursor(2);
        app.handle_event(EditorEvent::MoveLineDown);
        assert_eq!(app.active_buffer().to_bytes(), b"beta\nalpha\ngamma".to_vec());
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
        EditorApp::new_with_documents(docs)
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
            app.active_doc().view.last_seen_cursor, 0,
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
        assert_eq!(app.close_confirm.as_ref().unwrap().choice, CloseChoice::Discard);
        app.cycle_close_choice(1);
        assert_eq!(app.close_confirm.as_ref().unwrap().choice, CloseChoice::Cancel);
        app.cycle_close_choice(1);
        assert_eq!(app.close_confirm.as_ref().unwrap().choice, CloseChoice::Save);
        app.cycle_close_choice(-1);
        assert_eq!(app.close_confirm.as_ref().unwrap().choice, CloseChoice::Cancel);
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
        let path = dir.join(format!("the_editor_gui_close_save_{}.txt", std::process::id()));
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
        assert!(app.close_confirm.is_none(), "prompt dropped on save failure");
        let status = app.status_message.as_deref().unwrap_or("");
        assert!(status.contains("Save error"),
                "expected save error in status: {status}");
        assert!(!app.should_quit);
    }

    // ----- open-file dialog (parity with TUI) -----

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
        assert_eq!(app.active_buffer().to_bytes(), b"hello".to_vec());
    }

    #[test]
    fn open_file_dialog_submit_loads_existing_file() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("the_editor_gui_open_existing_{}.txt", std::process::id()));
        std::fs::write(&path, b"from disk").unwrap();
        let mut app = app_with("buffer");
        app.handle_event(EditorEvent::OpenFile(None));
        for c in path.to_string_lossy().chars() {
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
        let dir = std::env::temp_dir();
        let path = dir.join(format!("the_editor_gui_open_some_{}.txt", std::process::id()));
        std::fs::write(&path, b"hi").unwrap();
        let mut app = app_with("buffer");
        app.handle_event(EditorEvent::OpenFile(Some(path.clone())));
        assert!(app.open_file_dialog.is_none());
        assert_eq!(app.active_buffer().to_bytes(), b"hi".to_vec());
        assert_eq!(app.active_doc().path(), Some(path.as_path()));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn open_file_with_nonexistent_path_creates_empty_buffer_with_path() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("the_editor_gui_open_new_{}.txt", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut app = app_with("buffer");
        app.handle_event(EditorEvent::OpenFile(Some(path.clone())));
        assert_eq!(app.active_buffer().to_bytes(), b"".to_vec());
        assert_eq!(app.active_doc().path(), Some(path.as_path()));
    }

    #[test]
    fn close_confirm_drops_open_file_dialog_when_opened() {
        let mut app = app_with("hello");
        app.handle_event(EditorEvent::Insert('!'));
        app.handle_event(EditorEvent::OpenFile(None));
        assert!(app.open_file_dialog.is_some());
        app.handle_event(EditorEvent::CloseDoc);
        assert!(app.close_confirm.is_some());
        assert!(app.open_file_dialog.is_none());
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
}