//! The `App` — owns the `Buffer`, view state, and event handling.
//!
//! Split into its own module so `main.rs` stays a thin entry point and
//! so we can unit-test the event-handling logic without spinning up a
//! real terminal.

use std::time::Duration;

use core::{Buffer, BytePos, EditorEvent, Movement, Selection};
use crossterm::event::{self as cxevent, Event};
use ratatui::Terminal;

/// The application. One `App` per running frontend instance.
pub struct App {
    pub buffer: Box<dyn Buffer>,
    pub should_quit: bool,
    pub status_message: Option<String>,
    /// First visible line in the viewport. Adjusted by `adjust_viewport`.
    pub viewport_top_line: usize,
    /// Last rendered viewport height in lines (set during `render`).
    pub viewport_height: u16,
}

impl App {
    /// Create an `App` around an already-loaded `Buffer`.
    pub fn new(buffer: Box<dyn Buffer>) -> Self {
        Self {
            buffer,
            should_quit: false,
            status_message: None,
            viewport_top_line: 0,
            viewport_height: 0,
        }
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
                            crate::event::classify_clipboard_key(key, &*self.buffer)
                        {
                            self.apply_clipboard_action(&mut clipboard, action);
                            continue;
                        }
                        if let Some(editor_event) = crate::event::translate_key(key) {
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
                let pos = self.buffer.cursor();
                let s = ch.to_string();
                match self.buffer.insert(pos, &s) {
                    Ok(new_pos) => {
                        // Buffer advances its own cursor; ensure consistency.
                        self.buffer.set_cursor(new_pos);
                        self.buffer.set_selection(Selection::collapsed(new_pos));
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
                let pos = self.buffer.cursor();
                if pos > 0 {
                    match self.buffer.delete((pos - 1)..pos) {
                        Ok(new_pos) => {
                            self.buffer.set_cursor(new_pos);
                            self.buffer.set_selection(Selection::collapsed(new_pos));
                        }
                        Err(e) => self.status_message = Some(format!("delete error: {e}")),
                    }
                }
            }
            EditorEvent::DeleteRight => {
                if self.delete_selection_if_any() {
                    return;
                }
                let pos = self.buffer.cursor();
                if pos < self.buffer.len() {
                    match self.buffer.delete(pos..(pos + 1)) {
                        Ok(new_pos) => {
                            self.buffer.set_cursor(new_pos);
                            self.buffer.set_selection(Selection::collapsed(new_pos));
                        }
                        Err(e) => self.status_message = Some(format!("delete error: {e}")),
                    }
                }
            }
            EditorEvent::DeleteSelection => {
                self.delete_selection_if_any();
            }
            EditorEvent::Move(movement) => {
                let new_pos = self.compute_target(movement);
                self.buffer.set_cursor(new_pos);
                self.buffer.set_selection(Selection::collapsed(new_pos));
            }
            EditorEvent::SelectExtend(movement) => {
                let new_pos = self.compute_target(movement);
                let anchor = self.buffer.selection().anchor;
                self.buffer.set_cursor(new_pos);
                self.buffer.set_selection(Selection {
                    anchor,
                    head: new_pos,
                });
            }
            EditorEvent::SetCursor { pos } => {
                let clamped = pos.min(self.buffer.len());
                self.buffer.set_cursor(clamped);
                self.buffer.set_selection(Selection::collapsed(clamped));
            }
            EditorEvent::SelectExtendTo { pos } => {
                let clamped = pos.min(self.buffer.len());
                let anchor = self.buffer.selection().anchor;
                self.buffer.set_cursor(clamped);
                self.buffer.set_selection(Selection {
                    anchor,
                    head: clamped,
                });
            }
            EditorEvent::Paste(text) => {
                // Selection-aware paste: replace the selection if any,
                // otherwise insert at cursor.
                self.delete_selection_if_any();
                let pos = self.buffer.cursor();
                match self.buffer.insert(pos, &text) {
                    Ok(new_pos) => {
                        self.buffer.set_cursor(new_pos);
                        self.buffer.set_selection(Selection::collapsed(new_pos));
                        self.status_message = None;
                    }
                    Err(e) => self.status_message = Some(format!("paste error: {e}")),
                }
            }
            EditorEvent::Save => match self.buffer.save() {
                Ok(()) => self.status_message = Some("Saved.".to_string()),
                Err(e) => self.status_message = Some(format!("Save error: {e}")),
            },
            EditorEvent::Undo => {
                if self.buffer.undo() {
                    self.status_message = Some("Undid.".to_string());
                }
            }
            EditorEvent::Redo => {
                if self.buffer.redo() {
                    self.status_message = Some("Redid.".to_string());
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
        let sel = self.buffer.selection();
        if sel.is_collapsed() {
            return false;
        }
        let range = sel.range();
        match self.buffer.delete(range) {
            Ok(new_pos) => {
                self.buffer.set_cursor(new_pos);
                self.buffer.set_selection(Selection::collapsed(new_pos));
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
        let pos = self.buffer.cursor();
        let len = self.buffer.len();
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
                let (line, col) = self
                    .buffer
                    .pos_to_linecol(pos)
                    .unwrap_or((0, 0));
                if line == 0 {
                    0
                } else {
                    self.buffer
                        .linecol_to_pos(line - 1, col)
                        .unwrap_or(pos)
                }
            }
            Movement::Down => {
                let (line, col) = self
                    .buffer
                    .pos_to_linecol(pos)
                    .unwrap_or((0, 0));
                if line + 1 >= self.buffer.line_count() {
                    pos
                } else {
                    self.buffer
                        .linecol_to_pos(line + 1, col)
                        .unwrap_or(pos)
                }
            }
            Movement::PageUp => {
                let (line, col) = self
                    .buffer
                    .pos_to_linecol(pos)
                    .unwrap_or((0, 0));
                let page = self.viewport_lines();
                if line == 0 {
                    0
                } else {
                    let target = line.saturating_sub(page);
                    self.buffer
                        .linecol_to_pos(target, col)
                        .unwrap_or(pos)
                }
            }
            Movement::PageDown => {
                let (line, col) = self
                    .buffer
                    .pos_to_linecol(pos)
                    .unwrap_or((0, 0));
                let page = self.viewport_lines();
                let last = self.buffer.line_count().saturating_sub(1);
                let target = (line + page).min(last);
                self.buffer
                    .linecol_to_pos(target, col)
                    .unwrap_or(pos)
            }
            Movement::WordLeft => self.skip_word_left_from(pos),
            Movement::WordRight => self.skip_word_right_from(pos),
            Movement::LineStart => {
                let (line, _) = self.buffer.pos_to_linecol(pos).unwrap_or((0, 0));
                self.buffer.linecol_to_pos(line, 0).unwrap_or(0)
            }
            Movement::LineEnd => {
                let (line, _) = self.buffer.pos_to_linecol(pos).unwrap_or((0, 0));
                // line_byte_range gives the [start, end) byte range for the
                // line; its end is the cursor's LineEnd target.
                self.buffer
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
        let (line, _) = self.buffer.pos_to_linecol(pos).unwrap_or((0, 0));
        let line_text = match self.buffer.line_text(line) {
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
        let len = self.buffer.len();
        if pos >= len {
            return len;
        }
        let (line, _) = self.buffer.pos_to_linecol(pos).unwrap_or((0, 0));
        let line_text = match self.buffer.line_text(line) {
            Some(cow) => cow.into_owned(),
            None => return (pos + 1).min(len),
        };
        let line_byte_start = self
            .buffer
            .line_byte_range(line)
            .map(|r| r.start)
            .unwrap_or(0);
        let line_byte_end = self
            .buffer
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
        if new_byte >= line_byte_end && line + 1 < self.buffer.line_count() {
            return self
                .buffer
                .line_byte_range(line + 1)
                .map(|r| r.start)
                .unwrap_or(len);
        }
        new_byte.min(len)
    }

    /// Adjust `viewport_top_line` so the cursor is visible. Called by
    /// the renderer after it determines the viewport height.
    pub fn adjust_viewport(&mut self, viewport_height: u16) {
        self.viewport_height = viewport_height;
        let cursor_pos = self.buffer.cursor();
        let cursor_line = self
            .buffer
            .pos_to_linecol(cursor_pos)
            .map(|(l, _)| l)
            .unwrap_or(0);
        let vh = viewport_height as usize;
        if vh == 0 {
            return;
        }
        if cursor_line < self.viewport_top_line {
            self.viewport_top_line = cursor_line;
        } else if cursor_line >= self.viewport_top_line + vh {
            self.viewport_top_line = cursor_line + 1 - vh;
        }
    }

    /// Whether the buffer has unsaved edits. Convenience wrapper around
    /// the trait method so render code doesn't need to import the trait.
    pub fn is_dirty(&self) -> bool {
        self.buffer.is_dirty()
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
        let doc_line = self.viewport_top_line + content_row;
        let total_lines = self.buffer.line_count();
        if doc_line >= total_lines {
            // Click past last line — clamp to end of last line.
            return Some(self.buffer.len());
        }

        let gutter_width = total_lines.to_string().len().max(2);
        let prefix_text = format!("{:>width$} │ ", 1, width = gutter_width);
        let prefix_chars = prefix_text.chars().count() as u16;

        let line_byte_start = self.buffer.line_byte_range(doc_line)?.start;

        if col < prefix_chars {
            // Click in the gutter — position at the start of the line.
            return Some(line_byte_start);
        }

        let line_text = self.buffer.line_text(doc_line)?.into_owned();
        let text_col = (col - prefix_chars) as usize;
        let char_count = line_text.chars().count();
        let char_col = text_col.min(char_count);
        let byte_col = core::char_col_to_byte_col(&line_text, char_col);

        Some(line_byte_start + byte_col)
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
        assert_eq!(app.buffer.to_bytes(), b"hi".to_vec());
        assert_eq!(app.buffer.cursor(), 2);
        assert!(app.is_dirty());
    }

    #[test]
    fn backspace_at_start_is_noop() {
        let mut app = app_with("abc");
        app.buffer.set_cursor(0);
        app.handle_event(EditorEvent::DeleteLeft);
        assert_eq!(app.buffer.to_bytes(), b"abc".to_vec());
        assert_eq!(app.buffer.cursor(), 0);
    }

    #[test]
    fn delete_right_at_end_is_noop() {
        let mut app = app_with("abc");
        app.buffer.set_cursor(3);
        app.handle_event(EditorEvent::DeleteRight);
        assert_eq!(app.buffer.to_bytes(), b"abc".to_vec());
    }

    #[test]
    fn arrow_left_right_moves_cursor() {
        let mut app = app_with("abc");
        app.buffer.set_cursor(1);
        app.handle_event(EditorEvent::Move(Movement::Left));
        assert_eq!(app.buffer.cursor(), 0);
        app.handle_event(EditorEvent::Move(Movement::Right));
        assert_eq!(app.buffer.cursor(), 1);
    }

    #[test]
    fn home_and_end_via_movement() {
        let mut app = app_with("hello\nworld");
        app.buffer.set_cursor(8);
        app.handle_event(EditorEvent::Move(Movement::LineStart));
        assert_eq!(app.buffer.cursor(), 6);
        app.handle_event(EditorEvent::Move(Movement::LineEnd));
        assert_eq!(app.buffer.cursor(), 11);
    }

    #[test]
    fn document_start_and_end() {
        let mut app = app_with("hello\nworld");
        app.buffer.set_cursor(5);
        app.handle_event(EditorEvent::Move(Movement::DocumentStart));
        assert_eq!(app.buffer.cursor(), 0);
        app.handle_event(EditorEvent::Move(Movement::DocumentEnd));
        assert_eq!(app.buffer.cursor(), 11);
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
        app.buffer.set_cursor(2);
        app.buffer.set_selection(Selection::collapsed(2));
        app.handle_event(EditorEvent::SelectExtend(Movement::Right));
        let sel = app.buffer.selection();
        assert_eq!(sel.anchor, 2);
        assert_eq!(sel.head, 3);
    }

    #[test]
    fn set_cursor_event_collapses_selection() {
        let mut app = app_with("hello world");
        app.buffer.set_selection(Selection {
            anchor: 0,
            head: 5,
        });
        app.handle_event(EditorEvent::SetCursor { pos: 8 });
        assert_eq!(app.buffer.cursor(), 8);
        assert!(app.buffer.selection().is_collapsed());
    }

    #[test]
    fn select_extend_to_preserves_anchor() {
        let mut app = app_with("hello world");
        // First click sets anchor=5.
        app.handle_event(EditorEvent::SetCursor { pos: 5 });
        // Then drag to position 9.
        app.handle_event(EditorEvent::SelectExtendTo { pos: 9 });
        let sel = app.buffer.selection();
        assert_eq!(sel.anchor, 5);
        assert_eq!(sel.head, 9);
    }

    // ----- click_to_byte_pos: terminal coords → byte offset -----

    #[test]
    fn click_in_gutter_snaps_to_line_start() {
        let mut app = app_with("hello\nworld");
        app.viewport_top_line = 0;
        app.viewport_height = 10;
        // Click on row 2 (the "world" line), col 0 (gutter).
        // Row 0 = header, rows 1..N = content.
        let pos = app.click_to_byte_pos(0, 2).unwrap();
        assert_eq!(pos, 6, "should snap to start of line 1 ('world')");
    }

    #[test]
    fn click_in_text_positions_at_char() {
        let mut app = app_with("hello\nworld");
        app.viewport_top_line = 0;
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
        app.viewport_top_line = 0;
        app.viewport_height = 5;
        // Row 6 = status (rows 0=header, 1..5=content, 6=status)
        let pos = app.click_to_byte_pos(0, 6);
        assert!(pos.is_none(), "status bar is not part of content");
    }

    #[test]
    fn click_past_last_line_clamps_to_end() {
        let mut app = app_with("hello");
        app.viewport_top_line = 0;
        app.viewport_height = 10;
        // Row 5 = past the end of the buffer (only 1 line at row 1).
        let pos = app.click_to_byte_pos(0, 5).unwrap();
        assert_eq!(pos, 5, "clamped to buffer length");
    }

    #[test]
    fn click_with_viewport_scrolled() {
        let mut app = app_with("aaa\nbbb\nccc\nddd");
        app.viewport_top_line = 2; // viewport shows lines 2,3 (ccc, ddd)
        app.viewport_height = 5;
        // Row 1 (first content row) = viewport_top_line + 0 = line 2 ("ccc").
        // Click col 0 (gutter).
        let pos = app.click_to_byte_pos(0, 1).unwrap();
        assert_eq!(pos, 8, "should snap to start of line 2 ('ccc')");
    }

    // ----- selection-aware editing -----

    #[test]
    fn insert_replaces_selection() {
        let mut app = app_with("hello world");
        app.buffer.set_selection(Selection {
            anchor: 6,
            head: 11,
        });
        app.handle_event(EditorEvent::Insert('!'));
        assert_eq!(app.buffer.to_bytes(), b"hello !".to_vec());
        assert_eq!(app.buffer.cursor(), 7);
    }

    #[test]
    fn delete_left_with_selection_deletes_selection() {
        let mut app = app_with("hello world");
        app.buffer.set_selection(Selection {
            anchor: 6,
            head: 11,
        });
        app.handle_event(EditorEvent::DeleteLeft);
        assert_eq!(app.buffer.to_bytes(), b"hello ".to_vec());
        assert_eq!(app.buffer.cursor(), 6);
    }

    #[test]
    fn delete_right_with_selection_deletes_selection() {
        let mut app = app_with("hello world");
        app.buffer.set_selection(Selection {
            anchor: 6,
            head: 11,
        });
        app.handle_event(EditorEvent::DeleteRight);
        assert_eq!(app.buffer.to_bytes(), b"hello ".to_vec());
        assert_eq!(app.buffer.cursor(), 6);
    }

    #[test]
    fn delete_selection_noop_when_collapsed() {
        let mut app = app_with("hello");
        app.buffer.set_cursor(2);
        app.buffer.set_selection(Selection::collapsed(2));
        app.handle_event(EditorEvent::DeleteSelection);
        assert_eq!(app.buffer.to_bytes(), b"hello".to_vec());
        assert_eq!(app.buffer.cursor(), 2);
    }

    #[test]
    fn paste_replaces_selection() {
        let mut app = app_with("hello world");
        app.buffer.set_selection(Selection {
            anchor: 6,
            head: 11,
        });
        app.handle_event(EditorEvent::Paste("Rust".to_string()));
        assert_eq!(app.buffer.to_bytes(), b"hello Rust".to_vec());
        assert_eq!(app.buffer.cursor(), 10);
    }

    #[test]
    fn paste_with_no_selection_inserts_at_cursor() {
        let mut app = app_with("hello world");
        app.buffer.set_cursor(6);
        app.buffer.set_selection(Selection::collapsed(6));
        app.handle_event(EditorEvent::Paste("beautiful ".to_string()));
        assert_eq!(app.buffer.to_bytes(), b"hello beautiful world".to_vec());
    }

    // ----- PageUp / PageDown -----

    #[test]
    fn page_up_moves_cursor_up_one_viewport() {
        let mut app = make_multi_line_app();
        app.viewport_height = 5;
        // Start at line 9 col 2, page up by 5 → line 4 col 2.
        app.buffer.set_cursor(app.buffer.linecol_to_pos(9, 2).unwrap());
        app.handle_event(EditorEvent::Move(Movement::PageUp));
        let (line, col) = app.buffer.pos_to_linecol(app.buffer.cursor()).unwrap();
        assert_eq!((line, col), (4, 2));
    }

    #[test]
    fn page_up_clamps_to_top() {
        let mut app = make_multi_line_app();
        app.viewport_height = 10;
        app.buffer.set_cursor(app.buffer.linecol_to_pos(2, 0).unwrap());
        app.handle_event(EditorEvent::Move(Movement::PageUp));
        assert_eq!(app.buffer.cursor(), 0);
    }

    #[test]
    fn page_down_moves_cursor_down_one_viewport() {
        let mut app = make_multi_line_app();
        app.viewport_height = 3;
        app.buffer.set_cursor(app.buffer.linecol_to_pos(0, 0).unwrap());
        app.handle_event(EditorEvent::Move(Movement::PageDown));
        let (line, _) = app.buffer.pos_to_linecol(app.buffer.cursor()).unwrap();
        assert_eq!(line, 3);
    }

    #[test]
    fn page_down_clamps_to_last_line() {
        let mut app = make_multi_line_app();
        app.viewport_height = 100;
        app.buffer.set_cursor(app.buffer.linecol_to_pos(0, 0).unwrap());
        app.handle_event(EditorEvent::Move(Movement::PageDown));
        let (line, _) = app.buffer.pos_to_linecol(app.buffer.cursor()).unwrap();
        assert_eq!(line, 9); // last line of 10-line buffer
    }

    // ----- Word movement -----

    #[test]
    fn word_right_alternates_end_of_word_and_start_of_next() {
        // Standard Ctrl+Right: in-word → end of word; in-whitespace →
        // start of next word. So 0 → 5 → 8 → 13 → 14 → 17.
        let mut app = app_with("hello   world foo");
        app.buffer.set_cursor(0);
        app.handle_event(EditorEvent::Move(Movement::WordRight));
        assert_eq!(app.buffer.cursor(), 5, "end of 'hello'");
        app.handle_event(EditorEvent::Move(Movement::WordRight));
        assert_eq!(app.buffer.cursor(), 8, "start of 'world'");
        app.handle_event(EditorEvent::Move(Movement::WordRight));
        assert_eq!(app.buffer.cursor(), 13, "end of 'world'");
        app.handle_event(EditorEvent::Move(Movement::WordRight));
        assert_eq!(app.buffer.cursor(), 14, "start of 'foo'");
        app.handle_event(EditorEvent::Move(Movement::WordRight));
        assert_eq!(app.buffer.cursor(), 17, "end of 'foo'");
    }

    #[test]
    fn word_left_skips_word_back() {
        let mut app = app_with("hello world");
        app.buffer.set_cursor(11);
        app.handle_event(EditorEvent::Move(Movement::WordLeft));
        assert_eq!(app.buffer.cursor(), 6, "start of 'world'");
        app.handle_event(EditorEvent::Move(Movement::WordLeft));
        assert_eq!(app.buffer.cursor(), 0, "start of 'hello'");
    }

    #[test]
    fn word_right_from_mid_word_jumps_to_end() {
        let mut app = app_with("hello world");
        app.buffer.set_cursor(2); // inside "hello"
        app.handle_event(EditorEvent::Move(Movement::WordRight));
        assert_eq!(app.buffer.cursor(), 5, "end of 'hello'");
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
}