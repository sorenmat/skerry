//! GUI chrome theme system.
//!
//! A `GuiTheme` is a complete color palette for the editor surface and all
//! egui chrome. The active theme is applied to egui's global `Visuals` each
//! frame so native widgets (buttons, inputs, dropdowns, windows) pick it up
//! automatically.

use eframe::egui::{self, Color32, Shadow, Stroke, Vec2, Visuals};

/// A complete UI color palette.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GuiTheme {
    pub name: &'static str,
    pub is_dark: bool,

    // Editor surface
    pub editor_bg: Color32,
    pub text: Color32,
    pub caret: Color32,
    pub line_highlight: Color32,
    pub selection_bg: Color32,

    // Gutter
    pub gutter_bg: Color32,
    pub gutter_text: Color32,
    pub line_number_active: Color32,

    // Chrome panels
    pub panel_bg: Color32,
    pub panel_text: Color32,
    pub dim_text: Color32,
    pub status_bg: Color32,
    pub status_text: Color32,
    pub sidebar_bg: Color32,

    // Accents
    pub accent: Color32,
    pub accent_text: Color32,
    pub accent_hover: Color32,
    pub error: Color32,
    pub warning: Color32,

    // Widgets
    pub button_bg: Color32,
    pub button_text: Color32,
    pub input_bg: Color32,
    pub border: Color32,
    pub separator: Color32,
    pub indent_guide: Color32,

    // Popups / floating windows
    pub popup_bg: Color32,
    pub popup_border: Color32,

    // Find/replace matches
    pub match_current: Color32,
    pub match_current_text: Color32,
    pub match_other: Color32,
    pub match_other_text: Color32,

    // Selected list rows
    pub selected_bg: Color32,

    // Git gutter
    pub git_added: Color32,
    pub git_modified: Color32,
    pub git_deleted: Color32,
}

impl GuiTheme {
    /// Apply this theme to egui's global visuals.
    pub fn apply(&self, ctx: &egui::Context) {
        let mut visuals = if self.is_dark {
            Visuals::dark()
        } else {
            Visuals::light()
        };

        visuals.dark_mode = self.is_dark;
        visuals.override_text_color = Some(self.text);

        // Panels and floating windows.
        visuals.panel_fill = self.panel_bg;
        visuals.window_fill = self.popup_bg;
        visuals.window_stroke = Stroke::new(1.0_f32, self.popup_border);
        visuals.window_rounding = egui::Rounding::same(8.0);
        visuals.menu_rounding = egui::Rounding::same(6.0);
        visuals.window_shadow = Shadow {
            offset: Vec2::new(0.0, 8.0),
            blur: 20.0,
            spread: 0.0,
            color: if self.is_dark {
                Color32::from_black_alpha(120)
            } else {
                Color32::from_black_alpha(60)
            },
        };
        visuals.popup_shadow = visuals.window_shadow;

        // Selection / highlights / inputs.
        visuals.selection.bg_fill = self.selection_bg;
        visuals.faint_bg_color = self.line_highlight;
        visuals.extreme_bg_color = self.input_bg;
        visuals.hyperlink_color = self.accent;
        visuals.error_fg_color = self.error;
        visuals.warn_fg_color = self.warning;

        // Widget chrome. Set rounded corners on every state so buttons,
        // combo boxes and inputs look soft instead of boxy.
        visuals.button_frame = true;
        let rounding = egui::Rounding::same(4.0);
        let text_cursor = visuals.text_cursor;

        let mut states = [
            &mut visuals.widgets.noninteractive,
            &mut visuals.widgets.inactive,
            &mut visuals.widgets.hovered,
            &mut visuals.widgets.active,
            &mut visuals.widgets.open,
        ];
        for state in &mut states {
            state.rounding = rounding;
        }
        visuals.widgets.noninteractive.bg_fill = self.panel_bg;
        visuals.widgets.noninteractive.fg_stroke.color = self.panel_text;
        visuals.widgets.noninteractive.weak_bg_fill = self.panel_bg;

        visuals.widgets.inactive.bg_fill = self.button_bg;
        visuals.widgets.inactive.weak_bg_fill = self.button_bg;
        visuals.widgets.inactive.fg_stroke.color = self.button_text;

        visuals.widgets.hovered.bg_fill = self.accent_hover;
        visuals.widgets.hovered.weak_bg_fill = self.accent_hover;
        visuals.widgets.hovered.fg_stroke.color = self.accent_text;

        visuals.widgets.active.bg_fill = self.accent;
        visuals.widgets.active.weak_bg_fill = self.accent;
        visuals.widgets.active.fg_stroke.color = self.accent_text;

        visuals.widgets.open.bg_fill = self.accent;
        visuals.widgets.open.weak_bg_fill = self.accent;
        visuals.widgets.open.fg_stroke.color = self.accent_text;

        // Keep the text cursor style from the base theme, just ensure it uses
        // the editor caret color.
        visuals.text_cursor = text_cursor;

        ctx.set_visuals(visuals);
    }

