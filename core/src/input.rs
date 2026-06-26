//! Frontend-agnostic editor events.
//!
//! Both frontends (TUI via crossterm, GUI via winit/egui) translate their
//! native input events into [`EditorEvent`]. The handling logic against
//! the `Buffer` is identical for both — see ADR 0005 (full parity).
//!
//! The translation functions (`translate_key`, etc.) live with each
//! frontend because they depend on the frontend's native input types.
//! The type itself lives here so both frontends see the same shape.

use crate::BytePos;

/// An input event the editor acts on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditorEvent {
    /// Insert a character at the cursor position. If the selection is
    /// non-empty, the selection is replaced by the inserted character.
    Insert(char),
    /// Delete the character to the left of the cursor (Backspace). If
    /// the selection is non-empty, the entire selection is deleted
    /// instead — same as `DeleteSelection`.
    DeleteLeft,
    /// Delete the character at the cursor (Delete key). If the selection
    /// is non-empty, the entire selection is deleted instead.
    DeleteRight,
    /// Delete the current selection. No-op when the selection is
    /// collapsed (cursor only).
    DeleteSelection,
    /// Delete the word to the left of the cursor (Ctrl+Backspace).
    /// Uses the same word boundary definition as `Movement::WordLeft`.
    DeleteWordLeft,
    /// Delete the word to the right of the cursor (Ctrl+Delete).
    DeleteWordRight,
    /// Delete the current line (Ctrl+K or Ctrl+Shift+K). The trailing
    /// newline is removed too, so two lines collapse into one.
    DeleteLine,
    /// Duplicate the current line (Ctrl+D or Ctrl+Shift+D). The cursor
    /// moves to the start of the new copy.
    DuplicateLine,
    /// Move the current line up by one (Alt+Up). Swaps with the
    /// previous line.
    MoveLineUp,
    /// Move the current line down by one (Alt+Down).
    MoveLineDown,
    /// Move the cursor. Selection is collapsed to the new position.
    Move(Movement),
    /// Extend the selection by moving one end of it.
    SelectExtend(Movement),
    /// Set the cursor to an absolute byte position, collapsing the
    /// selection. Used by mouse clicks.
    SetCursor { pos: BytePos },
    /// Extend the selection to an absolute byte position, leaving the
    /// anchor where it was. Used by mouse drag.
    SelectExtendTo { pos: BytePos },
    /// Insert `text` at the cursor position. If the selection is
    /// non-empty, the selection is replaced by `text`. Used for paste;
    /// the frontend reads the OS clipboard and produces this event.
    Paste(String),
    /// Save the buffer.
    Save,
    /// Undo the most recent edit.
    Undo,
    /// Redo the most recently undone edit.
    Redo,
    /// Quit the editor. Frontend may prompt to save dirty buffers.
    Quit,
}

/// A direction or target for cursor movement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Movement {
    Left,
    Right,
    Up,
    Down,
    /// Move the cursor up by approximately one viewport page.
    /// Implementations compute the page size from the current viewport.
    PageUp,
    /// Move the cursor down by approximately one viewport page.
    PageDown,
    WordLeft,
    WordRight,
    LineStart,
    LineEnd,
    DocumentStart,
    DocumentEnd,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variants_exist_and_pattern_match() {
        let e = EditorEvent::Insert('x');
        assert!(matches!(e, EditorEvent::Insert('x')));
        let m = Movement::Left;
        assert!(matches!(m, Movement::Left));
    }

    #[test]
    fn movement_variants_are_distinct() {
        // Ensure no two variants accidentally collapse to the same value.
        let variants = [
            Movement::Left,
            Movement::Right,
            Movement::Up,
            Movement::Down,
            Movement::PageUp,
            Movement::PageDown,
            Movement::WordLeft,
            Movement::WordRight,
            Movement::LineStart,
            Movement::LineEnd,
            Movement::DocumentStart,
            Movement::DocumentEnd,
        ];
        for (i, a) in variants.iter().enumerate() {
            for (j, b) in variants.iter().enumerate() {
                if i != j {
                    assert_ne!(a, b, "variants {i:?} and {j:?} collide");
                }
            }
        }
    }

    #[test]
    fn set_cursor_carries_position() {
        let e = EditorEvent::SetCursor { pos: 42 };
        match e {
            EditorEvent::SetCursor { pos } => assert_eq!(pos, 42),
            _ => panic!("expected SetCursor"),
        }
    }

    #[test]
    fn select_extend_to_carries_position() {
        let e = EditorEvent::SelectExtendTo { pos: 100 };
        match e {
            EditorEvent::SelectExtendTo { pos } => assert_eq!(pos, 100),
            _ => panic!("expected SelectExtendTo"),
        }
    }
}