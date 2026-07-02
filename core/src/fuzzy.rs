//! Fuzzy string matching for quick-open and filter UIs.
//!
//! The matcher is intentionally small and dependency-free. It scores a
//! query against a candidate by requiring every query character to appear
//! in order (case-insensitive) and awarding bonuses for:
//!
//! * exact case matches
//! * consecutive matches
//! * matches at the start of the candidate or at a path/word separator
//!
//! Penalties are applied for gaps between matched characters and for
//! overall candidate length.

/// A successful fuzzy match with a score and the matched character
/// positions in the candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuzzyMatch {
    /// Higher is better.
    pub score: i64,
    /// Byte positions in the candidate string that matched query
    /// characters, in ascending order.
    pub positions: Vec<usize>,
}

impl FuzzyMatch {
    /// Convenience constructor used in tests and for empty queries.
    pub fn empty() -> Self {
        Self {
            score: 0,
            positions: Vec::new(),
        }
    }
}

/// Return `true` if `c` is a separator that starts a new path component
/// or word.
fn is_separator(c: char) -> bool {
    matches!(c, '/' | '\\' | '_' | '-' | '.' | ' ')
}

/// Score `query` against `candidate`. Returns `None` if not every query
/// character can be found in order.
pub fn fuzzy_score(query: &str, candidate: &str) -> Option<FuzzyMatch> {
    if query.is_empty() {
        return Some(FuzzyMatch::empty());
    }
    if candidate.is_empty() {
        return None;
    }

    let query_lower: Vec<char> = query.to_lowercase().chars().collect();
    let query_orig: Vec<char> = query.chars().collect();
    let candidate_lower: Vec<char> = candidate.to_lowercase().chars().collect();
    let candidate_orig: Vec<char> = candidate.chars().collect();

    let mut positions = Vec::new();
    let mut q_idx = 0;
    let mut prev_match: Option<usize> = None;
    let mut score: i64 = 0;

    for (i, c) in candidate_lower.iter().enumerate() {
        if q_idx >= query_lower.len() {
            break;
        }
        let q = query_lower[q_idx];
        if *c == q {
            positions.push(i);

            // Exact-case match bonus.
            if candidate_orig[i] == query_orig[q_idx] {
                score += 5;
            }

            // Start-of-string or start-of-word bonus.
            if i == 0 || is_separator(candidate_orig[i.saturating_sub(1)]) {
                score += 15;
            }

            // Consecutive-match bonus; gap penalty otherwise.
            if let Some(prev) = prev_match {
                if i == prev + 1 {
                    score += 15;
                } else {
                    score -= ((i - prev) as i64 * 2).min(30);
                }
            }

            q_idx += 1;
            prev_match = Some(i);
        }
    }

    if q_idx < query_lower.len() {
        return None;
    }

    // Slight preference for shorter candidates among otherwise equal
    // matches. Use the original char count, not byte length.
    score -= candidate_orig.len() as i64 / 3;

    Some(FuzzyMatch { score, positions })
}

/// Filter `candidates` to those that match `query` and return them sorted
/// by score (highest first). An empty query returns every candidate in
/// alphabetical order.
pub fn filter_and_rank(query: &str, candidates: &[String]) -> Vec<(usize, FuzzyMatch)> {
    if query.is_empty() {
        let mut all: Vec<(usize, FuzzyMatch)> = candidates
            .iter()
            .enumerate()
            .map(|(i, _)| (i, FuzzyMatch::empty()))
            .collect();
        all.sort_by(|a, b| candidates[a.0].cmp(&candidates[b.0]));
        return all;
    }

    let mut results: Vec<(usize, FuzzyMatch)> = candidates
        .iter()
        .enumerate()
        .filter_map(|(i, c)| fuzzy_score(query, c).map(|m| (i, m)))
        .collect();

    results.sort_by(|a, b| {
        b.1.score
            .cmp(&a.1.score)
            .then_with(|| a.1.positions.first().cmp(&b.1.positions.first()))
            .then_with(|| candidates[a.0].len().cmp(&candidates[b.0].len()))
            .then_with(|| candidates[a.0].cmp(&candidates[b.0]))
    });

    results
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_query_matches_everything() {
        assert_eq!(fuzzy_score("", "hello"), Some(FuzzyMatch::empty()));
    }

    #[test]
    fn query_longer_than_candidate_fails() {
        assert!(fuzzy_score("hello", "hi").is_none());
    }

    #[test]
    fn case_insensitive_match() {
        let m = fuzzy_score("abc", "AbcDef").unwrap();
        assert_eq!(m.positions, vec![0, 1, 2]);
    }

    #[test]
    fn non_consecutive_match_scores_lower() {
        let a = fuzzy_score("abc", "a_b_c").unwrap();
        let b = fuzzy_score("abc", "abc").unwrap();
        assert!(b.score > a.score);
    }

    #[test]
    fn word_start_bonus() {
        let a = fuzzy_score("buf", "buffer.rs").unwrap();
        let b = fuzzy_score("buf", "xbuffer.rs").unwrap();
        assert!(a.score > b.score);
    }

    #[test]
    fn filter_and_rank_orders_by_score() {
        let candidates = vec![
            "xbuffer.rs".to_string(),
            "buffer.rs".to_string(),
            "main.rs".to_string(),
        ];
        let ranked = filter_and_rank("buf", &candidates);
        assert_eq!(ranked.len(), 2);
        assert_eq!(ranked[0].0, 1); // buffer.rs
        assert_eq!(ranked[1].0, 0); // xbuffer.rs
    }

    #[test]
    fn filter_and_rank_empty_query_is_alphabetical() {
        let candidates = vec![
            "zebra.rs".to_string(),
            "apple.rs".to_string(),
            "banana.rs".to_string(),
        ];
        let ranked = filter_and_rank("", &candidates);
        let indices: Vec<usize> = ranked.iter().map(|(i, _)| *i).collect();
        assert_eq!(indices, vec![1, 2, 0]);
    }
}