    /// Look up a built-in theme by name (case-insensitive).
    pub fn by_name(name: &str) -> Option<&'static Self> {
        let name = name.to_ascii_lowercase();
        Self::all()
            .iter()
            .find(|t| t.name.eq_ignore_ascii_case(&name))
    }

    /// Default fallback theme.
    pub fn default_dark() -> &'static Self {
        &DARK
    }

    /// All built-in themes.
    pub fn all() -> &'static [Self] {
        &[
            DARK,
            ONE_DARK,
            FJORD_NIGHT,
            AUBERGINE,
            SANDSTONE,
            HIGH_CONTRAST,
            LIGHT,
        ]
    }
}

/// Modern dark editor chrome.
pub const DARK: GuiTheme = GuiTheme {
    name: "Dark",
    is_dark: true,

    editor_bg: Color32::from_rgb(30, 30, 30),
    text: Color32::from_rgb(220, 220, 220),
    caret: Color32::from_rgb(200, 200, 200),
    line_highlight: Color32::from_rgb(45, 45, 48),
    selection_bg: Color32::from_rgb(60, 100, 160),

    gutter_bg: Color32::from_rgb(37, 37, 38),
    gutter_text: Color32::from_rgb(100, 100, 100),
    line_number_active: Color32::from_rgb(200, 200, 200),

    panel_bg: Color32::from_rgb(45, 45, 48),
    panel_text: Color32::from_rgb(200, 200, 200),
    dim_text: Color32::from_rgb(150, 150, 150),
    status_bg: Color32::from_rgb(45, 45, 48),
    status_text: Color32::from_rgb(200, 200, 200),
    sidebar_bg: Color32::from_rgb(40, 40, 42),

    accent: Color32::from_rgb(60, 130, 220),
    accent_text: Color32::BLACK,
    accent_hover: Color32::from_rgb(80, 155, 250),
    error: Color32::from_rgb(230, 90, 90),
    warning: Color32::from_rgb(230, 190, 80),

    button_bg: Color32::from_rgb(65, 65, 68),
    button_text: Color32::from_rgb(230, 230, 230),
    input_bg: Color32::from_rgb(55, 55, 58),
    border: Color32::from_rgb(80, 80, 80),
    separator: Color32::from_rgb(70, 70, 70),
    indent_guide: Color32::from_rgb(50, 50, 55),

    popup_bg: Color32::from_rgb(48, 48, 51),
    popup_border: Color32::from_rgb(85, 85, 88),

    match_current: Color32::from_rgb(200, 160, 40),
    match_current_text: Color32::BLACK,
    match_other: Color32::from_rgb(120, 100, 40),
    match_other_text: Color32::WHITE,

    selected_bg: Color32::from_rgb(60, 90, 140),

    git_added: Color32::from_rgb(80, 180, 100),
    git_modified: Color32::from_rgb(230, 170, 70),
    git_deleted: Color32::from_rgb(230, 90, 90),
};

