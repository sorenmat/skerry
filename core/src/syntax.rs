//! Syntax highlighting via [syntect](https://crates.io/crates/syntect).
//!
//! syntect wraps the Sublime Text highlighting engine: 200+ languages
//! via `.sublime-syntax` definitions, theme support via `.tmTheme`
//! files. This module exposes a thin API that the frontends call —
//! they don't need to know about syntect internals.
//!
//! Architecture:
//! - [`SyntaxEngine`] — global, created once at startup. Holds the
//!   `SyntaxSet` (all language defs) and `Theme` (active color scheme).
//! - [`SyntaxCache`] — per-document, lives on `Document`. Lazily
//!   populated, invalidated on every edit.
//! - [`ColorSegment`] — a byte range + color. What the renderer
//!   receives per line.

use std::collections::HashMap;
use std::path::Path;

use syntect::easy::HighlightLines;
use syntect::highlighting::{Color as SColor, Style, Theme, ThemeSet};
use syntect::parsing::{SyntaxReference, SyntaxSet};

/// Maximum file size (in bytes) for which syntax highlighting is
/// enabled. Files above this skip tokenization entirely.
pub const SYNTAX_SIZE_LIMIT: usize = 2 * 1024 * 1024;

/// A colored byte range within a line. The color comes directly from
/// the syntect theme — the frontend just converts it to its native
/// color type and draws.
#[derive(Debug, Clone)]
pub struct ColorSegment {
    /// Byte range within the line's text (line-local, NOT document).
    pub range: std::ops::Range<usize>,
    pub color: SColor,
}

/// Per-document, lazily-populated, edit-invalidated per-line
/// highlight cache. Lives on `Document` so it survives tab switches.
#[derive(Clone, Debug, Default)]
pub struct SyntaxCache {
    /// Color segments keyed by line index. Empty when highlighting
    /// is disabled (file too large, unknown extension, or just
    /// invalidated by an edit).
    pub lines: HashMap<usize, Vec<ColorSegment>>,
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
    /// onwards" is always safe because syntect highlighters are strictly
    /// left-to-right within a line and a changed line only affects itself
    /// and lines below it.
    pub fn invalidate_from(&mut self, line: usize) {
        self.lines.retain(|&k, _| k < line);
        self.dirty = true;
    }
}

/// Global syntax engine. Created once at startup and shared across
/// all documents. Holds all language definitions and the active theme.
///
/// Lives on the frontend's `App` struct (not on `Document`) because
/// it's window-global, not per-document. Both frontends create their
/// own instance — the user runs either GUI or TUI, not both.
pub struct SyntaxEngine {
    syntax_set: SyntaxSet,
    theme_set: ThemeSet,
    theme_name: String,
    theme: Theme,
}

impl SyntaxEngine {
    /// Create with syntect's bundled syntaxes + a dark theme.
    /// `base16-ocean.dark` is a high-contrast dark theme that covers
    /// most syntax scopes; it's what `bat` defaults to.
    pub fn default_dark() -> Self {
        let syntax_set = SyntaxSet::load_defaults_newlines();
        let theme_set = ThemeSet::load_defaults();
        let default_name = "base16-ocean.dark";
        let theme_name = theme_set
            .themes
            .contains_key(default_name)
            .then(|| default_name.to_string())
            .or_else(|| theme_set.themes.keys().next().cloned())
            .unwrap_or_else(|| "default".to_string());
        let theme = theme_set
            .themes
            .get(&theme_name)
            .cloned()
            .unwrap_or_default();
        Self {
            syntax_set,
            theme_set,
            theme_name,
            theme,
        }
    }

    /// Name of the currently active theme.
    pub fn theme_name(&self) -> &str {
        &self.theme_name
    }

