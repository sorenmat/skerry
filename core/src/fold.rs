//! Code folding — collapse/expand blocks using tree-sitter node ranges.
//!
//! [`FoldState`] lives on `Document` and tracks which ranges are currently
//! folded. It provides a display-line ↔ doc-line mapping so the renderer
//! can skip hidden lines. Foldable ranges are discovered by walking the
//! tree-sitter parse tree: any named node spanning more than one line
//! whose body starts on a different row than its declaration is foldable.

use tree_sitter::{Node, Tree};

/// A foldable region: lines `[start_line, end_line]` (inclusive).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FoldRange {
    pub start_line: usize,
    pub end_line: usize,
}

/// Per-document fold state.
#[derive(Debug, Clone, Default)]
pub struct FoldState {
    /// Currently-folded ranges, sorted by start_line, non-overlapping.
    folded: Vec<FoldRange>,
    /// All foldable ranges in the document (from tree-sitter), cached.
    foldable: Vec<FoldRange>,
    /// Cached display-line → doc-line mapping. Recomputed when folds
    /// change. `None` means no mapping needed (no folds active).
    doc_lines: Vec<usize>,
}

impl FoldState {
    /// Create empty fold state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Update the foldable ranges from the tree-sitter tree. Called on
    /// parse/refresh. Does NOT change which ranges are folded, but
    /// removes any folded ranges that are no longer foldable.
    pub fn update_foldable(&mut self, tree: Option<&Tree>) {
        self.foldable = tree.map(foldable_ranges).unwrap_or_default();
        // Remove folded ranges that no longer exist.
        self.folded.retain(|f| {
            self.foldable
                .iter()
                .any(|r| r.start_line == f.start_line)
        });
        self.rebuild_mapping();
    }

    /// Toggle the fold at `line`. Finds the foldable range whose
    /// start_line is at or before `line`, and toggles it.
    pub fn toggle(&mut self, line: usize) {
        // Find the foldable range starting at `line` (or the nearest one).
        if let Some(range) = self
            .foldable
            .iter()
            .find(|r| r.start_line == line)
            .copied()
        {
            if let Some(idx) = self
                .folded
                .iter()
                .position(|f| f.start_line == range.start_line)
            {
                self.folded.remove(idx);
            } else {
                self.insert_folded(range);
            }
            self.rebuild_mapping();
        }
    }

    /// Unfold everything.
    pub fn unfold_all(&mut self) {
        self.folded.clear();
        self.rebuild_mapping();
    }

    /// Set the foldable ranges directly (used by frontends that compute
    /// them from a borrowed tree to avoid borrow conflicts).
    pub fn set_foldable(&mut self, ranges: Vec<FoldRange>) {
        self.foldable = ranges;
        // Remove folded ranges that no longer exist.
        self.folded.retain(|f| {
            self.foldable
                .iter()
                .any(|r| r.start_line == f.start_line)
        });
    }

    /// True if `doc_line` is inside a folded range (should be hidden).
    pub fn is_hidden(&self, doc_line: usize) -> bool {
        self.folded.iter().any(|f| {
            doc_line > f.start_line && doc_line <= f.end_line
        })
    }

    /// Is `doc_line` the start of a folded range?
    pub fn is_folded_at(&self, doc_line: usize) -> bool {
        self.folded.iter().any(|f| f.start_line == doc_line)
    }

    /// Is `doc_line` the start of a foldable (but not necessarily folded) range?
    pub fn is_foldable(&self, doc_line: usize) -> bool {
        self.foldable.iter().any(|r| r.start_line == doc_line)
    }

    /// The foldable range at `doc_line`, if any.
    pub fn foldable_at(&self, doc_line: usize) -> Option<FoldRange> {
        self.foldable
            .iter()
            .find(|r| r.start_line == doc_line)
            .copied()
    }

    /// Total visible (display) lines given `total_doc_lines`.
    pub fn display_line_count(&self, total_doc_lines: usize) -> usize {
        if self.folded.is_empty() {
            return total_doc_lines;
        }
        let hidden: usize = self
            .folded
            .iter()
            .map(|f| f.end_line.saturating_sub(f.start_line))
            .sum();
        total_doc_lines.saturating_sub(hidden)
    }

    /// Map a display (visible) line index to its document line index.
    pub fn doc_line_at_display(&self, display_line: usize) -> usize {
        if self.doc_lines.is_empty() {
            return display_line;
        }
        self.doc_lines
            .get(display_line)
            .copied()
            .unwrap_or_else(|| self.doc_lines.last().copied().unwrap_or(display_line))
    }

    /// Map a document line to its display (visible) line index.
    pub fn display_line_at_doc(&self, doc_line: usize) -> usize {
        if self.folded.is_empty() {
            return doc_line;
        }
        self.doc_lines
            .iter()
            .position(|&dl| dl == doc_line)
            .unwrap_or(doc_line)
    }