/// Clean light editor chrome.
pub const LIGHT: GuiTheme = GuiTheme {
    name: "Light",
    is_dark: false,

    editor_bg: Color32::WHITE,
    text: Color32::from_rgb(40, 40, 40),
    caret: Color32::from_rgb(40, 40, 40),
    line_highlight: Color32::from_rgb(245, 245, 245),
    selection_bg: Color32::from_rgb(170, 210, 250),

    gutter_bg: Color32::from_rgb(250, 250, 250),
    gutter_text: Color32::from_rgb(130, 130, 130),
    line_number_active: Color32::from_rgb(80, 80, 80),

    panel_bg: Color32::from_rgb(245, 245, 245),
    panel_text: Color32::from_rgb(60, 60, 60),
    dim_text: Color32::from_rgb(120, 120, 120),
    status_bg: Color32::from_rgb(245, 245, 245),
    status_text: Color32::from_rgb(60, 60, 60),
    sidebar_bg: Color32::from_rgb(245, 245, 245),

    accent: Color32::from_rgb(40, 100, 200),
    accent_text: Color32::WHITE,
    accent_hover: Color32::from_rgb(45, 105, 205),
    error: Color32::from_rgb(200, 60, 60),
    warning: Color32::from_rgb(200, 150, 40),

    button_bg: Color32::from_rgb(235, 235, 235),
    button_text: Color32::from_rgb(50, 50, 50),
    input_bg: Color32::WHITE,
    border: Color32::from_rgb(200, 200, 200),
    separator: Color32::from_rgb(220, 220, 220),
    indent_guide: Color32::from_rgb(235, 235, 235),

    popup_bg: Color32::from_rgb(250, 250, 250),
    popup_border: Color32::from_rgb(200, 200, 200),

    match_current: Color32::from_rgb(240, 180, 60),
    match_current_text: Color32::BLACK,
    match_other: Color32::from_rgb(210, 180, 110),
    match_other_text: Color32::BLACK,

    selected_bg: Color32::from_rgb(200, 220, 250),

    git_added: Color32::from_rgb(40, 150, 70),
    git_modified: Color32::from_rgb(210, 140, 30),
    git_deleted: Color32::from_rgb(210, 60, 60),
};

/// Cool blue-grey palette with restrained cyan accents.
pub const FJORD_NIGHT: GuiTheme = GuiTheme {
    name: "Fjord Night",
    is_dark: true,
    editor_bg: Color32::from_rgb(28, 33, 46),
    text: Color32::from_rgb(216, 222, 233),
    caret: Color32::from_rgb(136, 192, 208),
    line_highlight: Color32::from_rgb(36, 42, 58),
    selection_bg: Color32::from_rgb(59, 76, 105),
    gutter_bg: Color32::from_rgb(31, 37, 51),
    gutter_text: Color32::from_rgb(101, 112, 139),
    line_number_active: Color32::from_rgb(229, 233, 240),
    panel_bg: Color32::from_rgb(35, 41, 56),
    panel_text: Color32::from_rgb(216, 222, 233),
    dim_text: Color32::from_rgb(129, 143, 166),
    status_bg: Color32::from_rgb(39, 46, 63),
    status_text: Color32::from_rgb(216, 222, 233),
    sidebar_bg: Color32::from_rgb(32, 38, 52),
    accent: Color32::from_rgb(94, 175, 201),
    accent_text: Color32::from_rgb(24, 29, 40),
    accent_hover: Color32::from_rgb(118, 198, 223),
    error: Color32::from_rgb(218, 112, 116),
    warning: Color32::from_rgb(235, 203, 139),
    button_bg: Color32::from_rgb(49, 58, 79),
    button_text: Color32::from_rgb(229, 233, 240),
    input_bg: Color32::from_rgb(25, 30, 42),
    border: Color32::from_rgb(65, 76, 101),
    separator: Color32::from_rgb(53, 63, 84),
    indent_guide: Color32::from_rgb(43, 51, 69),
    popup_bg: Color32::from_rgb(39, 46, 63),
    popup_border: Color32::from_rgb(76, 87, 112),
    match_current: Color32::from_rgb(235, 203, 139),
    match_current_text: Color32::from_rgb(28, 33, 46),
    match_other: Color32::from_rgb(92, 82, 62),
    match_other_text: Color32::from_rgb(229, 233, 240),
    selected_bg: Color32::from_rgb(59, 76, 105),
    git_added: Color32::from_rgb(163, 190, 140),
    git_modified: Color32::from_rgb(235, 203, 139),
    git_deleted: Color32::from_rgb(218, 112, 116),
};

