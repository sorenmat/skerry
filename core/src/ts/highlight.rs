//! Tree-sitter highlighting — runs the grammar's highlight query over a
//! document's parse tree, scoped to a byte range, and produces
//! [`ColorSegment`]s the renderer draws.
//!
//! The key property is **viewport-limited** querying: `QueryCursor` is
//! constrained to `byte_range` via `set_byte_range`, so only the visible
//! region of the document is examined — critical for the multi-MB file
//! case. A time-based cancellation budget is wired via
//! `captures_with_options` so a pathological query on a huge region can't
//! stall a frame; partial results are returned and re-tried next frame.
//!
//! Queries are compiled once per grammar and cached in a process-global
//! `OnceLock` table (compilation parses the `.scm` source and is the most
//! expensive one-time cost per language).

use std::ops::ControlFlow;
use std::ops::Range;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use tree_sitter::{Query, QueryCursor, QueryCursorOptions, StreamingIterator, Tree};

use crate::{ColorSegment, HighlightColor};

use super::theme::TsTheme;
use super::{DocTree, Grammar};

/// Maximum wall-clock time a single highlight pass may take before it is
/// cancelled. The viewport is already byte-range-limited so this is a
/// safety net for pathological queries (deeply nested captures, regex-heavy
/// patterns), not the primary bound. At 15 ms it leaves headroom in a 60 fps
/// frame (~16.6 ms) for layout and painting.
const HIGHLIGHT_BUDGET: Duration = Duration::from_millis(15);

/// Highlight the portion of `tree` within `byte_range`, returning merged
/// [`ColorSegment`]s whose ranges are **document-absolute** byte offsets
/// (the caller translates them to line-local offsets when building per-line
/// caches).
///
/// Captures whose name the theme doesn't recognise are skipped (that span
/// renders with the default text color). Overlapping captures are resolved
/// by query order: later captures paint over earlier ones, matching how
/// tree-sitter queries express precedence (e.g. a `variable.parameter`
/// capture inside a `function` capture wins).
///
/// If the query exceeds [`HIGHLIGHT_BUDGET`], it is cancelled and whatever
/// captures were collected so far are returned (the caller caches partial
/// results and re-runs the next frame when the viewport is revisited).
pub fn highlight_range(
    tree: &Tree,
    grammar: &Grammar,
    theme: &TsTheme,
    byte_range: Range<usize>,
    source: &[u8],
) -> Vec<ColorSegment> {
    let Some(query) = compiled_query(grammar) else {
        return Vec::new();
    };
    highlight_range_with_query(
        tree,
        query,
        theme,
        byte_range,
        source,
        Instant::now() + HIGHLIGHT_BUDGET,
    )
    .segments
}

/// Highlight a document tree, including Markdown's paired inline trees.
pub(crate) fn highlight_doc_range(
    doc_tree: &DocTree,
    grammar: &Grammar,
    theme: &TsTheme,
    byte_range: Range<usize>,
    source: &[u8],
) -> HighlightResult {
    let Some(tree) = doc_tree.tree() else {
        return HighlightResult::complete(Vec::new());
    };
    let Some(block_query) = compiled_query(grammar) else {
        return HighlightResult::complete(Vec::new());
    };
    let deadline = Instant::now() + HIGHLIGHT_BUDGET;
    let mut result = highlight_range_with_query(
        tree,
        block_query,
        theme,
        byte_range.clone(),
        source,
        deadline,
    );
    let Some(markdown_tree) = doc_tree.markdown_tree() else {
        return result;
    };
    let Some(inline_query) = compiled_markdown_inline_query(grammar) else {
        return result;
    };
    if !result.complete {
        return result;
    }
    let inline_trees = markdown_tree.inline_trees();
    let first = inline_trees.partition_point(|tree| {
        tree.root_node().byte_range().end <= byte_range.start
    });
    for inline_tree in &inline_trees[first..] {
        let root_range = inline_tree.root_node().byte_range();
        if root_range.start >= byte_range.end {
            break;
        }
        let inline_result = highlight_range_with_query(
            inline_tree,
            inline_query,
            theme,
            byte_range.clone(),
            source,
            deadline,
        );
        result.segments.extend(inline_result.segments);
        if !inline_result.complete {
            result.complete = false;
            break;
        }
    }
    let captures = result
        .segments
        .into_iter()
        .map(|segment| (segment.range, segment.color))
        .collect::<Vec<_>>();
    result.segments = merge_captures(&captures);
    result
}

