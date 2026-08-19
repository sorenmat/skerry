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
use std::sync::atomic::{AtomicU64, Ordering};

use crate::Buffer;

static NEXT_DOCUMENT_ID: AtomicU64 = AtomicU64::new(1);

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
    /// Whether the git gutter is rendered for this document.
    pub git_gutter_enabled: bool,
    /// Whether inline git blame is rendered for this document.
    pub git_blame_enabled: bool,
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
            git_gutter_enabled: true,
            git_blame_enabled: false,
        }
    }
}

/// One document open in the session.
///
/// Owns its [`Buffer`] exclusively. Closing a `Document` drops the
/// buffer (after save, if requested); switching the active document
/// keeps all other documents alive.
pub struct Document {
    /// Process-local identity used by frontend caches and widget state.
    id: u64,
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

    /// Per-document tree-sitter parse tree, kept current via incremental
    /// reparsing on edits. `None` when the document's language has no
    /// bundled grammar (plain-text rendering). Used by the tree-sitter
    /// highlighter (phase 3); built here in phase 2 so it is correct and
    /// current before highlighting depends on it.
    pub ts_tree: Option<crate::ts::DocTree>,

    /// The detected project/workspace for this document, if any.
    /// Derived from the buffer path by walking ancestors and looking
    /// for project markers (`.git`, `Cargo.toml`, etc.). Lives on the
    /// document so switching tabs keeps each file's project context.
    pub project: Option<crate::Project>,
    /// Whether the file has changed on disk since it was loaded or last
    /// saved by this editor. Set by the file watcher; cleared on reload.
    pub external_change: bool,
    /// Per-line git change state relative to `HEAD`.
    pub git_gutter: crate::GitGutter,
    /// Per-line git blame metadata relative to `HEAD`.
    pub git_blame: crate::GitBlame,
    /// Per-document code-fold state (which ranges are folded).
    pub folds: crate::FoldState,
    /// Memoized full-text copy of the buffer, keyed by buffer revision.
    /// Tree-sitter queries and incremental reparses need contiguous
    /// bytes, and the renderer issues one highlight query per visible
    /// line — without this cache each query paid a full-document memcpy.
    source_cache: Option<(u64, std::sync::Arc<Vec<u8>>)>,
}

impl Document {
    /// Wrap a buffer in a `Document`. The buffer's `source_path`
    /// becomes the document's display path. View state starts at
    /// defaults (no horizontal scroll).
    pub fn new(buffer: Box<dyn Buffer>) -> Self {
        Self::new_with_config(buffer, &crate::Config::default())
    }

    /// Wrap a buffer in a `Document`, applying persisted user defaults
    /// to the new document's view state.
    pub fn new_with_config(buffer: Box<dyn Buffer>, config: &crate::Config) -> Self {
        let project = buffer.source_path().and_then(crate::Project::from_path);
        let mut view = ViewState::default();
        config.apply_document_defaults(&mut view);
        let mut doc = Self {
            id: NEXT_DOCUMENT_ID.fetch_add(1, Ordering::Relaxed),
            buffer,
            view,
            syntax: crate::SyntaxCache::default(),
            ts_tree: None,
            project,
            external_change: false,
            git_gutter: crate::GitGutter::new(),
            git_blame: crate::GitBlame::new(),
            folds: crate::FoldState::new(),
            source_cache: None,
        };
        doc.init_ts_tree();
        if doc.path().is_some() {
            doc.refresh_git_gutter();
        }
        doc
    }

    /// Stable identity for this document instance.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Recompute the git gutter from the current buffer and path.
    pub fn refresh_git_gutter(&mut self) {
        let path = self.path().map(|p| p.to_path_buf());
        self.git_gutter.refresh(path.as_deref(), &*self.buffer);
    }

    /// Build the per-document tree-sitter parse tree from the buffer's
    /// current bytes. No-op for languages with no bundled grammar
    /// (`ts_tree` stays `None`, document renders as plain text).
    /// Maximum file size (in bytes) for which we build a tree-sitter parse
    /// tree on load. Above this the initial full parse could stall load;
    /// the document renders as plain text instead. Multi-MB JSON (the case
    /// that motivated this) is well under the limit — this guards against
    /// the 100 MB+ extreme.
    const TS_TREE_SIZE_LIMIT: usize = 32 * 1024 * 1024;

    pub fn init_ts_tree(&mut self) {
        let path = self.path();
        let Some(grammar) = crate::ts::grammar_for_path(path) else {
            return;
        };
        // Skip very large files: the initial full parse would stall load.
        // The doc renders as plain text; highlighting is a nice-to-have,
        // not worth a multi-second hang.
        if self.buffer.len() > Self::TS_TREE_SIZE_LIMIT {
            return;
        }
        let Some(mut tree) = crate::ts::DocTree::new(grammar) else {
            return;
        };
        // Fill the source cache with the parse copy so the first batch
        // of highlight queries reuses it instead of re-copying.
        let source = self.source_bytes();
        tree.parse(&source);
        self.ts_tree = Some(tree);
    }

