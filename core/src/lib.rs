//! the_editor `core` — UI-agnostic text manipulation engine.
//!
//! See `CONTEXT.md` at the workspace root for domain terms and
//! `docs/adr/` for the architectural decisions that shaped this crate.

mod buffer;
mod errors;
mod input;
mod piece_table;
mod undo;
mod view;

pub use buffer::{Buffer, BytePos, Selection};
pub use errors::{EditError, SaveError};
pub use input::{EditorEvent, Movement};
pub use piece_table::{Piece, PieceSource, PieceTableBuffer};
pub use view::{byte_to_char_col, char_col_to_byte_col, format_position, selection_in_line};