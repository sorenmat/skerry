//! Tree-sitter highlight themes.
//!
//! A [`TsTheme`] maps tree-sitter capture names (e.g. `keyword`,
//! `function.method`, `string.special`) to [`HighlightColor`]s. Capture
//! names follow the tree-sitter standard: dot-separated scopes where the
//! first segment is the category (`keyword`, `function`, `string`, …).
//! Lookup tries the full name first, then the top-level category, so a
//! theme that only defines `function` still colors `function.method` and
//! `function.builtin`.
//!
//! The theme set is hand-authored (a handful of palettes) rather than
//! loaded from `.tmTheme` files, so it has no external file dependencies.
//! The default is `Ocean Dark`, a port of the `base16-ocean.dark` scheme
//! the editor shipped before the tree-sitter swap.
//!
//! Themes are stored as `&'static` slices so the whole set can live in a
//! process-global `&'static [TsTheme]` without any heap allocation or
//! lifetime gymnastics.

use crate::HighlightColor;

/// A syntax color theme: a name plus a capture-name → color table.
///
/// The table is a flat `&'static` slice of `(capture_name, color)` pairs
/// rather than a `HashMap`; themes have ~25 entries, so a linear scan is
/// fast and keeps the whole theme `Copy` + `'static`.
#[derive(Clone, Copy, Debug)]
pub struct TsTheme {
    pub name: &'static str,
    pub colors: &'static [(&'static str, HighlightColor)],
}

impl TsTheme {
    /// Resolve a capture name to a color. Tries the full dotted name
    /// (`function.method`) then the top-level category (`function`), then
    /// normalises a few punctuation captures. Returns `None` for
    /// unrecognised captures (the caller renders that span with the
    /// default text color).
    pub fn color_for(&self, capture: &str) -> Option<HighlightColor> {
        self.get(capture)
            .or_else(|| {
                let category = capture.split('.').next().filter(|c| !c.is_empty())?;
                self.get(category)
            })
            .or_else(|| match capture {
                "punctuation.bracket" | "punctuation.delimiter" | "punctuation.special" => {
                    self.get("punctuation")
                }
                _ => None,
            })
    }

    /// Exact-match lookup against the color table.
    fn get(&self, name: &str) -> Option<HighlightColor> {
        self.colors
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, c)| *c)
    }
}

/// The default bundled theme set, in display order. The first entry is
/// the default selected at startup.
pub const fn bundled_themes() -> &'static [TsTheme] {
    &[OCEAN_DARK, GRUVBOX_DARK, SOLARIZED_LIGHT, ONE_DARK]
}

/// Find a bundled theme by exact name.
pub fn find_theme(name: &str) -> Option<&'static TsTheme> {
    bundled_themes().iter().find(|t| t.name == name)
}

const fn c(r: u8, g: u8, b: u8) -> HighlightColor {
    HighlightColor { r, g, b }
}

// === Theme definitions ===
//
// Each theme is a `const TsTheme` so the set is fully `'static`. Colors are
// sRGB triples. The capture names cover the union of what the bundled
// grammar queries emit (see ts/grammar.rs); dotted variants fall back to
// the category at lookup time.

/// Port of syntect's `base16-ocean.dark` — the editor's previous default.
pub const OCEAN_DARK: TsTheme = TsTheme {
    name: "Ocean Dark",
    colors: &[
        ("comment", c(96, 110, 134)),
        ("comment.documentation", c(96, 110, 134)),
        ("string", c(163, 190, 140)),
        ("string.special", c(180, 142, 173)),
        ("string.special.key", c(180, 142, 173)),
        ("number", c(208, 135, 112)),
        ("constant", c(208, 135, 112)),
        ("constant.builtin", c(208, 135, 112)),
        ("keyword", c(180, 142, 173)),
        ("operator", c(143, 188, 187)),
        ("function", c(143, 188, 187)),
        ("function.builtin", c(143, 188, 187)),
        ("function.method", c(143, 188, 187)),
        ("function.macro", c(180, 142, 173)),
        ("function.special", c(143, 188, 187)),
        ("type", c(236, 239, 244)),
        ("type.builtin", c(236, 239, 244)),
        ("constructor", c(235, 203, 139)),
        ("variable", c(229, 233, 240)),
        ("variable.builtin", c(208, 135, 112)),
        ("variable.parameter", c(235, 203, 139)),
        ("property", c(183, 189, 198)),
        ("label", c(180, 142, 173)),
        ("attribute", c(180, 142, 173)),
        ("escape", c(208, 135, 112)),
        ("punctuation", c(171, 178, 191)),
        ("embedded", c(229, 233, 240)),
    ],
};

