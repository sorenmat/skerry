//! Git gutter — per-line change status relative to `HEAD`.
//!
//! The gutter diff is done in memory so it can update live as the buffer
//! changes. It shells out to `git` only to read the `HEAD` blob and to
//! discover the repository root.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use similar::{Algorithm, DiffOp};

/// Maximum `HEAD` blob size we are willing to diff. Files larger than
/// this get no gutter, which keeps multi-GB log files responsive.
const MAX_HEAD_BYTES: usize = 5 * 1024 * 1024;

/// Per-line status in the current buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineStatus {
    /// Line is unchanged vs. `HEAD`.
    #[default]
    Unchanged,
    /// Line does not exist in `HEAD`.
    Added,
    /// Line replaced a different line in `HEAD`.
    Modified,
}

/// A block of lines that existed in `HEAD` but have been deleted. The
/// block is anchored to the first new line that follows it; a deletion
/// at the very end of the file uses `before_line == new_line_count`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemovedBlock {
    /// New-line index before which the removed lines were deleted.
    pub before_line: usize,
    /// Number of removed lines.
    pub count: usize,
    /// Content of the removed lines (without trailing newlines).
    pub lines: Vec<String>,
}

/// A contiguous changed region for hunk navigation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hunk {
    /// Inclusive start line in the current buffer.
    pub start_line: usize,
    /// Inclusive end line in the current buffer.
    pub end_line: usize,
}

/// Cached git-gutter state for a document.
#[derive(Debug, Clone, Default)]
pub struct GitGutter {
    statuses: Vec<LineStatus>,
    removed: Vec<RemovedBlock>,
    /// Removed-line count per anchor line (`RemovedBlock::before_line`),
    /// so the render path is one lookup per line instead of a scan of
    /// all blocks.
    removed_by_line: BTreeMap<usize, usize>,
    hunks: Vec<Hunk>,
    enabled: bool,
    dirty: bool,
    /// Cached `(canonical file path, canonical repo root)`. The repo
    /// root is invariant per file, so it is resolved once instead of
    /// re-running `git rev-parse` on every refresh.
    cached_root: Option<(PathBuf, PathBuf)>,
}

impl GitGutter {
    /// Create an empty, disabled gutter.
    pub fn new() -> Self {
        Self {
            statuses: Vec::new(),
            removed: Vec::new(),
            removed_by_line: BTreeMap::new(),
            hunks: Vec::new(),
            enabled: false,
            dirty: false,
            cached_root: None,
        }
    }

    /// Recompute the gutter for `path` against the document's current
    /// source. `source` is the memoized document bytes (see
    /// `Document::source_bytes`), borrowed — an unchanged document
    /// costs no copy.
    pub fn refresh(&mut self, path: Option<&Path>, source: &Arc<Vec<u8>>) {
        self.clear();
        let Some(path) = path else {
            return;
        };
        let Ok(path) = path.canonicalize() else {
            return;
        };
        let repo_root = match self.cached_root.as_ref() {
            Some((cached_path, root)) if cached_path == &path => root.clone(),
            _ => match repo_root_for(&path).and_then(|r| r.canonicalize().ok()) {
                Some(root) => {
                    self.cached_root = Some((path.clone(), root.clone()));
                    root
                }
                None => return,
            },
        };
        let rel_path = match path.strip_prefix(&repo_root) {
            Ok(p) => p,
            Err(_) => {
                self.enabled = false;
                return;
            }
        };

        let Ok(current) = std::str::from_utf8(source) else {
            return;
        };

        match head_blob(&repo_root, rel_path) {
            Some(base) => {
                self.compute_diff(&base, current);
                self.enabled = true;
            }
            None => {
                // File is untracked: every line is added.
                let current_line_count = current.lines().count();
                self.statuses = vec![LineStatus::Added; current_line_count];
                self.enabled = true;
            }
        }
        self.dirty = false;
    }

    /// Return the status for a given new-line index.
    pub fn status(&self, line: usize) -> LineStatus {
        self.statuses
            .get(line)
            .copied()
            .unwrap_or(LineStatus::Unchanged)
    }

    /// Return the removed blocks anchored exactly at `line` (blocks are
    /// sorted by anchor line, so this is a slice, not a fresh vec).
    pub fn removed_blocks_before(&self, line: usize) -> &[RemovedBlock] {
        let end = self.removed.partition_point(|b| b.before_line <= line);
        let start = self.removed[..end].partition_point(|b| b.before_line < line);
        &self.removed[start..end]
    }

    /// Return the total number of removed lines anchored at `line`.
    pub fn removed_count_before(&self, line: usize) -> usize {
        self.removed_by_line.get(&line).copied().unwrap_or(0)
    }

    /// Return the computed hunks.
    pub fn hunks(&self) -> &[Hunk] {
        &self.hunks
    }

