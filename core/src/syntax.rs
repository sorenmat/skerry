//! Syntax highlighting — shared types and the theme-name manager.
//!
//! The actual highlighting engine lives in [`crate::ts`] (tree-sitter).
//! This module holds the types both frontends depend on:
//! - [`SyntaxEngine`] — global, created once at startup. Tracks the
//!   active theme *name* and resolves it to a [`crate::ts::TsTheme`].
//! - [`SyntaxCache`] — per-document, lives on `Document`. Lazily
//!   populated, invalidated on every edit.
//! - [`ColorSegment`] / [`HighlightColor`] — a byte range + color. What
//!   the renderer receives per line.

use std::collections::HashMap;
use std::rc::Rc;

/// A theme-agnostic sRGB color carried by [`ColorSegment`]. Filled from
/// the active tree-sitter theme's capture → color map by
/// [`crate::ts::highlight_range`]. The field names (`r`, `g`, `b`) match
/// what both frontends' color converters already read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct HighlightColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

/// A colored byte range within a line. The color comes from the active
/// syntax theme — the frontend converts it to its native color type and
/// draws.
#[derive(Debug, Clone)]
pub struct ColorSegment {
    /// Byte range within the line's text (line-local, NOT document).
    pub range: std::ops::Range<usize>,
    pub color: HighlightColor,
}

/// Per-document, lazily-populated, edit-invalidated per-line
/// highlight cache. Lives on `Document` so it survives tab switches.
#[derive(Clone, Debug, Default)]
pub struct SyntaxCache {
    /// Color segments keyed by line index. Empty when highlighting
    /// is disabled (file too large, unknown extension, or just
    /// invalidated by an edit). Shared by reference so renderers can
    /// read a cached line without copying its segments.
    pub lines: HashMap<usize, Rc<Vec<ColorSegment>>>,
    /// `true` after any buffer edit; the renderer re-highlights
    /// affected lines on next render.
    pub dirty: bool,
}

impl SyntaxCache {
    /// Mark the cache as stale. Called on every buffer-mutating event.
    pub fn invalidate(&mut self) {
        self.lines.clear();
        self.dirty = true;
    }

    /// Invalidate only entries at or past `line`. Lines strictly above the
    /// edit keep their cached segments, so a keystroke on line 10 of a
    /// 4000-line file only re-tokenizes the visible portion of line 10 and
    /// below instead of the whole viewport.
    ///
    /// This is a superset-correct over-approximation: edits that change
    /// content above `line` (never happens) or that don't shift line
    /// counts could be invalidated more narrowly, but "drop from the edit
    /// onwards" is always safe because tree-sitter queries are strictly
    /// left-to-right within a line and a changed line only affects itself
    /// and lines below it.
    pub fn invalidate_from(&mut self, line: usize) {
        self.lines.retain(|&k, _| k < line);
        self.dirty = true;
    }
}

/// Global syntax engine. Created once at startup and shared across
/// all documents. Tracks the active theme name and resolves it to a
/// [`crate::ts::TsTheme`] for the tree-sitter highlighter.
///
/// Lives on the frontend's `App` struct (not on `Document`) because
/// it's window-global, not per-document. Both frontends create their
/// own instance — the user runs either GUI or TUI, not both.
pub struct SyntaxEngine {
    theme_name: String,
}

impl SyntaxEngine {
    /// Create with the default dark theme (`Ocean Dark`).
    pub fn default_dark() -> Self {
        Self {
            theme_name: "Ocean Dark".to_string(),
        }
    }

    /// Name of the currently active theme.
    pub fn theme_name(&self) -> &str {
        &self.theme_name
    }

    /// All bundled theme names, in deterministic order.
    pub fn theme_names(&self) -> Vec<&str> {
        crate::ts::bundled_themes().iter().map(|t| t.name).collect()
    }

