//! Pure helpers that frontends use to render the editor.
//!
//! These live in `core` so both the TUI and the GUI render the same way
//! from the same data. The helpers are stateless and take references to
//! the data they need — they don't own any state themselves.

use std::ops::Range;

use crate::{Buffer, BytePos, Selection};

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

/// Convert a visual column within a line into a byte offset.
///
/// This is the variant used by mouse/pixel click handlers: each character
/// occupies one visual column except tab characters, which occupy
/// `tab_width` columns. The returned byte offset is the start of the
/// character whose visual span contains `visual_col`, so clicking inside a
/// tab lands at the tab character, and clicking past the last character
/// lands at the end of the line.
pub fn visual_col_to_byte_col(line_text: &str, visual_col: usize, tab_width: usize) -> usize {
    let tab_width = tab_width.max(1);
    let mut current = 0usize;
    for (byte, ch) in line_text.char_indices() {
        let width = if ch == '\t' { tab_width } else { 1 };
        if visual_col < current + width {
            return byte;
        }
        current += width;
    }
    line_text.len()
}

/// Return the total visual width of `line_text` in columns, treating tabs
/// as `tab_width` columns and every other character as one.
pub fn visual_line_width(line_text: &str, tab_width: usize) -> usize {
    let tab_width = tab_width.max(1);
    line_text
        .chars()
        .map(|ch| if ch == '\t' { tab_width } else { 1 })
        .sum()
}

/// Compute the indentation string to insert after a newline on `line` of
/// `buffer`. Copies the current line's leading whitespace; adds one extra
/// indent level if the trimmed line ends with `{`, `(`, `[`, or `=>`.
///
/// `use_spaces` and `tab_width` match the document's indent mode (same
/// values `InsertTab` uses). Returns the indent as an owned String (no
/// leading newline — the caller inserts "\n" + this).
pub fn auto_indent(buffer: &dyn Buffer, line: usize, use_spaces: bool, tab_width: usize) -> String {
    let line_text = buffer
        .line_text(line)
        .map(|c| c.into_owned())
        .unwrap_or_default();

    // Leading whitespace of the current line (spaces + tabs).
    let leading: String = line_text
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect();

    // Does the line (ignoring trailing whitespace) end with a token that
    // opens a new block / continuation?
    let trimmed = line_text.trim_end();
    let extra_indent = trimmed.ends_with('{')
        || trimmed.ends_with('(')
        || trimmed.ends_with('[')
        || trimmed.ends_with("=>");

    let one_level = if use_spaces {
        " ".repeat(tab_width.max(1))
    } else {
        "\t".to_string()
    };

    if extra_indent {
        format!("{leading}{one_level}")
    } else {
        leading
    }
}

/// If `ch` is an opener bracket or quote, return its matching closer.
/// Otherwise return `None`. Used by auto-pairing.
pub fn matching_close(ch: char) -> Option<char> {
    match ch {
        '(' => Some(')'),
        '[' => Some(']'),
        '{' => Some('}'),
        '"' => Some('"'),
        '\'' => Some('\''),
        _ => None,
    }
}

/// Given a cursor position, find the matching bracket if the cursor is
/// adjacent to a bracket character. Checks the character at `pos` first,
/// then the character before `pos` (the common "cursor after bracket"
/// case). Returns `(bracket_pos, match_pos)` — the positions of both
/// brackets. Handles nesting (counts open/close depth).
///
/// Only matches `()[]{}` (not quotes — those are ambiguous).
pub fn matching_bracket(buffer: &dyn Buffer, pos: BytePos) -> Option<(BytePos, BytePos)> {
    // Try the char at `pos`, then the char before `pos`.
    let ch_after = char_at(buffer, pos);
    let ch_before = if pos > 0 {
        char_at(buffer, pos.saturating_sub(1))
    } else {
        None
    };
    // Prefer the char before the cursor (cursor sits after the bracket).
    if let Some(ch) = ch_before {
        if let Some(pair) = find_match(buffer, pos.saturating_sub(1), ch) {
            return Some(pair);
        }
    }
    if let Some(ch) = ch_after {
        if let Some(pair) = find_match(buffer, pos, ch) {
            return Some(pair);
        }
    }
    None
}

