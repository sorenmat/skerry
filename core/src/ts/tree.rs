//! Per-document tree-sitter parse tree with incremental reparsing.
//!
//! A [`DocTree`] owns a [`Parser`] configured for one grammar and the
//! latest [`Tree`] produced from the buffer. On an edit the caller describes
//! the change as an [`EditDelta`] (the byte range replaced and the
//! replacement length); [`DocTree::apply_edit`] feeds that to
//! [`Tree::edit`] and re-parses, reusing unchanged nodes so a single
//! keystroke only re-parses the touched region. This is the low-latency
//! pattern described in Zed's "syntax-aware editing" writeup.
//!
//! The tree itself is not used for highlighting until phase 3; this module
//! only keeps it correct and current.

use tree_sitter::{InputEdit, Parser, Point, Tree};

use super::Grammar;

/// A description of a buffer edit sufficient to drive incremental
/// reparsing. All byte offsets are absolute document byte offsets.
///
/// `start_byte..old_end_byte` is the region removed from the old buffer;
/// `new_end_byte` is where that region now ends in the new buffer
/// (`start_byte + replacement.len()`). For a pure insertion `old_end_byte
/// == start_byte`; for a pure deletion `new_end_byte == start_byte`.
#[derive(Clone, Copy, Debug)]
pub struct EditDelta {
    pub start_byte: usize,
    pub old_end_byte: usize,
    pub new_end_byte: usize,
    /// Row/column of `start_byte` in the OLD buffer.
    pub start_position: Point,
    /// Row/column of `old_end_byte` in the OLD buffer.
    pub old_end_position: Point,
    /// Row/column of `new_end_byte` in the NEW buffer.
    pub new_end_position: Point,
}

impl EditDelta {
    /// Build a delta for a single-line edit where the change does not add
    /// or remove newlines. This covers the common case (typing a char,
    /// deleting a char) without the caller having to compute three
    /// `Point`s by hand.
    ///
    /// `line` and `col` are the start of the edit; `len_diff` is
    /// `new_len - old_len` in bytes (positive for insertion, negative for
    /// deletion). Bytes are on UTF-8 boundaries.
    pub fn single_line(line: usize, col: usize, start_byte: usize, len_diff: i32) -> Self {
        let start_position = Point {
            row: line,
            column: col,
        };
        let old_end_byte;
        let new_end_byte;
        if len_diff >= 0 {
            old_end_byte = start_byte;
            new_end_byte = start_byte + len_diff as usize;
        } else {
            old_end_byte = start_byte + (-len_diff) as usize;
            new_end_byte = start_byte;
        }
        Self {
            start_byte,
            old_end_byte,
            new_end_byte,
            start_position,
            old_end_position: Point {
                row: line,
                column: col,
            },
            new_end_position: Point {
                row: line,
                column: (col as i32 + len_diff).max(0) as usize,
            },
        }
    }

    fn to_input_edit(self) -> InputEdit {
        InputEdit {
            start_byte: self.start_byte,
            old_end_byte: self.old_end_byte,
            new_end_byte: self.new_end_byte,
            start_position: self.start_position,
            old_end_position: self.old_end_position,
            new_end_position: self.new_end_position,
        }
    }
}

/// A parser plus its current tree for one document.
pub struct DocTree {
    parser: Parser,
    tree: Option<Tree>,
}

impl DocTree {
    /// Create a `DocTree` for `grammar`. The parser is configured but no
    /// parse is performed; call [`Self::parse`] with the buffer bytes.
    pub fn new(grammar: Grammar) -> Option<Self> {
        let mut parser = Parser::new();
        parser.set_language(&grammar.language).ok()?;
        Some(Self {
            parser,
            tree: None,
        })
    }

    /// Parse `source` from scratch (first parse) or incrementally
    /// (subsequent parses reuse `self.tree` when present). The tree is
    /// stored and returned by reference via [`Self::tree`].
    pub fn parse(&mut self, source: &[u8]) {
        self.tree = self.parser.parse(source, self.tree.as_ref());
    }

    /// Apply an edit and re-parse. The old tree is edited in place to
    /// reflect the byte/column shifts, then the parser re-parses using it
    /// as a hint — only the changed region is re-examined. `source` is the
    /// buffer bytes AFTER the edit.
    pub fn apply_edit(&mut self, delta: EditDelta, source: &[u8]) {
        if let Some(tree) = self.tree.as_mut() {
            tree.edit(&delta.to_input_edit());
        }
        self.tree = self.parser.parse(source, self.tree.as_ref());
    }

    /// The current parse tree, if one has been produced.
    pub fn tree(&self) -> Option<&Tree> {
        self.tree.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ts::grammar::grammar_for_extension;

    fn rust_tree(source: &str) -> DocTree {
        let g = grammar_for_extension("rs").unwrap();
        let mut dt = DocTree::new(g).unwrap();
        dt.parse(source.as_bytes());
        dt
    }

    #[test]
    fn parse_produces_tree() {
        let dt = rust_tree("fn main() {}");
        let tree = dt.tree().expect("parse should produce a tree");
        let root = tree.root_node();
        assert_eq!(root.kind(), "source_file");
        assert!(!root.has_error(), "tree should have no parse errors");
    }

    #[test]
    fn incremental_edit_keeps_tree_valid() {
        // "let x = 1;" — insert "y" right after "x" (byte 4) to get
        // "let xy = 1;". The edit is single-line at col 5, +1 byte.
        let mut dt = rust_tree("let x = 1;");
        let delta = EditDelta::single_line(0, 5, 5, 1);
        dt.apply_edit(delta, b"let xy = 1;");
        let tree = dt.tree().unwrap();
        assert!(!tree.root_node().has_error());
    }

    #[test]
    fn deletion_delta_re_parses() {
        // "let ab = 1;" — delete "b" at byte 5 to get "let a = 1;".
        let mut dt = rust_tree("let ab = 1;");
        let delta = EditDelta::single_line(0, 5, 5, -1);
        dt.apply_edit(delta, b"let a = 1;");
        let tree = dt.tree().unwrap();
        assert!(!tree.root_node().has_error());
    }

    #[test]
    fn json_grammar_parses() {
        let g = grammar_for_extension("json").unwrap();
        let mut dt = DocTree::new(g).unwrap();
        dt.parse(b"{\"key\": 42}");
        let tree = dt.tree().unwrap();
        assert_eq!(tree.root_node().kind(), "document");
        assert!(!tree.root_node().has_error());
    }
}
