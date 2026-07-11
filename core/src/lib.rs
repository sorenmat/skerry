//! the_editor `core` — UI-agnostic text manipulation engine.
//!
//! See `CONTEXT.md` at the workspace root for domain terms and
//! `docs/adr/` for the architectural decisions that shaped this crate.

mod buffer;
mod command_palette;
mod config;
mod document;
mod errors;
mod file_watcher;
mod fuzzy;
mod git_gutter;
mod input;
pub mod lsp;
mod piece_table;
mod project;
pub mod search;
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
pub use fuzzy::{filter_and_rank, fuzzy_score, FuzzyMatch};
pub use git_gutter::{GitGutter, Hunk, LineStatus, RemovedBlock};
pub use input::{EditorEvent, Movement};
pub use piece_table::{Piece, PieceSource, PieceTableBuffer};
pub use project::{
    FsNode, Project, ProjectSearchResult, ProjectTree, ReplaceError, ReplacePreview,
};
pub use search::Search;
pub use syntax::{ColorSegment, HighlightColor, SyntaxCache, SyntaxEngine};
pub use view::{
    byte_to_char_col, char_after, char_before, char_col_to_byte_col,
    clamped_line_charcol_to_pos, cursor_char_linecol, format_position, matching_close,
    matching_open, move_left_by_char, move_right_by_char, selection_in_line,
    visual_col_to_byte_col, visual_line_width,
};
