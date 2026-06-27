//! The `App` — owns the `Buffer`, view state, and event handling.
//!
//! Split into its own module so `main.rs` stays a thin entry point and
//! so we can unit-test the event-handling logic without spinning up a
//! real terminal.

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
                self.delete_selection_if_any();
                let pos = self.active_buffer().cursor();
                let s = ch.to_string();
                match self.active_buffer_mut().insert(pos, &s) {
                    Ok(new_pos) => {
                        // Buffer advances its own cursor; ensure consistency.
                        self.active_buffer_mut().set_cursor(new_pos);
                        self.active_buffer_mut().set_selection(Selection::collapsed(new_pos));
                        self.status_message = None;
                    }
                    Err(e) => self.status_message = Some(format!("insert error: {e}")),
                }
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
                self.close_active_doc();
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
                    self.active_buffer().linecol_to_pos(line - 1, col)
                        .unwrap_or(pos)
                }
            }
            Movement::Down => {
                let (line, col) = self.active_buffer().pos_to_linecol(pos)
                    .unwrap_or((0, 0));
                if line + 1 >= self.active_buffer().line_count() {
                    pos
                } else {
                    self.active_buffer().linecol_to_pos(line + 1, col)
                        .unwrap_or(pos)
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
                    self.active_buffer().linecol_to_pos(target, col)
                        .unwrap_or(pos)
                }
            }
            Movement::PageDown => {
                let (line, col) = self.active_buffer().pos_to_linecol(pos)
                    .unwrap_or((0, 0));
                let page = self.viewport_lines();
                let last = self.active_buffer().line_count().saturating_sub(1);
                let target = (line + page).min(last);
                self.active_buffer().linecol_to_pos(target, col)
                    .unwrap_or(pos)
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

    /// Adjust the active document's `scroll_top_line` so the cursor is
    /// visible. Called by the renderer after it determines the viewport
    /// height. Each document owns its own scroll offset, so switching
    /// tabs preserves where you were scrolled to in each one — the
    /// cursor-following clamp only fires for the doc you're currently
    /// looking at.
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
        let top = self.active_doc().view.scroll_top_line;
        let new_top = if cursor_line < top {
            cursor_line
        } else if cursor_line >= top + vh {
            cursor_line + 1 - vh
        } else {
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

    /// Close the active document. If it was the only document, the
    /// editor quits (`should_quit = true`). Otherwise the active
    /// index moves to a neighbour — the document at the same index
    /// after removal, or the new last if we closed the tail.
    ///
    /// v1: closes unconditionally — does NOT prompt on dirty buffers.
    /// A future stage will add a "save before close?" prompt.
    pub fn close_active_doc(&mut self) {
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
}