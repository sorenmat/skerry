//! the_editor `core` — UI-agnostic text manipulation engine.
//!
//! See `CONTEXT.md` at the workspace root for domain terms and
//! `docs/adr/` for the architectural decisions that shaped this crate.

mod buffer;
mod document;
mod errors;
mod input;
mod piece_table;
pub mod search;
mod syntax;
mod undo;
mod view;

pub use buffer::{Buffer, BytePos, Selection};
pub use document::Document;
pub use errors::{EditError, SaveError};
pub use input::{EditorEvent, Movement};
pub use piece_table::{Piece, PieceSource, PieceTableBuffer};
pub use search::Search;
pub use syntax::{tokenize_line, SyntaxCache, Token, TokenKind, SYNTAX_SIZE_LIMIT};
pub use view::{byte_to_char_col, char_col_to_byte_col, format_position, selection_in_line};