pub const GRUVBOX_DARK: TsTheme = TsTheme {
    name: "Gruvbox Dark",
    colors: &[
        ("comment", c(146, 131, 116)),
        ("string", c(152, 151, 26)),
        ("string.special", c(214, 93, 14)),
        ("number", c(211, 134, 155)),
        ("constant", c(211, 134, 155)),
        ("keyword", c(251, 73, 52)),
        ("operator", c(251, 73, 52)),
        ("function", c(250, 189, 47)),
        ("type", c(184, 187, 38)),
        ("constructor", c(250, 189, 47)),
        ("variable", c(235, 219, 178)),
        ("variable.builtin", c(214, 93, 14)),
        ("variable.parameter", c(250, 189, 47)),
        ("property", c(184, 187, 38)),
        ("label", c(250, 189, 47)),
        ("attribute", c(214, 93, 14)),
        ("escape", c(214, 93, 14)),
        ("punctuation", c(235, 219, 178)),
        ("embedded", c(235, 219, 178)),
    ],
};

pub const SOLARIZED_LIGHT: TsTheme = TsTheme {
    name: "Solarized Light",
    colors: &[
        ("comment", c(93, 122, 119)),
        ("string", c(133, 153, 0)),
        ("string.special", c(203, 75, 22)),
        ("number", c(181, 137, 0)),
        ("constant", c(181, 137, 0)),
        ("keyword", c(203, 75, 22)),
        ("operator", c(101, 123, 131)),
        ("function", c(26, 101, 131)),
        ("type", c(181, 137, 0)),
        ("constructor", c(181, 137, 0)),
        ("variable", c(101, 123, 131)),
        ("variable.builtin", c(203, 75, 22)),
        ("variable.parameter", c(26, 101, 131)),
        ("property", c(101, 123, 131)),
        ("label", c(26, 101, 131)),
        ("attribute", c(203, 75, 22)),
        ("escape", c(203, 75, 22)),
        ("punctuation", c(88, 110, 117)),
        ("embedded", c(101, 123, 131)),
    ],
};

pub const ONE_DARK: TsTheme = TsTheme {
    name: "One Dark",
    colors: &[
        ("comment", c(92, 101, 122)),
        ("string", c(152, 195, 121)),
        ("string.special", c(224, 175, 104)),
        ("number", c(209, 154, 102)),
        ("constant", c(224, 175, 104)),
        ("keyword", c(198, 120, 221)),
        ("operator", c(86, 182, 194)),
        ("function", c(97, 175, 239)),
        ("type", c(224, 175, 104)),
        ("constructor", c(224, 175, 104)),
        ("variable", c(171, 178, 191)),
        ("variable.builtin", c(86, 182, 194)),
        ("variable.parameter", c(224, 175, 104)),
        ("property", c(224, 175, 104)),
        ("label", c(97, 175, 239)),
        ("attribute", c(224, 175, 104)),
        ("escape", c(209, 154, 102)),
        ("punctuation", c(171, 178, 191)),
        ("embedded", c(171, 178, 191)),
    ],
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_name_beats_category() {
        let t = &OCEAN_DARK;
        // variable.parameter is yellow (235,203,139), distinct from the
        // base variable color.
        let vp = t.color_for("variable.parameter").unwrap();
        assert_eq!(vp, c(235, 203, 139));
    }

    #[test]
    fn category_fallback_for_undefined_dotted() {
        let t = &OCEAN_DARK;
        // function.call isn't defined explicitly, but function is.
        let fc = t.color_for("function.call").unwrap();
        let f = t.color_for("function").unwrap();
        assert_eq!(fc, f);
    }

    #[test]
    fn unknown_capture_returns_none() {
        let t = &OCEAN_DARK;
        assert!(t.color_for("totally.made.up").is_none());
    }

    #[test]
    fn punctuation_normalisation() {
        let t = &OCEAN_DARK;
        // punctuation.bracket has no explicit entry but normalises to
        // the punctuation category.
        let pb = t.color_for("punctuation.bracket").unwrap();
        let p = t.color_for("punctuation").unwrap();
        assert_eq!(pb, p);
    }

    #[test]
    fn bundled_themes_have_unique_names() {
        let names: Vec<&str> = bundled_themes().iter().map(|t| t.name).collect();
        let mut sorted = names.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), names.len(), "theme names must be unique");
    }

    #[test]
    fn find_theme_returns_by_name() {
        assert_eq!(find_theme("Ocean Dark").unwrap().name, "Ocean Dark");
        assert!(find_theme("nonexistent").is_none());
    }

    #[test]
    fn default_is_ocean_dark_for_continuity() {
        // base16-ocean.dark was the syntect default; keep Ocean Dark first
        // so existing config.theme values have a sensible match target.
        assert_eq!(bundled_themes()[0].name, "Ocean Dark");
    }
}
