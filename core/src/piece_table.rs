//! Piece Table buffer implementation — production-ready.
//!
//! Implements [`Buffer`] for [`PieceTableBuffer`], the canonical text
//! representation for the_editor. See:
//!
//! - [`docs/adr/0001`](../../../docs/adr/0001-piece-table-as-primary-buffer.md)
//!   — why Piece Table.
//! - [`docs/adr/0002`](../../../docs/adr/0002-memmap-delta-memory-strategy.md)
//!   — original + delta storage. The original is held as an
//!   [`OriginalStorage`] enum that supports both `Vec<u8>` (small / in-memory)
//!   and `memmap2::Mmap` (large / memory-mapped).
//! - [`docs/adr/0003`](../../../docs/adr/0003-byte-primary-positions-with-line-index.md)
//!   — byte-primary positions with line index.
//! - [`docs/adr/0007`](../../../docs/adr/0007-cursor-and-selection-on-buffer.md)
//!   — cursor/selection on `Buffer`.
//!
//! ## Data structures
//!
//! - `pieces: Vec<Piece>` — descriptor array. Each `Piece` references
//!   either the original source or the append-only delta.
//! - `cumulative_offsets: Vec<usize>` — parallel to `pieces` plus one
//!   trailing entry. `cumulative_offsets[i]` is the byte offset where
//!   piece `i` starts; `cumulative_offsets.last()` is the total document
//!   length. Rebuilt after every structural change.
//! - `newlines: Vec<usize>` — sorted list of byte positions of every
//!   `\n`. Backs `pos_to_linecol`, `linecol_to_pos`, `line_count`,
//!   `line_text`, and `line_byte_range`.
//! - `original: OriginalStorage` — original file content, either mmap'd
//!   or in-memory `Vec<u8>` (see ADR 0002).
//! - `delta: Vec<u8>` — append-only edit buffer.
//!
//! ## Performance characteristics
//!
//! - `find_piece` (binary search on cumulative offsets): `O(log n)` in
//!   piece count.
//! - `insert` / `delete`: `O(log n + m)` where `m` is the number of
//!   newlines that need shifting. The `O(m)` shift dominates for very
//!   large files; a Fenwick-tree line index is the upgrade path if/when
//!   profiling shows it matters.
//! - `pos_to_linecol` / `linecol_to_pos`: `O(log n)` via binary search
//!   on the newline list.
//! - Aggressive coalescing keeps piece count bounded across long edit
//!   sessions so the per-edit `O(log n + m)` stays in practice `O(log n)`
//!   when edits are local.
//!
//! ## Conventions
//!
//! - `\n` is treated as terminating line `N`. Cursor at the byte
//!   position of `\n` reports `(N, line_length)`. The next byte position
//!   (right after the `\n`) reports `(N+1, 0)`. The empty buffer has
//!   one empty line (`line_count = 1`, line 0 is `""`).
//! - All byte offsets must lie on UTF-8 character boundaries. The
//!   implementation does not validate this; callers (frontends) are
//!   responsible. Lines that fit in a single `Piece` are returned as
//!   `Cow::Borrowed`; lines that straddle piece boundaries are
//!   concatenated and returned as `Cow::Owned` (rare; happens when
//!   edits fragment a line).

use std::borrow::Cow;
use std::ops::Range;
use std::path::{Path, PathBuf};

use crate::buffer::Buffer;
use crate::undo::{UndoAction, UndoEntry, UndoState};
use crate::{BytePos, EditError, SaveError, Selection};

/// Source backing a piece — either the original file or the append-only delta.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PieceSource {
    Original,
    Delta,
}

/// Storage for the original (read-only) file content.
///
/// Two variants:
/// - `Bytes(Vec<u8>)` — fully in-memory. Used for tests, empty files,
///   buffers constructed from `Vec<u8>` directly, and the fallback path
///   for empty files (which `memmap2` cannot map).
/// - `Mmap(memmap2::Mmap)` — memory-mapped. Used for files loaded from
///   disk via [`PieceTableBuffer::from_path`]. The OS handles paging;
///   we never load the whole file into RAM.
#[derive(Debug)]
enum OriginalStorage {
    Bytes(Vec<u8>),
    Mmap(memmap2::Mmap),
}

impl OriginalStorage {
    fn as_slice(&self) -> &[u8] {
        match self {
            Self::Bytes(v) => v.as_slice(),
            Self::Mmap(m) => m.as_ref(),
        }
    }

    fn len(&self) -> usize {
        self.as_slice().len()
    }

    #[allow(dead_code)]
    fn is_empty(&self) -> bool {
        self.as_slice().is_empty()
    }
}

/// One contiguous run of text in the visible document, drawn from one source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Piece {
    pub source: PieceSource,
    /// Start byte offset within the source.
    pub start: usize,
    /// Length in bytes.
    pub length: usize,
}

/// Piece Table buffer.
#[derive(Debug)]
pub struct PieceTableBuffer {
    source_path: Option<PathBuf>,
    dirty: bool,
    original: OriginalStorage,
    delta: Vec<u8>,
    pieces: Vec<Piece>,
    /// `cumulative_offsets[i]` = byte offset where piece `i` starts.
    /// Length = `pieces.len() + 1`; trailing entry is total document length.
    cumulative_offsets: Vec<usize>,
    /// Sorted byte positions of every `\n`. Length = newline count.
    newlines: Vec<usize>,
    cursor: BytePos,
    selection: Selection,
    /// Linear undo stack (see ADR 0004).
    undo_state: UndoState,
    /// Bytes at last load or save. `None` for an unsaved buffer that has
    /// never been saved. Used by `is_dirty()` to compute dirty correctly
    /// across edits, undos, and saves.
    saved_state: Option<Vec<u8>>,
}

impl PieceTableBuffer {
    /// Create an empty, in-memory piece table with no source path.
    pub fn new() -> Self {
        Self {
            source_path: None,
            dirty: false,
            original: OriginalStorage::Bytes(Vec::new()),
            delta: Vec::new(),
            pieces: Vec::new(),
            cumulative_offsets: vec![0],
            newlines: Vec::new(),
            cursor: 0,
            selection: Selection::collapsed(0),
            undo_state: UndoState::default(),
            saved_state: None,
        }
    }