/// Deep plum palette with lavender and coral accents.
pub const AUBERGINE: GuiTheme = GuiTheme {
    name: "Aubergine",
    is_dark: true,
    editor_bg: Color32::from_rgb(35, 27, 43),
    text: Color32::from_rgb(237, 230, 244),
    caret: Color32::from_rgb(221, 170, 255),
    line_highlight: Color32::from_rgb(48, 37, 58),
    selection_bg: Color32::from_rgb(91, 62, 111),
    gutter_bg: Color32::from_rgb(40, 31, 49),
    gutter_text: Color32::from_rgb(132, 111, 145),
    line_number_active: Color32::from_rgb(245, 238, 250),
    panel_bg: Color32::from_rgb(46, 35, 56),
    panel_text: Color32::from_rgb(232, 222, 239),
    dim_text: Color32::from_rgb(156, 132, 169),
    status_bg: Color32::from_rgb(52, 39, 62),
    status_text: Color32::from_rgb(232, 222, 239),
    sidebar_bg: Color32::from_rgb(41, 31, 50),
    accent: Color32::from_rgb(190, 117, 226),
    accent_text: Color32::from_rgb(35, 27, 43),
    accent_hover: Color32::from_rgb(211, 145, 241),
    error: Color32::from_rgb(244, 112, 144),
    warning: Color32::from_rgb(244, 190, 106),
    button_bg: Color32::from_rgb(68, 50, 80),
    button_text: Color32::from_rgb(240, 232, 246),
    input_bg: Color32::from_rgb(31, 23, 38),
    border: Color32::from_rgb(91, 67, 105),
    separator: Color32::from_rgb(69, 51, 81),
    indent_guide: Color32::from_rgb(55, 41, 66),
    popup_bg: Color32::from_rgb(50, 38, 60),
    popup_border: Color32::from_rgb(99, 72, 114),
    match_current: Color32::from_rgb(244, 190, 106),
    match_current_text: Color32::from_rgb(35, 27, 43),
    match_other: Color32::from_rgb(105, 73, 79),
    match_other_text: Color32::WHITE,
    selected_bg: Color32::from_rgb(91, 62, 111),
    git_added: Color32::from_rgb(133, 214, 153),
    git_modified: Color32::from_rgb(244, 190, 106),
    git_deleted: Color32::from_rgb(244, 112, 144),
};

/// Warm low-contrast light palette suited to long writing sessions.
pub const SANDSTONE: GuiTheme = GuiTheme {
    name: "Sandstone",
    is_dark: false,
    editor_bg: Color32::from_rgb(250, 246, 235),
    text: Color32::from_rgb(67, 61, 52),
    caret: Color32::from_rgb(160, 76, 51),
    line_highlight: Color32::from_rgb(243, 237, 222),
    selection_bg: Color32::from_rgb(220, 205, 169),
    gutter_bg: Color32::from_rgb(246, 241, 229),
    gutter_text: Color32::from_rgb(151, 139, 118),
    line_number_active: Color32::from_rgb(86, 75, 62),
    panel_bg: Color32::from_rgb(239, 232, 216),
    panel_text: Color32::from_rgb(75, 67, 56),
    dim_text: Color32::from_rgb(137, 124, 105),
    status_bg: Color32::from_rgb(235, 226, 208),
    status_text: Color32::from_rgb(75, 67, 56),
    sidebar_bg: Color32::from_rgb(243, 237, 224),
    accent: Color32::from_rgb(174, 82, 55),
    accent_text: Color32::WHITE,
    accent_hover: Color32::from_rgb(180, 88, 61),
    error: Color32::from_rgb(184, 63, 63),
    warning: Color32::from_rgb(170, 116, 35),
    button_bg: Color32::from_rgb(229, 219, 198),
    button_text: Color32::from_rgb(69, 61, 51),
    input_bg: Color32::from_rgb(255, 252, 244),
    border: Color32::from_rgb(207, 195, 172),
    separator: Color32::from_rgb(219, 209, 189),
    indent_guide: Color32::from_rgb(232, 224, 207),
    popup_bg: Color32::from_rgb(250, 246, 235),
    popup_border: Color32::from_rgb(199, 186, 162),
    match_current: Color32::from_rgb(235, 184, 88),
    match_current_text: Color32::from_rgb(53, 47, 39),
    match_other: Color32::from_rgb(232, 215, 163),
    match_other_text: Color32::from_rgb(53, 47, 39),
    selected_bg: Color32::from_rgb(220, 205, 169),
    git_added: Color32::from_rgb(78, 137, 79),
    git_modified: Color32::from_rgb(179, 119, 32),
    git_deleted: Color32::from_rgb(184, 63, 63),
};

