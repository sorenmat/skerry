//! Frontend-agnostic editor events.
//!
//! Both frontends (TUI via crossterm, GUI via winit/egui) translate their
//! native input events into [`EditorEvent`]. The handling logic against
//! the `Buffer` is identical for both — see ADR 0005 (full parity).
//!
//! The translation functions (`translate_key`, etc.) live with each
//! frontend because they depend on the frontend's native input types.
//! The type itself lives here so both frontends see the same shape.

use std::path::PathBuf;

use crate::BytePos;

/// An input event the editor acts on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditorEvent {
    /// Insert a character at the cursor position. If the selection is
    /// non-empty, the selection is replaced by the inserted character.
    Insert(char),
    /// Insert an indent at the cursor. The App resolves this to
    /// either a literal `\t` or `tab_width` space characters based
    /// on the active document's indent mode (see `ViewState`).
    /// Selection-replacement is the same as `Insert`.
    InsertTab,
    /// Delete the character to the left of the cursor (Backspace). If
    /// the selection is non-empty, the entire selection is deleted
    /// instead — same as `DeleteSelection`.
    DeleteLeft,
    /// Delete the character at the cursor (Delete key). If the selection
    /// is non-empty, the entire selection is deleted instead.
    DeleteRight,
    /// Delete the current selection. No-op when the selection is
    /// collapsed (cursor only).
    DeleteSelection,
    /// Delete the word to the left of the cursor (Ctrl+Backspace).
    /// Uses the same word boundary definition as `Movement::WordLeft`.
    DeleteWordLeft,
    /// Delete the word to the right of the cursor (Ctrl+Delete).
    DeleteWordRight,
    /// Delete the current line (Ctrl+K or Ctrl+Shift+K). The trailing
    /// newline is removed too, so two lines collapse into one.
    DeleteLine,
    /// Duplicate the current line (Ctrl+D or Ctrl+Shift+D). The cursor
    /// moves to the start of the new copy.
    DuplicateLine,
    /// Move the current line up by one (Alt+Up). Swaps with the
    /// previous line.
    MoveLineUp,
    /// Move the current line down by one (Alt+Down).
    MoveLineDown,
    /// Scroll the viewport one column to the left (Shift+Left).
    ScrollLeft,
    /// Scroll the viewport one column to the right (Shift+Right).
    ScrollRight,
    /// Open the find bar. No-op if it's already open.
    FindOpen,
    /// Close the find bar.
    FindClose,
    /// Replace the current find query and re-run the search. The
    /// match list is rebuilt incrementally as the user types.
    FindQueryChanged(String),
    /// Move to the next match AFTER the cursor. Wraps around at the
    /// end of the buffer. No-op if no matches.
    FindNext,
    /// Move to the previous match BEFORE the cursor. Wraps around.
    FindPrev,
    /// Toggle regex mode for the find bar. When on, the query is
    /// interpreted as a Rust `regex` pattern; when off, it is a literal
    /// substring. Default binding: Alt+R while the find bar is open.
    ToggleFindRegex,
    /// Open the replace bar (text input for the replacement string).
    /// No-op if it's already open. Closing the find bar also closes
    /// the replace bar — they're a coupled pair. Default binding:
    /// Cmd/Ctrl+R.
    ReplaceOpen,
    /// Close the replace bar.
    ReplaceClose,
    /// Replace the replace bar's text and re-run the search. The
    /// match list is rebuilt incrementally as the user types.
    ReplaceQueryChanged(String),
    /// Replace the currently-active find match with the replace
    /// query, then advance to the next match. No-op when there's no
    /// current match or the replace query is empty.
    ReplaceOne,
    /// Replace every find match with the replace query, as a single
    /// undo entry. No-op when there are no matches or the replace
    /// query is empty. v1 has no confirmation prompt — undo if you
    /// regret it.
    ReplaceAll,
    /// Set the indent mode for the active document. When `use_spaces`
    /// is true, the Tab key inserts `tab_width` spaces; when false,
    /// it inserts a single `\t`. Affects Tab-key behaviour only —
    /// existing `\t` characters in the buffer are not re-rendered.
    /// Default binding: Cmd/Ctrl+I cycles through
    /// (spaces:2, spaces:4, spaces:8, tabs).
    SetIndentMode { use_spaces: bool, tab_width: usize },
    /// Cycle the indent mode of the active document. Walks through
    /// the four common presets: spaces:2 → spaces:4 → spaces:8 →
    /// tabs (width 4) → spaces:2. No-op when no documents are open.
    /// Default binding: Cmd/Ctrl+I.
    CycleIndentMode,
    /// Toggle soft-wrap on the active document. Long lines render
    /// on multiple visual rows when on; horizontal scroll when off.
    /// No-op when no documents are open. Default binding:
    /// Cmd/Ctrl+Shift+W (W for "wrap" — Cmd+W is close).
    ToggleSoftWrap,
    /// Cycle to the next syntax-highlighting theme. Wraps around at
    /// the end of the bundled theme list. Invalidates the syntax cache
    /// for every open document so the new colors are visible
    /// immediately. Default binding: F5.
    CycleTheme,
    /// Move the cursor. Selection is collapsed to the new position.
    Move(Movement),
    /// Extend the selection by moving one end of it.
    SelectExtend(Movement),
    /// Set the cursor to an absolute byte position, collapsing the
    /// selection. Used by mouse clicks.
    SetCursor { pos: BytePos },
    /// Extend the selection to an absolute byte position, leaving the
    /// anchor where it was. Used by mouse drag.
    SelectExtendTo { pos: BytePos },
    /// Insert `text` at the cursor position. If the selection is
    /// non-empty, the selection is replaced by `text`. Used for paste;
    /// the frontend reads the OS clipboard and produces this event.
    Paste(String),
    /// Save the buffer.
    Save,
    /// Save the buffer to a new path. `None` asks the frontend to show
    /// a save dialog (GUI native dialog today; TUI reports that it's not
    /// supported). `Some(path)` sets the buffer's source path and saves.
    SaveAs(Option<std::path::PathBuf>),
    /// Reload the active document from disk. If the buffer has unsaved
    /// edits, the frontend should confirm with the user first. Default
    /// binding: Ctrl+Shift+R.
    ReloadFile,
    /// Undo the most recent edit.
    Undo,
    /// Redo the most recently undone edit.
    Redo,
    /// Open a new, empty, unsaved document and switch to it.
    /// Default binding: Cmd/Ctrl+T.
    NewDoc,
    /// Close the active document. If it was the only document, the
    /// editor quits. The active index moves to the neighbour on close;
    /// if the closed document was last, the new last becomes active.
    /// Default binding: Cmd/Ctrl+W.
    ///
    /// v1: closes unconditionally — does NOT prompt on dirty buffers.
    /// A future stage will add a "save before close?" prompt.
    CloseDoc,
    /// Switch to the next document in the session, wrapping at the end.
    /// Default binding: Cmd/Ctrl+Tab.
    NextDoc,
    /// Switch to the previous document in the session, wrapping at
    /// the start. Default binding: Cmd/Ctrl+Shift+Tab.
    PrevDoc,
    /// Open a file. `None` asks the frontend to show its file picker
    /// (TUI text input today; a future GUI iteration may swap to a
    /// native dialog). `Some(path)` loads the path into the active
    /// document — the buffer is replaced, view state resets, and the
    /// document stays open. If the path does not exist yet, it is
    /// treated as a new file (the buffer is empty but `source_path`
    /// is set so the next Save writes to that path).
    ///
    /// Default binding for `None`: Cmd/Ctrl+O. The frontend's
    /// picker translates the user's selection back into
    /// `OpenFile(Some(path))`.
    OpenFile(Option<PathBuf>),
    /// Jump the cursor to a specific 1-based line. `None` opens a
    /// small input prompt; `Some(line)` performs the jump. Out-of-range
    /// line numbers are clamped to the first or last line.
    /// Default binding: Cmd/Ctrl+G.
    GoToLine(Option<usize>),
    /// Toggle the project file-tree sidebar. When the active document
    /// belongs to a project, the sidebar shows the project's files and
    /// lets the user open them. Default binding: F2.
    ToggleProjectTree,
    /// Move the project-tree selection up or down by `delta` rows.
    ProjectTreeMove { delta: isize },
    /// Open the currently-selected project-tree file. If the file is
    /// already open, switches to that document; otherwise loads it as a
    /// new document.
    ProjectTreeOpen,
    /// Open the project-wide search dialog. `None` toggles the dialog
    /// open/closed; `Some(query)` sets the query and runs the search.
    /// Default binding: Cmd/Ctrl+Shift+F.
    ProjectSearch(Option<String>),
    /// Update the project-search query while the dialog is open.
    ProjectSearchQueryChanged(String),
    /// Move the project-search result selection up or down by `delta`.
    ProjectSearchMove { delta: isize },
    /// Open the currently-selected project-search result. Loads the file
    /// (or switches to it if already open) and jumps the cursor to the
    /// match position.
    ProjectSearchOpenResult,
    /// Close the project-search dialog.
    ProjectSearchClose,
    /// Update the project-replace query while the dialog is open.
    ProjectSearchReplaceQueryChanged(String),
    /// Toggle focus between the find and replace fields in the
    /// project-search dialog (TUI only; egui manages focus itself).
    ProjectSearchToggleFocus,
    /// Replace all occurrences of the project-search query with the
    /// project-replace query across the project. The frontends should
    /// show a preview first (via `Project::replace_preview`) and confirm
    /// with the user before calling this event.
    ProjectSearchReplaceAll,
    /// Confirm the project-wide replace-all after the user has reviewed
    /// the preview. Only valid while the project-search confirmation
    /// prompt is shown.
    ProjectSearchReplaceAllConfirm,
    /// Cancel the project-wide replace-all confirmation prompt and return
    /// to the preview.
    ProjectSearchReplaceAllCancel,
    /// Open the fuzzy file finder. `None` toggles the finder open/closed;
    /// `Some(query)` sets the initial query.
    /// Default binding: Cmd/Ctrl+P.
    FuzzyFinder(Option<String>),
    /// Update the fuzzy finder query while it's open.
    FuzzyFinderQueryChanged(String),
    /// Move the fuzzy finder selection up or down by `delta`.
    FuzzyFinderMove { delta: isize },
    /// Open the currently-selected fuzzy finder result.
    FuzzyFinderExecute,
    /// Close the fuzzy finder.
    FuzzyFinderClose,
    /// Toggle the keybindings help window.
    ToggleKeybindingsHelp,
    /// Toggle the git gutter on the active document.
    ToggleGitGutter,
    /// Refresh the git gutter for the active document.
    RefreshGitGutter,
    /// Jump to the next git hunk at or after the cursor.
    NextHunk,
    /// Jump to the previous git hunk before the cursor.
    PrevHunk,
    /// Open the command palette. `None` toggles the palette open/closed;
    /// `Some(query)` sets the filter query.
    /// Default binding: Cmd/Ctrl+Shift+P.
    CommandPalette(Option<String>),
    /// Update the command-palette query while it's open.
    CommandPaletteQueryChanged(String),
    /// Move the command-palette selection up or down by `delta`.
    CommandPaletteMove { delta: isize },
    /// Execute the currently-selected command in the palette.
    CommandPaletteExecute,
    /// Close the command palette.
    CommandPaletteClose,
    /// Quit the editor. Frontend may prompt to save dirty buffers.
    Quit,
}

