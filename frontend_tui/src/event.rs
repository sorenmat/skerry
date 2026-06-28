//! Crossterm → `EditorEvent` translation.
//!
//! The `EditorEvent` and `Movement` types live in `core::input` (see
//! ADR 0005). This module only contains the crossterm-specific glue.
//!
//! Clipboard-aware shortcuts (Ctrl+C/X/V) are NOT translated here — the
//! OS clipboard is a frontend concern, so they are handled directly in
//! [`crate::app::App::run`]. This module only emits plain
//! `EditorEvent`s.

use core::{Buffer, EditorEvent, Movement, Selection};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

use crate::app::App;

/// Translate a crossterm `KeyEvent` into an `EditorEvent`.
///
/// Returns `None` for keys we don't bind to anything (e.g. function keys,
/// modifier-only presses). The caller decides whether `None` is "ignore"
/// or "unhandled".
///
/// When the find bar is open, Enter / Esc / printable chars go to the
/// bar instead of the buffer. The caller passes the current App so
/// we can append to the live query.
pub fn translate_key(key: KeyEvent, app: Option<&App>) -> Option<EditorEvent> {
    if let Some(app) = app {
        if app.search.bar_open {
            return find_bar_translate(key, app);
        }
    }
    translate_buffer_key(key)
}

fn find_bar_translate(key: KeyEvent, app: &App) -> Option<EditorEvent> {
    // Replace bar takes priority — if it's open, all key events go to
    // the replace bar regardless of whether the find bar is also open.
    if app.search.replace_bar_open {
        return replace_bar_translate(key, app);
    }
    match key.code {
        KeyCode::Esc => Some(EditorEvent::FindClose),
        KeyCode::Enter => {
            if key.modifiers.contains(KeyModifiers::SHIFT) {
                Some(EditorEvent::FindPrev)
            } else {
                Some(EditorEvent::FindNext)
            }
        }
        KeyCode::Backspace => {
            // Pop the last char off the query.
            let mut q = app.search.query.clone();
            q.pop();
            Some(EditorEvent::FindQueryChanged(q))
        }
        KeyCode::Char(c) => {
            let mut q = app.search.query.clone();
            q.push(c);
            Some(EditorEvent::FindQueryChanged(q))
        }
        _ => None,
    }
}

/// Translate a key event while the replace bar is open.
/// Enter = ReplaceOne (replace current + advance), Shift+Enter =
/// ReplaceAll, Tab = cycle focus between find and replace bars
/// (handled implicitly because of `bar_open` state — keeping it
/// simple for v1: Tab does nothing here, the user can Esc the find
/// bar to refocus on the buffer), Esc = close replace bar (find
/// bar stays open), Backspace / printable chars edit the replacement.
fn replace_bar_translate(key: KeyEvent, app: &App) -> Option<EditorEvent> {
    match key.code {
        KeyCode::Esc => Some(EditorEvent::ReplaceClose),
        KeyCode::Enter => {
            if key.modifiers.contains(KeyModifiers::SHIFT) {
                Some(EditorEvent::ReplaceAll)
            } else {
                Some(EditorEvent::ReplaceOne)
            }
        }
        KeyCode::Backspace => {
            let mut q = app.search.replace_query.clone();
            q.pop();
            Some(EditorEvent::ReplaceQueryChanged(q))
        }
        KeyCode::Char(c) => {
            let mut q = app.search.replace_query.clone();
            q.push(c);
            Some(EditorEvent::ReplaceQueryChanged(q))
        }
        _ => None,
    }
}

