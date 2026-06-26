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
    /// Scroll the viewport one column to the left (Shift+Left).
    ScrollLeft,
    /// Scroll the viewport one column to the right (Shift+Right).
    ScrollRight,
    /// Open the find bar. No-op if it's already open.
    FindOpen,
    /// Close the find bar.
    FindClose,
    /// Replace the current find query and re-run the search. The
    /// match list is rebuilt incrementally as the user types.
    FindQueryChanged(String),
    /// Move to the next match AFTER the cursor. Wraps around at the
    /// end of the buffer. No-op if no matches.
    FindNext,
    /// Move to the previous match BEFORE the cursor. Wraps around.
    FindPrev,
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
    /// Open a new, empty, unsaved document and switch to it.
    /// Default binding: Cmd/Ctrl+T.
    NewDoc,
    /// Close the active document. If it was the only document, the
    /// editor quits. The active index moves to the neighbour on close;
    /// if the closed document was last, the new last becomes active.
    /// Default binding: Cmd/Ctrl+W.
    ///
    /// v1: closes unconditionally — does NOT prompt on dirty buffers.
    /// A future stage will add a "save before close?" prompt.
    CloseDoc,
    /// Switch to the next document in the session, wrapping at the end.
    /// Default binding: Cmd/Ctrl+Tab.
    NextDoc,
    /// Switch to the previous document in the session, wrapping at
    /// the start. Default binding: Cmd/Ctrl+Shift+Tab.
    PrevDoc,
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

    #[test]
    fn doc_event_variants_exist() {
        // If you add another document-lifecycle event, mirror it here
        // so the type-level exhaustiveness check catches handlers that
        // forget to wire it up.
        let _ = EditorEvent::NewDoc;
        let _ = EditorEvent::CloseDoc;
        let _ = EditorEvent::NextDoc;
        let _ = EditorEvent::PrevDoc;
    }
}