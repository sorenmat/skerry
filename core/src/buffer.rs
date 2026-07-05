//! The `Buffer` trait — the core abstraction frontends operate against.
//!
//! See `docs/adr/0001-piece-table-as-primary-buffer.md` (Piece Table),
//! `0002-memmap-delta-memory-strategy.md` (memmap+delta),
//! `0003-byte-primary-positions-with-line-index.md` (positions),
//! `0004-linear-undo-text-cursor.md` (undo), and
//! `0007-cursor-and-selection-on-buffer.md` (cursor/selection on Buffer)
//! for the rationale behind this shape.

use std::borrow::Cow;
use std::ops::Range;
use std::path::Path;


///
/// A position on the `Buffer`, expressed as a UTF-8 byte offset.
/// The core never deals in characters; byte offsets are unambiguous and
/// match the Piece Table's internal descriptor format (`Piece::start`,
/// `Piece::length`).
pub type BytePos = usize;

/// A range selection on the `Buffer` — anchor and head, both byte offsets.
///
/// `anchor == head` means collapsed (cursor only, no range). Single selection
/// only in v0.1; multi-cursor is deferred.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    /// Where the selection started (byte offset).
    pub anchor: BytePos,
    /// Where the selection ends / cursor is (byte offset).
    pub head: BytePos,
}

impl Selection {
    /// A collapsed selection at the given byte offset.
    pub fn collapsed(at: BytePos) -> Self {
        Self {
            anchor: at,
            head: at,
        }
    }

    /// True when the selection has no range (anchor == head).
    pub fn is_collapsed(&self) -> bool {
        self.anchor == self.head
    }

    /// The selection as a normalised byte range `start..end` (start <= end).
    pub fn range(&self) -> Range<BytePos> {
        self.anchor.min(self.head)..self.anchor.max(self.head)
    }
}

/// The core text manipulation contract. Both TUI and GUI frontends operate
/// against this trait; neither owns text state independently.
pub trait Buffer {
    // === Identity ===

    /// Path this buffer was loaded from, if any. Unsaved buffers return `None`.
    fn source_path(&self) -> Option<&Path>;

    /// Set or change the path this buffer will be saved to.
    fn set_source_path(&mut self, path: std::path::PathBuf);

    /// True if there are unsaved edits.
    fn is_dirty(&self) -> bool;

    // === Geometry ===

    /// Total byte length of the buffer.
    fn len(&self) -> usize;

    /// True if the buffer is empty.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Number of lines. A file with no trailing newline still has a final line.
    fn line_count(&self) -> usize;

    /// One line's text, excluding its trailing newline.
    ///
    /// Returns `Cow::Borrowed(&str)` when the line fits inside a single
    /// `Piece` (the common case for freshly-loaded files and aggressively
    /// coalesced buffers). Returns `Cow::Owned(String)` when the line
    /// straddles piece boundaries (rare; happens after edits fragment a
    /// line into multiple pieces). Returns `None` if `line` is out of range.
    fn line_text(&self, line: usize) -> Option<Cow<'_, str>>;

    /// Byte range covered by one line, excluding the trailing newline.
    /// Returns `None` if `line` is out of range.
    fn line_byte_range(&self, line: usize) -> Option<Range<BytePos>>;

    /// Slice of the document as a (lossy) UTF-8 `String`. `range` is a
    /// byte range; it must be on UTF-8 boundaries (i.e. between chars).
    /// Returns `None` if `range` is out of bounds.
    ///
    /// Used by copy/cut to hand the selected text to the OS clipboard
    /// without having to walk the buffer line-by-line. Implementations
    /// should aim for O(|range|); the Piece Table does this by stitching
    /// together only the pieces the range spans.
    fn slice(&self, range: Range<BytePos>) -> Option<String>;

    // === Position conversion (UTF-8 byte ↔ line/column) ===

    /// Convert a byte offset to a `(line, column)` pair.
    /// Returns `None` if `byte_pos` is out of range.
    fn pos_to_linecol(&self, byte_pos: BytePos) -> Option<(usize, usize)>;

    /// Convert a `(line, column)` pair to a byte offset.
    /// Returns `None` if `line` is out of range or `col` exceeds the line length.
    fn linecol_to_pos(&self, line: usize, col: usize) -> Option<BytePos>;

    // === Cursor (single, primary) ===

    /// Current cursor byte offset.
    fn cursor(&self) -> BytePos;

    /// Set the cursor byte offset. The buffer does not validate that
    /// `byte_pos` is within bounds; callers should validate via
    /// `pos_to_linecol` first.
    fn set_cursor(&mut self, byte_pos: BytePos);

    // === Selection (single, primary) ===

    /// Current selection.
    fn selection(&self) -> Selection;

    /// Set the selection.
    fn set_selection(&mut self, sel: Selection);

    // === Edits ===

    /// Insert `text` at `byte_pos`. Returns the new cursor position
    /// (typically end of inserted text).
    fn insert(&mut self, byte_pos: BytePos, text: &str) -> Result<BytePos, crate::EditError>;

    /// Delete the byte `range`. Returns the new cursor position (typically
    /// the start of the deleted range).
    fn delete(&mut self, range: Range<BytePos>) -> Result<BytePos, crate::EditError>;

    /// Replace the byte range `range` with `text`. Equivalent to
    /// `delete(range)` followed by `insert(text)`, but as a single
    /// atomic operation (one undo entry instead of two).
    ///
    /// Returns the new cursor position (typically end of the inserted
    /// `text`).
    fn replace(&mut self, range: Range<BytePos>, text: &str) -> Result<BytePos, crate::EditError>;

    /// Insert `text` at `byte_pos` WITHOUT recording an undo entry.
    /// Used by line-level operations (move-line, duplicate-line) where
    /// the operation is itself recorded as a single undo step; the
    /// internal inserts should not pile up extra undo entries.
    fn insert_silent(&mut self, byte_pos: BytePos, text: &str)
        -> Result<BytePos, crate::EditError>;

    /// Delete `range` WITHOUT recording an undo entry. Pairs with
    /// `insert_silent` for line-level ops.
    fn delete_silent(&mut self, range: Range<BytePos>) -> Result<BytePos, crate::EditError>;

    // === Undo / redo (linear stack; text + cursor only) ===

    /// Undo the most recent edit group. Returns `false` if the undo stack is empty.
    fn undo(&mut self) -> bool;

    /// Redo the most recently undone edit group. Returns `false` if nothing to redo.
    fn redo(&mut self) -> bool;

    /// Begin an edit group: subsequent edits collapse into one undo entry
    /// until `end_edit_group` is called. Edit groups do not nest in v0.1.
    fn begin_edit_group(&mut self);

    /// End the current edit group.
    fn end_edit_group(&mut self);

    // === Persistence ===

    /// Write the buffer back to `source_path`.
    ///
    /// Behaviour for buffers without a `source_path` is implementation-defined;
    /// the trait currently requires `source_path()` to be `Some` for save to succeed.
    /// Save internals (atomic rename, fsync, encoding roundtrip) are deferred —
    /// see ADR 0002.
    fn save(&mut self) -> Result<(), crate::SaveError>;

    // === Full-document access (for save, export, debug) ===

    /// Reconstruct the full document as a `Vec<u8>`. Allocates a fresh
    /// buffer on every call; intended for save paths and one-shot
    /// conversions, not the rendering hot path. For zero-copy access,
    /// use `line_text` per visible line.
    fn to_bytes(&self) -> Vec<u8>;
}
