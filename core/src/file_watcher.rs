//! Cross-platform file watching for externally changed files.
//!
//! Wraps `notify` with a small synchronous API that the frontends can
//! poll each frame/iteration. We watch the parent directories of the
//! tracked files rather than the files themselves: this works reliably
//! on macOS (fsevents) and Linux (inotify), whereas watching individual
//! files can be flaky or unsupported on some platforms.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use sha2::{Digest, Sha256};

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
    /// Signatures for writes performed by Skerry. Directory watchers
    /// also report our own atomic saves; matching events must not be
    /// treated as external changes and reload the document.
    self_writes: HashMap<PathBuf, ExpectedWrite>,
}

type ContentFingerprint = [u8; 32];

/// Signature of a write Skerry itself performed. The common case is
/// settled from metadata alone (length + mtime — a couple of cheap
/// stat syscalls, no file read); the expected bytes are kept so a
/// SHA-256 content comparison can fall back when the metadata
/// disagrees. They are shared through an `Arc`, so the save path pays
/// no copy and no hashing.
#[derive(Clone)]
pub struct ExpectedWrite {
    bytes: Arc<Vec<u8>>,
    len: u64,
    modified: Option<SystemTime>,
}

impl ExpectedWrite {
    /// Build a signature from the bytes the editor just saved and the
    /// file metadata read immediately after the save.
    pub fn new(bytes: Arc<Vec<u8>>, metadata: &std::fs::Metadata) -> Self {
        Self {
            bytes,
            len: metadata.len(),
            modified: metadata.modified().ok(),
        }
    }
}

// Documents can be many gigabytes; never print their bytes in logs.
impl std::fmt::Debug for ExpectedWrite {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExpectedWrite")
            .field("len", &self.len)
            .field("modified", &self.modified)
            .finish()
    }
}

fn content_fingerprint(bytes: &[u8]) -> ContentFingerprint {
    Sha256::digest(bytes).into()
}

fn file_fingerprint(path: &Path, expected_len: u64) -> Option<ContentFingerprint> {
    let mut file = std::fs::File::open(path).ok()?;
    if file.metadata().ok()?.len() != expected_len {
        return None;
    }
    let mut hasher = Sha256::new();
    let mut chunk = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut chunk).ok()?;
        if read == 0 {
            return Some(hasher.finalize().into());
        }
        hasher.update(&chunk[..read]);
    }
}

/// Whether the on-disk state of `path` still matches `expected` — i.e.
/// the pending change is our own save rather than an external one.
///
/// Fast path: length and mtime are identical to the post-save stat —
/// no file read, no hashing. When the metadata disagrees (an external
/// writer raced the save, or a filesystem with coarse mtime
/// granularity) we fall back to a SHA-256 content comparison so a
/// same-length external overwrite is still reported.
fn is_ours_on_disk(path: &Path, expected: &ExpectedWrite) -> bool {
    let meta = match std::fs::metadata(path).ok() {
        Some(m) => m,
        None => return false,
    };
    if meta.len() == expected.len && meta.modified().ok() == expected.modified {
        return true;
    }
    file_fingerprint(path, expected.len) == Some(content_fingerprint(&expected.bytes))
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
            self_writes: HashMap::new(),
        })
    }

    /// Record a successful editor save so its watcher notifications are
    /// ignored. The expected bytes come from the buffer (shared as an
    /// `Arc` — no copy, no hashing here) and the metadata from a
    /// post-save read, so an external writer that raced the save cannot
    /// be mistaken for Skerry.
    pub fn acknowledge_write(&mut self, path: &Path, expected: ExpectedWrite) {
        let path = normalize_path(path);
        self.self_writes.insert(path, expected);
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
        self.self_writes.remove(&path);
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
    pub fn poll_changes(&mut self) -> Vec<FileChange> {
        let mut changes = Vec::new();
        // An atomic save yields several events per batch, so cache the
        // self-write decision per path to stat/hash at most once each.
        let mut self_write_decisions: HashMap<PathBuf, bool> = HashMap::new();
        while let Ok(change) = self.receiver.try_recv() {
            let normalized = normalize_path(&change.path);
            if self.tracked_files.contains(&normalized) || self.tracked_files.contains(&change.path)
            {
                if let Some(expected) = self.self_writes.get(&normalized).cloned() {
                    let ours = *self_write_decisions
                        .entry(normalized.clone())
                        .or_insert_with(|| is_ours_on_disk(&normalized, &expected));
                    if ours {
                        continue;
                    }
                    // A different on-disk value is genuinely external. Drop
                    // the stale acknowledgement before reporting it so future
                    // events cannot be suppressed accidentally.
                    self.self_writes.remove(&normalized);
                }
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

    #[test]
    fn acknowledged_editor_write_is_not_reported_as_external() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("self-save.txt");
        fs::write(&path, "initial").unwrap();
        let mut watcher = FileWatcher::new().unwrap();
        watcher.watch(&path);
        thread::sleep(Duration::from_millis(250));

        let saved = b"saved by editor";
        fs::write(&path, saved).unwrap();
        let meta = fs::metadata(&path).unwrap();
        watcher.acknowledge_write(
            &path,
            ExpectedWrite::new(Arc::new(saved.to_vec()), &meta),
        );

        for _ in 0..20 {
            assert!(
                watcher.poll_changes().is_empty(),
                "an acknowledged save must not look external"
            );
            thread::sleep(Duration::from_millis(25));
        }

        fs::write(&path, "a genuinely external and different update").unwrap();
        let canonical = normalize_path(&path);
        let mut found = false;
        for _ in 0..100 {
            if watcher
                .poll_changes()
                .iter()
                .any(|change| change.path == canonical)
            {
                found = true;
                break;
            }
            thread::sleep(Duration::from_millis(25));
        }
        assert!(found, "a later external write must still be reported");
    }

    #[test]
    fn external_overwrite_between_save_and_acknowledgement_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("save-race.txt");
        fs::write(&path, "old value").unwrap();
        let mut watcher = FileWatcher::new().unwrap();
        watcher.watch(&path);
        thread::sleep(Duration::from_millis(250));

        let editor_bytes = b"editor123";
        fs::write(&path, editor_bytes).unwrap();
        // Skerry captures the metadata right after its own save, before
        // the external writer wins the race.
        let editor_meta = fs::metadata(&path).unwrap();
        let editor_modified = editor_meta.modified().unwrap();

        // Simulate another process winning the race before Skerry records
        // its successful save. Same length as the editor's write, but a
        // fresh mtime — so only the metadata mismatch and the content
        // fallback hash can expose it. (An external writer that also
        // forged the mtime would be indistinguishable from our own save;
        // that case is deliberately accepted.)
        fs::write(&path, "outside12").unwrap();
        let file = fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.set_times(
            std::fs::FileTimes::new().set_modified(editor_modified + Duration::from_secs(1)),
        )
        .unwrap();
        watcher.acknowledge_write(
            &path,
            ExpectedWrite::new(Arc::new(editor_bytes.to_vec()), &editor_meta),
        );

        let canonical = normalize_path(&path);
        let mut found = false;
        for _ in 0..100 {
            if watcher
                .poll_changes()
                .iter()
                .any(|change| change.path == canonical)
            {
                found = true;
                break;
            }
            thread::sleep(Duration::from_millis(25));
        }
        assert!(
            found,
            "the external winner must not be acknowledged as ours"
        );
        assert!(!watcher.self_writes.contains_key(&canonical));
    }
}
