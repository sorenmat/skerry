//! Snippet expansion — tab-triggered text templates.
//!
//! Snippets are configured in `config.json` as trigger→body pairs. When
//! the user types a trigger word and presses Tab, the word is replaced
//! by the body. The body can contain tab-stop placeholders:
//!
//! - `$0` — final cursor position (the cursor lands here after expansion)
//! - `${1:default}` — tab stop 1 with default text (future: tab between
//!   stops; today just the final cursor lands at `$0`)
//!
//! Today the implementation is simple: expand the body, place the cursor
//! at `$0` (or the end if no `$0`). Multi-tab-stop navigation is a
//! follow-up.

/// Expand a snippet body, returning the expanded text and the byte
/// offset within the expanded text where the cursor should land.
///
/// `$0` is replaced with nothing and its position becomes the cursor.
/// `${N:default}` placeholders have their default text substituted in.
/// `$$` produces a literal `$`.
pub fn expand(body: &str) -> (String, usize) {
    let mut result = String::new();
    let mut cursor_pos: Option<usize> = None;
    let bytes = body.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'$' && i + 1 < bytes.len() {
            match bytes[i + 1] {
                b'$' => {
                    result.push('$');
                    i += 2;
                }
                b'0' => {
                    cursor_pos = Some(result.len());
                    i += 2;
                }
                b'{' => {
                    // ${N:default} — find the closing }.
                    if let Some(close) = body[i + 2..].find('}') {
                        let inner = &body[i + 2..i + 2 + close];
                        // Split on ':' if present — N:default
                        if let Some(colon) = inner.find(':') {
                            result.push_str(&inner[colon + 1..]);
                        }
                        // If no default text, insert nothing.
                        i += 2 + close + 1;
                    } else {
                        // Malformed — treat literally.
                        result.push('$');
                        i += 1;
                    }
                }
                _ => {
                    // $N (numbered tab stop without braces) — skip it.
                    if bytes[i + 1].is_ascii_digit() {
                        i += 2;
                    } else {
                        result.push('$');
                        i += 1;
                    }
                }
            }
        } else {
            // Copy one UTF-8 character.
            let ch = body[i..].chars().next().unwrap();
            result.push(ch);
            i += ch.len_utf8();
        }
    }
    let final_pos = cursor_pos.unwrap_or(result.len());
    (result, final_pos)
}

/// Try to find a snippet trigger at the end of the current line before
/// the cursor. Returns the trigger word and its byte range if found.
pub fn trigger_at_cursor(line_text: &str, cursor_byte_col: usize) -> Option<(String, std::ops::Range<usize>)> {
    let chars: Vec<char> = line_text.chars().collect();
    let col = cursor_byte_col.min(chars.len());
    // Walk backward from the cursor to find the word boundary.
    if col == 0 {
        return None;
    }
    let is_trigger_char = |c: char| c.is_alphanumeric() || c == '_';
    // The cursor sits at `col`; the last typed char is at `col - 1`.
    let end = col;
    let mut start = col;
    while start > 0 && is_trigger_char(chars[start - 1]) {
        start -= 1;
    }
    if start == end {
        return None;
    }
    let word: String = chars[start..end].iter().collect();
    // Convert char indices to byte indices.
    let byte_start: usize = chars[..start].iter().map(|c| c.len_utf8()).sum();
    let byte_end: usize = chars[..end].iter().map(|c| c.len_utf8()).sum();
    Some((word, byte_start..byte_end))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_simple_body() {
        let (text, pos) = expand("hello world");
        assert_eq!(text, "hello world");
        assert_eq!(pos, 11);
    }

    #[test]
    fn expand_with_cursor_marker() {
        let (text, pos) = expand("fn main() {\n    $0\n}");
        assert_eq!(text, "fn main() {\n    \n}");
        assert_eq!(pos, 16); // position after "fn main() {\n    "
    }

    #[test]
    fn expand_with_placeholder() {
        let (text, _) = expand("let ${1:name} = $0;");
        assert_eq!(text, "let name = ;");
    }

    #[test]
    fn expand_dollar_escape() {
        let (text, _) = expand("cost: $$5");
        assert_eq!(text, "cost: $5");
    }

    #[test]
    fn expand_no_cursor_at_end() {
        let (text, pos) = expand("no marker here");
        assert_eq!(pos, text.len());
    }

    #[test]
    fn trigger_at_end_of_line() {
        let (word, range) = trigger_at_cursor("hello for", 9).unwrap();
        assert_eq!(word, "for");
        assert_eq!(range, 6..9);
    }

    #[test]
    fn trigger_at_start_of_line() {
        let (word, _) = trigger_at_cursor("for", 3).unwrap();
        assert_eq!(word, "for");
    }

    #[test]
    fn no_trigger_on_empty() {
        assert!(trigger_at_cursor("", 0).is_none());
    }

    #[test]
    fn no_trigger_on_non_word() {
        assert!(trigger_at_cursor("   ", 3).is_none());
    }
}