/// Familiar charcoal palette paired with the bundled One Dark syntax colors.
pub const ONE_DARK: GuiTheme = GuiTheme {
    name: "One Dark",
    is_dark: true,
    editor_bg: Color32::from_rgb(40, 44, 52),
    text: Color32::from_rgb(171, 178, 191),
    caret: Color32::from_rgb(82, 139, 255),
    line_highlight: Color32::from_rgb(44, 49, 58),
    selection_bg: Color32::from_rgb(62, 68, 81),
    gutter_bg: Color32::from_rgb(40, 44, 52),
    gutter_text: Color32::from_rgb(91, 99, 115),
    line_number_active: Color32::from_rgb(171, 178, 191),
    panel_bg: Color32::from_rgb(33, 37, 43),
    panel_text: Color32::from_rgb(171, 178, 191),
    dim_text: Color32::from_rgb(108, 117, 135),
    status_bg: Color32::from_rgb(33, 37, 43),
    status_text: Color32::from_rgb(171, 178, 191),
    sidebar_bg: Color32::from_rgb(37, 41, 48),
    accent: Color32::from_rgb(82, 139, 255),
    accent_text: Color32::from_rgb(20, 24, 30),
    accent_hover: Color32::from_rgb(104, 157, 255),
    error: Color32::from_rgb(224, 108, 117),
    warning: Color32::from_rgb(229, 192, 123),
    button_bg: Color32::from_rgb(53, 59, 69),
    button_text: Color32::from_rgb(202, 207, 216),
    input_bg: Color32::from_rgb(30, 33, 39),
    border: Color32::from_rgb(62, 68, 81),
    separator: Color32::from_rgb(50, 55, 65),
    indent_guide: Color32::from_rgb(48, 53, 63),
    popup_bg: Color32::from_rgb(44, 49, 58),
    popup_border: Color32::from_rgb(69, 76, 90),
    match_current: Color32::from_rgb(229, 192, 123),
    match_current_text: Color32::from_rgb(40, 44, 52),
    match_other: Color32::from_rgb(104, 88, 50),
    match_other_text: Color32::WHITE,
    selected_bg: Color32::from_rgb(62, 68, 81),
    git_added: Color32::from_rgb(152, 195, 121),
    git_modified: Color32::from_rgb(229, 192, 123),
    git_deleted: Color32::from_rgb(224, 108, 117),
};