pub(crate) struct HighlightResult {
    pub segments: Vec<ColorSegment>,
    pub complete: bool,
}

impl HighlightResult {
    fn complete(segments: Vec<ColorSegment>) -> Self {
        Self {
            segments,
            complete: true,
        }
    }
}

fn highlight_range_with_query(
    tree: &Tree,
    query: &Query,
    theme: &TsTheme,
    byte_range: Range<usize>,
    source: &[u8],
    deadline: Instant,
) -> HighlightResult {
    let capture_names = query.capture_names();

    let mut cursor = QueryCursor::new();
    cursor.set_byte_range(byte_range);

    // Collect raw captures (node byte range + capture name), then merge
    // into non-overlapping ColorSegments. We collect first because the
    // streaming iterator borrows the cursor and we want to own the
    // results before building segments.
    //
    // `QueryCaptures` yields `(QueryMatch, capture_index_within_match)` —
    // one capture at a time, in document order — which is exactly what we
    // need for a left-to-right paint walk.
    //
    // The progress callback cancels the query if it exceeds the budget.
    // Partial results are fine — the caller caches them and re-queries the
    // next time the viewport is revisited.
    let cancelled = std::cell::Cell::new(false);
    let mut callback = |_state: &tree_sitter::QueryCursorState| {
        if Instant::now() > deadline {
            cancelled.set(true);
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let mut options = QueryCursorOptions::new().progress_callback(&mut callback);
    let mut raw: Vec<(Range<usize>, HighlightColor)> = Vec::new();
    let root = tree.root_node();
    let mut captures = cursor.captures_with_options(query, root, source, options.reborrow());
    // StreamingIterator contract: advance() must be called before get()
    // returns data. Loop = advance, then read, then break when done.
    loop {
        captures.advance();
        let Some((m, cap_idx)) = captures.get() else { break };
        let Some(cap) = m.captures.get(*cap_idx) else { continue };
        let Some(name) = capture_names.get(cap.index as usize).copied() else {
            continue;
        };
        let Some(color) = theme.color_for(name) else { continue };
        raw.push((cap.node.byte_range(), color));
    }
    drop(captures);

    if raw.is_empty() {
        return HighlightResult {
            segments: Vec::new(),
            complete: !cancelled.get(),
        };
    }

    HighlightResult {
        segments: merge_captures(&raw),
        complete: !cancelled.get(),
    }
}

fn compiled_markdown_inline_query(grammar: &Grammar) -> Option<&'static Query> {
    let inline = grammar.inline.as_ref()?;
    static QUERY: OnceLock<Option<Query>> = OnceLock::new();
    QUERY
        .get_or_init(|| Query::new(&inline.language, inline.highlights_query).ok())
        .as_ref()
}

/// Compile (and cache) the highlight query for a grammar. Returns `None`
/// if the grammar's query source fails to compile — the document then
/// renders as plain text rather than panicking.
fn compiled_query(grammar: &Grammar) -> Option<&'static Query> {
    static QUERIES: OnceLock<Vec<(&'static str, Query)>> = OnceLock::new();
    // Keyed by the grammar's stable `name` field. A linear scan over ~8
    // entries is negligible and avoids the pointer-aliasing problem that
    // broke an earlier &'static str identity scheme (`&'static str`
    // constants aren't guaranteed to share an address across compilation
    // units).
    let queries = QUERIES.get_or_init(|| {
        let mut v = Vec::new();
        for g in all_grammars() {
            if let Ok(q) = Query::new(&g.language, g.highlights_query) {
                v.push((g.name, q));
            }
        }
        v
    });
    queries
        .iter()
        .find(|(name, _)| *name == grammar.name)
        .map(|(_, q)| q)
}

/// The full set of grammars, for eager query compilation on first use.
/// Mirrors the constructors in `grammar.rs`; kept here so the highlight
/// module owns its query compilation without grammar.rs depending on it.
fn all_grammars() -> Vec<Grammar> {
    use super::grammar::{c, go, javascript, json, markdown, python, rust, tsx, typescript};
    vec![
        rust(),
        go(),
        javascript(),
        typescript(),
        tsx(),
        python(),
        c(),
        json(),
        markdown(),
    ]
}

/// Merge possibly-overlapping capture ranges into a flat list of
/// non-overlapping `ColorSegment`s. Inner captures override outer ones
/// (the capture that started most recently and still covers the position
/// wins); gaps between captures produce no segment (the caller fills them
/// with the default text color).
///
/// Approach: a sweep line over capture boundaries. At each boundary the
/// set of active captures changes; the color for the span up to the next
/// boundary is that of the innermost active capture (latest start).
fn merge_captures(caps: &[(Range<usize>, HighlightColor)]) -> Vec<ColorSegment> {
    if caps.is_empty() {
        return Vec::new();
    }

    // Build boundary events: (byte, is_start, capture_index). Starts are
    // processed before ends at the same byte so adjacent captures join
    // without a gap.
    #[derive(Clone, Copy)]
    enum Kind {
        Start,
        End,
    }
    let mut events: Vec<(usize, Kind, usize)> = Vec::with_capacity(caps.len() * 2);
    for (i, (range, _)) in caps.iter().enumerate() {
        events.push((range.start, Kind::Start, i));
        events.push((range.end, Kind::End, i));
    }
    // Sort by byte; starts before ends at the same position.
    events.sort_by(|a, b| {
        a.0.cmp(&b.0).then_with(|| {
            let ka = matches!(a.1, Kind::Start);
            let kb = matches!(b.1, Kind::Start);
            kb.cmp(&ka) // Start=true sorts before End=false
        })
    });

    let mut segments: Vec<ColorSegment> = Vec::new();
    // Active captures as (start_byte, end_byte, color), kept in start
    // order; the last element is the innermost (most recently started).
    let mut active: Vec<(usize, usize, HighlightColor)> = Vec::new();
    let mut current_color: Option<HighlightColor> = None;
    let mut span_start: Option<usize> = None;

    for (byte, kind, idx) in events {
        // Flush the span up to this byte before mutating active set.
        if let (Some(start), Some(color)) = (span_start, current_color) {
            if byte > start {
                // Emit / coalesce.
                if let Some(last) = segments.last_mut() {
                    if last.color == color && last.range.end == start {
                        last.range.end = byte;
                    } else {
                        segments.push(ColorSegment { range: start..byte, color });
                    }
                } else {
                    segments.push(ColorSegment { range: start..byte, color });
                }
            }
        }

        match kind {
            Kind::Start => active.push((caps[idx].0.start, caps[idx].0.end, caps[idx].1)),
            Kind::End => {
                // Remove the specific capture whose end matches this event.
                // Each End event corresponds to exactly one capture.
                if let Some(pos) = active.iter().position(|(_, end, _)| *end == byte) {
                    active.remove(pos);
                }
            }
        }

        // Innermost active capture = the one with the latest start.
        let next_color = active.last().map(|(_, _, c)| *c);
        if next_color != current_color || span_start.is_none() {
            current_color = next_color;
            span_start = if next_color.is_some() { Some(byte) } else { None };
        }
    }

    segments
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ts::grammar::grammar_for_extension;
    use crate::ts::theme::OCEAN_DARK;

    fn parse(source: &str, ext: &str) -> (Tree, Grammar) {
        let g = grammar_for_extension(ext).unwrap();
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&g.language).unwrap();
        let tree = parser.parse(source.as_bytes(), None).unwrap();
        (tree, g)
    }

    #[test]
    fn highlight_rust_covers_visible_range() {
        let src = "fn main() { let x = 42; }";
        let (tree, g) = parse(src, "rs");
        let segs = highlight_range(&tree, &g, &OCEAN_DARK, 0..src.len(), src.as_bytes());
        // Should produce at least one segment (keywords, strings, numbers...).
        assert!(!segs.is_empty(), "rust source should highlight");
        // Segments must be within the source bounds and non-empty.
        for s in &segs {
            assert!(s.range.start < s.range.end, "segment must be non-empty");
            assert!(s.range.end <= src.len(), "segment must be in bounds");
        }
    }

    #[test]
    fn highlight_byte_range_limits_captures() {
        let src = "fn a() {} fn b() {}";
        let (tree, g) = parse(src, "rs");
        // Only query the second half — no captures should start before
        // byte 9 ("fn b() {}").
        let segs = highlight_range(&tree, &g, &OCEAN_DARK, 9..src.len(), src.as_bytes());
        for s in &segs {
            assert!(
                s.range.start >= 9 || s.range.end > 9,
                "segment {:?} should overlap the queried range",
                s.range
            );
        }
    }

    #[test]
    fn merge_captures_no_overlap() {
        let color = HighlightColor { r: 1, g: 2, b: 3 };
        let caps = vec![(0..3, color), (5..8, color)];
        let segs = merge_captures(&caps);
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].range, 0..3);
        assert_eq!(segs[1].range, 5..8);
    }

    #[test]
    fn merge_captures_inner_overrides_outer() {
        let outer = HighlightColor { r: 1, g: 1, b: 1 };
        let inner = HighlightColor { r: 9, g: 9, b: 9 };
        // Outer 0..10, inner 3..6. Result: [0..3 outer, 3..6 inner, 6..10 outer].
        let caps = vec![(0..10, outer), (3..6, inner)];
        let segs = merge_captures(&caps);
        let colors: Vec<HighlightColor> = segs.iter().map(|s| s.color).collect();
        assert!(colors.contains(&inner), "inner color should appear");
        assert!(colors.contains(&outer), "outer color should appear");
    }

    #[test]
    fn highlight_json_keys_and_values() {
        let src = r#"{"name": "test", "count": 42}"#;
        let (tree, g) = parse(src, "json");
        let segs = highlight_range(&tree, &g, &OCEAN_DARK, 0..src.len(), src.as_bytes());
        assert!(!segs.is_empty(), "json should highlight");
    }

    #[test]
    fn compiled_query_caches_across_calls() {
        // Two calls should return the same static Query (pointer-equal).
        let g1 = grammar_for_extension("rs").unwrap();
        let g2 = grammar_for_extension("rs").unwrap();
        let q1 = compiled_query(&g1);
        let q2 = compiled_query(&g2);
        assert!(q1.is_some() && q2.is_some());
        assert!(std::ptr::eq(q1.unwrap(), q2.unwrap()));
    }

    #[test]
    fn markdown_highlights_block_and_inline_syntax() {
        let src = "# Title\n\nA **strong** word and `code`.\n";
        let grammar = grammar_for_extension("md").unwrap();
        let mut tree = DocTree::new(grammar.clone()).unwrap();
        tree.parse(src.as_bytes());
        let segments = highlight_doc_range(
            &tree,
            &grammar,
            &OCEAN_DARK,
            0..src.len(),
            src.as_bytes(),
        )
        .segments;
        let title = src.find("Title").unwrap();
        let strong = src.find("strong").unwrap();
        let code = src.find("code").unwrap();
        for byte in [title, strong, code] {
            assert!(
                segments.iter().any(|segment| segment.range.contains(&byte)),
                "Markdown byte {byte} should be highlighted: {segments:?}"
            );
        }
    }

    #[test]
    fn markdown_highlighting_respects_viewport_range() {
        let src = "First **outside** paragraph.\n\nSecond `inside` paragraph.\n";
        let grammar = grammar_for_extension("markdown").unwrap();
        let mut tree = DocTree::new(grammar.clone()).unwrap();
        tree.parse(src.as_bytes());
        let start = src.find("Second").unwrap();
        let segments = highlight_doc_range(
            &tree,
            &grammar,
            &OCEAN_DARK,
            start..src.len(),
            src.as_bytes(),
        )
        .segments;
        let outside = src.find("outside").unwrap();
        let inside = src.find("inside").unwrap();
        assert!(!segments
            .iter()
            .any(|segment| segment.range.contains(&outside)));
        assert!(segments
            .iter()
            .any(|segment| segment.range.contains(&inside)));
    }

    #[test]
    fn markdown_inline_highlights_stay_aligned_after_incremental_edit() {
        let original = "Text **strong** and `code`.\n";
        let edited = "Text new **strong** and `code`.\n";
        let grammar = grammar_for_extension("md").unwrap();
        let mut tree = DocTree::new(grammar.clone()).unwrap();
        tree.parse(original.as_bytes());
        tree.apply_edit(
            crate::ts::EditDelta::single_line(0, 5, 5, 4),
            edited.as_bytes(),
        );

        let result = highlight_doc_range(
            &tree,
            &grammar,
            &OCEAN_DARK,
            0..edited.len(),
            edited.as_bytes(),
        );
        assert!(result.complete);
        for token in ["strong", "code"] {
            let byte = edited.find(token).unwrap();
            assert!(result
                .segments
                .iter()
                .any(|segment| segment.range.contains(&byte)));
        }
    }
}