fn translate_buffer_key(key: KeyEvent) -> Option<EditorEvent> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);

    // Ctrl-modified keys first. Clipboard shortcuts (C / X / V) are
    // handled separately — see [`classify_clipboard_key`].
    if ctrl && !shift {
        return match key.code {
            KeyCode::Char('s') => Some(EditorEvent::Save),
            KeyCode::Char('z') => Some(EditorEvent::Undo),
            KeyCode::Char('y') => Some(EditorEvent::Redo),
            KeyCode::Char('q') => Some(EditorEvent::Quit),
            KeyCode::Char('f') => Some(EditorEvent::FindOpen),
            KeyCode::Char('r') => Some(EditorEvent::ReplaceOpen),
            KeyCode::Char('i') => Some(EditorEvent::CycleIndentMode),
            KeyCode::Char('k') => Some(EditorEvent::DeleteLine),
            KeyCode::Char('d') => Some(EditorEvent::DuplicateLine),
            KeyCode::Char('t') => Some(EditorEvent::NewDoc),
            KeyCode::Char('w') => Some(EditorEvent::CloseDoc),
            KeyCode::Char('o') => Some(EditorEvent::OpenFile(None)),
            KeyCode::Tab => Some(EditorEvent::NextDoc),
            KeyCode::Backspace => Some(EditorEvent::DeleteWordLeft),
            KeyCode::Delete => Some(EditorEvent::DeleteWordRight),
            KeyCode::Left => Some(movement(Movement::WordLeft, false)),
            KeyCode::Right => Some(movement(Movement::WordRight, false)),
            _ => None,
        };
    }

    // Ctrl+Shift-modified keys. Tab cycles the other direction.
    if ctrl && shift {
        return match key.code {
            KeyCode::Tab => Some(EditorEvent::PrevDoc),
            _ => None,
        };
    }

    // Alt-modified keys (line move).
    if key.modifiers.contains(KeyModifiers::ALT) && !ctrl && !shift {
        return match key.code {
            KeyCode::Up => Some(EditorEvent::MoveLineUp),
            KeyCode::Down => Some(EditorEvent::MoveLineDown),
            _ => None,
        };
    }

    let select = shift;

    match key.code {
        KeyCode::Char(c) => Some(EditorEvent::Insert(c)),
        KeyCode::Backspace => Some(EditorEvent::DeleteLeft),
        KeyCode::Delete => Some(EditorEvent::DeleteRight),
        KeyCode::Enter => Some(EditorEvent::Insert('\n')),
        KeyCode::Tab => Some(EditorEvent::InsertTab),
        KeyCode::Esc => Some(EditorEvent::Quit),
        KeyCode::Left if shift => Some(EditorEvent::ScrollLeft),
        KeyCode::Right if shift => Some(EditorEvent::ScrollRight),
        KeyCode::Left => Some(movement(Movement::Left, select)),
        KeyCode::Right => Some(movement(Movement::Right, select)),
        KeyCode::Up => Some(movement(Movement::Up, select)),
        KeyCode::Down => Some(movement(Movement::Down, select)),
        KeyCode::Home => Some(movement(Movement::LineStart, select)),
        KeyCode::End => Some(movement(Movement::LineEnd, select)),
        KeyCode::PageUp => Some(movement(Movement::PageUp, select)),
        KeyCode::PageDown => Some(movement(Movement::PageDown, select)),
        _ => None,
    }
}

/// A side-effect-producing clipboard action that needs OS access and
/// therefore can't be a plain `EditorEvent`. The caller
/// ([`crate::app::App::run`]) reads the actual clipboard via arboard
/// when it sees one of these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardAction {
    /// Ctrl+C: copy the selection text to the OS clipboard.
    Copy(String),
    /// Ctrl+X: copy the selection text to the OS clipboard, then delete
    /// the selection from the buffer.
    Cut(String),
}

/// If the key event is a clipboard shortcut (Ctrl+C / Ctrl+X) and the
/// current selection is non-empty, return the corresponding
/// [`ClipboardAction`]. Otherwise `None`.
pub fn classify_clipboard_key(key: KeyEvent, buffer: &dyn Buffer) -> Option<ClipboardAction> {
    if !key.modifiers.contains(KeyModifiers::CONTROL) {
        return None;
    }
    if key.modifiers.contains(KeyModifiers::SHIFT) {
        return None;
    }
    let target = match key.code {
        KeyCode::Char('c') => 'c',
        KeyCode::Char('x') => 'x',
        _ => return None,
    };
    let sel: Selection = buffer.selection();
    if sel.is_collapsed() {
        return None;
    }
    let text = buffer.slice(sel.range())?;
    if target == 'c' {
        Some(ClipboardAction::Copy(text))
    } else {
        Some(ClipboardAction::Cut(text))
    }
}

fn movement(m: Movement, select: bool) -> EditorEvent {
    if select {
        EditorEvent::SelectExtend(m)
    } else {
        EditorEvent::Move(m)
    }
}