    /// Return `(added, modified, removed)` counts.
    pub fn summary(&self) -> (usize, usize, usize) {
        let mut added = 0;
        let mut modified = 0;
        for &s in &self.statuses {
            match s {
                LineStatus::Added => added += 1,
                LineStatus::Modified => modified += 1,
                LineStatus::Unchanged => {}
            }
        }
        let removed: usize = self.removed_by_line.values().sum();
        (added, modified, removed)
    }

    /// Whether the gutter has any data to show.
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Whether the current state is stale and should be refreshed.
    pub fn dirty(&self) -> bool {
        self.dirty
    }

    /// Mark the gutter stale (call after buffer edits).
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    fn clear(&mut self) {
        self.statuses.clear();
        self.removed.clear();
        self.removed_by_line.clear();
        self.hunks.clear();
        self.enabled = false;
    }

    fn push_removed(&mut self, before_line: usize, count: usize, lines: Vec<String>) {
        *self.removed_by_line.entry(before_line).or_insert(0) += count;
        self.removed.push(RemovedBlock {
            before_line,
            count,
            lines,
        });
    }

    fn compute_diff(&mut self, base: &str, current: &str) {
        self.statuses.clear();
        self.removed.clear();
        self.removed_by_line.clear();

        let base_lines: Vec<&str> = base.lines().collect();
        let current_lines: Vec<&str> = current.lines().collect();
        let current_line_count = current_lines.len();
        self.statuses = vec![LineStatus::Unchanged; current_line_count];

        let diff = similar::TextDiff::configure()
            .algorithm(Algorithm::Patience)
            .diff_slices(&base_lines, &current_lines);

        for op in diff.ops() {
            match *op {
                DiffOp::Equal { .. } => {}
                DiffOp::Delete {
                    old_index,
                    old_len,
                    new_index,
                } => {
                    let count = old_len.max(1);
                    let lines: Vec<String> = base_lines[old_index..old_index + old_len]
                        .iter()
                        .map(|s| s.to_string())
                        .collect();
                    self.push_removed(new_index, count, lines);
                }
                DiffOp::Insert {
                    old_index: _,
                    new_index,
                    new_len,
                } => {
                    for i in new_index..new_index + new_len {
                        if let Some(s) = self.statuses.get_mut(i) {
                            *s = LineStatus::Added;
                        }
                    }
                }
                DiffOp::Replace {
                    old_index,
                    old_len,
                    new_index,
                    new_len,
                } => {
                    let paired = old_len.min(new_len);
                    for i in new_index..new_index + paired {
                        if let Some(s) = self.statuses.get_mut(i) {
                            *s = LineStatus::Modified;
                        }
                    }
                    if new_len > paired {
                        for i in new_index + paired..new_index + new_len {
                            if let Some(s) = self.statuses.get_mut(i) {
                                *s = LineStatus::Added;
                            }
                        }
                    }
                    if old_len > paired {
                        let lines: Vec<String> = base_lines
                            [old_index + paired..old_index + old_len]
                            .iter()
                            .map(|s| s.to_string())
                            .collect();
                        self.push_removed(new_index, old_len - paired, lines);
                    }
                }
            }
        }

        self.compute_hunks(current_line_count);
    }

    fn compute_hunks(&mut self, current_line_count: usize) {
        self.hunks.clear();
        if current_line_count == 0 {
            return;
        }

        let mut start: Option<usize> = None;
        for line in 0..=current_line_count {
            // The `line == current_line_count` sentinel catches a
            // removed-only block at EOF.
            let changed = self.removed_by_line.contains_key(&line)
                || (line < current_line_count
                    && self.statuses[line] != LineStatus::Unchanged);

            if changed && start.is_none() {
                start = Some(line);
            } else if !changed && start.is_some() {
                let s = start.unwrap();
                let e = if s == current_line_count {
                    s
                } else {
                    line.saturating_sub(1)
                };
                self.hunks.push(Hunk {
                    start_line: s,
                    end_line: e,
                });
                start = None;
            }
        }
    }
}

/// Run `git rev-parse --show-toplevel` from `path`'s directory.
pub(crate) fn repo_root_for(path: &Path) -> Option<PathBuf> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let output = Command::new("git")
        .arg("rev-parse")
        .arg("--show-toplevel")
        .current_dir(dir)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let s = String::from_utf8(output.stdout).ok()?;
    Some(PathBuf::from(s.trim()))
}

