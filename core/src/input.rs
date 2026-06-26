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
    /// Insert a character at the cursor position.
    Insert(char),
    /// Delete the character to the left of the cursor (Backspace).
    DeleteLeft,
    /// Delete the character at the cursor (Delete key).
    DeleteRight,
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