/// Translate a crossterm `MouseEvent` into an `EditorEvent`.
///
/// Supports:
/// - Left-button press → `SetCursor` at the click position (collapses selection)
/// - Left-button drag → `SelectExtendTo` at the drag position
/// - Left-button release → ignored (selection already finalized at last drag)
///
/// Scroll wheel and other buttons are ignored. The click position is
/// resolved against the current `App` viewport via
/// [`App::click_to_byte_pos`].
pub fn translate_mouse(mouse: MouseEvent, app: &App) -> Option<EditorEvent> {
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            let pos = app.click_to_byte_pos(mouse.column, mouse.row)?;
            Some(EditorEvent::SetCursor { pos })
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            let pos = app.click_to_byte_pos(mouse.column, mouse.row)?;
            Some(EditorEvent::SelectExtendTo { pos })
        }
        // Up and ScrollUp/ScrollDown are ignored for v1.
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn key_ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn key_shift(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::SHIFT)
    }

    #[test]
    fn printable_char_inserts() {
        assert_eq!(translate_key(key(KeyCode::Char('a')), None), Some(EditorEvent::Insert('a')));
    }

    #[test]
    fn enter_inserts_newline() {
        assert_eq!(translate_key(key(KeyCode::Enter), None), Some(EditorEvent::Insert('\n')));
    }

    #[test]
    fn backspace_deletes_left() {
        assert_eq!(translate_key(key(KeyCode::Backspace), None), Some(EditorEvent::DeleteLeft));
    }

    #[test]
    fn delete_deletes_right() {
        assert_eq!(translate_key(key(KeyCode::Delete), None), Some(EditorEvent::DeleteRight));
    }

    #[test]
    fn arrows_move() {
        assert_eq!(translate_key(key(KeyCode::Left), None), Some(EditorEvent::Move(Movement::Left)));
        assert_eq!(translate_key(key(KeyCode::Right), None), Some(EditorEvent::Move(Movement::Right)));
        assert_eq!(translate_key(key(KeyCode::Up), None), Some(EditorEvent::Move(Movement::Up)));
        assert_eq!(translate_key(key(KeyCode::Down), None), Some(EditorEvent::Move(Movement::Down)));
    }

    #[test]
    fn shift_arrows_extend_selection() {
        // Shift+Up/Down still extend selection.
        assert_eq!(
            translate_key(key_shift(KeyCode::Up), None),
            Some(EditorEvent::SelectExtend(Movement::Up))
        );
        assert_eq!(
            translate_key(key_shift(KeyCode::Down), None),
            Some(EditorEvent::SelectExtend(Movement::Down))
        );
    }

    #[test]
    fn shift_left_right_scrolls_horizontally() {
        assert_eq!(
            translate_key(key_shift(KeyCode::Left), None),
            Some(EditorEvent::ScrollLeft)
        );
        assert_eq!(
            translate_key(key_shift(KeyCode::Right), None),
            Some(EditorEvent::ScrollRight)
        );
    }

    #[test]
    fn ctrl_s_saves() {
        assert_eq!(translate_key(key_ctrl('s'), None), Some(EditorEvent::Save));
    }

    #[test]
    fn ctrl_q_quits() {
        assert_eq!(translate_key(key_ctrl('q'), None), Some(EditorEvent::Quit));
    }

    #[test]
    fn ctrl_z_undoes() {
        assert_eq!(translate_key(key_ctrl('z'), None), Some(EditorEvent::Undo));
    }

    #[test]
    fn ctrl_y_redoes() {
        assert_eq!(translate_key(key_ctrl('y'), None), Some(EditorEvent::Redo));
    }

    #[test]
    fn esc_quits() {
        assert_eq!(translate_key(key(KeyCode::Esc), None), Some(EditorEvent::Quit));
    }

    #[test]
    fn unmapped_key_returns_none() {
        assert_eq!(translate_key(key(KeyCode::F(1)), None), None);
    }

    // ----- mouse event translation -----

    use crate::app::App;
    use core::PieceTableBuffer;

    fn app_with(content: &str) -> App {
        let buf: Box<dyn core::Buffer> =
            Box::new(PieceTableBuffer::from_bytes(content.as_bytes().to_vec()));
        App::new(buf)
    }

    fn mouse_event(kind: MouseEventKind, col: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    #[test]
    fn left_click_translates_to_set_cursor() {
        let mut app = app_with("hello\nworld");
        app.active_doc_mut().view.scroll_top_line = 0;
        app.viewport_height = 10;
        // Click on row 2 ("world"), col 4 (gutter end), expect byte 6.
        let ev = translate_mouse(
            mouse_event(MouseEventKind::Down(MouseButton::Left), 4, 2),
            &app,
        );
        match ev {
            Some(EditorEvent::SetCursor { pos }) => assert_eq!(pos, 6),
            _ => panic!("expected SetCursor, got {ev:?}"),
        }
    }

    #[test]
    fn left_drag_translates_to_select_extend_to() {
        let mut app = app_with("hello\nworld");
        app.active_doc_mut().view.scroll_top_line = 0;
        app.viewport_height = 10;
        // First click sets the anchor.
        app.handle_event(EditorEvent::SetCursor {
            pos: app.click_to_byte_pos(0, 2).unwrap(),
        });
        // Drag to col 8 (gutter=5 → text col 3 = 'r' in 'world' = byte 9).
        let ev = translate_mouse(
            mouse_event(MouseEventKind::Drag(MouseButton::Left), 8, 2),
            &app,
        );
        match ev {
            Some(EditorEvent::SelectExtendTo { pos }) => assert_eq!(pos, 9),
            _ => panic!("expected SelectExtendTo, got {ev:?}"),
        }
    }

    #[test]
    fn right_click_returns_none() {
        let app = app_with("hello");
        let ev = translate_mouse(
            mouse_event(MouseEventKind::Down(MouseButton::Right), 5, 1),
            &app,
        );
        assert!(ev.is_none());
    }

    #[test]
    fn scroll_returns_none() {
        let app = app_with("hello");
        let ev = translate_mouse(
            mouse_event(MouseEventKind::ScrollUp, 5, 1),
            &app,
        );
        assert!(ev.is_none());
    }

    #[test]
    fn release_returns_none() {
        // Up is intentionally ignored — selection is finalized at last drag.
        let app = app_with("hello");
        let ev = translate_mouse(
            mouse_event(MouseEventKind::Up(MouseButton::Left), 5, 1),
            &app,
        );
        assert!(ev.is_none());
    }

    // ----- clipboard key classification -----

    use core::Selection;

    fn buf_with(text: &str) -> core::PieceTableBuffer {
        core::PieceTableBuffer::from_bytes(text.as_bytes().to_vec())
    }

    #[test]
    fn ctrl_c_with_selection_returns_copy_action() {
        let mut buf = buf_with("hello world");
        buf.set_selection(Selection {
            anchor: 0,
            head: 5,
        });
        let ev = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(
            classify_clipboard_key(ev, &buf),
            Some(ClipboardAction::Copy("hello".to_string()))
        );
    }

    #[test]
    fn ctrl_x_with_selection_returns_cut_action() {
        let mut buf = buf_with("hello world");
        buf.set_selection(Selection {
            anchor: 0,
            head: 5,
        });
        let ev = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL);
        assert_eq!(
            classify_clipboard_key(ev, &buf),
            Some(ClipboardAction::Cut("hello".to_string()))
        );
    }

    #[test]
    fn ctrl_c_without_selection_returns_none() {
        let buf = buf_with("hello");
        let ev = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(classify_clipboard_key(ev, &buf), None);
    }

    #[test]
    fn plain_c_does_not_match_clipboard_shortcut() {
        let mut buf = buf_with("hello");
        buf.set_selection(Selection {
            anchor: 0,
            head: 5,
        });
        let ev = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE);
        assert_eq!(classify_clipboard_key(ev, &buf), None);
    }

    #[test]
    fn ctrl_shift_c_is_not_clipboard_shortcut() {
        let mut buf = buf_with("hello");
        buf.set_selection(Selection {
            anchor: 0,
            head: 5,
        });
        let ev = KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        );
        assert_eq!(classify_clipboard_key(ev, &buf), None);
    }

    #[test]
    fn ctrl_v_is_handled_separately_not_by_classify() {
        // Ctrl+V is not a Copy/Cut action — it triggers a paste, which
        // happens via EditorEvent::Paste after the OS clipboard is
        // read in the event loop. classify_clipboard_key returns
        // None for it.
        let mut buf = buf_with("hello");
        buf.set_selection(Selection {
            anchor: 0,
            head: 5,
        });
        let ev = KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL);
        assert_eq!(classify_clipboard_key(ev, &buf), None);
    }

    // ----- multi-buffer key bindings -----

    #[test]
    fn ctrl_t_opens_new_doc() {
        assert_eq!(
            translate_key(key_ctrl('t'), None),
            Some(EditorEvent::NewDoc)
        );
    }

    #[test]
    fn ctrl_w_closes_active_doc() {
        assert_eq!(
            translate_key(key_ctrl('w'), None),
            Some(EditorEvent::CloseDoc)
        );
    }

    #[test]
    fn ctrl_tab_cycles_next_doc() {
        assert_eq!(
            translate_key(
                KeyEvent::new(KeyCode::Tab, KeyModifiers::CONTROL),
                None
            ),
            Some(EditorEvent::NextDoc)
        );
    }

    #[test]
    fn ctrl_shift_tab_cycles_prev_doc() {
        assert_eq!(
            translate_key(
                KeyEvent::new(KeyCode::Tab, KeyModifiers::CONTROL | KeyModifiers::SHIFT),
                None
            ),
            Some(EditorEvent::PrevDoc)
        );
    }

    #[test]
    fn plain_tab_inserts_indent_not_doc_event() {
        // Plain Tab (no modifier) emits InsertTab so the App can
        // resolve it per the active document's indent mode (spaces
        // vs tab character). Used to emit Insert('\t'); refactored
        // when indent settings landed.
        assert_eq!(
            translate_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), None),
            Some(EditorEvent::InsertTab)
        );
    }

    #[test]
    fn ctrl_i_translates_to_cycle_indent_mode() {
        assert_eq!(
            translate_key(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::CONTROL), None),
            Some(EditorEvent::CycleIndentMode)
        );
    }
}