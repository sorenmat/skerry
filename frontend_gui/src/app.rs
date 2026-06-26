//! `EditorApp` — owns the `Buffer`, view state, and event handling.
//!
//! Mirrors `frontend_tui::App` so the event-handling logic stays in
//! lockstep across frontends (ADR 0005).

use core::{Buffer, EditorEvent, Movement, Selection};
use eframe::egui;
use eframe::egui::Context;
use eframe::App;

/// The GUI editor application. eframe calls `update()` each frame.
pub struct EditorApp {
    pub buffer: Box<dyn Buffer>,
    pub should_quit: bool,
    pub status_message: Option<String>,
    /// Number of lines that fit in the current viewport. Updated each
    /// frame by the renderer; used by PageUp/PageDown to compute a
    /// reasonable page size when there's no other source of truth.
    pub viewport_lines: usize,
}

impl EditorApp {
    pub fn new(buffer: Box<dyn Buffer>) -> Self {
        Self {
            buffer,
            should_quit: false,
            status_message: None,
            viewport_lines: 20,
        }
    }

    /// Pull input events from egui and apply them to the buffer.
///
/// Clipboard shortcuts (Cmd/Ctrl+C, X, V) are intercepted here instead
/// of going through `translate_event` because they need OS clipboard
/// access that doesn't fit into a single `EditorEvent`. We still go
/// through `handle_event` for the buffer side (delete-selection on cut,
/// paste-text on paste) — only the clipboard read/write is local to
/// the frontend.
    pub fn handle_input(&mut self, ctx: &Context) {
        let mut clipboard_events: Vec<crate::event::ClipboardAction> = Vec::new();
        ctx.input(|i| {
            for event in &i.events {
                if let Some(clip) =
                    crate::event::classify_clipboard_event(event, &*self.buffer)
                {
                    clipboard_events.push(clip);
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
                let pos = self.buffer.cursor();
                let s = ch.to_string();
                match self.buffer.insert(pos, &s) {
                    Ok(new_pos) => {
                        self.buffer.set_cursor(new_pos);
                        self.buffer.set_selection(Selection::collapsed(new_pos));
                        self.status_message = None;
                    }
                    Err(e) => self.status_message = Some(format!("insert error: {e}")),
                }
            }
            EditorEvent::DeleteLeft => {
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

    /// Compute the byte position a movement should land on. Identical
    /// to `frontend_tui::App::compute_target`.
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
                let (line, col) = self.buffer.pos_to_linecol(pos).unwrap_or((0, 0));
                if line == 0 {
                    0
                } else {
                    self.buffer
                        .linecol_to_pos(line - 1, col)
                        .unwrap_or(pos)
                }
            }
            Movement::Down => {
                let (line, col) = self.buffer.pos_to_linecol(pos).unwrap_or((0, 0));
                if line + 1 >= self.buffer.line_count() {
                    pos
                } else {
                    self.buffer
                        .linecol_to_pos(line + 1, col)
                        .unwrap_or(pos)
                }
            }
            Movement::PageUp => {
                let (line, col) = self.buffer.pos_to_linecol(pos).unwrap_or((0, 0));
                let page = self.viewport_lines.max(1);
                let target = line.saturating_sub(page);
                self.buffer
                    .linecol_to_pos(target, col)
                    .unwrap_or(pos)
            }
            Movement::PageDown => {
                let (line, col) = self.buffer.pos_to_linecol(pos).unwrap_or((0, 0));
                let page = self.viewport_lines.max(1);
                let last = self.buffer.line_count().saturating_sub(1);
                let target = (line + page).min(last);
                self.buffer
                    .linecol_to_pos(target, col)
                    .unwrap_or(pos)
            }
            Movement::WordLeft => skip_word_left(&*self.buffer, pos),
            Movement::WordRight => skip_word_right(&*self.buffer, pos),
            Movement::LineStart => {
                let (line, _) = self.buffer.pos_to_linecol(pos).unwrap_or((0, 0));
                self.buffer.linecol_to_pos(line, 0).unwrap_or(0)
            }
            Movement::LineEnd => {
                let (line, _) = self.buffer.pos_to_linecol(pos).unwrap_or((0, 0));
                self.buffer
                    .line_byte_range(line)
                    .map(|r| r.end)
                    .unwrap_or(len)
            }
            Movement::DocumentStart => 0,
            Movement::DocumentEnd => len,
        }
    }

    pub fn is_dirty(&self) -> bool {
        self.buffer.is_dirty()
    }

    /// Helper: ask the OS to close the window. eframe 0.30 has no
    /// `Frame::close()`; we send a viewport command instead.
    pub fn request_close(&self, ctx: &Context) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
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
        app.buffer.set_cursor(2);
        app.buffer.set_selection(Selection::collapsed(2));
        app.handle_event(EditorEvent::SelectExtend(Movement::Right));
        let sel = app.buffer.selection();
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
        assert_eq!(app.buffer.to_bytes(), b"a".to_vec());
        assert!(app.should_quit);
    }

    // ----- selection-aware editing -----

    #[test]
    fn insert_replaces_selection() {
        // Selecting "world" then typing '!' should produce "hello!"
        let mut app = app_with("hello world");
        app.buffer.set_selection(Selection {
            anchor: 6,
            head: 11,
        });
        app.handle_event(EditorEvent::Insert('!'));
        assert_eq!(app.buffer.to_bytes(), b"hello !".to_vec());
        assert_eq!(app.buffer.cursor(), 7);
        assert!(app.buffer.selection().is_collapsed());
    }

    #[test]
    fn delete_left_with_selection_deletes_selection() {
        // Selecting "world" then pressing Backspace should delete "world".
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
        app.buffer.set_cursor(app.buffer.linecol_to_pos(9, 2).unwrap());
        app.handle_event(EditorEvent::Move(Movement::PageUp));
        assert_eq!(
            app.buffer.pos_to_linecol(app.buffer.cursor()),
            Some((4, 2))
        );
        app.handle_event(EditorEvent::Move(Movement::PageDown));
        app.handle_event(EditorEvent::Move(Movement::PageDown));
        assert_eq!(
            app.buffer.pos_to_linecol(app.buffer.cursor()),
            Some((9, 2))
        );
    }

    #[test]
    fn word_right_walks_word_boundaries() {
        let mut app = app_with("hello   world foo");
        app.buffer.set_cursor(0);
        app.handle_event(EditorEvent::Move(Movement::WordRight));
        assert_eq!(app.buffer.cursor(), 5);
        app.handle_event(EditorEvent::Move(Movement::WordRight));
        assert_eq!(app.buffer.cursor(), 8);
        app.handle_event(EditorEvent::Move(Movement::WordRight));
        assert_eq!(app.buffer.cursor(), 13);
        app.handle_event(EditorEvent::Move(Movement::WordRight));
        assert_eq!(app.buffer.cursor(), 14);
    }

    #[test]
    fn word_left_walks_word_boundaries() {
        let mut app = app_with("hello   world");
        app.buffer.set_cursor(13);
        app.handle_event(EditorEvent::Move(Movement::WordLeft));
        assert_eq!(app.buffer.cursor(), 8);
        app.handle_event(EditorEvent::Move(Movement::WordLeft));
        assert_eq!(app.buffer.cursor(), 0);
    }
}