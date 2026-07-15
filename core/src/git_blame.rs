//! Inline git blame — per-line commit metadata shown in the gutter.
//!
//! Mirrors [`crate::GitGutter`] in shape: a `GitBlame` struct lives on
//! `Document`, stores per-line [`BlameEntry`] data, and is populated by
//! shelling out to `git blame --line-porcelain`. Refreshed on a debounce
//! (same pattern as the gutter) so it doesn't run on every keystroke.

use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

/// Maximum file size for which we run `git blame`. Blame walks history
/// per line and is expensive on large files; above this limit the blame
/// column stays empty.
const MAX_BLAME_BYTES: usize = 5 * 1024 * 1024;

/// Per-line commit metadata from `git blame`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlameEntry {
    /// Short commit hash (first 7 chars).
    pub short_hash: String,
    /// Author name (truncated to 12 chars for display).
    pub author: String,
    /// Pre-formatted relative time (e.g. "2d", "3w", "just now").
    pub relative_time: String,
}

/// Cached git-blame state for a document.
#[derive(Debug, Clone, Default)]
pub struct GitBlame {
    /// Per-line blame data, indexed by line number. `None` for lines
    /// that couldn't be blamed (e.g. untracked file).
    entries: Vec<Option<BlameEntry>>,
    enabled: bool,
    dirty: bool,
}

impl GitBlame {
    /// Create an empty, disabled blame cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// Recompute blame for `path` against `HEAD`.
    /// `buffer_len` and `line_count` are passed directly to avoid a
    /// `&dyn Buffer` borrow conflict in the frontend's debounced refresh.
    pub fn refresh(&mut self, path: Option<&Path>, buffer_len: usize, line_count: usize) {
        self.clear();
        let Some(path) = path else {
            return;
        };
        let Ok(path) = path.canonicalize() else {
            return;
        };
        let Some(repo_root) = crate::git_gutter::repo_root_for(&path) else {
            return;
        };
        let Ok(repo_root) = repo_root.canonicalize() else {
            return;
        };
        let rel_path = match path.strip_prefix(&repo_root) {
            Ok(p) => p,
            Err(_) => return,
        };

        // Guard: skip very large files (blame is expensive).
        if buffer_len > MAX_BLAME_BYTES {
            return;
        }

        let Some(output) = blame_output(&repo_root, rel_path) else {
            return;
        };
        self.entries = parse_porcelain(&output, line_count);
        self.enabled = true;
        self.dirty = false;
    }

    /// Return the blame entry for a given line, if any.
    pub fn entry(&self, line: usize) -> Option<&BlameEntry> {
        self.entries.get(line).and_then(|e| e.as_ref())
    }

    /// Whether the blame data is available.
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Whether the current state is stale and should be refreshed.
    pub fn dirty(&self) -> bool {
        self.dirty
    }

    /// Mark the blame stale (call after buffer edits).
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.enabled = false;
    }
}