    /// The full buffer contents as one contiguous byte string, memoized
    /// on the buffer revision. The piece table can be scattered across
    /// pieces (especially after edits to a memmapped file), so repeated
    /// `to_bytes` calls are full-document copies; tree-sitter needs a
    /// contiguous slice, so callers share this cached copy.
    fn source_bytes(&mut self) -> std::sync::Arc<Vec<u8>> {
        let rev = self.buffer.revision();
        if let Some((cached_rev, bytes)) = &self.source_cache {
            if *cached_rev == rev {
                return bytes.clone();
            }
        }
        let bytes = std::sync::Arc::new(self.buffer.to_bytes());
        self.source_cache = Some((rev, bytes.clone()));
        bytes
    }

    /// Apply a buffer edit to the parse tree and re-parse incrementally.
    /// Call this AFTER the buffer mutation has been committed, passing a
    /// delta that describes the change in the coordinates of the
    /// pre-edit buffer. No-op when the document has no tree-sitter tree.
    pub fn apply_ts_edit(&mut self, delta: crate::ts::EditDelta) {
        if self.ts_tree.is_some() {
            // The tree-sitter parser needs the post-edit source. The
            // incremental edit has already shifted the old tree, so
            // re-parsing reuses unchanged nodes.
            let source = self.source_bytes();
            if let Some(tree) = self.ts_tree.as_mut() {
                tree.apply_edit(delta, &source);
            }
        }
    }

    /// Highlight a contiguous range of lines using the tree-sitter parse
    /// tree, returning per-line color segments with **line-local** byte
    /// ranges (the format `SyntaxCache` and both renderers expect).
    ///
    /// `theme` is the active syntax theme. Returns an empty `Vec` for any
    /// line with no tree (unsupported language) or no captures — the
    /// caller renders those lines with the default text color.
    ///
    /// This highlights the whole `start..end` line range as a single
    /// tree-sitter query over the combined byte range, then buckets the
    /// resulting document-absolute segments back into per-line entries.
    /// One query for the viewport is cheaper than one-per-line and is how
    /// tree-sitter queries are meant to be used.
    pub fn highlight_lines_ts(
        &mut self,
        start_line: usize,
        end_line: usize,
        theme: &crate::ts::TsTheme,
    ) -> (Vec<Vec<crate::ColorSegment>>, bool) {
        let count = end_line.saturating_sub(start_line);
        let mut per_line = vec![Vec::new(); count];

        let Some(grammar) = crate::ts::grammar_for_path(self.path()) else {
            return (per_line, true);
        };

        // Byte range covering start_line..=end_line. We extend one line
        // past end_line (if it exists) so captures that straddle a line
        // boundary aren't clipped at the bottom edge.
        let Some(first_range) = self.buffer.line_byte_range(start_line) else {
            return (per_line, true);
        };
        let last_end = self
            .buffer
            .line_byte_range(end_line)
            .map(|r| r.end)
            .unwrap_or_else(|| self.buffer.len());
        let source = self.source_bytes();
        let Some(tree) = self.ts_tree.as_ref() else {
            return (per_line, true);
        };
        let result = crate::ts::highlight_doc_range(
            tree,
            &grammar,
            theme,
            first_range.start..last_end,
            &source,
        );

        // Bucket document-absolute segments into their lines, translating
        // ranges to line-local byte offsets.
        let mut line_starts: Vec<usize> = Vec::with_capacity(count);
        for i in start_line..end_line {
            let start = self
                .buffer
                .line_byte_range(i)
                .map(|r| r.start)
                .unwrap_or(last_end);
            line_starts.push(start);
        }
        for seg in result.segments {
            // Find which line this segment starts on.
            let rel = seg.range.start.saturating_sub(first_range.start);
            let line_offset = match line_starts.iter().position(|&s| s > seg.range.start) {
                Some(idx) => idx.saturating_sub(1),
                None => count.saturating_sub(1),
            };
            let _ = rel; // kept for clarity; line_offset is what we use
            if line_offset >= count {
                continue;
            }
            let line_start = line_starts[line_offset];
            let local_start = seg.range.start.saturating_sub(line_start);
            let local_end = seg.range.end.saturating_sub(line_start);
            if local_end <= local_start {
                continue;
            }
            per_line[line_offset].push(crate::ColorSegment {
                range: local_start..local_end,
                color: seg.color,
            });
        }

        (per_line, result.complete)
    }

