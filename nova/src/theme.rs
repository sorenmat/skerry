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
        &[DARK, LIGHT]
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
    accent_text: Color32::WHITE,
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
    accent_hover: Color32::from_rgb(60, 125, 230),
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
