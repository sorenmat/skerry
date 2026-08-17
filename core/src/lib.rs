//! Skerry `core` — UI-agnostic text manipulation engine.
//!
//! See `CONTEXT.md` at the workspace root for domain terms and
//! `docs/adr/` for the architectural decisions that shaped this crate.

mod buffer;
mod command_palette;
mod config;
mod document;
mod errors;
mod file_watcher;
pub mod fold;
mod formatter;
mod fuzzy;
mod git_blame;
mod git_gutter;
mod input;
mod keymap;
pub mod lsp;
mod piece_table;
mod project;
pub mod search;
mod snippet;
mod syntax;
pub mod ts;
mod undo;
mod view;

pub use buffer::{Buffer, BytePos, Selection};
pub use command_palette::{filter_commands, Command, COMMANDS};
pub use config::Config;
pub use document::{Document, ViewState};
pub use errors::{EditError, SaveError};
pub use file_watcher::{FileChange, FileWatcher};
pub use fold::{FoldRange, FoldState};
pub use formatter::{formatter_for_language, run_external_formatter};
pub use fuzzy::{filter_and_rank, fuzzy_score, FuzzyMatch};
pub use git_blame::{BlameCommit, BlameEntry, GitBlame};
pub use git_gutter::{GitGutter, Hunk, LineStatus, RemovedBlock};
pub use input::{EditorEvent, Movement};
pub use keymap::{
    KeyCode, KeyInput, KeyModifiers, KeybindingMode, KeymapOutput, KeymapState, VimMode,
};
pub use piece_table::{Piece, PieceSource, PieceTableBuffer};
pub use project::{
    FsNode, Project, ProjectSearchResult, ProjectTree, ReplaceError, ReplacePreview,
};
pub use search::Search;
pub use snippet::{expand as expand_snippet, trigger_at_cursor as snippet_trigger_at_cursor};
pub use syntax::{ColorSegment, HighlightColor, SyntaxCache, SyntaxEngine};
pub use view::{
    all_occurrence_selections, auto_indent, auto_pair_action, byte_to_char_col, char_after,
    char_before, char_col_to_byte_col, clamped_line_charcol_to_pos, column_selections,
    compute_comment_toggles, cursor_char_linecol, format_position, line_comment_prefix,
    matching_bracket, matching_close, matching_open, move_left_by_char, move_right_by_char,
    selection_in_line, visual_col_to_byte_col, visual_line_width, AutoPairAction,
};