/// A direction or target for cursor movement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Movement {
    Left,
    Right,
    Up,
    Down,
    /// Move the cursor up by approximately one viewport page.
    /// Implementations compute the page size from the current viewport.
    PageUp,
    /// Move the cursor down by approximately one viewport page.
    PageDown,
    WordLeft,
    WordRight,
    LineStart,
    LineEnd,
    DocumentStart,
    DocumentEnd,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variants_exist_and_pattern_match() {
        let e = EditorEvent::Insert('x');
        assert!(matches!(e, EditorEvent::Insert('x')));
        let m = Movement::Left;
        assert!(matches!(m, Movement::Left));
    }

    #[test]
    fn movement_variants_are_distinct() {
        // Ensure no two variants accidentally collapse to the same value.
        let variants = [
            Movement::Left,
            Movement::Right,
            Movement::Up,
            Movement::Down,
            Movement::PageUp,
            Movement::PageDown,
            Movement::WordLeft,
            Movement::WordRight,
            Movement::LineStart,
            Movement::LineEnd,
            Movement::DocumentStart,
            Movement::DocumentEnd,
        ];
        for (i, a) in variants.iter().enumerate() {
            for (j, b) in variants.iter().enumerate() {
                if i != j {
                    assert_ne!(a, b, "variants {i:?} and {j:?} collide");
                }
            }
        }
    }

    #[test]
    fn set_cursor_carries_position() {
        let e = EditorEvent::SetCursor { pos: 42 };
        match e {
            EditorEvent::SetCursor { pos } => assert_eq!(pos, 42),
            _ => panic!("expected SetCursor"),
        }
    }

    #[test]
    fn select_extend_to_carries_position() {
        let e = EditorEvent::SelectExtendTo { pos: 100 };
        match e {
            EditorEvent::SelectExtendTo { pos } => assert_eq!(pos, 100),
            _ => panic!("expected SelectExtendTo"),
        }
    }

    #[test]
    fn doc_event_variants_exist() {
        // If you add another document-lifecycle event, mirror it here
        // so the type-level exhaustiveness check catches handlers that
        // forget to wire it up.
        let _ = EditorEvent::NewDoc;
        let _ = EditorEvent::CloseDoc;
        let _ = EditorEvent::NextDoc;
        let _ = EditorEvent::PrevDoc;
        let _ = EditorEvent::FindNext;
        let _ = EditorEvent::FindPrev;
        let _ = EditorEvent::ToggleFindRegex;
        let _ = EditorEvent::InsertTab;
        let _ = EditorEvent::ReplaceOpen;
        let _ = EditorEvent::ReplaceClose;
        let _ = EditorEvent::ReplaceQueryChanged(String::new());
        let _ = EditorEvent::ReplaceOne;
        let _ = EditorEvent::ReplaceAll;
        let _ = EditorEvent::SetIndentMode {
            use_spaces: true,
            tab_width: 4,
        };
        let _ = EditorEvent::CycleIndentMode;
        let _ = EditorEvent::ToggleSoftWrap;
        let _ = EditorEvent::CycleTheme;
        let _ = EditorEvent::OpenFile(None);
        let _ = EditorEvent::OpenFile(Some(PathBuf::from("/tmp/example.rs")));
        let _ = EditorEvent::GoToLine(None);
        let _ = EditorEvent::GoToLine(Some(42));
        let _ = EditorEvent::ProjectSearchReplaceAllConfirm;
        let _ = EditorEvent::ProjectSearchReplaceAllCancel;
        let _ = EditorEvent::FuzzyFinder(None);
        let _ = EditorEvent::FuzzyFinderQueryChanged(String::new());
        let _ = EditorEvent::FuzzyFinderMove { delta: 1 };
        let _ = EditorEvent::FuzzyFinderExecute;
        let _ = EditorEvent::FuzzyFinderClose;
        let _ = EditorEvent::ToggleKeybindingsHelp;
        let _ = EditorEvent::ToggleGitGutter;
        let _ = EditorEvent::RefreshGitGutter;
        let _ = EditorEvent::NextHunk;
        let _ = EditorEvent::PrevHunk;
    }
}