/// Read the `HEAD` blob for `rel_path` inside `repo_root`.
fn head_blob(repo_root: &Path, rel_path: &Path) -> Option<String> {
    let spec = format!("HEAD:{}", rel_path.to_string_lossy().replace('\\', "/"));
    let output = Command::new("git")
        .arg("show")
        .arg(&spec)
        .current_dir(repo_root)
        .output()
        .ok()?;
    if !output.status.success() || output.stdout.len() > MAX_HEAD_BYTES {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;

    fn temp_repo(name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("skerry_git_gutter_{}_{}", std::process::id(), name));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        run_git(&dir, &["init", "-q"]);
        run_git(&dir, &["config", "user.email", "test@example.com"]);
        run_git(&dir, &["config", "user.name", "Test"]);
        let file_path = dir.join("file.txt");
        (dir, file_path)
    }

    fn run_git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .expect("git should be installed for tests");
        assert!(status.success(), "git {:?} failed", args);
    }

    fn src(text: &str) -> Arc<Vec<u8>> {
        Arc::new(text.as_bytes().to_vec())
    }

    #[test]
    fn unchanged_file_has_all_unchanged() {
        let (dir, path) = temp_repo("unchanged");
        fs::write(&path, "line1\nline2\nline3\n").unwrap();
        run_git(&dir, &["add", "file.txt"]);
        run_git(&dir, &["commit", "-m", "initial"]);

        let mut gutter = GitGutter::new();
        gutter.refresh(Some(&path), &src("line1\nline2\nline3\n"));
        assert!(gutter.enabled());
        assert_eq!(gutter.status(0), LineStatus::Unchanged);
        assert_eq!(gutter.status(1), LineStatus::Unchanged);
        assert_eq!(gutter.status(2), LineStatus::Unchanged);
        assert!(gutter.hunks().is_empty());
    }

    #[test]
    fn added_lines_are_marked() {
        let (dir, path) = temp_repo("added");
        fs::write(&path, "line1\nline3\n").unwrap();
        run_git(&dir, &["add", "file.txt"]);
        run_git(&dir, &["commit", "-m", "initial"]);

        let mut gutter = GitGutter::new();
        gutter.refresh(Some(&path), &src("line1\nline2\nline3\n"));
        assert_eq!(gutter.status(0), LineStatus::Unchanged);
        assert_eq!(gutter.status(1), LineStatus::Added);
        assert_eq!(gutter.status(2), LineStatus::Unchanged);
        assert_eq!(gutter.summary(), (1, 0, 0));
    }

    #[test]
    fn modified_lines_are_marked() {
        let (dir, path) = temp_repo("modified");
        fs::write(&path, "old1\nold2\n").unwrap();
        run_git(&dir, &["add", "file.txt"]);
        run_git(&dir, &["commit", "-m", "initial"]);

        let mut gutter = GitGutter::new();
        gutter.refresh(Some(&path), &src("new1\nold2\n"));
        assert_eq!(gutter.status(0), LineStatus::Modified);
        assert_eq!(gutter.status(1), LineStatus::Unchanged);
        assert_eq!(gutter.summary(), (0, 1, 0));
    }

    #[test]
    fn deleted_lines_create_removed_block() {
        let (dir, path) = temp_repo("deleted");
        fs::write(&path, "line1\nline2\nline3\n").unwrap();
        run_git(&dir, &["add", "file.txt"]);
        run_git(&dir, &["commit", "-m", "initial"]);

        let mut gutter = GitGutter::new();
        gutter.refresh(Some(&path), &src("line1\nline3\n"));
        assert_eq!(gutter.status(0), LineStatus::Unchanged);
        assert_eq!(gutter.status(1), LineStatus::Unchanged);
        assert_eq!(gutter.summary(), (0, 0, 1));
        let removed = gutter.removed_blocks_before(1);
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].count, 1);
        assert_eq!(removed[0].lines, vec!["line2"]);
    }

    #[test]
    fn untracked_file_is_all_added() {
        let (_dir, path) = temp_repo("untracked");
        // Commit nothing; file is untracked.
        fs::write(&path, "a\nb\n").unwrap();

        let mut gutter = GitGutter::new();
        gutter.refresh(Some(&path), &src("a\nb\n"));
        assert!(gutter.enabled());
        assert_eq!(gutter.status(0), LineStatus::Added);
        assert_eq!(gutter.status(1), LineStatus::Added);
        assert_eq!(gutter.summary(), (2, 0, 0));
    }

    #[test]
    fn hunk_navigation_computes_ranges() {
        let (dir, path) = temp_repo("hunks");
        fs::write(&path, "a\nb\nc\nd\ne\n").unwrap();
        run_git(&dir, &["add", "file.txt"]);
        run_git(&dir, &["commit", "-m", "initial"]);

        let mut gutter = GitGutter::new();
        gutter.refresh(Some(&path), &src("a\nB\nc\nD\ne\n"));
        let hunks = gutter.hunks().to_vec();
        assert_eq!(hunks.len(), 2);
        assert_eq!(hunks[0].start_line, 1);
        assert_eq!(hunks[0].end_line, 1);
        assert_eq!(hunks[1].start_line, 3);
        assert_eq!(hunks[1].end_line, 3);
    }

    #[test]
    fn non_repo_file_is_disabled() {
        let dir =
            std::env::temp_dir().join(format!("skerry_no_git_{}_{}", std::process::id(), "x"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("file.txt");
        fs::write(&path, "hello").unwrap();

        let mut gutter = GitGutter::new();
        gutter.refresh(Some(&path), &src("hello"));
        assert!(!gutter.enabled());
    }
}
