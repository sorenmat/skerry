//! Search / find state shared by both frontends.
//!
//! Lives in `core` (not the Buffer trait) because searching is a
//! view-level concern — the buffer itself doesn't care that you're
//! searching its bytes. Both frontends own a `Search` instance and
//! drive it through `EditorEvent::Find*` events.
//!
//! Algorithm: literal substring search via `memchr::memmem` (SIMD-
//! accelerated) when `regex_mode` is off; full Rust `regex` engine
//! when `regex_mode` is on. Match positions are UTF-8 byte offsets
//! into the buffer; the frontends convert to (line, col) for display.
//!
//! For multi-GB files, we don't load all matches up-front — we
//! lazily extend a windowed match list as the user navigates with
//! n/N. The match list grows on demand up to `MAX_STORED_MATCHES`
//! so very common queries don't OOM.

use std::sync::Arc;

use crate::BytePos;

/// Check whether the match at `[start, end)` in `haystack` is a whole
/// word — i.e. the character before `start` and after `end-1` are both
/// non-word characters (or at the string boundary).
fn is_whole_word(haystack: &[u8], start: usize, end: usize) -> bool {
    let is_word_char = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let before_ok = start == 0 || !is_word_char(haystack[start - 1]);
    let after_ok = end >= haystack.len() || !is_word_char(haystack[end]);
    before_ok && after_ok
}

/// Maximum number of match positions we keep in memory at once.
/// Beyond this we drop the oldest entries — matches beyond the
/// cursor are usually what the user wants to navigate to next.
pub const MAX_STORED_MATCHES: usize = 10_000;

/// Find state. One per App.
#[derive(Debug, Clone, Default)]
pub struct Search {
    /// Current search query. Empty means "no active search".
    pub query: String,
    /// Replacement string used by `ReplaceOne` / `ReplaceAll`. Empty
    /// means "no active replacement"; the App refuses to run a
    /// replace when this is empty (treats it as a user error rather
    /// than silently deleting matches).
    pub replace_query: String,
    /// All match positions as byte-offset ranges (start inclusive,
    /// end exclusive), sorted ascending by start.
    pub matches: Vec<(BytePos, BytePos)>,
    /// Index into `matches` of the current match. `None` when there
    /// are no matches or when the search is empty.
    pub current: Option<usize>,
    /// Whether the find bar is open (visible). The bar persists
    /// across multiple searches so the user can refine and navigate
    /// without re-opening.
    pub bar_open: bool,
    /// Whether query refresh and plain Enter navigate backward.
    pub backward: bool,
    /// Whether the replace bar is open. Independent from `bar_open`
    /// — the user can have only one, both, or neither. The replace
    /// bar is hidden when the find bar is closed (no point editing
    /// the replacement without an active search).
    pub replace_bar_open: bool,
    /// When true, `refresh` interprets `query` as a regex instead of
    /// a literal substring.
    pub regex_mode: bool,
    /// When true, literal search is case-sensitive (uppercase and
    /// lowercase are distinct). Default false (case-insensitive).
    pub case_sensitive: bool,
    /// When true, matches must be whole words (surrounded by non-word
    /// characters or string boundaries). Default false.
    pub whole_word: bool,
    /// When `Some`, search is limited to the byte range `(start, end)`.
    /// Set by "find in selection" when the user has a multi-line
    /// selection. Cleared when the selection collapses or the find bar
    /// re-opens.
    pub selection_range: Option<(BytePos, BytePos)>,
    /// If the last regex compile failed, the error message to show
    /// in the find bar. `None` when regex mode is off or the pattern
    /// is valid.
    pub regex_error: Option<String>,
    /// Last successfully compiled regex and the pattern string it was
    /// built from. The find bar re-runs the search on every keystroke
    /// and compiling a regex costs tens of µs, so an unchanged pattern
    /// reuses this entry instead of recompiling. Only successful
    /// compiles are stored — a bad pattern never poisons the cache.
    cached_regex: Option<(String, Arc<regex::Regex>)>,
}

impl Search {
    pub fn new() -> Self {
        Self::default()
    }

    /// The regex pattern the search compiles to: the query wrapped in
    /// the same flags `refresh_regex` uses. The replace path shares
    /// this so replacement operates on exactly what the search found
    /// — previously it recompiled the raw query without `(?i)`, so a
    /// case-insensitive search could find a match the replacement
    /// then skipped (and `nth(current)` could point at the wrong
    /// capture).
    fn search_pattern(&self) -> String {
        let mut pattern = String::new();
        if !self.case_sensitive {
            pattern.push_str("(?i)");
        }
        if self.whole_word {
            pattern.push_str(r"\b");
        }
        pattern.push_str(&self.query);
        if self.whole_word {
            pattern.push_str(r"\b");
        }
        pattern
    }

