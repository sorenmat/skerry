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

/// Per-document view state that both frontends share.
///
/// Only fields that are meaningful to BOTH the TUI and the GUI live
/// here. Renderer-specific bits stay on the frontend `App` struct
/// because only that frontend knows how to use them.
#[derive(Clone, Debug)]
pub struct ViewState {
    /// Horizontal scroll offset in character columns. 0 = the line
    /// starts at the left edge of the content area. Lines longer
    /// than the viewport get clipped on the left when `scroll_x_cols`
    /// is positive. Both frontends read this.
    pub scroll_x_cols: usize,
    /// First visible line in the viewport. 0 = the document's first
    /// line is at the top of the content area. Lives on the document
    /// (not the frontend) so switching tabs preserves each doc's
    /// scroll position — same fix pattern as `scroll_x_cols`. Both
    /// frontends read/write this; the GUI's `egui::ScrollArea`
    /// additionally manages its own scroll offset keyed by document
    /// id, so this field is the source of truth that the GUI re-syncs
    /// to when the user switches documents.
    pub scroll_top_line: usize,
    /// Last cursor byte position the renderer observed for THIS doc.
    /// Lives on the document (not the app) so tab switches don't
    /// trigger spurious "cursor moved" events that would scroll the
    /// freshly-activated doc to ITS cursor even when the user just
    /// wanted to peek at it without disturbing its scroll. The
    /// TUI's `adjust_viewport` doesn't read this (its cursor-following
    /// clamp always runs in `render`), but the GUI's
    /// `scroll_to_rect` logic does.
    pub last_seen_cursor: usize,
    /// Scroll margin (Emacs `scroll-margin`). When the cursor moves
    /// within this many lines of the viewport's top or bottom row, the
    /// view pre-emptively scrolls so the cursor stops at this margin
    /// from the edge (with N rows of buffer still visible above /
    /// below). The user lands the cursor near the bottom and
    /// continues pressing Down without the view jumping at the
    /// last visible row.
    ///
    /// **Default 3** — matches common Emacs configurations of
    /// `scroll-margin: 3`. Both frontends fall back to legacy
    /// edge-stick (margin = 0) when the viewport is too small for
    /// the safe zone to fit (i.e. when `2 * margin + 1 >= vh`), so
    /// a non-zero default never causes "scroll on every keypress" in
    /// a tiny window. Set to 0 to opt into the legacy v0.1 behaviour
    /// (scroll only when the cursor actually leaves the viewport).
    /// Per-document so different reading modes can use different
    /// settings.
    pub scroll_margin_lines: usize,
    /// Indent mode for this document. When `use_spaces` is true,
    /// pressing Tab inserts `tab_width` space characters; when false,
    /// it inserts a single `\t`. Affects ONLY what Tab produces —
    /// the renderer doesn't expand existing `\t` characters in the
    /// buffer to `tab_width` columns (a v2 polish; for v1 the user
    /// gets "what does Tab insert?" which is what 99% of indent
    /// settings control in editors).
    ///
    /// Defaults: spaces + width 4. Matches the de-facto standard
    /// for most modern codebases. Per-document so different files
    /// can use different conventions without re-configuring.
    pub use_spaces: bool,
    pub tab_width: usize,
    /// Soft-wrap toggle. When true, long lines render on multiple
    /// visual rows without inserting newlines (the buffer is
    /// unchanged). When false (the default), long lines extend
    /// past the right edge and the user scrolls horizontally with
    /// Shift+wheel (or the GUI's per-doc scroll offset).
    ///
    /// Per-document so files with mixed wrap preferences (e.g.
    /// Markdown prose vs. code) keep their own setting.
    pub soft_wrap: bool,
}

impl Default for ViewState {
    fn default() -> Self {
        Self {
            scroll_x_cols: 0,
            scroll_top_line: 0,
            last_seen_cursor: 0,
            scroll_margin_lines: 3,
            use_spaces: true,
            tab_width: 4,
            soft_wrap: false,
        }
    }
}

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

    /// Per-document view state shared across frontends. See
    /// [`ViewState`] for what's in here and why.
    pub view: ViewState,

    /// Per-line syntax token cache. Lazily populated by the renderer,
    /// invalidated on every edit. See [`crate::SyntaxCache`].
    pub syntax: crate::SyntaxCache,
}

impl Document {
    /// Wrap a buffer in a `Document`. The buffer's `source_path`
    /// becomes the document's display path. View state starts at
    /// defaults (no horizontal scroll).
    pub fn new(buffer: Box<dyn Buffer>) -> Self {
        Self {
            buffer,
            view: ViewState::default(),
            syntax: crate::SyntaxCache::default(),
        }
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