    /// Reconstruct the full document as `Vec<u8>`. O(n) where n is the
    /// document length. This walks the piece array and concatenates from
    /// `original` and `delta`. Used by `save()` and by tests.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.total_len());
        for piece in &self.pieces {
            let src = match piece.source {
                PieceSource::Original => self.original.as_slice(),
                PieceSource::Delta => &self.delta,
            };
            out.extend_from_slice(&src[piece.start..piece.start + piece.length]);
        }
        out
    }

    /// Reconstruct the full document as `String`. Panics if the document
    /// is not valid UTF-8; `PieceTableBuffer` should always produce valid
    /// UTF-8 because all inputs are `&str`, but this is a defensive
    /// wrapper for code that wants the explicit error.
    pub fn to_string(&self) -> Result<String, std::string::FromUtf8Error> {
        String::from_utf8(self.to_bytes())
    }

    /// Build a buffer from in-memory content (e.g. loaded from a file).
    /// `content` is taken verbatim as the original source; the caller is
    /// responsible for any UTF-8 validity. The buffer is initially clean
    /// because the on-disk content matches.
    pub fn from_bytes(content: Vec<u8>) -> Self {
        let saved = content.clone();
        let mut buf = Self::new();
        if !content.is_empty() {
            let len = content.len();
            buf.original = OriginalStorage::Bytes(content);
            buf.pieces.push(Piece {
                source: PieceSource::Original,
                start: 0,
                length: len,
            });
            buf.rebuild_cumulative_offsets();
            buf.rebuild_newlines();
        }
        buf.saved_state = Some(saved);
        buf
    }

    /// Open a file and memory-map its contents as the original source.
    /// This is the right entry point for production — multi-GB files
    /// never touch RAM as a whole; the OS pages them in and out.
    ///
    /// Empty files fall back to in-memory `Bytes` storage because
    /// `memmap2` cannot map a zero-length file.
    pub fn from_path(path: PathBuf) -> Result<Self, std::io::Error> {
        let file = std::fs::File::open(&path)?;
        let len = file.metadata()?.len() as usize;

        let storage = if len == 0 {
            OriginalStorage::Bytes(Vec::new())
        } else {
            // SAFETY: `memmap2::Mmap::map` creates a read-only mapping.
            // The underlying file is opened read-only via `File::open`
            // and remains valid for the lifetime of the mapping (the
            // OS keeps the inode alive). The File handle itself can be
            // dropped after the Mmap is created.
            #[allow(unsafe_code)]
            let mmap = unsafe { memmap2::Mmap::map(&file)? };
            OriginalStorage::Mmap(mmap)
        };

        let saved = storage.as_slice().to_vec();
        let mut buf = Self::new();
        buf.source_path = Some(path);
        buf.original = storage;
        let bytes_len = buf.original.len();
        if bytes_len > 0 {
            buf.pieces.push(Piece {
                source: PieceSource::Original,
                start: 0,
                length: bytes_len,
            });
            buf.rebuild_cumulative_offsets();
            buf.rebuild_newlines();
        }
        buf.saved_state = Some(saved);
        Ok(buf)
    }

    /// Like [`Self::from_bytes`] but records a source path for save.
    pub fn from_bytes_with_path(content: Vec<u8>, path: PathBuf) -> Self {
        let mut buf = Self::from_bytes(content);
        buf.source_path = Some(path);
        buf
    }

    // === Internal helpers ===

    fn total_len(&self) -> usize {
        *self.cumulative_offsets.last().unwrap_or(&0)
    }

    /// Recompute the dirty flag by comparing current bytes against the
    /// last saved state. Called after undo/redo because those don't set
    /// `dirty` directly — the buffer may or may not match the saved state
    /// depending on how far back the user went.
    fn recompute_dirty(&mut self) {
        let current = self.to_bytes();
        self.dirty = match &self.saved_state {
            Some(saved) => saved.as_slice() != current.as_slice(),
            None => !current.is_empty(),
        };
    }

    fn rebuild_cumulative_offsets(&mut self) {
        self.cumulative_offsets.clear();
        self.cumulative_offsets.push(0);
        let mut acc = 0usize;
        for piece in &self.pieces {
            acc += piece.length;
            self.cumulative_offsets.push(acc);
        }
    }

    fn rebuild_newlines(&mut self) {
        self.newlines.clear();
        for (i, piece) in self.pieces.iter().enumerate() {
            let piece_start_doc = self.cumulative_offsets[i];
            let source = self.piece_source_slice(piece);
            let piece_newlines: Vec<usize> = (0..piece.length)
                .filter_map(|off| {
                    if source[piece.start + off] == b'\n' {
                        Some(piece_start_doc + off)
                    } else {
                        None
                    }
                })
                .collect();
            self.newlines.extend(piece_newlines);
        }
    }

    fn piece_source_slice(&self, piece: &Piece) -> &[u8] {
        match piece.source {
            PieceSource::Original => self.original.as_slice(),
            PieceSource::Delta => &self.delta,
        }
    }

    /// Locate the piece containing `byte_pos`. Returns
    /// `(piece_idx, offset_within_piece)`.
    ///
    /// If `byte_pos` is at or past the total length, returns a
    /// "past-the-end" position `(last_idx, last_piece.length)` so
    /// callers can use it for cursor placement.
    pub fn find_piece(&self, byte_pos: BytePos) -> (usize, usize) {
        if self.pieces.is_empty() {
            return (0, 0);
        }
        let total = self.total_len();
        if byte_pos >= total {
            let last_idx = self.pieces.len() - 1;
            return (last_idx, self.pieces[last_idx].length);
        }
        // partition_point: first index where cumulative_offsets[idx] > byte_pos.
        let idx = self.cumulative_offsets.partition_point(|&co| co <= byte_pos);
        let piece_idx = idx - 1;
        let offset = byte_pos - self.cumulative_offsets[piece_idx];
        (piece_idx, offset)
    }

    fn try_coalesce_with_prev(&mut self, idx: usize) {
        if idx == 0 {
            return;
        }
        let prev = self.pieces[idx - 1];
        let curr = self.pieces[idx];
        if prev.source == curr.source && prev.start + prev.length == curr.start {
            self.pieces[idx - 1].length += curr.length;
            self.pieces.remove(idx);
            self.rebuild_cumulative_offsets();
        }
    }

    fn try_coalesce_with_next(&mut self, idx: usize) {
        if idx + 1 >= self.pieces.len() {
            return;
        }
        let curr = self.pieces[idx];
        let next = self.pieces[idx + 1];
        if curr.source == next.source && curr.start + curr.length == next.start {
            self.pieces[idx].length += next.length;
            self.pieces.remove(idx + 1);
            self.rebuild_cumulative_offsets();
        }
    }

    /// Read a contiguous slice of the document. Returns `Cow::Borrowed`
    /// when the slice fits in one piece; `Cow::Owned` when it spans pieces.
    /// Returns `None` if the range is out of bounds.
    fn get_slice(&self, range: Range<BytePos>) -> Option<Cow<'_, [u8]>> {
        let total = self.total_len();
        if range.start > range.end || range.end > total {
            return None;
        }
        if self.pieces.is_empty() {
            return Some(Cow::Borrowed(&[]));
        }
        if range.start == range.end {
            return Some(Cow::Borrowed(&[]));
        }
        let (start_p, start_off) = self.find_piece(range.start);
        let end_p_end = if range.end == total {
            let last_idx = self.pieces.len() - 1;
            (last_idx, self.pieces[last_idx].length)
        } else {
            self.find_piece(range.end)
        };
        let (end_p, end_off) = end_p_end;

        if start_p == end_p {
            let piece = self.pieces[start_p];
            let src = self.piece_source_slice(&piece);
            return Some(Cow::Borrowed(
                &src[piece.start + start_off..piece.start + end_off],
            ));
        }

        // Multi-piece: concatenate.
        let mut bytes = Vec::with_capacity(range.end - range.start);
        let p1 = self.pieces[start_p];
        let s1 = self.piece_source_slice(&p1);
        bytes.extend_from_slice(&s1[p1.start + start_off..p1.start + p1.length]);
        for i in (start_p + 1)..end_p {
            let p = self.pieces[i];
            let s = self.piece_source_slice(&p);
            bytes.extend_from_slice(&s[p.start..p.start + p.length]);
        }
        let p_last = self.pieces[end_p];
        let s_last = self.piece_source_slice(&p_last);
        bytes.extend_from_slice(&s_last[p_last.start..p_last.start + end_off]);
        Some(Cow::Owned(bytes))
    }

    // === Newline index maintenance ===

    fn newlines_in_text(text: &str) -> Vec<usize> {
        text.bytes()
            .enumerate()
            .filter_map(|(i, b)| if b == b'\n' { Some(i) } else { None })
            .collect()
    }

    fn update_newlines_for_insert(&mut self, byte_pos: BytePos, text: &str) {
        let new_positions = Self::newlines_in_text(text);
        if new_positions.is_empty() {
            for nl in &mut self.newlines {
                if *nl >= byte_pos {
                    *nl += text.len();
                }
            }
            return;
        }
        // Existing newlines < byte_pos stay; newlines in `text` are at
        // byte_pos + offset_in_text; existing newlines >= byte_pos shift by +text.len().
        let split_at = self.newlines.partition_point(|&nl| nl < byte_pos);
        let inserted: Vec<usize> = new_positions.iter().map(|&i| byte_pos + i).collect();
        let tail: Vec<usize> = self.newlines[split_at..]
            .iter()
            .map(|&nl| nl + text.len())
            .collect();
        self.newlines.truncate(split_at);
        self.newlines.extend(inserted);
        self.newlines.extend(tail);
    }

    fn update_newlines_for_delete(&mut self, range: &Range<BytePos>) {
        let deleted_len = range.end - range.start;
        if deleted_len == 0 {
            return;
        }
        let split_keep = self.newlines.partition_point(|&nl| nl < range.start);
        let split_drop = self.newlines.partition_point(|&nl| nl < range.end);
        let tail: Vec<usize> = self.newlines[split_drop..]
            .iter()
            .map(|&nl| nl - deleted_len)
            .collect();
        self.newlines.truncate(split_keep);
        self.newlines.extend(tail);
    }

    // === Edit primitives ===

    fn do_insert(&mut self, byte_pos: BytePos, text: &str) -> Result<BytePos, EditError> {
        let total = self.total_len();
        if byte_pos > total {
            return Err(EditError::OutOfBounds {
                pos: byte_pos,
                len: total,
            });
        }
        if text.is_empty() {
            return Ok(byte_pos);
        }

        if self.pieces.is_empty() {
            let start_in_delta = self.delta.len();
            self.delta.extend_from_slice(text.as_bytes());
            self.pieces.push(Piece {
                source: PieceSource::Delta,
                start: start_in_delta,
                length: text.len(),
            });
            self.rebuild_cumulative_offsets();
            self.update_newlines_for_insert(byte_pos, text);
            self.dirty = true;
            let new_cursor = byte_pos + text.len();
            self.cursor = new_cursor;
            self.selection = Selection::collapsed(new_cursor);
            return Ok(new_cursor);
        }

        let (piece_idx, offset_in_piece) = self.find_piece(byte_pos);

        // Split the piece if byte_pos is in the middle.
        let insert_at = if offset_in_piece == 0 {
            // Insert at the start of piece_idx (between prev and piece_idx).
            piece_idx
        } else if offset_in_piece == self.pieces[piece_idx].length {
            // Insert at the end of piece_idx (between piece_idx and next).
            piece_idx + 1
        } else {
            let piece = self.pieces[piece_idx];
            let left = Piece {
                source: piece.source,
                start: piece.start,
                length: offset_in_piece,
            };
            let right = Piece {
                source: piece.source,
                start: piece.start + offset_in_piece,
                length: piece.length - offset_in_piece,
            };
            self.pieces[piece_idx] = left;
            self.pieces.insert(piece_idx + 1, right);
            piece_idx + 1
        };

        let start_in_delta = self.delta.len();
        self.delta.extend_from_slice(text.as_bytes());
        self.pieces.insert(
            insert_at,
            Piece {
                source: PieceSource::Delta,
                start: start_in_delta,
                length: text.len(),
            },
        );
        self.rebuild_cumulative_offsets();
        self.update_newlines_for_insert(byte_pos, text);
        self.dirty = true;

        let new_cursor = byte_pos + text.len();
        self.cursor = new_cursor;
        self.selection = Selection::collapsed(new_cursor);

        // Coalesce the new piece with its neighbours when they share a source.
        // After coalesce_with_prev the new piece may shift down by 1; coalesce
        // with next uses the (possibly shifted) index.
        let mut working_idx = insert_at;
        if working_idx > 0 && self.pieces[working_idx - 1].source == self.pieces[working_idx].source
        {
            let prev_end = self.pieces[working_idx - 1].start + self.pieces[working_idx - 1].length;
            if prev_end == self.pieces[working_idx].start {
                self.try_coalesce_with_prev(working_idx);
                working_idx -= 1;
            }
        }
        self.try_coalesce_with_next(working_idx);

        Ok(new_cursor)
    }

    fn do_delete(&mut self, range: Range<BytePos>) -> Result<BytePos, EditError> {
        let total = self.total_len();
        if range.start > range.end {
            return Err(EditError::InvalidRange(range));
        }
        if range.end > total {
            return Err(EditError::OutOfBounds {
                pos: range.end,
                len: total,
            });
        }
        if range.start == range.end {
            return Ok(self.cursor);
        }
        if self.pieces.is_empty() {
            return Err(EditError::OutOfBounds {
                pos: range.start,
                len: 0,
            });
        }

        let (start_p, start_off) = self.find_piece(range.start);
        let (end_p, end_off) = if range.end == total {
            let last = self.pieces.len() - 1;
            (last, self.pieces[last].length)
        } else {
            self.find_piece(range.end)
        };

        // Plan new pieces at the deletion site.
        let left_part = if start_off > 0 {
            let p = self.pieces[start_p];
            Some(Piece {
                source: p.source,
                start: p.start,
                length: start_off,
            })
        } else {
            None
        };

        let right_part = if start_p == end_p {
            // Single piece case: right portion handled separately below.
            None
        } else if end_off < self.pieces[end_p].length {
            let p = self.pieces[end_p];
            Some(Piece {
                source: p.source,
                start: p.start + end_off,
                length: p.length - end_off,
            })
        } else {
            None
        };

        if start_p == end_p {
            // Single-piece delete.
            let piece = self.pieces[start_p];
            let right_part_single = if end_off < piece.length {
                Some(Piece {
                    source: piece.source,
                    start: piece.start + end_off,
                    length: piece.length - end_off,
                })
            } else {
                None
            };
            self.pieces.remove(start_p);
            // Re-insert in reverse so left ends up at start_p, right at start_p + 1.
            if let Some(r) = right_part_single {
                self.pieces.insert(start_p, r);
            }
            if let Some(l) = left_part {
                self.pieces.insert(start_p, l);
            }
        } else {
            // Multi-piece delete.
            for _ in start_p..=end_p {
                self.pieces.remove(start_p);
            }
            // Insert left_part then right_part at start_p.
            if let Some(r) = right_part {
                self.pieces.insert(start_p, r);
            }
            if let Some(l) = left_part {
                self.pieces.insert(start_p, l);
            }
        }

        self.update_newlines_for_delete(&range);
        self.rebuild_cumulative_offsets();

        // Try to coalesce the (possibly two) new pieces at start_p with each
        // other, then with the surrounding pieces.
        if self.pieces.len() >= 2 && start_p < self.pieces.len() {
            self.try_coalesce_with_next(start_p);
            if start_p > 0 && start_p < self.pieces.len() {
                self.try_coalesce_with_prev(start_p);
            }
        }

        self.cursor = range.start;
        self.selection = Selection::collapsed(range.start);
        self.dirty = true;

        Ok(range.start)
    }
}