/// Read a single character at a byte position in the buffer.
fn char_at(buffer: &dyn Buffer, pos: BytePos) -> Option<char> {
    if pos >= buffer.len() {
        return None;
    }
    let end = move_right_by_char(buffer, pos);
    if end <= pos {
        return None;
    }
    buffer.slice(pos..end).and_then(|s| s.chars().next())
}

/// Find the matching bracket for `ch` at `start` position, scanning
/// forward for openers and backward for closers. Handles nesting.
fn find_match(buffer: &dyn Buffer, start: BytePos, ch: char) -> Option<(BytePos, BytePos)> {
    match ch {
        '(' | '[' | '{' => {
            let close = matching_close(ch)?;
            // Scan forward, counting depth.
            let mut depth = 0i32;
            let mut p = start;
            let len = buffer.len();
            while p < len {
                let c = char_at(buffer, p)?;
                let step = move_right_by_char(buffer, p);
                if c == ch {
                    depth += 1;
                } else if c == close {
                    depth -= 1;
                    if depth == 0 {
                        return Some((start, p));
                    }
                }
                p = step;
            }
            None
        }
        ')' | ']' | '}' => {
            let open = matching_open(ch)?;
            // Scan backward, counting depth.
            let mut depth = 0i32;
            let mut p = start;
            while p > 0 {
                p = move_left_by_char(buffer, p);
                let c = char_at(buffer, p)?;
                if c == ch {
                    depth += 1;
                } else if c == open {
                    depth -= 1;
                    if depth == 0 {
                        return Some((start, p));
                    }
                }
            }
            None
        }
        _ => None,
    }
}

/// If `ch` is a closer bracket or quote, return its matching opener.
/// Otherwise return `None`. Used by the backspace-deletes-pair check.
pub fn matching_open(ch: char) -> Option<char> {
    match ch {
        ')' => Some('('),
        ']' => Some('['),
        '}' => Some('{'),
        '"' => Some('"'),
        '\'' => Some('\''),
        _ => None,
    }
}

/// What to do when the user types `ch` at a collapsed cursor. Returned by
/// [`auto_pair_action`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoPairAction {
    /// Insert `ch` together with the given closer, cursor between them.
    Pair(char),
    /// The identical char is already after the cursor and the context
    /// reads as closing — move past it without inserting anything.
    SkipOver,
    /// Insert `ch` on its own, with no pairing.
    Plain,
}

/// Decide how a typed character should be inserted with respect to
/// auto-pairing. Brackets always pair (and closers always skip over an
/// identical following char). Quotes are ambiguous — the same character
/// opens and closes — so they use word-context heuristics:
///
/// - skip over a following identical quote only when the char before the
///   cursor suggests closing intent (a word char or the same quote);
/// - never pair directly before or after a word char, so typing `'` in
///   `don|t` or `"` around existing text inserts a single quote;
/// - typing a quote right before an existing quote (opening intent)
///   inserts a single quote rather than swallowing the keystroke.
pub fn auto_pair_action(buffer: &dyn Buffer, pos: usize, ch: char) -> AutoPairAction {
    let is_quote = ch == '"' || ch == '\'';
    let before = char_before(buffer, pos);
    let after = char_after(buffer, pos);

    if matching_open(ch).is_some() && after == Some(ch) {
        if is_quote {
            let closing_intent = before == Some(ch) || before.is_some_and(|c| c.is_alphanumeric());
            return if closing_intent {
                AutoPairAction::SkipOver
            } else {
                AutoPairAction::Plain
            };
        }
        return AutoPairAction::SkipOver;
    }
    if let Some(close) = matching_close(ch) {
        if is_quote {
            let next_to_word = [before, after]
                .into_iter()
                .flatten()
                .any(|c| c.is_alphanumeric());
            if next_to_word || after == Some(ch) {
                return AutoPairAction::Plain;
            }
        }
        return AutoPairAction::Pair(close);
    }
    AutoPairAction::Plain
}

