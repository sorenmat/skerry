//! Pure helpers that frontends use to render the editor.
//!
//! These live in `core` so both the TUI and the GUI render the same way
//! from the same data. The helpers are stateless and take references to
//! the data they need — they don't own any state themselves.

use std::ops::Range;

use crate::Buffer;

/// Compute the byte range of `selection` that falls within `line`
/// (the line's own byte range). Returns `None` if they don't overlap.
///
/// All ranges are byte offsets in the document. The returned range uses
/// absolute document byte offsets; convert to per-line offsets by
/// subtracting `line.start` if needed.
///
/// ```text
/// line:    [────────────)
/// selection:        [──────────)
/// overlap:           [────)
/// ```
pub fn selection_in_line(line: Range<usize>, selection: Range<usize>) -> Option<Range<usize>> {
    let start = selection.start.max(line.start);
    let end = selection.end.min(line.end);
    if start < end {
        Some(start..end)
    } else {
        None
    }
}

/// Convert a byte offset within a line into a character column.
///
/// Byte offsets count UTF-8 bytes; char columns count Unicode scalar
/// values. For ASCII line content these are the same; for multibyte
/// content (e.g. emoji, CJK), they differ. Cursor positioning needs char
/// columns for visual correctness.
///
/// `byte_col` is clamped to the line length and snapped to the nearest
/// valid UTF-8 character boundary so callers never panic on a misaligned
/// offset.
pub fn byte_to_char_col(line_text: &str, byte_col: usize) -> usize {
    let len = line_text.len();
    if len == 0 {
        return 0;
    }
    let mut clamped = byte_col.min(len);
    while clamped > 0 && !line_text.is_char_boundary(clamped) {
        clamped -= 1;
    }
    line_text[..clamped].chars().count()
}

/// Convert a character column within a line into a byte offset.
///
/// Inverse of [`byte_to_char_col`]. Used by mouse-click handlers that
/// compute a char column from a pixel position and need the byte offset
/// to drive the core API. `char_col` is clamped to the line's char
/// length; out-of-range values land on the line boundary.
pub fn char_col_to_byte_col(line_text: &str, char_col: usize) -> usize {
    line_text
        .char_indices()
        .nth(char_col)
        .map(|(i, _)| i)
        .unwrap_or(line_text.len())
}

/// Convert a buffer byte position into `(line, character_column)`.
pub fn cursor_char_linecol(buffer: &dyn Buffer, pos: usize) -> (usize, usize) {
    let (line, byte_col) = buffer.pos_to_linecol(pos).unwrap_or((0, 0));
    let Some(line_text) = buffer.line_text(line) else {
        return (line, byte_col);
    };
    (line, byte_to_char_col(line_text.as_ref(), byte_col))
}

/// Resolve `(line, char_col)` to a byte position, clamping the
/// character column to the target line's actual visual length.
pub fn clamped_line_charcol_to_pos(buffer: &dyn Buffer, line: usize, char_col: usize) -> usize {
    let Some(range) = buffer.line_byte_range(line) else {
        return buffer.len();
    };
    let Some(line_text) = buffer.line_text(line) else {
        return range.end;
    };
    let byte_col = char_col_to_byte_col(line_text.as_ref(), char_col);
    buffer.linecol_to_pos(line, byte_col).unwrap_or(range.end)
}

/// Move `pos` left by one Unicode scalar value without landing inside
/// a multi-byte UTF-8 character.
pub fn move_left_by_char(buffer: &dyn Buffer, pos: usize) -> usize {
    if pos == 0 {
        return 0;
    }
    let (line, _) = buffer.pos_to_linecol(pos).unwrap_or((0, 0));
    let Some(range) = buffer.line_byte_range(line) else {
        return pos.saturating_sub(1);
    };
    let Some(line_text) = buffer.line_text(line) else {
        return pos.saturating_sub(1);
    };
    let rel = pos.saturating_sub(range.start).min(range.end - range.start);
    if rel == 0 {
        return pos.saturating_sub(1);
    }

    let mut target = 0;
    for (idx, _) in line_text.char_indices() {
        if idx >= rel {
            break;
        }
        target = idx;
    }
    range.start + target
}

/// Move `pos` right by one Unicode scalar value without landing inside
/// a multi-byte UTF-8 character.
pub fn move_right_by_char(buffer: &dyn Buffer, pos: usize) -> usize {
    let len = buffer.len();
    if pos >= len {
        return len;
    }
    let (line, _) = buffer.pos_to_linecol(pos).unwrap_or((0, 0));
    let Some(range) = buffer.line_byte_range(line) else {
        return (pos + 1).min(len);
    };
    let Some(line_text) = buffer.line_text(line) else {
        return (pos + 1).min(len);
    };
    let rel = pos.saturating_sub(range.start).min(range.end - range.start);
    for (idx, _) in line_text.char_indices() {
        if idx > rel {
            return range.start + idx;
        }
    }
    if rel < line_text.len() {
        range.end
    } else {
        (pos + 1).min(len)
    }
}

