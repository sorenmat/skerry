//! Tree-sitter integration — grammar registry, per-document parse trees,
//! and highlight queries.
//!
//! This module is the syntax highlighting backend. It builds and
//! incrementally maintains a parse tree per document, then runs
//! viewport-scoped highlight queries to produce the [`ColorSegment`]s
//! both frontends render.
//!
//! See `docs/adr/0001-piece-table-as-primary-buffer.md` for the buffer
//! architecture this layers on top of — the tree is derived from the
//! buffer's bytes and re-parsed incrementally on edit.

pub mod grammar;
pub mod highlight;
pub mod theme;
pub mod tree;

pub use grammar::{grammar_for_path, Grammar};
pub use highlight::highlight_range;
pub(crate) use highlight::highlight_doc_range;
pub use theme::{bundled_themes, find_theme, TsTheme};
pub use tree::{DocTree, EditDelta};