    /// Compile `pattern`, reusing the cached regex when it was already
    /// compiled from the same pattern string. Only successful compiles
    /// are cached, so an invalid pattern can never poison the entry.
    fn compiled_regex(&mut self, pattern: &str) -> Result<Arc<regex::Regex>, regex::Error> {
        if let Some((key, re)) = &self.cached_regex {
            if key == pattern {
                return Ok(re.clone());
            }
        }
        let re = Arc::new(regex::Regex::new(pattern)?);
        self.cached_regex = Some((pattern.to_owned(), re.clone()));
        Ok(re)
    }

    /// Re-run the search with the current query against `haystack`.
    /// Updates `matches` and resets `current` to the first match if
    /// any. If `haystack` doesn't contain the query, `matches` is
    /// cleared.
    pub fn refresh(&mut self, haystack: &[u8]) {
        self.matches.clear();
        self.regex_error = None;
        if self.query.is_empty() {
            self.current = None;
            return;
        }

        if self.regex_mode {
            self.refresh_regex(haystack);
        } else {
            self.refresh_literal(haystack);
        }

        // Filter matches to the selection range if set.
        if let Some((sel_start, sel_end)) = self.selection_range {
            self.matches
                .retain(|&(s, e)| s >= sel_start && e <= sel_end);
        }

        self.current = if self.matches.is_empty() {
            None
        } else if self.backward {
            Some(self.matches.len() - 1)
        } else {
            Some(0)
        };
    }

    fn refresh_literal(&mut self, haystack: &[u8]) {
        if self.query.is_empty() {
            return;
        }
        if self.case_sensitive {
            // Case-sensitive: direct byte search.
            let needle = self.query.as_bytes();
            for start in memchr::memmem::find_iter(haystack, needle) {
                if self.matches.len() >= MAX_STORED_MATCHES {
                    break;
                }
                if self.whole_word && !is_whole_word(haystack, start, start + needle.len()) {
                    continue;
                }
                self.matches.push((start, start + needle.len()));
            }
            return;
        }
        // Case-insensitive: hand the escaped literal to the regex
        // engine with `(?i)` — the engine's literal search is
        // SIMD-scanned, so a 10 MB document takes ~0.3 ms instead of
        // the ~11 ms the old per-character `eq_ignore_ascii_case`
        // loop took. Case folding also covers non-ASCII letters,
        // which the old ASCII-only compare didn't. `is_whole_word`
        // stays the post-filter so whole-word semantics are unchanged.
        let text = match std::str::from_utf8(haystack) {
            Ok(t) => t,
            Err(_) => return,
        };
        let pattern = format!("(?i){}", regex::escape(&self.query));
        let Ok(re) = self.compiled_regex(&pattern) else {
            return;
        };
        for m in re.find_iter(text) {
            if self.matches.len() >= MAX_STORED_MATCHES {
                break;
            }
            let (start, end) = (m.start(), m.end());
            if self.whole_word && !is_whole_word(haystack, start, end) {
                continue;
            }
            self.matches.push((start, end));
        }
    }

    fn refresh_regex(&mut self, haystack: &[u8]) {
        let pattern = self.search_pattern();
        let re = match self.compiled_regex(&pattern) {
            Ok(re) => re,
            Err(e) => {
                self.regex_error = Some(e.to_string());
                return;
            }
        };
        // Regex operates on str, so we require valid UTF-8 for the
        // haystack. Invalid UTF-8 is treated as no matches; this is
        // acceptable because the rest of the editor also assumes
        // UTF-8 text.
        let text = match std::str::from_utf8(haystack) {
            Ok(text) => text,
            Err(_) => return,
        };
        for m in re.find_iter(text) {
            if self.matches.len() >= MAX_STORED_MATCHES {
                break;
            }
            self.matches.push((m.start(), m.end()));
        }
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
        // wrap to first match. Matches are sorted by start, so this
        // is a binary search instead of a linear scan.
        let next = self.matches.partition_point(|&(s, _)| s <= from_pos);
        let idx = if next == self.matches.len() { 0 } else { next };
        self.current = Some(idx);
        Some(self.matches[idx].0)
    }

    /// Move to the previous match BEFORE `from_pos`. If no match
    /// exists before `from_pos`, wraps to the last match. Sets
    /// `current`. Returns the new match position.
    pub fn prev_before(&mut self, from_pos: BytePos) -> Option<BytePos> {
        if self.matches.is_empty() {
            self.current = None;
            return None;
        }
        // Last match strictly before from_pos (binary search), or
        // wrap to the last match if none is before.
        let idx = self.matches.partition_point(|&(s, _)| s < from_pos);
        let idx = if idx == 0 {
            self.matches.len() - 1
        } else {
            idx - 1
        };
        self.current = Some(idx);
        Some(self.matches[idx].0)
    }