/// Format a "L{line}:{col} / L{total_lines}" status indicator.
/// `col` is a 1-indexed character column; pass `0` to show column 1
/// (the convention in most editors).
pub fn format_position(line: usize, col: usize, total_lines: usize) -> String {
    let col_disp = col + 1; // 0-indexed col → 1-indexed display
    format!("L{}:{} / L{}", line + 1, col_disp, total_lines)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Buffer, PieceTableBuffer};

    #[test]
    fn selection_inside_line() {
        let line = 0..10;
        let sel = 3..7;
        assert_eq!(selection_in_line(line, sel), Some(3..7));
    }

    #[test]
    fn selection_starts_before_line() {
        let line = 5..15;
        let sel = 0..8;
        assert_eq!(selection_in_line(line, sel), Some(5..8));
    }

    #[test]
    fn selection_ends_after_line() {
        let line = 5..15;
        let sel = 10..20;
        assert_eq!(selection_in_line(line, sel), Some(10..15));
    }

    #[test]
    fn selection_spans_line() {
        let line = 5..15;
        let sel = 0..20;
        assert_eq!(selection_in_line(line, sel), Some(5..15));
    }

    #[test]
    fn selection_no_overlap_before() {
        let line = 10..20;
        let sel = 0..5;
        assert_eq!(selection_in_line(line, sel), None);
    }

    #[test]
    fn selection_no_overlap_after() {
        let line = 10..20;
        let sel = 25..30;
        assert_eq!(selection_in_line(line, sel), None);
    }

    #[test]
    fn selection_touches_line_boundary() {
        // Selection ends exactly at line.start — no overlap (start == end).
        let line = 5..15;
        let sel = 0..5;
        assert_eq!(selection_in_line(line, sel), None);
    }

    #[test]
    fn byte_to_char_ascii() {
        assert_eq!(byte_to_char_col("hello", 0), 0);
        assert_eq!(byte_to_char_col("hello", 3), 3);
        assert_eq!(byte_to_char_col("hello", 5), 5);
    }

    #[test]
    fn byte_to_char_multibyte() {
        // "héllo" — 'é' is 2 bytes (0xC3 0xA9). Total 6 bytes.
        // Char columns: h=0, é=1, l=2, l=3, o=4
        let s = "héllo";
        assert_eq!(byte_to_char_col(s, 0), 0, "before 'h'");
        assert_eq!(byte_to_char_col(s, 1), 1, "before 'é' (1 byte into 'é')");
        assert_eq!(byte_to_char_col(s, 3), 2, "after 'é' (3 bytes = 2 chars)");
        assert_eq!(byte_to_char_col(s, 6), 5, "past end");
    }

    #[test]
    fn byte_to_char_clamps_to_len() {
        let s = "abc";
        // Out-of-bounds byte cols clamp to the actual length.
        assert_eq!(byte_to_char_col(s, 100), 3);
    }

    #[test]
    fn char_col_to_byte_round_trip() {
        for s in ["hello", "héllo", "🦀rust", "abc\ndef"] {
            // Only iterate over valid UTF-8 byte boundaries — mid-multi-byte
            // positions aren't valid input (callers should never pass them).
            let valid_bytes: Vec<usize> = s
                .char_indices()
                .map(|(i, _)| i)
                .chain(std::iter::once(s.len()))
                .collect();
            for byte_col in valid_bytes {
                let char_col = byte_to_char_col(s, byte_col);
                let back = char_col_to_byte_col(s, char_col);
                assert_eq!(back, byte_col, "roundtrip failed for {s:?} at {byte_col}");
            }
        }
    }

    #[test]
    fn char_col_to_byte_out_of_range() {
        let s = "abc";
        // Out-of-range char col clamps to the byte length.
        assert_eq!(char_col_to_byte_col(s, 100), 3);
    }

    #[test]
    fn char_col_to_byte_multibyte() {
        let s = "héllo"; // h=0, é=1, l=2, l=3, o=4 (chars); 0, 1, 3, 4, 5 (bytes); len 6
        assert_eq!(char_col_to_byte_col(s, 0), 0, "before 'h'");
        assert_eq!(char_col_to_byte_col(s, 1), 1, "start of 'é'");
        assert_eq!(
            char_col_to_byte_col(s, 2),
            3,
            "after 'é' (2 chars = 3 bytes)"
        );
        assert_eq!(char_col_to_byte_col(s, 4), 5, "after 'o'");
        assert_eq!(char_col_to_byte_col(s, 5), 6, "end of string");
    }

    #[test]
    fn move_left_and_right_by_char_skip_multibyte_characters() {
        let buffer = PieceTableBuffer::from_bytes("héllo".as_bytes().to_vec());
        assert_eq!(move_left_by_char(&buffer, 3), 1);
        assert_eq!(move_left_by_char(&buffer, 1), 0);
        assert_eq!(move_right_by_char(&buffer, 0), 1);
        assert_eq!(move_right_by_char(&buffer, 1), 3);
    }

    #[test]
    fn clamped_line_charcol_to_pos_uses_visual_column() {
        let buffer = PieceTableBuffer::from_bytes("ab\néx".as_bytes().to_vec());
        assert_eq!(cursor_char_linecol(&buffer, 2), (0, 2));
        assert_eq!(clamped_line_charcol_to_pos(&buffer, 1, 2), 6);
        assert_eq!(buffer.pos_to_linecol(6), Some((1, 3)));
    }

    #[test]
    fn format_position_basic() {
        assert_eq!(format_position(0, 0, 10), "L1:1 / L10");
        assert_eq!(format_position(41, 4, 100), "L42:5 / L100");
    }
}
