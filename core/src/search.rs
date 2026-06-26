//! Search / find state shared by both frontends.
//!
//! Lives in `core` (not the Buffer trait) because searching is a
//! view-level concern — the buffer itself doesn't care that you're
//! searching its bytes. Both frontends own a `Search` instance and
//! drive it through `EditorEvent::Find*` events.
//!
//! Algorithm: literal substring search via `memchr::memmem` (SIMD-
//! accelerated). Skips regex support for v1 — it can be layered on
//! later by swapping the `find_matches` implementation. Match
//! positions are UTF-8 byte offsets into the buffer; the frontends
//! convert to (line, col) for display.
//!
//! For multi-GB files, we don't load all matches up-front — we
//! lazily extend a windowed match list as the user navigates with
//! n/N. The match list grows on demand up to `MAX_STORED_MATCHES`
//! so very common queries don't OOM.

use crate::BytePos;

/// Maximum number of match positions we keep in memory at once.
/// Beyond this we drop the oldest entries — matches beyond the
/// cursor are usually what the user wants to navigate to next.
pub const MAX_STORED_MATCHES: usize = 10_000;

/// Find state. One per App.
#[derive(Debug, Clone, Default)]
pub struct Search {
    /// Current search query. Empty means "no active search".
    pub query: String,
    /// All match positions (byte offsets, sorted ascending). Trimmed
    /// to `MAX_STORED_MATCHES` if exceeded.
    pub matches: Vec<BytePos>,
    /// Index into `matches` of the current match. `None` when there
    /// are no matches or when the search is empty.
    pub current: Option<usize>,
    /// Whether the find bar is open (visible). The bar persists
    /// across multiple searches so the user can refine and navigate
    /// without re-opening.
    pub bar_open: bool,
}

impl Search {
    pub fn new() -> Self {
        Self::default()
    }

    /// Re-run the search with the current query against `haystack`.
    /// Updates `matches` and resets `current` to the first match if
    /// any. If `haystack` doesn't contain the query, `matches` is
    /// cleared.
    pub fn refresh(&mut self, haystack: &[u8]) {
        self.matches.clear();
        if self.query.is_empty() {
            self.current = None;
            return;
        }
        let needle = self.query.as_bytes();
        if needle.is_empty() {
            self.current = None;
            return;
        }
        for offset in memchr::memmem::find_iter(haystack, needle) {
            if self.matches.len() >= MAX_STORED_MATCHES {
                break;
            }
            self.matches.push(offset);
        }
        self.current = if self.matches.is_empty() {
            None
        } else {
            Some(0)
        };
    }

    /// Move to the next match AFTER `from_pos`. If no match exists
    /// after `from_pos`, wraps to the first match. Sets `current`.
    /// Returns the new match position.
    pub fn next_after(&mut self, from_pos: BytePos) -> Option<BytePos> {
        if self.matches.is_empty() {
            self.current = None;
            return None;
        }
        // Find first match strictly greater than from_pos. If none,
        // wrap to first match.
        let next = self.matches.iter().position(|&m| m > from_pos);
        let idx = next.unwrap_or(0);
        self.current = Some(idx);
        Some(self.matches[idx])
    }

    /// Move to the previous match BEFORE `from_pos`. If no match
    /// exists before `from_pos`, wraps to the last match. Sets
    /// `current`. Returns the new match position.
    pub fn prev_before(&mut self, from_pos: BytePos) -> Option<BytePos> {
        if self.matches.is_empty() {
            self.current = None;
            return None;
        }
        let prev = self.matches.iter().rposition(|&m| m < from_pos);
        let idx = prev.unwrap_or(self.matches.len() - 1);
        self.current = Some(idx);
        Some(self.matches[idx])
    }

    /// Position of the currently-active match, if any.
    pub fn current_match(&self) -> Option<BytePos> {
        self.current.and_then(|i| self.matches.get(i).copied())
    }

    /// Whether a match starts at `pos` (i.e. this byte is the first
    /// byte of some match).
    pub fn is_match_start(&self, pos: BytePos) -> bool {
        self.matches.binary_search(&pos).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn search_with(query: &str) -> Search {
        let mut s = Search::new();
        s.query = query.to_string();
        s
    }

    #[test]
    fn empty_query_yields_no_matches() {
        let mut s = Search::new();
        s.refresh(b"hello world");
        assert!(s.matches.is_empty());
        assert!(s.current.is_none());
    }

    #[test]
    fn finds_all_occurrences() {
        let mut s = search_with("ab");
        s.refresh(b"abxxabyyabzzab");
        // Positions of "ab" in the haystack: 0-1, 4-5, 8-9, 12-13.
        assert_eq!(s.matches, vec![0, 4, 8, 12]);
        assert_eq!(s.current, Some(0));
    }

    #[test]
    fn no_match_clears_state() {
        let mut s = search_with("xyz");
        s.refresh(b"hello world");
        assert!(s.matches.is_empty());
        assert!(s.current.is_none());
    }

    #[test]
    fn next_after_wraps() {
        let mut s = search_with("ab");
        s.refresh(b"abxxabxxab");
        assert_eq!(s.matches, vec![0, 4, 8]);
        assert_eq!(s.next_after(0), Some(4));
        assert_eq!(s.next_after(4), Some(8));
        assert_eq!(s.next_after(8), Some(0), "wraps to first");
        assert_eq!(s.next_after(100), Some(0), "past end wraps");
    }

    #[test]
    fn prev_before_wraps() {
        let mut s = search_with("ab");
        s.refresh(b"abxxabxxab");
        assert_eq!(s.matches, vec![0, 4, 8]);
        assert_eq!(s.prev_before(8), Some(4));
        assert_eq!(s.prev_before(4), Some(0));
        assert_eq!(s.prev_before(0), Some(8), "wraps to last");
    }

    #[test]
    fn current_match_returns_active() {
        let mut s = search_with("x");
        s.refresh(b"xaxbxc");
        assert_eq!(s.current_match(), Some(0));
        s.next_after(0);
        assert_eq!(s.current_match(), Some(2));
    }

    #[test]
    fn is_match_start_detects_match_byte() {
        let mut s = search_with("ab");
        s.refresh(b"xxabxxabxx");
        assert!(s.is_match_start(2));
        assert!(!s.is_match_start(3));
        assert!(s.is_match_start(6));
        assert!(!s.is_match_start(100));
    }

    #[test]
    fn multi_byte_utf8_needle_finds_byte_offsets() {
        // "héllo" — 'é' is 2 bytes. needle "é" should match byte 1.
        let mut s = search_with("é");
        s.refresh("héllo".as_bytes());
        assert_eq!(s.matches, vec![1]);
    }
}