    /// Create a fresh, empty, unsaved document.
    pub fn empty() -> Self {
        Self::new(Box::new(crate::PieceTableBuffer::new()))
    }

    /// Re-detect the project root from the current buffer path. Call
    /// this after the buffer's source path changes (e.g. Save-As to a
    /// different directory).
    pub fn refresh_project(&mut self) {
        self.project = self
            .buffer
            .source_path()
            .and_then(crate::Project::from_path);
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

    /// LSP language id inferred from the file extension, if any.
    pub fn language_id(&self) -> Option<&'static str> {
        self.path()
            .and_then(|p| p.extension())
            .and_then(|e| e.to_str())
            .and_then(language_id_from_extension)
    }

    /// `file://` URI for this document, if it has a path.
    pub fn uri(&self) -> Option<url::Url> {
        self.path().and_then(|p| url::Url::from_file_path(p).ok())
    }

    /// Full document text as a UTF-8 string. Invalid UTF-8 sequences
    /// are replaced with the Unicode replacement character so the LSP
    /// client always has a valid `String` to send.
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.buffer.to_bytes()).to_string()
    }

    /// Root URI to pass to the LSP server. Uses the project root when
    /// known, otherwise the file's parent directory.
    pub fn lsp_root_uri(&self) -> Option<url::Url> {
        if let Some(project) = &self.project {
            return url::Url::from_file_path(&project.root).ok();
        }
        self.path()
            .and_then(|p| p.parent())
            .and_then(|p| url::Url::from_file_path(p).ok())
    }
}

fn language_id_from_extension(ext: &str) -> Option<&'static str> {
    let ext = ext.to_ascii_lowercase();
    match ext.as_str() {
        "rs" => Some("rust"),
        "go" => Some("go"),
        "js" => Some("javascript"),
        "ts" => Some("typescript"),
        "tsx" => Some("typescriptreact"),
        "jsx" => Some("javascriptreact"),
        "py" => Some("python"),
        "c" => Some("c"),
        "cpp" | "cc" | "cxx" | "hpp" => Some("cpp"),
        "h" => Some("c"),
        "json" => Some("json"),
        "yaml" | "yml" => Some("yaml"),
        "sh" | "bash" | "zsh" => Some("shellscript"),
        "toml" => Some("toml"),
        "html" | "htm" => Some("html"),
        "css" => Some("css"),
        "md" | "markdown" => Some("markdown"),
        "csv" => Some("csv"),
        _ => None,
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
    fn documents_have_distinct_stable_ids() {
        let first = Document::empty();
        let second = Document::empty();
        assert_ne!(first.id(), second.id());
        assert_eq!(first.id(), first.id());
    }

    #[test]
    fn path_proxies_to_buffer() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("skerry_doc_test_{}.txt", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let buf: Box<dyn Buffer> = Box::new(PieceTableBuffer::from_bytes_with_path(
            b"hello".to_vec(),
            path.clone(),
        ));
        let doc = Document::new(buf);
        assert_eq!(doc.path(), Some(path.as_path()));
        assert_eq!(
            doc.display_name(),
            path.file_name().unwrap().to_str().unwrap()
        );
        assert_eq!(doc.path_buf(), Some(path));
    }

    #[test]
    fn dirty_flag_proxies_to_buffer() {
        let mut doc = Document::empty();
        assert!(!doc.is_dirty());
        doc.buffer.insert(0, "x").unwrap();
        assert!(doc.is_dirty());
    }

    #[test]
    fn language_id_from_path_extension() {
        let cases = &[
            ("main.rs", Some("rust")),
            ("main.go", Some("go")),
            ("main.ts", Some("typescript")),
            ("main.tsx", Some("typescriptreact")),
            ("main.js", Some("javascript")),
            ("main.jsx", Some("javascriptreact")),
            ("main.py", Some("python")),
            ("main.cpp", Some("cpp")),
            ("main.c", Some("c")),
            ("README.md", Some("markdown")),
            ("notes.markdown", Some("markdown")),
            ("data.csv", Some("csv")),
            ("index.html", Some("html")),
            ("page.htm", Some("html")),
            ("styles.css", Some("css")),
            ("deploy.sh", Some("shellscript")),
            ("run.bash", Some("shellscript")),
            (".zshrc", None), // dotfile: no extension to map
            ("readme.txt", None),
        ];
        for (name, expected) in cases {
            let path = std::env::temp_dir().join(name);
            let buf: Box<dyn Buffer> = Box::new(PieceTableBuffer::from_bytes_with_path(
                b"".to_vec(),
                path.clone(),
            ));
            let doc = Document::new(buf);
            assert_eq!(doc.language_id(), *expected, "for {name}");
        }
    }
}
