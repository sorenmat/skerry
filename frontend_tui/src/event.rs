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
pub fn translate_key(key: KeyEvent) -> Option<EditorEvent> {
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
            _ => None,
        };
    }

    let select = shift;

    match key.code {
        KeyCode::Char(c) => Some(EditorEvent::Insert(c)),
        KeyCode::Backspace => Some(EditorEvent::DeleteLeft),
        KeyCode::Delete => Some(EditorEvent::DeleteRight),
        KeyCode::Enter => Some(EditorEvent::Insert('\n')),
        KeyCode::Tab => Some(EditorEvent::Insert('\t')),
        KeyCode::Esc => Some(EditorEvent::Quit),
        KeyCode::Left => Some(movement(Movement::Left, select)),
        KeyCode::Right => Some(movement(Movement::Right, select)),
        KeyCode::Up => Some(movement(Movement::Up, select)),
        KeyCode::Down => Some(movement(Movement::Down, select)),
        KeyCode::Home => Some(movement(Movement::LineStart, select)),
        KeyCode::End => Some(movement(Movement::LineEnd, select)),
        KeyCode::PageUp => Some(movement(Movement::DocumentStart, select)),
        KeyCode::PageDown => Some(movement(Movement::DocumentEnd, select)),
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
        assert_eq!(translate_key(key(KeyCode::Char('a'))), Some(EditorEvent::Insert('a')));
    }

    #[test]
    fn enter_inserts_newline() {
        assert_eq!(translate_key(key(KeyCode::Enter)), Some(EditorEvent::Insert('\n')));
    }

    #[test]
    fn backspace_deletes_left() {
        assert_eq!(translate_key(key(KeyCode::Backspace)), Some(EditorEvent::DeleteLeft));
    }

    #[test]
    fn delete_deletes_right() {
        assert_eq!(translate_key(key(KeyCode::Delete)), Some(EditorEvent::DeleteRight));
    }

    #[test]
    fn arrows_move() {
        assert_eq!(translate_key(key(KeyCode::Left)), Some(EditorEvent::Move(Movement::Left)));
        assert_eq!(translate_key(key(KeyCode::Right)), Some(EditorEvent::Move(Movement::Right)));
        assert_eq!(translate_key(key(KeyCode::Up)), Some(EditorEvent::Move(Movement::Up)));
        assert_eq!(translate_key(key(KeyCode::Down)), Some(EditorEvent::Move(Movement::Down)));
    }

    #[test]
    fn shift_arrows_extend_selection() {
        assert_eq!(
            translate_key(key_shift(KeyCode::Right)),
            Some(EditorEvent::SelectExtend(Movement::Right))
        );
    }

    #[test]
    fn ctrl_s_saves() {
        assert_eq!(translate_key(key_ctrl('s')), Some(EditorEvent::Save));
    }

    #[test]
    fn ctrl_q_quits() {
        assert_eq!(translate_key(key_ctrl('q')), Some(EditorEvent::Quit));
    }

    #[test]
    fn ctrl_z_undoes() {
        assert_eq!(translate_key(key_ctrl('z')), Some(EditorEvent::Undo));
    }

    #[test]
    fn ctrl_y_redoes() {
        assert_eq!(translate_key(key_ctrl('y')), Some(EditorEvent::Redo));
    }

    #[test]
    fn esc_quits() {
        assert_eq!(translate_key(key(KeyCode::Esc)), Some(EditorEvent::Quit));
    }

    #[test]
    fn unmapped_key_returns_none() {
        assert_eq!(translate_key(key(KeyCode::F(1))), None);
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
        app.viewport_top_line = 0;
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
        app.viewport_top_line = 0;
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
}