/// Maximum-separation dark palette for accessibility and bright environments.
pub const HIGH_CONTRAST: GuiTheme = GuiTheme {
    name: "High Contrast",
    is_dark: true,
    editor_bg: Color32::BLACK,
    text: Color32::WHITE,
    caret: Color32::from_rgb(0, 255, 255),
    line_highlight: Color32::from_rgb(24, 24, 24),
    selection_bg: Color32::from_rgb(0, 80, 145),
    gutter_bg: Color32::BLACK,
    gutter_text: Color32::from_rgb(180, 180, 180),
    line_number_active: Color32::WHITE,
    panel_bg: Color32::from_rgb(12, 12, 12),
    panel_text: Color32::WHITE,
    dim_text: Color32::from_rgb(190, 190, 190),
    status_bg: Color32::from_rgb(0, 45, 80),
    status_text: Color32::WHITE,
    sidebar_bg: Color32::from_rgb(8, 8, 8),
    accent: Color32::from_rgb(0, 170, 255),
    accent_text: Color32::BLACK,
    accent_hover: Color32::from_rgb(70, 205, 255),
    error: Color32::from_rgb(255, 90, 90),
    warning: Color32::from_rgb(255, 220, 70),
    button_bg: Color32::from_rgb(30, 30, 30),
    button_text: Color32::WHITE,
    input_bg: Color32::BLACK,
    border: Color32::WHITE,
    separator: Color32::from_rgb(150, 150, 150),
    indent_guide: Color32::from_rgb(80, 80, 80),
    popup_bg: Color32::BLACK,
    popup_border: Color32::WHITE,
    match_current: Color32::YELLOW,
    match_current_text: Color32::BLACK,
    match_other: Color32::from_rgb(120, 100, 0),
    match_other_text: Color32::WHITE,
    selected_bg: Color32::from_rgb(0, 80, 145),
    git_added: Color32::from_rgb(70, 255, 100),
    git_modified: Color32::YELLOW,
    git_deleted: Color32::from_rgb(255, 90, 90),
};

#[cfg(test)]
mod tests {
    use super::*;

    fn contrast_ratio(a: Color32, b: Color32) -> f32 {
        fn luminance(color: Color32) -> f32 {
            let channel = |value: u8| {
                let value = f32::from(value) / 255.0;
                if value <= 0.04045 {
                    value / 12.92
                } else {
                    ((value + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * channel(color.r()) + 0.7152 * channel(color.g()) + 0.0722 * channel(color.b())
        }
        let (lighter, darker) = {
            let a = luminance(a);
            let b = luminance(b);
            if a >= b {
                (a, b)
            } else {
                (b, a)
            }
        };
        (lighter + 0.05) / (darker + 0.05)
    }

    #[test]
    fn bundled_theme_names_are_unique_and_resolvable() {
        let mut names: Vec<&str> = GuiTheme::all().iter().map(|theme| theme.name).collect();
        let count = names.len();
        names.sort_unstable();
        names.dedup();

        assert_eq!(names.len(), count);
        for name in names {
            assert_eq!(GuiTheme::by_name(name).map(|theme| theme.name), Some(name));
        }
    }

    #[test]
    fn curated_interface_themes_have_matching_syntax_palettes() {
        for name in [
            "One Dark",
            "Fjord Night",
            "Aubergine",
            "Sandstone",
            "High Contrast",
        ] {
            assert!(
                GuiTheme::by_name(name).is_some(),
                "missing UI theme: {name}"
            );
            assert!(
                core::ts::find_theme(name).is_some(),
                "missing syntax theme: {name}"
            );
        }
    }

    #[test]
    fn theme_controls_keep_readable_text() {
        for theme in GuiTheme::all() {
            assert!(
                contrast_ratio(theme.text, theme.editor_bg) >= 4.5,
                "{} CSV row-number contrast",
                theme.name
            );
            assert!(
                contrast_ratio(theme.text, theme.line_highlight) >= 4.5,
                "{} striped CSV row-number contrast",
                theme.name
            );
            assert!(
                contrast_ratio(theme.button_text, theme.button_bg) >= 4.5,
                "{} inactive control and table-header contrast",
                theme.name
            );
            assert!(
                contrast_ratio(theme.accent_text, theme.accent) >= 4.5,
                "{} active control contrast",
                theme.name
            );
            assert!(
                contrast_ratio(theme.accent_text, theme.accent_hover) >= 4.5,
                "{} hover control contrast",
                theme.name
            );
        }
    }
}