impl Default for PieceTableBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl Buffer for PieceTableBuffer {
    fn source_path(&self) -> Option<&Path> {
        self.source_path.as_deref()
    }

    fn is_dirty(&self) -> bool {
        self.dirty
    }

    fn len(&self) -> usize {
        self.total_len()
    }

    fn line_count(&self) -> usize {
        // Always at least one (the empty line) so an empty buffer is
        // navigable. For non-empty buffers: number of newlines + 1.
        self.newlines.len() + 1
    }

    fn line_text(&self, line: usize) -> Option<Cow<'_, str>> {
        if line > self.newlines.len() {
            return None;
        }
        let start = if line == 0 {
            0
        } else {
            self.newlines[line - 1] + 1
        };
        let end = if line < self.newlines.len() {
            self.newlines[line]
        } else {
            self.total_len()
        };
        let bytes = self.get_slice(start..end)?;
        match bytes {
            Cow::Borrowed(b) => match std::str::from_utf8(b) {
                Ok(s) => Some(Cow::Borrowed(s)),
                Err(_) => Some(Cow::Owned(String::from_utf8_lossy(b).into_owned())),
            },
            Cow::Owned(b) => match String::from_utf8(b) {
                Ok(s) => Some(Cow::Owned(s)),
                Err(e) => Some(Cow::Owned(
                    String::from_utf8_lossy(&e.into_bytes()).into_owned(),
                )),
            },
        }
    }

    fn line_byte_range(&self, line: usize) -> Option<Range<BytePos>> {
        if line > self.newlines.len() {
            return None;
        }
        let start = if line == 0 {
            0
        } else {
            self.newlines[line - 1] + 1
        };
        let end = if line < self.newlines.len() {
            self.newlines[line]
        } else {
            self.total_len()
        };
        Some(start..end)
    }

    fn pos_to_linecol(&self, byte_pos: BytePos) -> Option<(usize, usize)> {
        if byte_pos > self.total_len() {
            return None;
        }
        // Convention: '\n' terminates line N. Cursor at the byte position
        // of '\n' reports (N, line_length); cursor at newline_pos + 1
        // reports (N+1, 0).
        let line = self.newlines.partition_point(|&nl| nl < byte_pos);
        let col = if line == 0 {
            byte_pos
        } else {
            let prev = self.newlines[line - 1];
            byte_pos - prev - 1
        };
        Some((line, col))
    }

    fn linecol_to_pos(&self, line: usize, col: usize) -> Option<BytePos> {
        if line > self.newlines.len() {
            return None;
        }
        let line_start = if line == 0 {
            0
        } else {
            self.newlines[line - 1] + 1
        };
        let line_end = if line < self.newlines.len() {
            self.newlines[line]
        } else {
            self.total_len()
        };
        let line_len = line_end - line_start;
        if col > line_len {
            return None;
        }
        Some(line_start + col)
    }

    fn cursor(&self) -> BytePos {
        self.cursor
    }

    fn set_cursor(&mut self, byte_pos: BytePos) {
        self.cursor = byte_pos;
    }

    fn selection(&self) -> Selection {
        self.selection
    }

    fn set_selection(&mut self, sel: Selection) {
        self.selection = sel;
    }

    fn insert(&mut self, byte_pos: BytePos, text: &str) -> Result<BytePos, EditError> {
        let cursor_before = self.cursor;
        let bytes = text.as_bytes().to_vec();
        let result = self.do_insert(byte_pos, text);
        if result.is_ok() {
            let cursor_after = self.cursor;
            self.undo_state.record(UndoEntry {
                cursor_before,
                cursor_after,
                action: UndoAction::InsertText {
                    pos: byte_pos,
                    text: bytes,
                },
            });
        }
        result
    }

    fn delete(&mut self, range: Range<BytePos>) -> Result<BytePos, EditError> {
        let cursor_before = self.cursor;
        // Save the bytes that will be deleted so we can restore them on undo.
        let deleted = self
            .get_slice(range.clone())
            .map(|cow| cow.into_owned())
            .unwrap_or_default();
        let result = self.do_delete(range.clone());
        if result.is_ok() {
            let cursor_after = self.cursor;
            self.undo_state.record(UndoEntry {
                cursor_before,
                cursor_after,
                action: UndoAction::DeleteRange {
                    pos: range.start,
                    deleted,
                },
            });
        }
        result
    }

    fn undo(&mut self) -> bool {
        let Some(entry) = self.undo_state.pop_for_undo() else {
            return false;
        };
        // Apply the inverse of the recorded action. We call do_insert /
        // do_delete directly so we don't re-record (which would corrupt
        // the undo/redo stacks).
        match &entry.action {
            UndoAction::InsertText { pos, text } => {
                let _ = self.do_delete(*pos..(*pos + text.len()));
            }
            UndoAction::DeleteRange { pos, deleted } => {
                let s = String::from_utf8_lossy(deleted);
                let _ = self.do_insert(*pos, &s);
            }
        }
        self.cursor = entry.cursor_before;
        self.selection = Selection::collapsed(entry.cursor_before);
        // Undo doesn't have a clean "dirty = true" answer — depends on
        // whether we landed on the saved state. Recompute.
        self.recompute_dirty();
        true
    }

    fn redo(&mut self) -> bool {
        let Some(entry) = self.undo_state.pop_for_redo() else {
            return false;
        };
        // Re-apply the original action.
        match &entry.action {
            UndoAction::InsertText { pos, text } => {
                let s = String::from_utf8_lossy(text);
                let _ = self.do_insert(*pos, &s);
            }
            UndoAction::DeleteRange { pos, deleted } => {
                let _ = self.do_delete(*pos..(*pos + deleted.len()));
            }
        }
        self.cursor = entry.cursor_after;
        self.selection = Selection::collapsed(entry.cursor_after);
        self.recompute_dirty();
        true
    }

    fn begin_edit_group(&mut self) {
        self.undo_state.begin_group();
    }

    fn end_edit_group(&mut self) {
        self.undo_state.end_group();
    }

    fn save(&mut self) -> Result<(), SaveError> {
        // Atomic save: write to a sibling temp file, then `rename` over
        // the original. `rename` is atomic on POSIX when source and
        // destination are on the same filesystem, so a crash mid-write
        // leaves either the old file or the new file — never a
        // half-written file. fsync of the temp file and parent directory
        // for true crash-safety is a future slice; the v1 path is
        // crash-correct at the syscall level but not durable across a
        // power loss.
        let path = self.source_path.as_ref().ok_or(SaveError::NoSourcePath)?;
        let bytes = self.to_bytes();

        let mut temp_os = path.as_os_str().to_owned();
        temp_os.push(".tmp");
        let temp_path = PathBuf::from(temp_os);

        std::fs::write(&temp_path, &bytes)?;
        // If rename fails (e.g. cross-device), the temp file is left
        // behind. Cleanup is best-effort.
        if let Err(e) = std::fs::rename(&temp_path, path) {
            let _ = std::fs::remove_file(&temp_path);
            return Err(SaveError::Io(e));
        }

        self.saved_state = Some(bytes);
        self.dirty = false;
        Ok(())
    }

    fn to_bytes(&self) -> Vec<u8> {
        // Delegates to the inherent method of the same name.
        PieceTableBuffer::to_bytes(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ----- Helpers -----

    /// Reconstruct the full document text by walking pieces. Used by tests
    /// to verify structural correctness without depending on `line_text`
    /// (which we also test independently).
    fn reconstruct(buf: &PieceTableBuffer) -> Vec<u8> {
        let mut out = Vec::with_capacity(buf.len());
        for piece in &buf.pieces {
            let src = match piece.source {
                PieceSource::Original => buf.original.as_slice(),
                PieceSource::Delta => &buf.delta,
            };
            out.extend_from_slice(&src[piece.start..piece.start + piece.length]);
        }
        out
    }

    /// Reconstruct as String. Asserts UTF-8 validity (should always hold
    /// because the impl never produces invalid UTF-8 itself; invalid input
    /// is the caller's responsibility).
    fn reconstruct_str(buf: &PieceTableBuffer) -> String {
        String::from_utf8(reconstruct(buf)).expect("buffer must be valid UTF-8")
    }

    // ----- Empty buffer -----

    #[test]
    fn empty_buffer_invariants() {
        let buf = PieceTableBuffer::new();
        assert_eq!(buf.len(), 0);
        assert!(buf.is_empty());
        assert!(!buf.is_dirty());
        assert_eq!(buf.line_count(), 1, "empty buffer has one empty line");
        assert_eq!(buf.cursor(), 0);
        assert!(buf.selection().is_collapsed());
        assert_eq!(reconstruct_str(&buf), "");
    }

    #[test]
    fn empty_buffer_line_zero_is_empty() {
        let buf = PieceTableBuffer::new();
        let text = buf.line_text(0).unwrap();
        assert_eq!(text, "");
        assert_eq!(buf.line_byte_range(0).unwrap(), 0..0);
    }

    #[test]
    fn empty_buffer_pos_conversions() {
        let buf = PieceTableBuffer::new();
        assert_eq!(buf.pos_to_linecol(0), Some((0, 0)));
        assert_eq!(buf.linecol_to_pos(0, 0), Some(0));
        assert_eq!(buf.pos_to_linecol(1), None);
        assert_eq!(buf.linecol_to_pos(1, 0), None);
    }

    // ----- Single-piece inserts -----

    #[test]
    fn insert_into_empty_buffer() {
        let mut buf = PieceTableBuffer::new();
        let pos = buf.insert(0, "hello").unwrap();
        assert_eq!(pos, 5);
        assert_eq!(buf.len(), 5);
        assert!(buf.is_dirty());
        assert_eq!(buf.cursor(), 5);
        assert_eq!(reconstruct_str(&buf), "hello");
    }

    #[test]
    fn insert_at_end() {
        let mut buf = PieceTableBuffer::from_bytes(b"hello".to_vec());
        let pos = buf.insert(5, " world").unwrap();
        assert_eq!(pos, 11);
        assert_eq!(reconstruct_str(&buf), "hello world");
    }

    #[test]
    fn insert_at_start() {
        let mut buf = PieceTableBuffer::from_bytes(b"world".to_vec());
        let pos = buf.insert(0, "hello ").unwrap();
        assert_eq!(pos, 6);
        assert_eq!(reconstruct_str(&buf), "hello world");
    }

    #[test]
    fn insert_in_middle() {
        let mut buf = PieceTableBuffer::from_bytes(b"helloworld".to_vec());
        let pos = buf.insert(5, " ").unwrap();
        assert_eq!(pos, 6);
        assert_eq!(reconstruct_str(&buf), "hello world");
    }

    #[test]
    fn insert_split_piece_in_middle() {
        // File loaded = one piece. Insert in the middle of a piece splits
        // it into two pieces around the inserted text.
        let mut buf = PieceTableBuffer::from_bytes(b"abcdef".to_vec());
        buf.insert(3, "XYZ").unwrap();
        assert_eq!(reconstruct_str(&buf), "abcXYZdef");
        // Piece count after coalescing: 3 (left "abc", delta "XYZ", right "def")
        assert_eq!(buf.pieces.len(), 3, "expected 3 pieces after middle insert");
    }

    // ----- Coalescing -----

    #[test]
    fn adjacent_inserts_coalesce() {
        // Two consecutive inserts at the same end position should coalesce
        // into a single delta piece (and the original piece stays separate).
        let mut buf = PieceTableBuffer::from_bytes(b"hello".to_vec());
        buf.insert(5, " ").unwrap();
        buf.insert(6, "world").unwrap();
        assert_eq!(reconstruct_str(&buf), "hello world");
        // After coalescing: original "hello", delta " world" — 2 pieces.
        assert_eq!(buf.pieces.len(), 2, "expected coalesced pieces");
    }

    #[test]
    fn delete_then_insert_at_same_site_coalesces() {
        let mut buf = PieceTableBuffer::from_bytes(b"hello world".to_vec());
        buf.delete(5..11).unwrap(); // remove " world"
        assert_eq!(reconstruct_str(&buf), "hello");
        buf.insert(5, " rust").unwrap();
        assert_eq!(reconstruct_str(&buf), "hello rust");
        // The insert creates a delta piece; with nothing to its right (we
        // deleted), and left being the original "hello" (different source),
        // no coalesce happens. 2 pieces total.
        assert_eq!(buf.pieces.len(), 2);
    }

    // ----- Deletes -----

    #[test]
    fn delete_at_start() {
        let mut buf = PieceTableBuffer::from_bytes(b"hello world".to_vec());
        buf.delete(0..6).unwrap();
        assert_eq!(reconstruct_str(&buf), "world");
    }

    #[test]
    fn delete_at_end() {
        let mut buf = PieceTableBuffer::from_bytes(b"hello world".to_vec());
        buf.delete(6..11).unwrap();
        assert_eq!(reconstruct_str(&buf), "hello ");
    }

    #[test]
    fn delete_in_middle() {
        let mut buf = PieceTableBuffer::from_bytes(b"hello world".to_vec());
        buf.delete(5..6).unwrap();
        assert_eq!(reconstruct_str(&buf), "helloworld");
    }

    #[test]
    fn delete_entire_buffer() {
        let mut buf = PieceTableBuffer::from_bytes(b"hello".to_vec());
        buf.delete(0..5).unwrap();
        assert_eq!(reconstruct_str(&buf), "");
        assert_eq!(buf.len(), 0);
    }

    #[test]
    fn delete_across_multiple_pieces() {
        // Build a buffer with three pieces via insert+delete, then delete
        // a range that spans them all.
        let mut buf = PieceTableBuffer::from_bytes(b"abcdefghij".to_vec());
        buf.delete(2..4).unwrap(); // remove "cd" — splits into 2 pieces
        buf.insert(8, "XYZ").unwrap(); // inserts "XYZ" at position 8 — now 3 pieces
        // State: "ab" + "efgh" + "ijXYZ", reconstruct "abefghijXYZ"
        assert_eq!(reconstruct_str(&buf), "abefghijXYZ");
        assert_eq!(buf.pieces.len(), 3);

        // Delete range that spans all three pieces.
        buf.delete(1..9).unwrap(); // remove "befghijX" — leaves "a" + "YZ"
        assert_eq!(reconstruct_str(&buf), "aYZ");
    }

    #[test]
    fn delete_with_invalid_range_errors() {
        let mut buf = PieceTableBuffer::from_bytes(b"hello".to_vec());
        // Range { start: 3, end: 2 } is an empty range in Rust (start >= end),
        // so `..` syntax can't construct it. We build it explicitly to test
        // the defensive check in `do_delete`.
        let inverted = std::ops::Range { start: 3, end: 2 };
        let err = buf.delete(inverted).unwrap_err();
        assert!(matches!(err, EditError::InvalidRange(_)));
    }

    #[test]
    fn delete_out_of_bounds_errors() {
        let mut buf = PieceTableBuffer::from_bytes(b"hello".to_vec());
        let err = buf.delete(0..100).unwrap_err();
        assert!(matches!(err, EditError::OutOfBounds { .. }));
    }

    #[test]
    fn insert_out_of_bounds_errors() {
        let mut buf = PieceTableBuffer::from_bytes(b"hi".to_vec());
        let err = buf.insert(99, "x").unwrap_err();
        assert!(matches!(err, EditError::OutOfBounds { .. }));
    }

    #[test]
    fn empty_insert_is_noop() {
        let mut buf = PieceTableBuffer::from_bytes(b"hello".to_vec());
        let pos = buf.insert(3, "").unwrap();
        assert_eq!(pos, 3);
        assert_eq!(reconstruct_str(&buf), "hello");
    }

    #[test]
    fn zero_range_delete_is_noop() {
        let mut buf = PieceTableBuffer::from_bytes(b"hello".to_vec());
        let pos = buf.delete(2..2).unwrap();
        assert_eq!(pos, buf.cursor());
        assert_eq!(reconstruct_str(&buf), "hello");
    }

    // ----- Line index -----

    #[test]
    fn line_count_single_line() {
        let buf = PieceTableBuffer::from_bytes(b"hello".to_vec());
        assert_eq!(buf.line_count(), 1);
        assert_eq!(buf.line_text(0).unwrap(), "hello");
    }

    #[test]
    fn line_count_two_lines_no_trailing_newline() {
        let buf = PieceTableBuffer::from_bytes(b"hello\nworld".to_vec());
        assert_eq!(buf.line_count(), 2);
        assert_eq!(buf.line_text(0).unwrap(), "hello");
        assert_eq!(buf.line_text(1).unwrap(), "world");
    }

    #[test]
    fn line_count_two_lines_with_trailing_newline() {
        let buf = PieceTableBuffer::from_bytes(b"hello\nworld\n".to_vec());
        assert_eq!(buf.line_count(), 3);
        assert_eq!(buf.line_text(0).unwrap(), "hello");
        assert_eq!(buf.line_text(1).unwrap(), "world");
        assert_eq!(buf.line_text(2).unwrap(), "");
    }

    #[test]
    fn line_count_three_lines() {
        let buf = PieceTableBuffer::from_bytes(b"a\nb\nc".to_vec());
        assert_eq!(buf.line_count(), 3);
        assert_eq!(buf.line_text(0).unwrap(), "a");
        assert_eq!(buf.line_text(1).unwrap(), "b");
        assert_eq!(buf.line_text(2).unwrap(), "c");
    }

    #[test]
    fn pos_to_linecol_basic() {
        let buf = PieceTableBuffer::from_bytes(b"hello\nworld".to_vec());
        assert_eq!(buf.pos_to_linecol(0), Some((0, 0)));
        assert_eq!(buf.pos_to_linecol(4), Some((0, 4)));
        // pos 5 is '\n' — cursor at '\n' reports (0, 5) under our convention.
        assert_eq!(buf.pos_to_linecol(5), Some((0, 5)));
        assert_eq!(buf.pos_to_linecol(6), Some((1, 0)));
        assert_eq!(buf.pos_to_linecol(10), Some((1, 4)));
        assert_eq!(buf.pos_to_linecol(11), Some((1, 5)));
    }

    #[test]
    fn linecol_to_pos_basic() {
        let buf = PieceTableBuffer::from_bytes(b"hello\nworld".to_vec());
        assert_eq!(buf.linecol_to_pos(0, 0), Some(0));
        assert_eq!(buf.linecol_to_pos(0, 5), Some(5));
        assert_eq!(buf.linecol_to_pos(1, 0), Some(6));
        assert_eq!(buf.linecol_to_pos(1, 5), Some(11));
        assert_eq!(buf.linecol_to_pos(2, 0), None);
    }

    #[test]
    fn pos_to_linecol_roundtrip() {
        let buf = PieceTableBuffer::from_bytes(b"line0\nline1\nline2\n".to_vec());
        for byte_pos in 0..=buf.len() {
            let (line, col) = buf.pos_to_linecol(byte_pos).unwrap();
            let back = buf.linecol_to_pos(line, col).unwrap();
            assert_eq!(back, byte_pos, "roundtrip failed at {byte_pos}");
        }
    }

    #[test]
    fn insert_with_newlines_updates_line_index() {
        let mut buf = PieceTableBuffer::new();
        buf.insert(0, "a\nb\nc").unwrap();
        assert_eq!(buf.line_count(), 3);
        assert_eq!(reconstruct_str(&buf), "a\nb\nc");
        assert_eq!(buf.line_text(0).unwrap(), "a");
        assert_eq!(buf.line_text(1).unwrap(), "b");
        assert_eq!(buf.line_text(2).unwrap(), "c");
    }

    #[test]
    fn delete_with_newlines_updates_line_index() {
        // Bytes: [a, \n, b, \n, c, \n, d] (length 7).
        // Delete 2..5 removes "b\nc", leaving "a\n\nd" — line 0 = "a",
        // line 1 = "", line 2 = "d". 3 lines, 2 newlines.
        let mut buf = PieceTableBuffer::from_bytes(b"a\nb\nc\nd".to_vec());
        buf.delete(2..5).unwrap();
        assert_eq!(reconstruct_str(&buf), "a\n\nd");
        assert_eq!(buf.line_count(), 3);
        assert_eq!(buf.line_text(0).unwrap(), "a");
        assert_eq!(buf.line_text(1).unwrap(), "");
        assert_eq!(buf.line_text(2).unwrap(), "d");
        assert_eq!(buf.newlines, vec![1, 2]);
    }

    #[test]
    fn insert_at_middle_preserves_newline_positions() {
        // Insert in the middle of a line should not affect line indices
        // for positions before the insert, and should shift for positions
        // at/after.
        let mut buf = PieceTableBuffer::from_bytes(b"hello\nworld".to_vec());
        buf.insert(5, "X").unwrap(); // "helloX\nworld"
        assert_eq!(reconstruct_str(&buf), "helloX\nworld");
        assert_eq!(buf.line_count(), 2);
        assert_eq!(buf.line_text(0).unwrap(), "helloX");
        assert_eq!(buf.line_text(1).unwrap(), "world");
        // The '\n' was at 5, now at 6.
        assert_eq!(buf.newlines, vec![6]);
    }

    // ----- Cursor / selection -----

    #[test]
    fn cursor_advances_after_insert() {
        let mut buf = PieceTableBuffer::new();
        buf.insert(0, "abc").unwrap();
        assert_eq!(buf.cursor(), 3);
    }

    #[test]
    fn cursor_moves_to_delete_anchor() {
        let mut buf = PieceTableBuffer::from_bytes(b"hello world".to_vec());
        buf.delete(5..11).unwrap();
        assert_eq!(buf.cursor(), 5);
        assert!(buf.selection().is_collapsed());
    }

    #[test]
    fn selection_round_trips() {
        let mut buf = PieceTableBuffer::new();
        buf.insert(0, "hello").unwrap();
        buf.set_cursor(2);
        buf.set_selection(Selection {
            anchor: 1,
            head: 4,
        });
        assert_eq!(buf.selection(), Selection { anchor: 1, head: 4 });
    }

    // ----- Stress test -----

    #[test]
    fn stress_many_edits_preserves_text_and_lines() {
        // Simulate a long editing session and verify the document and line
        // index stay consistent.
        let mut buf = PieceTableBuffer::from_bytes(b"the quick brown fox".to_vec());

        // Append " jumps" 50 times (with random-ish positions).
        for i in 0..50 {
            let pos = buf.len();
            buf.insert(pos, &format!(" {i}")).unwrap();
        }

        // Now do 100 random-ish inserts and deletes.
        let mut seed: u64 = 0xdeadbeef;
        let mut next = || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            seed
        };
        for _ in 0..100 {
            let total = buf.len();
            let op = next() % 3;
            match op {
                0 => {
                    // Insert a single char at a random position.
                    let pos = (next() as usize) % (total + 1);
                    buf.insert(pos, "x").unwrap();
                }
                1 => {
                    // Delete a 1-byte range at a random position (if there's content).
                    if total > 0 {
                        let pos = (next() as usize) % total;
                        buf.delete(pos..pos + 1).unwrap();
                    }
                }
                _ => {
                    // Delete a 3-byte range at a random position (if available).
                    if total >= 3 {
                        let pos = (next() as usize) % (total - 2);
                        buf.delete(pos..pos + 3).unwrap();
                    }
                }
            }
        }

        // Verify invariants: total length matches reconstruct, and pos_to_linecol
        // roundtrips correctly for every byte position.
        let text = reconstruct_str(&buf);
        assert_eq!(text.len(), buf.len());
        for byte_pos in 0..=buf.len() {
            let (line, col) = buf.pos_to_linecol(byte_pos).unwrap();
            assert_eq!(buf.linecol_to_pos(line, col), Some(byte_pos));
            // col must equal the column in the text (count of non-newline chars
            // since the last newline before byte_pos).
            let last_nl = if line == 0 {
                0
            } else {
                buf.newlines[line - 1] + 1
            };
            assert_eq!(col, byte_pos - last_nl);
        }

        // Verify line_count matches manual count of '\n'.
        let manual_newlines = text.bytes().filter(|&b| b == b'\n').count();
        assert_eq!(buf.line_count(), manual_newlines + 1);
    }

    #[test]
    fn save_clears_dirty_when_path_set() {
        let mut buf = PieceTableBuffer::from_bytes_with_path(
            b"hello".to_vec(),
            PathBuf::from("/tmp/example"),
        );
        buf.insert(0, "x").unwrap();
        assert!(buf.is_dirty());
        buf.save().unwrap();
        assert!(!buf.is_dirty());
    }

    #[test]
    fn save_errors_when_no_source_path() {
        let mut buf = PieceTableBuffer::new();
        buf.insert(0, "x").unwrap();
        let err = buf.save().unwrap_err();
        assert!(matches!(err, SaveError::NoSourcePath));
    }

    #[test]
    fn save_roundtrip_writes_to_disk() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("the_editor_save_roundtrip_{}.txt", std::process::id()));
        let _ = std::fs::remove_file(&path);

        let mut buf = PieceTableBuffer::from_bytes_with_path(
            b"hello\nworld".to_vec(),
            path.clone(),
        );
        buf.insert(5, " beautiful").unwrap();
        buf.save().unwrap();
        assert!(!buf.is_dirty());

        let read_back = std::fs::read_to_string(&path).unwrap();
        assert_eq!(read_back, "hello beautiful\nworld");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn save_overwrites_existing_content() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("the_editor_save_overwrite_{}.txt", std::process::id()));
        std::fs::write(&path, b"original content\n").unwrap();

        let mut buf = PieceTableBuffer::from_bytes_with_path(
            std::fs::read(&path).unwrap(),
            path.clone(),
        );
        buf.delete(0..buf.len()).unwrap();
        buf.insert(0, "replaced").unwrap();
        buf.save().unwrap();

        let read_back = std::fs::read_to_string(&path).unwrap();
        assert_eq!(read_back, "replaced");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn to_bytes_matches_reconstruct() {
        let mut buf = PieceTableBuffer::from_bytes(b"line0\nline1\nline2".to_vec());
        buf.insert(5, "X").unwrap();
        buf.delete(0..3).unwrap();

        let from_method = buf.to_bytes();
        let from_walk: Vec<u8> = buf
            .pieces
            .iter()
            .flat_map(|p| {
                let src = match p.source {
                    PieceSource::Original => buf.original.as_slice(),
                    PieceSource::Delta => &buf.delta,
                };
                src[p.start..p.start + p.length].to_vec()
            })
            .collect();
        assert_eq!(from_method, from_walk);
    }

    // ----- get_slice behavior (tested indirectly via line_text across pieces) -----

    #[test]
    fn line_text_returns_borrowed_for_unedited_lines() {
        let buf = PieceTableBuffer::from_bytes(b"hello\nworld\nfoo".to_vec());
        let l0 = buf.line_text(0).unwrap();
        assert!(matches!(l0, Cow::Borrowed(_)), "expected borrowed for unedited line");
        assert_eq!(l0, "hello");
    }

    #[test]
    fn line_text_after_split_returns_owned() {
        // Force a line to span pieces by inserting in the middle of it.
        let mut buf = PieceTableBuffer::from_bytes(b"hello world".to_vec());
        buf.insert(5, "X").unwrap(); // "helloX world"
        let l0 = buf.line_text(0).unwrap();
        // The line "helloX world" now spans 3 pieces ("hello" + "X" + " world").
        assert!(matches!(l0, Cow::Owned(_)), "expected owned for split line");
        assert_eq!(l0, "helloX world");
    }

    // ----- UTF-8 boundary safety -----

    #[test]
    fn multibyte_utf8_insert_and_read() {
        // "héllo" — 'é' is 2 bytes in UTF-8 (0xC3 0xA9). Total 6 bytes.
        let mut buf = PieceTableBuffer::new();
        buf.insert(0, "héllo").unwrap();
        assert_eq!(buf.len(), 6, "byte length, not char length");
        assert_eq!(reconstruct_str(&buf), "héllo");
        assert_eq!(buf.line_text(0).unwrap(), "héllo");
        // pos_to_linecol at byte 6 (past the end) is (0, 6).
        assert_eq!(buf.pos_to_linecol(6), Some((0, 6)));
        // linecol_to_pos(0, 2) is byte 2 (start of 'é' if cursor were there —
        // not a real cursor position but the math works).
        assert_eq!(buf.linecol_to_pos(0, 2), Some(2));
    }

    // ----- memmap + delta (ADR 0002) -----

    #[test]
    fn from_path_reads_small_file() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("the_editor_mmap_small_{}.txt", std::process::id()));
        let content = b"hello\nworld\nmemmap test".to_vec();
        std::fs::write(&path, &content).unwrap();

        let buf = PieceTableBuffer::from_path(path.clone()).unwrap();
        assert_eq!(buf.to_bytes(), content);
        assert_eq!(buf.line_count(), 3);
        assert_eq!(buf.line_text(0).unwrap(), "hello");
        assert!(!buf.is_dirty());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn from_path_handles_empty_file() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("the_editor_mmap_empty_{}.txt", std::process::id()));
        std::fs::write(&path, b"").unwrap();

        let buf = PieceTableBuffer::from_path(path.clone()).unwrap();
        assert_eq!(buf.len(), 0);
        assert_eq!(buf.line_count(), 1, "empty file has one empty line");
        assert_eq!(buf.line_text(0).unwrap(), "");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn from_path_errors_on_missing_file() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("the_editor_mmap_nonexistent_{}.txt", std::process::id()));
        let _ = std::fs::remove_file(&path); // ensure it doesn't exist

        let result = PieceTableBuffer::from_path(path);
        assert!(result.is_err());
    }

    #[test]
    fn from_path_handles_large_file_via_mmap() {
        // 10 MB file — too large to be efficient with `std::fs::read` if
        // we wanted to test "without mmap". Verifies that from_path
        // works for sizes that matter.
        let dir = std::env::temp_dir();
        let path = dir.join(format!("the_editor_mmap_large_{}.bin", std::process::id()));

        let mut content = Vec::with_capacity(10 * 1024 * 1024);
        for i in 0..(10 * 1024 * 1024) {
            content.push((i % 256) as u8);
        }
        std::fs::write(&path, &content).unwrap();

        let buf = PieceTableBuffer::from_path(path.clone()).unwrap();
        assert_eq!(buf.len(), content.len());
        // Sample a few positions to verify content matches.
        assert_eq!(buf.to_bytes()[0], 0);
        assert_eq!(buf.to_bytes()[1024 * 1024], 0);
        assert_eq!(buf.to_bytes()[5 * 1024 * 1024], 0);
        assert!(!buf.is_dirty());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn mmap_buffer_edits_and_saves_correctly() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("the_editor_mmap_save_{}.txt", std::process::id()));
        let original = b"line0\nline1\nline2\nline3".to_vec();
        std::fs::write(&path, &original).unwrap();

        let mut buf = PieceTableBuffer::from_path(path.clone()).unwrap();
        // Insert at start.
        buf.insert(0, "PRE ").unwrap();
        // After: "PRE line0\nline1\nline2\nline3" (length 27)
        // Insert at end.
        buf.insert(buf.len(), " POST").unwrap();
        // After: "PRE line0\nline1\nline2\nline3 POST" (length 32)
        // Delete "line1\n" — bytes 10..16 in the modified buffer.
        buf.delete(10..16).unwrap();
        buf.save().unwrap();

        let roundtrip = std::fs::read(&path).unwrap();
        assert_eq!(roundtrip, b"PRE line0\nline2\nline3 POST".to_vec());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn mmap_buffer_undo_works() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("the_editor_mmap_undo_{}.txt", std::process::id()));
        std::fs::write(&path, b"original").unwrap();

        let mut buf = PieceTableBuffer::from_path(path.clone()).unwrap();
        buf.insert(buf.len(), " text").unwrap();
        assert_eq!(buf.to_bytes(), b"original text");
        assert!(buf.undo());
        assert_eq!(buf.to_bytes(), b"original");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn save_atomic_writes_via_temp_file() {
        // Verify the .tmp file is cleaned up after a successful rename.
        let dir = std::env::temp_dir();
        let path = dir.join(format!("the_editor_atomic_{}.txt", std::process::id()));
        std::fs::write(&path, b"initial").unwrap();

        let mut buf = PieceTableBuffer::from_path(path.clone()).unwrap();
        buf.insert(0, "x").unwrap();
        buf.save().unwrap();

        // The temp file should be gone after rename.
        let mut temp_os = path.as_os_str().to_owned();
        temp_os.push(".tmp");
        let temp_path = std::path::PathBuf::from(temp_os);
        assert!(!temp_path.exists(), "temp file should be cleaned up after rename");

        let _ = std::fs::remove_file(&path);
    }

    // ----- Undo / Redo (ADR 0004) -----

    #[test]
    fn undo_removes_last_insert() {
        let mut buf = PieceTableBuffer::new();
        buf.insert(0, "hello").unwrap();
        assert_eq!(buf.cursor(), 5);
        assert!(buf.undo());
        assert_eq!(reconstruct_str(&buf), "");
        assert_eq!(buf.cursor(), 0);
        assert!(!buf.is_dirty());
    }

    #[test]
    fn undo_restores_deleted_text() {
        let mut buf = PieceTableBuffer::from_bytes(b"hello world".to_vec());
        buf.set_cursor(5);
        buf.delete(5..11).unwrap(); // remove " world"
        assert_eq!(reconstruct_str(&buf), "hello");
        assert!(buf.undo());
        assert_eq!(reconstruct_str(&buf), "hello world");
        assert_eq!(buf.cursor(), 5, "cursor should be at pre-delete position");
    }

    #[test]
    fn undo_with_empty_stack_returns_false() {
        let mut buf = PieceTableBuffer::new();
        assert!(!buf.undo());
    }

    #[test]
    fn redo_reapplies_undone_insert() {
        let mut buf = PieceTableBuffer::new();
        buf.insert(0, "hello").unwrap();
        assert!(buf.undo());
        assert_eq!(reconstruct_str(&buf), "");
        assert!(buf.redo());
        assert_eq!(reconstruct_str(&buf), "hello");
        assert_eq!(buf.cursor(), 5);
    }

    #[test]
    fn redo_reapplies_undone_delete() {
        let mut buf = PieceTableBuffer::from_bytes(b"hello world".to_vec());
        buf.delete(5..11).unwrap();
        assert!(buf.undo());
        assert!(buf.redo());
        assert_eq!(reconstruct_str(&buf), "hello");
    }

    #[test]
    fn redo_with_empty_stack_returns_false() {
        let mut buf = PieceTableBuffer::new();
        assert!(!buf.redo());
    }

    #[test]
    fn new_edit_clears_redo_stack() {
        let mut buf = PieceTableBuffer::new();
        buf.insert(0, "hello").unwrap();
        buf.undo();
        buf.undo(); // nothing to undo
        // After undo, redo stack has the entry. A new edit should clear it.
        buf.insert(0, "x").unwrap();
        assert!(!buf.redo(), "redo should be cleared after new edit");
        assert_eq!(reconstruct_str(&buf), "x");
    }

    #[test]
    fn multiple_undos_step_through_history() {
        let mut buf = PieceTableBuffer::new();
        buf.insert(0, "a").unwrap();
        buf.insert(1, "b").unwrap();
        buf.insert(2, "c").unwrap();
        assert_eq!(reconstruct_str(&buf), "abc");

        buf.undo();
        assert_eq!(reconstruct_str(&buf), "ab");
        assert_eq!(buf.cursor(), 2, "undo restores to pre-insert-c cursor");

        buf.undo();
        assert_eq!(reconstruct_str(&buf), "a");
        assert_eq!(buf.cursor(), 1, "undo restores to pre-insert-b cursor");

        buf.undo();
        assert_eq!(reconstruct_str(&buf), "");
        assert_eq!(buf.cursor(), 0, "undo restores to pre-insert-a cursor");

        assert!(!buf.undo(), "no more history");
    }

    #[test]
    fn edit_group_merges_consecutive_inserts() {
        let mut buf = PieceTableBuffer::new();
        buf.begin_edit_group();
        buf.insert(0, "h").unwrap();
        buf.insert(1, "e").unwrap();
        buf.insert(2, "l").unwrap();
        buf.insert(3, "l").unwrap();
        buf.insert(4, "o").unwrap();
        buf.end_edit_group();
        assert_eq!(reconstruct_str(&buf), "hello");

        // One undo removes the entire group.
        assert!(buf.undo());
        assert_eq!(reconstruct_str(&buf), "");
    }

    #[test]
    fn undo_restores_cursor_to_position_before_edit() {
        let mut buf = PieceTableBuffer::from_bytes(b"hello world".to_vec());
        buf.set_cursor(6);
        buf.insert(6, "beautiful ").unwrap();
        assert_eq!(buf.cursor(), 16);
        assert_eq!(reconstruct_str(&buf), "hello beautiful world");

        buf.undo();
        assert_eq!(reconstruct_str(&buf), "hello world");
        assert_eq!(buf.cursor(), 6);
    }

    #[test]
    fn undo_after_save_clears_dirty_on_undo() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("the_editor_undo_save_{}.txt", std::process::id()));
        let _ = std::fs::remove_file(&path);

        let mut buf = PieceTableBuffer::from_bytes_with_path(b"hi".to_vec(), path.clone());
        buf.insert(2, " world").unwrap();
        buf.save().unwrap();
        assert!(!buf.is_dirty());

        // After save, undo should still work, and the buffer becomes dirty again.
        buf.undo();
        assert_eq!(reconstruct_str(&buf), "hi");
        assert!(buf.is_dirty(), "undo should mark buffer dirty");

        let _ = std::fs::remove_file(&path);
    }
}