//! `Document` — a buffer held inside an editor session.
//!
//! Today this is a thin wrapper around a [`Buffer`]. It exists as its
//! own type so the session (`App`) can carry a list of buffers with a
//! stable identity independent of any single buffer's lifetime, and
//! so future per-document state (search history, view markers,
//! per-file undo groups, etc.) has an obvious home without another
//! refactor.
//!
//! See ADR 0005 — multi-buffer/workspace lives in core scope from
//! day one; CONTEXT.md defines the term.

use std::path::{Path, PathBuf};

use crate::Buffer;

/// One document open in the session.
///
/// Owns its [`Buffer`] exclusively. Closing a `Document` drops the
/// buffer (after save, if requested); switching the active document
/// keeps all other documents alive.
pub struct Document {
    /// The text content and cursor/selection state. The piece-table
    /// implementation is the only concrete `Buffer` today; the
    /// indirection through `Box<dyn Buffer>` lets us swap
    /// implementations later (rope, in-memory only, etc.) without
    /// touching `Document`'s API.
    pub buffer: Box<dyn Buffer>,
}

impl Document {
    /// Wrap a buffer in a `Document`. The buffer's `source_path`
    /// becomes the document's display path.
    pub fn new(buffer: Box<dyn Buffer>) -> Self {
        Self { buffer }
    }

    /// Create a fresh, empty, unsaved document.
    pub fn empty() -> Self {
        Self::new(Box::new(crate::PieceTableBuffer::new()))
    }

    /// The path the buffer was loaded from, if any. Convenience
    /// wrapper around [`Buffer::source_path`] so callers don't have
    /// to go through the inner buffer.
    pub fn path(&self) -> Option<&Path> {
        self.buffer.source_path()
    }

    /// Whether the document has unsaved edits.
    pub fn is_dirty(&self) -> bool {
        self.buffer.is_dirty()
    }

    /// Convenience for the common "display name" need — basename for
    /// saved docs, `[No Name]` for unsaved ones. Renderers should use
    /// this in tab bars and titles.
    pub fn display_name(&self) -> String {
        match self.path() {
            Some(p) => p
                .file_name()
                .and_then(|n| n.to_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| p.display().to_string()),
            None => "[No Name]".to_string(),
        }
    }

    /// Owning clone of the path, if any. Useful when passing to
    /// [`crate::PieceTableBuffer::from_path`] on reopen or when
    /// building a file-picker UI later.
    pub fn path_buf(&self) -> Option<PathBuf> {
        self.path().map(|p| p.to_path_buf())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PieceTableBuffer;

    fn buffer_with(text: &str) -> Box<dyn Buffer> {
        Box::new(PieceTableBuffer::from_bytes(text.as_bytes().to_vec()))
    }

    #[test]
    fn unsaved_document_has_no_path() {
        let doc = Document::empty();
        assert!(doc.path().is_none());
        assert_eq!(doc.display_name(), "[No Name]");
        assert!(!doc.is_dirty());
    }

    #[test]
    fn path_proxies_to_buffer() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("the_editor_doc_test_{}.txt", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let buf: Box<dyn Buffer> = Box::new(PieceTableBuffer::from_bytes_with_path(
            b"hello".to_vec(),
            path.clone(),
        ));
        let doc = Document::new(buf);
        assert_eq!(doc.path(), Some(path.as_path()));
        assert_eq!(doc.display_name(), path.file_name().unwrap().to_str().unwrap());
        assert_eq!(doc.path_buf(), Some(path));
    }

    #[test]
    fn dirty_flag_proxies_to_buffer() {
        let mut doc = Document::empty();
        assert!(!doc.is_dirty());
        doc.buffer.insert(0, "x").unwrap();
        assert!(doc.is_dirty());
    }
}
