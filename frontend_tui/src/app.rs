//! The `App` — owns the `Buffer`, view state, and event handling.
//!
//! Split into its own module so `main.rs` stays a thin entry point and
//! so we can unit-test the event-handling logic without spinning up a
//! real terminal.

use std::time::Duration;

use core::{Buffer, BytePos, EditorEvent, Movement, Selection};
use crossterm::event::{self as cxevent, Event};
use ratatui::Terminal;

/// Maximum number of cursor positions to scan when computing word
/// movements. Word movement is currently a "skip until space" heuristic
/// — see `skip_word_*`. A bounded scan prevents pathological behaviour
/// on huge single-line files.
const MAX_WORD_SCAN: usize = 4096;

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
    pub fn run<B: ratatui::backend::Backend>(
        &mut self,
        terminal: &mut Terminal<B>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        loop {
            // Render.
            terminal.draw(|frame| crate::ui::render(frame, self))?;

            // Poll for events.
            if cxevent::poll(Duration::from_millis(100))? {
                match cxevent::read()? {
                    Event::Key(key) => {
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

    /// Apply an `EditorEvent` to the buffer / app state.
    pub fn handle_event(&mut self, event: EditorEvent) {
        match event {
            EditorEvent::Insert(ch) => {
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

    /// Currently unused — kept as a stub for future word-movement
    /// shortcuts (Ctrl+Left/Right). The current `Movement` enum doesn't
    /// expose them yet.
    #[allow(dead_code)]
    fn skip_word_left(&self, pos: usize) -> usize {
        let bytes = self.buffer.line_text(self.buffer.pos_to_linecol(pos).unwrap_or((0, 0)).0);
        if let Some(cow) = bytes {
            let s = cow.as_ref();
            let mut i = pos.min(self.buffer.len());
            let mut scanned = 0;
            // Skip non-word chars.
            while i > 0 && scanned < MAX_WORD_SCAN {
                let prev = prev_char_boundary(s, i);
                if prev == i {
                    break;
                }
                let ch = s[prev..i].chars().next().unwrap_or(' ');
                if is_word_char(ch) {
                    break;
                }
                i = prev;
                scanned += 1;
            }
            // Skip word chars.
            while i > 0 && scanned < MAX_WORD_SCAN {
                let prev = prev_char_boundary(s, i);
                if prev == i {
                    break;
                }
                let ch = s[prev..i].chars().next().unwrap_or(' ');
                if !is_word_char(ch) {
                    break;
                }
                i = prev;
                scanned += 1;
            }
            i
        } else {
            pos.saturating_sub(1)
        }
    }
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn prev_char_boundary(s: &str, mut i: usize) -> usize {
    i = i.min(s.len());
    while i > 0 {
        i -= 1;
        if s.is_char_boundary(i) {
            return i;
        }
    }
    0
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
}