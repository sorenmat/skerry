//! Cross-platform file watching for externally changed files.
//!
//! Wraps `notify` with a small synchronous API that the frontends can
//! poll each frame/iteration. We watch the parent directories of the
//! tracked files rather than the files themselves: this works reliably
//! on macOS (fsevents) and Linux (inotify), whereas watching individual
//! files can be flaky or unsupported on some platforms.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;

use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

/// Best-effort canonicalization. Used so that paths obtained from the
/// application (which may contain symlinks such as `/var` on macOS)
/// match event paths reported by the OS.
fn normalize_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// A file-system watcher that reports changes to watched files.
pub struct FileWatcher {
    /// The underlying notify watcher. Kept so it stays alive.
    #[allow(dead_code)]
    watcher: RecommendedWatcher,
    /// Channel receiving change notifications.
    receiver: Receiver<FileChange>,
    /// Set of tracked file paths we care about.
    tracked_files: HashSet<PathBuf>,
    /// Set of parent directories currently being watched.
    watched_dirs: HashSet<PathBuf>,
}

/// A single externally detected file change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    /// Absolute or canonical path of the changed file.
    pub path: PathBuf,
}

impl FileWatcher {
    /// Create a new watcher. Returns an error if the platform watcher
    /// cannot be initialized.
    pub fn new() -> notify::Result<Self> {
        let (tx, receiver) = channel();
        let watcher = RecommendedWatcher::new(
            move |res: notify::Result<Event>| {
                if let Ok(event) = res {
                    if matches!(
                        event.kind,
                        EventKind::Modify(_) | EventKind::Create(_) | EventKind::Remove(_)
                    ) {
                        for path in event.paths {
                            let _ = tx.send(FileChange { path });
                        }
                    }
                }
            },
            Config::default().with_poll_interval(Duration::from_millis(500)),
        )?;
        Ok(Self {
            watcher,
            receiver,
            tracked_files: HashSet::new(),
            watched_dirs: HashSet::new(),
        })
    }

    /// Start tracking `path`. The path's parent directory is added to the
    /// watch list if it isn't already.
    pub fn watch(&mut self, path: &Path) {
        let path = normalize_path(path);
        if !self.tracked_files.insert(path.clone()) {
            return;
        }
        if let Some(parent) = path.parent() {
            let parent = normalize_path(parent);
            if self.watched_dirs.insert(parent.clone()) {
                let _ = self.watcher.watch(&parent, RecursiveMode::NonRecursive);
            }
        }
    }

    /// Stop tracking `path`. The parent directory is removed from the
    /// watch list when no other tracked file shares it.
    pub fn unwatch(&mut self, path: &Path) {
        let path = normalize_path(path);
        if !self.tracked_files.remove(&path) {
            return;
        }
        if let Some(parent) = path.parent() {
            let parent = normalize_path(parent);
            let still_needed = self
                .tracked_files
                .iter()
                .any(|p| p.parent().map(normalize_path) == Some(parent.clone()));
            if !still_needed && self.watched_dirs.remove(&parent) {
                let _ = self.watcher.unwatch(&parent);
            }
        }
    }

    /// Replace the tracked file set with `paths`, adding new files and
    /// removing stale ones.
    pub fn sync_watch_list(&mut self, paths: &[PathBuf]) {
        let new_files: HashSet<PathBuf> = paths.iter().map(|p| normalize_path(p)).collect();
        let to_untrack: Vec<PathBuf> = self.tracked_files.difference(&new_files).cloned().collect();
        let to_track: Vec<PathBuf> = new_files.difference(&self.tracked_files).cloned().collect();
        for path in to_untrack {
            self.unwatch(&path);
        }
        for path in to_track {
            self.watch(&path);
        }
    }

    /// Drain all pending change notifications without blocking. Only
    /// returns changes whose path is in the tracked set.
    pub fn poll_changes(&self) -> Vec<FileChange> {
        let mut changes = Vec::new();
        while let Ok(change) = self.receiver.try_recv() {
            let normalized = normalize_path(&change.path);
            if self.tracked_files.contains(&normalized) || self.tracked_files.contains(&change.path)
            {
                changes.push(FileChange { path: normalized });
            }
        }
        changes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn watcher_detects_file_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("watched.txt");
        fs::write(&path, "initial").unwrap();

        let mut watcher = FileWatcher::new().unwrap();
        watcher.watch(&path);

        // Give the watcher a moment to start observing.
        thread::sleep(Duration::from_millis(250));
        fs::write(&path, "changed").unwrap();

        // Poll with a timeout to avoid flakiness on slow CI.
        let canonical = normalize_path(&path);
        let mut found = false;
        for _ in 0..100 {
            let changes = watcher.poll_changes();
            if changes.iter().any(|c| c.path == canonical) {
                found = true;
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }
        assert!(found, "expected watcher to detect the file change");
    }

    #[test]
    fn sync_watch_list_adds_and_removes() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.txt");
        let b = dir.path().join("b.txt");
        fs::write(&a, "").unwrap();
        fs::write(&b, "").unwrap();

        let canonical_a = normalize_path(&a);
        let canonical_b = normalize_path(&b);

        let mut watcher = FileWatcher::new().unwrap();
        watcher.sync_watch_list(std::slice::from_ref(&a));
        assert!(watcher.tracked_files.contains(&canonical_a));
        assert!(!watcher.tracked_files.contains(&canonical_b));

        watcher.sync_watch_list(std::slice::from_ref(&b));
        assert!(!watcher.tracked_files.contains(&canonical_a));
        assert!(watcher.tracked_files.contains(&canonical_b));
    }
}