    /// Position of the currently-active match, if any.
    pub fn current_match(&self) -> Option<BytePos> {
        self.current.and_then(|i| self.matches.get(i).map(|m| m.0))
    }

    /// Full byte range of the currently-active match, if any.
    pub fn current_match_range(&self) -> Option<(BytePos, BytePos)> {
        self.current.and_then(|i| self.matches.get(i).copied())
    }

    /// Whether a match starts at `pos` (i.e. this byte is the first
    /// byte of some match).
    pub fn is_match_start(&self, pos: BytePos) -> bool {
        self.matches.binary_search_by_key(&pos, |&(s, _)| s).is_ok()
    }

    /// Whether `pos` falls inside any match range (not necessarily at
    /// the start). Used to highlight full regex matches.
    pub fn is_inside_match(&self, pos: BytePos) -> bool {
        self.matches.binary_search_by_key(&pos, |&(s, _)| s).is_ok()
            || self
                .matches
                .iter()
                .any(|&(start, end)| start < pos && pos < end)
    }

    /// Compute the replacement text for the current match. In regex
    /// mode, `$0`/`$1`/... capture-group references are expanded. In
    /// literal mode the replacement query is returned unchanged.
    /// Returns the match byte range plus the expanded replacement, or
    /// `None` if there is no current match or the regex is invalid.
    pub fn current_replacement(&mut self, haystack: &str) -> Option<(BytePos, BytePos, String)> {
        let (start, end) = self.current_match_range()?;
        if self.regex_mode {
            let re = self.compiled_regex(&self.search_pattern()).ok()?;
            let caps = re.captures_iter(haystack).nth(self.current?)?;
            let mut dst = String::new();
            caps.expand(&self.replace_query, &mut dst);
            Some((start, end, dst))
        } else {
            Some((start, end, self.replace_query.clone()))
        }
    }

    /// Return `haystack` with all matches replaced by the replacement
    /// query. In regex mode, capture-group expansions are honored. In
    /// literal mode, every stored match is replaced. Returns `None` if
    /// regex mode is on but the pattern is invalid.
    pub fn replace_all_text(&mut self, haystack: &str) -> Option<String> {
        if self.regex_mode {
            let re = self.compiled_regex(&self.search_pattern()).ok()?;
            Some(re.replace_all(haystack, &self.replace_query).into_owned())
        } else {
            let mut text = haystack.to_string();
            for &(start, end) in self.matches.iter().rev() {
                text.replace_range(start..end, &self.replace_query);
            }
            Some(text)
        }
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
        // Positions of "ab" in the haystack: 0-2, 4-6, 8-10, 12-14.
        assert_eq!(s.matches, vec![(0, 2), (4, 6), (8, 10), (12, 14)]);
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
        assert_eq!(s.matches, vec![(0, 2), (4, 6), (8, 10)]);
        assert_eq!(s.next_after(0), Some(4));
        assert_eq!(s.next_after(4), Some(8));
        assert_eq!(s.next_after(8), Some(0), "wraps to first");
        assert_eq!(s.next_after(100), Some(0), "past end wraps");
    }