/// The character immediately before the cursor, if any. Handles UTF-8
/// boundaries by walking left to the previous char start.
pub fn char_before(buffer: &dyn Buffer, pos: usize) -> Option<char> {
    if pos == 0 {
        return None;
    }
    let prev = move_left_by_char(buffer, pos);
    if prev >= pos {
        return None;
    }
    buffer.slice(prev..pos).and_then(|s| s.chars().next())
}

/// The character immediately after the cursor, if any.
pub fn char_after(buffer: &dyn Buffer, pos: usize) -> Option<char> {
    if pos >= buffer.len() {
        return None;
    }
    let next = move_right_by_char(buffer, pos);
    if next <= pos {
        return None;
    }
    buffer.slice(pos..next).and_then(|s| s.chars().next())
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

/// Build a per-line selection list for a rectangular (column) selection.
/// Each line in `[from_line, to_line]` gets one `Selection` spanning
/// `[col_lo, col_hi)` in character columns. Handles varying line lengths
/// (columns past the end are clamped to the line's last char) and tabs
/// (columns are character-based, not byte-based).
pub fn column_selections(
    buffer: &dyn Buffer,
    from_line: usize,
    from_col: usize,
    to_line: usize,
    to_col: usize,
) -> Vec<Selection> {
    let (lo_line, hi_line) = (from_line.min(to_line), from_line.max(to_line));
    let (lo_col, hi_col) = (from_col.min(to_col), from_col.max(to_col));
    let mut sels = Vec::new();
    for line in lo_line..=hi_line {
        let Some(text) = buffer.line_text(line) else {
            continue;
        };
        let line_chars = text.chars().count();
        if lo_col >= line_chars {
            // Column is past this line's end — place a collapsed cursor.
            if let Some(pos) = buffer.linecol_to_pos(line, line_chars) {
                sels.push(Selection::collapsed(pos));
            }
            continue;
        }
        let actual_hi = hi_col.min(line_chars);
        let start_byte = buffer
            .linecol_to_pos(line, lo_col)
            .unwrap_or_else(|| buffer.line_byte_range(line).map(|r| r.start).unwrap_or(0));
        let end_byte = buffer
            .linecol_to_pos(line, actual_hi)
            .unwrap_or_else(|| buffer.line_byte_range(line).map(|r| r.end).unwrap_or(0));
        sels.push(Selection {
            anchor: start_byte,
            head: end_byte,
        });
    }
    sels.sort_by_key(|s| s.anchor);
    sels
}

/// Build one selection per occurrence of `needle` in `text`, for the
/// select-all-occurrences (multi-cursor) command. When `whole_word` is
/// set, matches that sit inside a larger word are skipped — callers use
/// this when the needle was derived from the word under the cursor
/// (VS Code semantics: word-derived needles match whole words only,
/// explicit selections match the exact string).
///
/// Capped at [`crate::search::MAX_STORED_MATCHES`] so a pathological
/// needle (e.g. selecting one space in an indented file) can't build a
/// million-selection list.
pub fn all_occurrence_selections(text: &str, needle: &str, whole_word: bool) -> Vec<Selection> {
    if needle.is_empty() {
        return Vec::new();
    }
    let mut sels = Vec::new();
    for start in memchr::memmem::find_iter(text.as_bytes(), needle.as_bytes()) {
        if sels.len() >= crate::search::MAX_STORED_MATCHES {
            break;
        }
        let end = start + needle.len();
        if whole_word && !word_boundary(text.as_bytes(), start, end) {
            continue;
        }
        sels.push(Selection {
            anchor: start,
            head: end,
        });
    }
    sels
}

/// True when `[start, end)` in `bytes` is not immediately preceded or
/// followed by a word character.
fn word_boundary(bytes: &[u8], start: usize, end: usize) -> bool {
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80;
    (start == 0 || !is_word(bytes[start - 1])) && (end == bytes.len() || !is_word(bytes[end]))
}

/// The line-comment prefix for a language ID, if known.
/// Returns `None` for languages without a line comment syntax (JSON).
pub fn line_comment_prefix(language_id: &str) -> Option<&'static str> {
    match language_id {
        "rust" | "c" | "cpp" => Some("// "),
        "go" | "javascript" | "javascriptreact" | "typescript" | "typescriptreact" => Some("// "),
        "python" | "toml" => Some("# "),
        _ => None,
    }
}