    /// All bundled theme names, in deterministic order.
    pub fn theme_names(&self) -> Vec<&str> {
        self.theme_set.themes.keys().map(|s| s.as_str()).collect()
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

    /// Activate a theme by exact name. Returns `true` if the name was
    /// found and the theme changed.
    pub fn set_theme_by_name(&mut self, name: &str) -> bool {
        let Some(theme) = self.theme_set.themes.get(name) else {
            return false;
        };
        self.theme_name = name.to_string();
        self.theme = theme.clone();
        true
    }

    /// Find the syntax definition for a file path by extension.
    /// Returns `None` for unrecognized extensions or no path.
    pub fn syntax_for_path(&self, path: Option<&Path>) -> Option<&SyntaxReference> {
        let path = path?;
        self.syntax_set.find_syntax_for_file(path).ok().flatten()
    }

    /// Create a fresh `HighlightLines` for `syntax`. The renderer should
    /// create one highlighter per render pass and reuse it for every
    /// visible line; this avoids the per-line setup cost of the pure-Rust
    /// `regex-fancy` backend and lets multi-line constructs (block
    /// comments, etc.) carry state across consecutive lines.
    pub fn highlighter_for<'a>(&'a self, syntax: &'a SyntaxReference) -> HighlightLines<'a> {
        HighlightLines::new(syntax, &self.theme)
    }

    /// Highlight a single line using an existing `HighlightLines`. Pass
    /// the highlighter created by [`Self::highlighter_for`] at the start
    /// of the render pass and reuse it for each visible line.
    pub fn highlight_line_with(
        &self,
        highlighter: &mut HighlightLines<'_>,
        line: &str,
    ) -> Vec<ColorSegment> {
        let regions: Vec<(Style, &str)> = match highlighter.highlight_line(line, &self.syntax_set) {
            Ok(r) => r,
            Err(_) => return Vec::new(),
        };

        // Convert (Style, &str) pairs into ColorSegments with byte
        // ranges, merging adjacent segments that share the same color.
        let mut segments: Vec<ColorSegment> = Vec::new();
        let mut byte_pos = 0;
        for (style, text) in &regions {
            let len = text.len();
            if len == 0 {
                continue;
            }
            let start = byte_pos;
            let end = byte_pos + len;
            byte_pos = end;

            let seg = ColorSegment {
                range: start..end,
                color: style.foreground,
            };
            // Merge with previous if same color and contiguous.
            if let Some(last) = segments.last_mut() {
                if last.color == seg.color && last.range.end == seg.range.start {
                    last.range.end = seg.range.end;
                    continue;
                }
            }
            segments.push(seg);
        }
        segments
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
        let color = SColor { r: 1, g: 2, b: 3, a: 255 };
        for line in 0..10 {
            cache.lines.insert(
                line,
                vec![ColorSegment { range: 0..1, color }],
            );
        }
        cache.invalidate_from(4);
        assert!(cache.dirty, "should mark dirty");
        assert_eq!(cache.lines.len(), 4, "lines 0..4 survive");
        for kept in 0..4 {
            assert!(cache.lines.contains_key(&kept), "line {kept} kept");
        }
        for dropped in 4..10 {
            assert!(!cache.lines.contains_key(&dropped), "line {dropped} dropped");
        }
    }

    #[test]
    fn rust_file_gets_syntax() {
        let e = engine();
        let path = std::path::Path::new("main.rs");
        assert!(e.syntax_for_path(Some(path)).is_some());
    }

    #[test]
    fn markdown_file_gets_syntax() {
        let e = engine();
        let path = std::path::Path::new("README.md");
        assert!(e.syntax_for_path(Some(path)).is_some());
    }

    #[test]
    fn python_file_gets_syntax() {
        let e = engine();
        let path = std::path::Path::new("script.py");
        assert!(e.syntax_for_path(Some(path)).is_some());
    }

    #[test]
    fn json_file_gets_syntax() {
        let e = engine();
        let path = std::path::Path::new("data.json");
        assert!(e.syntax_for_path(Some(path)).is_some());
    }

    #[test]
    fn unknown_extension_no_syntax() {
        let e = engine();
        let path = std::path::Path::new("file.xyz123");
        // Might or might not match — just verify it doesn't panic.
        let _ = e.syntax_for_path(Some(path));
    }

    #[test]
    fn no_path_no_syntax() {
        let e = engine();
        assert!(e.syntax_for_path(None).is_none());
    }

    #[test]
    fn highlight_rust_line_produces_segments() {
        let e = engine();
        let path = std::path::Path::new("main.rs");
        let syntax = e.syntax_for_path(Some(path)).unwrap();
        let mut h = e.highlighter_for(syntax);
        let segments = e.highlight_line_with(&mut h, "let x = 42;");
        assert!(!segments.is_empty(), "should produce at least one segment");
        // The full line should be covered by the segments.
        let covered: usize = segments.iter().map(|s| s.range.len()).sum();
        assert_eq!(covered, "let x = 42;".len());
    }

    #[test]
    fn highlight_merges_adjacent_same_color() {
        let e = engine();
        let path = std::path::Path::new("main.rs");
        let syntax = e.syntax_for_path(Some(path)).unwrap();
        let mut h = e.highlighter_for(syntax);
        let segments = e.highlight_line_with(&mut h, "let x = 42;");
        // Verify no two adjacent segments have the same color
        // (the merge step should have coalesced them).
        for w in segments.windows(2) {
            assert!(
                w[0].color != w[1].color || w[0].range.end != w[1].range.start,
                "adjacent segments with same color were not merged"
            );
        }
    }

    #[test]
    fn highlight_empty_line() {
        let e = engine();
        let path = std::path::Path::new("main.rs");
        let syntax = e.syntax_for_path(Some(path)).unwrap();
        let mut h = e.highlighter_for(syntax);
        let segments = e.highlight_line_with(&mut h, "");
        assert!(segments.is_empty() || segments.iter().all(|s| s.range.is_empty()));
    }

    #[test]
    fn syntax_cache_invalidate_clears_and_sets_dirty() {
        let mut cache = SyntaxCache::default();
        cache.lines.insert(0, Vec::new());
        cache.lines.insert(1, Vec::new());
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