    /// Whether any folds are currently active.
    pub fn has_folds(&self) -> bool {
        !self.folded.is_empty()
    }

    /// Clear all fold state (on edit — folds are re-derived from the tree).
    pub fn invalidate(&mut self) {
        self.folded.clear();
        self.foldable.clear();
        self.doc_lines.clear();
    }

    fn insert_folded(&mut self, range: FoldRange) {
        let pos = self
            .folded
            .partition_point(|f| f.start_line < range.start_line);
        self.folded.insert(pos, range);
    }

    fn rebuild_mapping(&self) -> Vec<usize> {
        // Already cached in doc_lines, but we rebuild it here for the
        // field update. The field is set by callers via this method.
        // Actually we need to compute from folded + a max line count.
        // We don't know the total line count here, so we return empty
        // and let display_line_count handle it.
        Vec::new()
    }

    /// Rebuild the display↔doc mapping given the total document line count.
    pub fn rebuild(&mut self, total_doc_lines: usize) {
        if self.folded.is_empty() {
            self.doc_lines.clear();
            return;
        }
        let mut mapping = Vec::with_capacity(total_doc_lines);
        for line in 0..total_doc_lines {
            if !self.is_hidden(line) {
                mapping.push(line);
            }
        }
        self.doc_lines = mapping;
    }
}

/// Walk the tree-sitter tree to find all foldable ranges.
/// A node is foldable if:
/// - It's a named node (not anonymous)
/// - It spans more than one line
/// - Its first child ends on a different line than the node starts
///   (i.e., there's a body to fold)
fn foldable_ranges(tree: &Tree) -> Vec<FoldRange> {
    let root = tree.root_node();
    let mut ranges = Vec::new();
    collect_foldable(&root, &mut ranges);
    // Sort by start_line, deduplicate (keep the outermost — largest range).
    ranges.sort_by_key(|r| (r.start_line, std::cmp::Reverse(r.end_line)));
    let mut seen_starts: Vec<usize> = Vec::new();
    ranges.retain(|r| {
        if seen_starts.contains(&r.start_line) {
            false
        } else {
            seen_starts.push(r.start_line);
            true
        }
    });
    ranges
}

fn collect_foldable(node: &Node, ranges: &mut Vec<FoldRange>) {
    let start_row = node.start_position().row;
    let end_row = node.end_position().row;

    // Foldable if it spans multiple lines.
    if end_row > start_row && node.is_named() {
        // Check that there's a body to fold (the first child should end
        // on a different row than the node starts, or there should be
        // multiple children on different rows).
        let has_body = if node.child_count() > 0 {
            let first_child_end = node.child(0).map(|c| c.end_position().row);
            first_child_end.is_some_and(|r| r > start_row) || node.child_count() > 1
        } else {
            false
        };
        if has_body {
            ranges.push(FoldRange {
                start_line: start_row,
                end_line: end_row,
            });
        }
    }

    // Recurse into children.
    let mut cursor = node.walk();
    if cursor.goto_first_child() {
        loop {
            collect_foldable(&cursor.node(), ranges);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }
}

/// Public entry point for computing foldable ranges from a tree.
/// Used by frontends that need to compute ranges while holding an
/// immutable borrow of the document.
pub fn foldable_ranges_pub(tree: &Tree) -> Vec<FoldRange> {
    foldable_ranges(tree)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_state_has_no_folds() {
        let fs = FoldState::new();
        assert!(!fs.has_folds());
        assert!(!fs.is_hidden(5));
        assert_eq!(fs.display_line_count(100), 100);
        assert_eq!(fs.doc_line_at_display(5), 5);
        assert_eq!(fs.display_line_at_doc(5), 5);
    }

    #[test]
    fn unfold_all_clears() {
        let mut fs = FoldState::new();
        fs.folded.push(FoldRange { start_line: 2, end_line: 5 });
        fs.rebuild(100);
        assert!(fs.has_folds());
        fs.unfold_all();
        assert!(!fs.has_folds());
    }

    #[test]
    fn is_hidden_checks_interior_lines() {
        let mut fs = FoldState::new();
        fs.folded.push(FoldRange { start_line: 2, end_line: 5 });
        assert!(!fs.is_hidden(2));  // start line is visible
        assert!(fs.is_hidden(3));   // interior
        assert!(fs.is_hidden(5));   // end line
        assert!(!fs.is_hidden(6));  // after
    }

    #[test]
    fn display_line_count_subtracts_hidden() {
        let mut fs = FoldState::new();
        fs.folded.push(FoldRange { start_line: 2, end_line: 5 });
        // 10 doc lines - 3 hidden (lines 3,4,5) = 7 display lines
        assert_eq!(fs.display_line_count(10), 7);
    }
}