/// Compute the set of edits to toggle line comments on the given line
/// range. Returns `Vec<(byte_pos, text_to_insert, range_to_delete)>`
/// where each entry is: delete `range_to_delete` and insert `text_to_insert`
/// at `byte_pos`.
///
/// If all lines in the range are already commented, the comments are
/// removed. If any line is uncommented, comments are added to all.
/// Blank lines are skipped when checking but are commented when adding.
pub fn compute_comment_toggles(
    buffer: &dyn Buffer,
    start_line: usize,
    end_line: usize,
    prefix: &str,
) -> Vec<(BytePos, String, Range<BytePos>)> {
    let mut edits = Vec::new();

    // First pass: are all non-empty lines already commented?
    let mut all_commented = true;
    for line in start_line..end_line {
        let Some(text) = buffer.line_text(line) else {
            continue;
        };
        let trimmed = text.trim_start();
        if trimmed.is_empty() {
            continue; // skip blank lines
        }
        if !trimmed.starts_with(prefix.trim_end()) {
            all_commented = false;
            break;
        }
    }

    if all_commented {
        // Remove comments from all lines that have them.
        for line in start_line..end_line {
            let Some(text) = buffer.line_text(line) else {
                continue;
            };
            let Some(range) = buffer.line_byte_range(line) else {
                continue;
            };
            let trimmed_start = text
                .char_indices()
                .take_while(|(_, c)| *c == ' ' || *c == '\t')
                .last()
                .map(|(i, _)| i)
                .unwrap_or(0);
            let after_ws = &text[trimmed_start..];
            if after_ws.starts_with(prefix.trim_end()) {
                let remove_start = range.start + trimmed_start;
                let remove_len = prefix.trim_end().len();
                edits.push((
                    remove_start,
                    String::new(),
                    remove_start..remove_start + remove_len,
                ));
            }
        }
    } else {
        // Add comments to all non-empty lines.
        for line in start_line..end_line {
            let Some(text) = buffer.line_text(line) else {
                continue;
            };
            let Some(range) = buffer.line_byte_range(line) else {
                continue;
            };
            if text.trim().is_empty() {
                continue; // don't comment blank lines
            }
            // Insert at the first non-whitespace position.
            let insert_pos = range.start
                + text
                    .char_indices()
                    .take_while(|(_, c)| *c == ' ' || *c == '\t')
                    .last()
                    .map(|(i, _)| i + 1)
                    .unwrap_or(0);
            edits.push((insert_pos, prefix.to_string(), insert_pos..insert_pos));
        }
    }

    edits
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
    fn matching_close_for_openers() {
        assert_eq!(matching_close('('), Some(')'));
        assert_eq!(matching_close('['), Some(']'));
        assert_eq!(matching_close('{'), Some('}'));
        assert_eq!(matching_close('"'), Some('"'));
        assert_eq!(matching_close('\''), Some('\''));
        assert_eq!(matching_close('x'), None);
    }

    #[test]
    fn matching_open_for_closers() {
        assert_eq!(matching_open(')'), Some('('));
        assert_eq!(matching_open('}'), Some('{'));
        assert_eq!(matching_open('x'), None);
    }

    #[test]
    fn char_before_and_after_cursor() {
        let buffer = PieceTableBuffer::from_bytes("hello".as_bytes().to_vec());
        assert_eq!(char_before(&buffer, 0), None);
        assert_eq!(char_before(&buffer, 1), Some('h'));
        assert_eq!(char_after(&buffer, 0), Some('h'));
        assert_eq!(char_after(&buffer, 5), None); // at end
    }

    #[test]
    fn char_before_after_multibyte() {
        // "éx" — é is 2 bytes. Cursor at byte 2 (after é).
        let buffer = PieceTableBuffer::from_bytes("éx".as_bytes().to_vec());
        assert_eq!(char_before(&buffer, 2), Some('é'));
        assert_eq!(char_after(&buffer, 2), Some('x'));
    }

    #[test]
    fn auto_indent_copies_leading_whitespace() {
        let buffer = PieceTableBuffer::from_bytes("    let x = 1;".as_bytes().to_vec());
        let indent = auto_indent(&buffer, 0, true, 4);
        assert_eq!(indent, "    ");
    }

    #[test]
    fn auto_indent_adds_level_after_open_brace() {
        let buffer = PieceTableBuffer::from_bytes("fn main() {".as_bytes().to_vec());
        let indent = auto_indent(&buffer, 0, true, 4);
        assert_eq!(
            indent, "    ",
            "no leading WS + one indent level = 4 spaces"
        );
    }

    #[test]
    fn auto_indent_adds_level_after_arrow() {
        let buffer = PieceTableBuffer::from_bytes("    let f = |x| =>".as_bytes().to_vec());
        let indent = auto_indent(&buffer, 0, true, 2);
        assert_eq!(indent, "      ", "copies 4 + adds 2");
    }

    #[test]
    fn auto_indent_no_extra_for_plain_line() {
        let buffer = PieceTableBuffer::from_bytes("\tlet x = 1;".as_bytes().to_vec());
        let indent = auto_indent(&buffer, 0, false, 4);
        assert_eq!(indent, "\t");
    }

    #[test]
    fn format_position_basic() {
        assert_eq!(format_position(0, 0, 10), "L1:1 / L10");
        assert_eq!(format_position(41, 4, 100), "L42:5 / L100");
    }

    #[test]
    fn visual_col_to_byte_col_accounts_for_tabs() {
        let s = "\tfoo";
        // Tab occupies columns 0..4, then 'f' at 4, 'o' at 5, 'o' at 6.
        assert_eq!(visual_col_to_byte_col(s, 0, 4), 0, "start of tab");
        assert_eq!(visual_col_to_byte_col(s, 3, 4), 0, "inside tab");
        assert_eq!(visual_col_to_byte_col(s, 4, 4), 1, "start of 'f'");
        assert_eq!(visual_col_to_byte_col(s, 6, 4), 3, "start of last 'o'");
        assert_eq!(visual_col_to_byte_col(s, 100, 4), 4, "past end clamps");
    }

    #[test]
    fn visual_col_to_byte_col_no_tabs() {
        let s = "héllo";
        assert_eq!(visual_col_to_byte_col(s, 0, 4), 0);
        assert_eq!(visual_col_to_byte_col(s, 1, 4), 1);
        assert_eq!(visual_col_to_byte_col(s, 4, 4), 5);
        assert_eq!(visual_col_to_byte_col(s, 5, 4), 6);
        assert_eq!(visual_col_to_byte_col(s, 100, 4), 6);
    }

    #[test]
    fn visual_line_width_counts_tabs() {
        assert_eq!(visual_line_width("abc", 4), 3);
        assert_eq!(visual_line_width("a\tb", 4), 6);
        assert_eq!(visual_line_width("\t\t", 2), 4);
    }
}