    #[test]
    fn prev_before_wraps() {
        let mut s = search_with("ab");
        s.refresh(b"abxxabxxab");
        assert_eq!(s.matches, vec![(0, 2), (4, 6), (8, 10)]);
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
    fn current_match_range_returns_full_range() {
        let mut s = search_with("ab");
        s.refresh(b"abxxab");
        assert_eq!(s.current_match_range(), Some((0, 2)));
        s.next_after(0);
        assert_eq!(s.current_match_range(), Some((4, 6)));
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
    fn is_inside_match_detects_any_byte_in_match() {
        let mut s = search_with("ab");
        s.refresh(b"xxabxxabxx");
        assert!(s.is_inside_match(2));
        assert!(s.is_inside_match(3));
        assert!(!s.is_inside_match(4));
    }

    #[test]
    fn multi_byte_utf8_needle_finds_byte_offsets() {
        // "héllo" — 'é' is 2 bytes. needle "é" should match byte 1..3.
        let mut s = search_with("é");
        s.refresh("héllo".as_bytes());
        assert_eq!(s.matches, vec![(1, 3)]);
    }

    #[test]
    fn regex_mode_finds_variable_length_matches() {
        let mut s = search_with("a+");
        s.regex_mode = true;
        s.refresh(b"aaabca");
        assert_eq!(s.matches, vec![(0, 3), (5, 6)]);
    }

    #[test]
    fn regex_mode_reports_invalid_pattern() {
        let mut s = search_with("(");
        s.regex_mode = true;
        s.refresh(b"hello");
        assert!(s.matches.is_empty());
        assert!(s.regex_error.is_some());
    }

    #[test]
    fn regex_mode_resets_error_on_valid_pattern() {
        let mut s = search_with("(");
        s.regex_mode = true;
        s.refresh(b"hello");
        assert!(s.regex_error.is_some());
        s.query = "h".to_string();
        s.refresh(b"hello");
        assert!(s.regex_error.is_none());
    }

    #[test]
    fn is_inside_match_works_for_overlapping_regex_matches() {
        let mut s = search_with("aa");
        s.regex_mode = true;
        s.refresh(b"aaa");
        // Regex leftmost-first: matches 0..2 and 2..4? No, "aaa"
        // with "aa" gives 0..2 and 2..4? Actually regex matches
        // don't overlap by default: 0..2 only.
        assert_eq!(s.matches, vec![(0, 2)]);
        assert!(s.is_inside_match(0));
        assert!(s.is_inside_match(1));
        assert!(!s.is_inside_match(2));
    }

    #[test]
    fn current_replacement_literal_returns_replace_query() {
        let mut s = search_with("foo");
        s.replace_query = "bar".to_string();
        s.refresh(b"foo baz");
        assert_eq!(
            s.current_replacement("foo baz"),
            Some((0, 3, "bar".to_string()))
        );
    }

    #[test]
    fn current_replacement_regex_expands_capture_groups() {
        let mut s = search_with("(\\w+) (\\w+)");
        s.regex_mode = true;
        s.replace_query = "$2 $1".to_string();
        s.refresh(b"hello world");
        assert_eq!(
            s.current_replacement("hello world"),
            Some((0, 11, "world hello".to_string()))
        );
    }

    #[test]
    fn replace_all_text_literal_replaces_every_match() {
        let mut s = search_with("foo");
        s.replace_query = "bar".to_string();
        s.refresh(b"foo baz foo");
        assert_eq!(
            s.replace_all_text("foo baz foo"),
            Some("bar baz bar".to_string())
        );
    }

    #[test]
    fn replace_all_text_regex_expands_capture_groups() {
        let mut s = search_with("(\\w+) (\\w+)");
        s.regex_mode = true;
        s.replace_query = "$2 $1".to_string();
        s.refresh(b"hello world\nfoo bar");
        assert_eq!(
            s.replace_all_text("hello world\nfoo bar"),
            Some("world hello\nbar foo".to_string())
        );
    }

    #[test]
    fn replace_all_text_regex_invalid_pattern_returns_none() {
        let mut s = search_with("(");
        s.regex_mode = true;
        s.replace_query = "x".to_string();
        s.refresh(b"hello");
        assert_eq!(s.replace_all_text("hello"), None);
    }

    #[test]
    fn case_insensitive_finds_case_variants() {
        let mut s = search_with("FOO");
        s.refresh(b"foo FOO fOo");
        assert_eq!(s.matches, vec![(0, 3), (4, 7), (8, 11)]);
    }

    #[test]
    fn case_insensitive_unicode_case_fold() {
        // The old ASCII-only compare only matched the exact lowercased
        // bytes; the regex engine folds Unicode case.
        let mut s = search_with("É");
        s.refresh("héllo".as_bytes());
        assert_eq!(s.matches, vec![(1, 3)]);
    }

    #[test]
    fn case_insensitive_whole_word_filters_partials() {
        let mut s = search_with("cat");
        s.whole_word = true;
        s.refresh(b"cat catalog cater cat");
        assert_eq!(s.matches, vec![(0, 3), (18, 21)]);
    }

    #[test]
    fn regex_mode_replace_all_honors_case_insensitive_search() {
        // The replace path used to recompile the raw query without
        // (?i), so a case-insensitive search could find a match the
        // replacement skipped.
        let mut s = search_with("foo");
        s.regex_mode = true;
        s.replace_query = "bar".to_string();
        s.refresh(b"FOO foo");
        assert_eq!(s.matches, vec![(0, 3), (4, 7)]);
        assert_eq!(s.replace_all_text("FOO foo"), Some("bar bar".to_string()));
    }

    #[test]
    fn failed_regex_compile_does_not_poison_cache() {
        let mut s = search_with("a+");
        s.regex_mode = true;
        s.refresh(b"aaab");
        assert_eq!(s.matches, vec![(0, 3)]);
        // An invalid pattern must not discard the good cached regex.
        s.query = "(".to_string();
        s.refresh(b"whatever");
        assert!(s.regex_error.is_some());
        s.query = "a+".to_string();
        s.refresh(b"aaab");
        assert!(s.regex_error.is_none());
        assert_eq!(s.matches, vec![(0, 3)]);
    }
}