/// Run `git blame --line-porcelain HEAD -- <path>` from `repo_root`.
fn blame_output(repo_root: &Path, rel_path: &Path) -> Option<String> {
    let output = Command::new("git")
        .arg("blame")
        .arg("--line-porcelain")
        .arg("HEAD")
        .arg("--")
        .arg(rel_path)
        .current_dir(repo_root)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

/// Parse `git blame --line-porcelain` output into per-line entries.
///
/// Each blame block in the porcelain format looks like:
/// ```text
/// <40-char hash> <orig-line> <final-line>\n
/// author <name>\n
/// author-mail <email>\n
/// author-time <unix-epoch>\n
/// author-tz <timezone>\n
/// committer ...\n
/// summary <one-line message>\n
/// \t<line content>\n
/// ```
/// We extract hash (shortened), author, and relative time from author-time.
fn parse_porcelain(output: &str, line_count: usize) -> Vec<Option<BlameEntry>> {
    let mut entries: Vec<Option<BlameEntry>> = vec![None; line_count];
    let mut current_hash = String::new();
    let mut current_final_line: usize = 0;
    let mut current_author = String::new();
    let mut current_time: u64 = 0;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    for line in output.lines() {
        if let Some(_content) = line.strip_prefix('\t') {
            // End of a blame block — emit the entry using accumulated
            // metadata. The final_line from the header tells us which
            // buffer line this maps to.
            if current_final_line > 0 && current_final_line <= line_count {
                let short_hash = if current_hash.len() >= 7 {
                    current_hash[..7].to_string()
                } else {
                    current_hash.clone()
                };
                entries[current_final_line - 1] = Some(BlameEntry {
                    short_hash,
                    author: truncate(&current_author, 12),
                    relative_time: relative_time_str(now.saturating_sub(current_time)),
                });
            }
            continue;
        }
        // Header line: "<40-char hash> <orig-line> <final-line>"
        if line.len() >= 40 && line.as_bytes()[..40].iter().all(|b| b.is_ascii_hexdigit()) {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 3 {
                current_hash = parts[0].to_string();
                current_final_line = parts[2].parse().unwrap_or(0);
            }
        } else if let Some(rest) = line.strip_prefix("author ") {
            current_author = rest.to_string();
        } else if let Some(rest) = line.strip_prefix("author-time ") {
            current_time = rest.parse().unwrap_or(0);
        }
    }
    entries
}

/// Truncate a string to at most `max` chars, appending "…" if truncated.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{truncated}…")
    }
}

/// Format a duration (in seconds) as a compact relative time string.
fn relative_time_str(secs: u64) -> String {
    let mins = secs / 60;
    let hours = mins / 60;
    let days = hours / 24;
    if secs < 60 {
        "now".to_string()
    } else if mins < 60 {
        format!("{mins}m")
    } else if hours < 24 {
        format!("{hours}h")
    } else if days < 7 {
        format!("{days}d")
    } else if days < 30 {
        format!("{}w", days / 7)
    } else if days < 365 {
        format!("{}mo", days / 30)
    } else {
        format!("{}y", days / 365)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_porcelain_basic() {
        let output = "\
abcdef0123456789abcdef0123456789abcdef01 1 1
author Alice
author-mail <alice@example.com>
author-time 1700000000
summary Initial commit
\tline one
abcdef0123456789abcdef0123456789abcdef02 2 2
author Bob
author-mail <bob@example.com>
author-time 1700000600
summary Second commit
\tline two
";
        let entries = parse_porcelain(output, 2);
        assert_eq!(entries.len(), 2);
        assert!(entries[0].is_some());
        assert_eq!(entries[0].as_ref().unwrap().short_hash, "abcdef0");
        assert_eq!(entries[0].as_ref().unwrap().author, "Alice");
        assert!(entries[1].is_some());
        assert_eq!(entries[1].as_ref().unwrap().short_hash, "abcdef0");
        assert_eq!(entries[1].as_ref().unwrap().author, "Bob");
    }

    #[test]
    fn relative_time_formats() {
        assert_eq!(relative_time_str(30), "now");
        assert_eq!(relative_time_str(120), "2m");
        assert_eq!(relative_time_str(7200), "2h");
        assert_eq!(relative_time_str(86400 * 3), "3d");
        assert_eq!(relative_time_str(86400 * 14), "2w");
        assert_eq!(relative_time_str(86400 * 60), "2mo");
        assert_eq!(relative_time_str(86400 * 400), "1y");
    }

    #[test]
    fn truncate_long_author() {
        assert_eq!(truncate("Alice", 12), "Alice");
        assert_eq!(truncate("Alexander Hamilton", 12), "Alexander H…");
    }

    #[test]
    fn empty_blame_is_disabled() {
        let b = GitBlame::new();
        assert!(!b.enabled());
        assert!(b.entry(0).is_none());
    }
}
