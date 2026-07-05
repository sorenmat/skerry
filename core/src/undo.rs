//! Linear undo with text + cursor tracking.
//!
//! See ADR 0004. Each undo entry stores the **forward** action (what the
//! edit did) plus the cursor position before and after. To undo, we
//! apply the **inverse** of the forward action:
//!
//! - `InsertText` → inverse is `DeleteRange`
//! - `DeleteRange` → inverse is `InsertText` (with the saved bytes)
//!
//! Selection state is not tracked in v0.1 — the frontend owns selection;
//! undo only restores the cursor.
//!
//! Edit groups (begin/end_edit_group) cause consecutive inserts at
//! adjacent positions to merge into a single undo entry. This is how
//! "paste a 100-char string" becomes a single undo instead of 100.

use crate::BytePos;

/// One operation that was performed on the buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UndoAction {
    /// An insert at `pos` of `text`. Inverse: delete `[pos, pos + text.len())`.
    InsertText { pos: BytePos, text: Vec<u8> },
    /// A delete at `pos` of `deleted` bytes. Inverse: insert `deleted` at `pos`.
    DeleteRange { pos: BytePos, deleted: Vec<u8> },
}

/// One undoable edit group. Stores the cursor positions before and after
/// the edit, plus the action that was performed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UndoEntry {
    pub cursor_before: BytePos,
    pub cursor_after: BytePos,
    pub action: UndoAction,
}

/// Linear undo state. Owned by `PieceTableBuffer`.
#[derive(Debug, Default, Clone)]
pub(crate) struct UndoState {
    pub undo_stack: Vec<UndoEntry>,
    pub redo_stack: Vec<UndoEntry>,
    /// When true, the next `record()` call merges with the previous entry
    /// if both are sequential inserts at adjacent positions. Set by
    /// `begin_edit_group`; cleared by `end_edit_group` or by an
    /// unmergeable `record`.
    pub merge_next: bool,
}

impl UndoState {
    /// Record a new edit. If `merge_next` is set, try to merge with the
    /// previous entry (only sequential inserts at adjacent positions
    /// merge today). New edits clear the redo stack.
    pub fn record(&mut self, entry: UndoEntry) {
        if self.merge_next {
            if let Some(last) = self.undo_stack.last() {
                if let Some(merged) = try_merge(last, &entry) {
                    self.undo_stack.pop();
                    self.undo_stack.push(merged);
                    // Keep `merge_next` true so subsequent edits in the
                    // group also merge. Cleared only by `end_group`.
                    return;
                }
                // Couldn't merge with the previous entry (e.g. the
                // previous was a Delete, or positions aren't adjacent).
                // Fall through and push as a separate entry. Keep
                // `merge_next` true so future inserts that ARE adjacent
                // to *this* entry can still merge.
            }
            // Empty undo stack or no merge attempted: just push the
            // entry. `merge_next` stays true.
            self.undo_stack.push(entry);
            self.redo_stack.clear();
            return;
        }
        self.undo_stack.push(entry);
        self.redo_stack.clear();
    }

    /// Pop the most recent entry and push it onto the redo stack.
    /// Returns the entry's inverse-action info (used by the caller to
    /// apply the inverse).
    pub fn pop_for_undo(&mut self) -> Option<UndoEntry> {
        let entry = self.undo_stack.pop()?;
        self.redo_stack.push(entry.clone());
        Some(entry)
    }

    /// Move the most recently undone entry back from the redo stack to
    /// the undo stack. Used when applying the inverse action fails so
    /// the undo entry is not lost.
    pub fn restore_last_undo(&mut self) {
        if let Some(entry) = self.redo_stack.pop() {
            self.undo_stack.push(entry);
        }
    }

    /// Pop the most recently undone entry and push it back onto the
    /// undo stack.
    pub fn pop_for_redo(&mut self) -> Option<UndoEntry> {
        let entry = self.redo_stack.pop()?;
        self.undo_stack.push(entry.clone());
        Some(entry)
    }

    /// Begin an edit group: the next edit will try to merge with the
    /// previous undo entry.
    pub fn begin_group(&mut self) {
        self.merge_next = true;
    }

    /// End the current edit group. Clears the merge flag.
    pub fn end_group(&mut self) {
        self.merge_next = false;
    }

    #[allow(dead_code)] // exposed for future Buffer::can_undo trait method
    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    #[allow(dead_code)]
    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }
}

/// Try to merge two undo entries. Only sequential inserts at adjacent
/// positions merge today — enough for typing characters and pasting.
fn try_merge(a: &UndoEntry, b: &UndoEntry) -> Option<UndoEntry> {
    match (&a.action, &b.action) {
        (
            UndoAction::InsertText {
                pos: pos_a,
                text: text_a,
            },
            UndoAction::InsertText {
                pos: pos_b,
                text: text_b,
            },
        ) if *pos_a + text_a.len() == *pos_b => {
            let mut merged_text = text_a.clone();
            merged_text.extend_from_slice(text_b);
            Some(UndoEntry {
                cursor_before: a.cursor_before,
                cursor_after: b.cursor_after,
                action: UndoAction::InsertText {
                    pos: *pos_a,
                    text: merged_text,
                },
            })
        }
        _ => None,
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    fn insert_entry(pos: BytePos, text: &str) -> UndoEntry {
        UndoEntry {
            cursor_before: pos,
            cursor_after: pos + text.len(),
            action: UndoAction::InsertText {
                pos,
                text: text.as_bytes().to_vec(),
            },
        }
    }

    #[test]
    fn sequential_inserts_merge() {
        let mut state = UndoState::default();
        state.begin_group();
        state.record(insert_entry(0, "h"));
        state.record(insert_entry(1, "e"));
        state.record(insert_entry(2, "l"));
        state.record(insert_entry(3, "l"));
        state.record(insert_entry(4, "o"));
        state.end_group();
        assert_eq!(state.undo_stack.len(), 1, "expected merge into one entry");
        match &state.undo_stack[0].action {
            UndoAction::InsertText { pos, text } => {
                assert_eq!(*pos, 0);
                assert_eq!(text.as_slice(), b"hello");
            }
            _ => panic!("expected InsertText"),
        }
    }

    #[test]
    fn non_adjacent_inserts_do_not_merge() {
        let mut state = UndoState::default();
        state.begin_group();
        state.record(insert_entry(0, "hello"));
        // Gap of 1 byte — not adjacent, no merge.
        state.record(insert_entry(6, "x"));
        state.end_group();
        assert_eq!(state.undo_stack.len(), 2);
    }

    #[test]
    fn record_without_group_creates_separate_entries() {
        let mut state = UndoState::default();
        state.record(insert_entry(0, "h"));
        state.record(insert_entry(1, "e"));
        assert_eq!(state.undo_stack.len(), 2);
    }

    #[test]
    fn new_edit_clears_redo_stack() {
        let mut state = UndoState::default();
        state.record(insert_entry(0, "a"));
        state.record(insert_entry(1, "b"));
        let _ = state.pop_for_undo();
        let _ = state.pop_for_undo();
        assert_eq!(state.redo_stack.len(), 2);
        // New edit clears redo.
        state.record(insert_entry(0, "z"));
        assert!(state.redo_stack.is_empty());
    }

    #[test]
    fn undo_redo_round_trip() {
        let mut state = UndoState::default();
        state.record(insert_entry(0, "hello"));
        let popped = state.pop_for_undo().unwrap();
        assert!(state.can_redo());
        assert!(!state.can_undo());
        let restored = state.pop_for_redo().unwrap();
        assert_eq!(popped.action, restored.action);
    }
}