    /// The active tree-sitter theme. Resolves the current theme name to
    /// a [`crate::ts::TsTheme`], falling back to the default (Ocean Dark)
    /// when the name doesn't match a bundled tree-sitter theme (e.g. an
    /// old persisted name like `base16-ocean.dark` from the syntect era).
    pub fn ts_theme(&self) -> &'static crate::ts::TsTheme {
        crate::ts::find_theme(&self.theme_name).unwrap_or_else(|| &crate::ts::bundled_themes()[0])
    }

    /// Switch to the next bundled theme, wrapping around. Returns the
    /// new theme name.
    pub fn cycle_theme(&mut self) -> &str {
        let names: Vec<String> = self
            .theme_names()
            .into_iter()
            .map(|s| s.to_string())
            .collect();
        if names.is_empty() {
            return &self.theme_name;
        }
        let current_idx = names
            .iter()
            .position(|n| n == &self.theme_name)
            .unwrap_or(0);
        let next_idx = (current_idx + 1) % names.len();
        let next_name = names[next_idx].clone();
        self.set_theme_by_name(&next_name);
        &self.theme_name
    }

    /// Activate a theme by exact name. The name must match a bundled
    /// tree-sitter theme (`Ocean Dark`, `Gruvbox Dark`, etc.). Returns
    /// `true` if the name was found and the theme changed.
    pub fn set_theme_by_name(&mut self, name: &str) -> bool {
        if crate::ts::find_theme(name).is_none() {
            return false;
        }
        self.theme_name = name.to_string();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine() -> SyntaxEngine {
        SyntaxEngine::default_dark()
    }

    #[test]
    fn invalidate_from_drops_at_and_after_line() {
        let mut cache = SyntaxCache::default();
        let color = HighlightColor { r: 1, g: 2, b: 3 };
        for line in 0..10 {
            cache
                .lines
                .insert(line, Rc::new(vec![ColorSegment { range: 0..1, color }]));
        }
        cache.invalidate_from(4);
        assert!(cache.dirty, "should mark dirty");
        assert_eq!(cache.lines.len(), 4, "lines 0..4 survive");
        for kept in 0..4 {
            assert!(cache.lines.contains_key(&kept), "line {kept} kept");
        }
        for dropped in 4..10 {
            assert!(
                !cache.lines.contains_key(&dropped),
                "line {dropped} dropped"
            );
        }
    }

    // Grammar resolution (extension → language) is tested in
    // ts::grammar::tests, and the highlight path in ts::highlight::tests.
    // These tests cover the SyntaxEngine theme-name API and SyntaxCache.

    #[test]
    fn syntax_cache_invalidate_clears_and_sets_dirty() {
        let mut cache = SyntaxCache::default();
        cache.lines.insert(0, Rc::new(Vec::new()));
        cache.lines.insert(1, Rc::new(Vec::new()));
        assert!(!cache.dirty);
        cache.invalidate();
        assert!(cache.dirty);
        assert!(cache.lines.is_empty());
    }

    #[test]
    fn engine_starts_on_default_theme() {
        let e = engine();
        assert!(!e.theme_name().is_empty());
    }

    #[test]
    fn cycle_theme_changes_name() {
        let mut e = engine();
        let start = e.theme_name().to_string();
        let after = e.cycle_theme().to_string();
        // With more than one bundled theme, cycling should land on a
        // different name; if there's only one, it stays the same.
        if e.theme_names().len() > 1 {
            assert_ne!(start, after);
        } else {
            assert_eq!(start, after);
        }
    }

    #[test]
    fn cycle_theme_wraps_around() {
        let mut e = engine();
        let start = e.theme_name().to_string();
        let count = e.theme_names().len();
        for _ in 0..count {
            e.cycle_theme();
        }
        assert_eq!(e.theme_name(), start);
    }

    #[test]
    fn set_theme_by_name_valid() {
        let mut e = engine();
        let first = e.theme_names().into_iter().next().unwrap().to_string();
        assert!(e.set_theme_by_name(&first));
        assert_eq!(e.theme_name(), first);
    }

    #[test]
    fn set_theme_by_name_invalid() {
        let mut e = engine();
        let before = e.theme_name().to_string();
        assert!(!e.set_theme_by_name("definitely-not-a-theme"));
        assert_eq!(e.theme_name(), before);
    }
}
