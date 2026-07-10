//! Tree-sitter integration — grammar registry, per-document parse trees,
//! and (phase 3) highlight queries.
//!
//! This module is the home for the tree-sitter backend that is replacing
//! syntect across phases 2-4. During the transition both backends coexist:
//! syntect still drives highlighting, while this module builds and
//! incrementally maintains a parse tree per document. Phase 3 wires the
//! tree into highlighting; phase 4 removes syntect.
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
pub use theme::{bundled_themes, find_theme, TsTheme};
pub use tree::{DocTree, EditDelta};
