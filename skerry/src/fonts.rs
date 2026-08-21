//! Bundled editor fonts.
//!
//! Skerry renders everything in a single monospace family. egui's built-in
//! default for that family is Hack; we prepend JetBrains Mono (bundled here
//! under SIL OFL 1.1 — see `assets/fonts/OFL.txt`) so every machine draws the
//! same glyphs and metrics. egui's own fallbacks stay behind it for the
//! characters the font lacks (√, emoji).

use std::sync::Arc;

use eframe::egui;

const JETBRAINS_MONO_REGULAR: &[u8] = include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf");

/// Build the app's [`egui::FontDefinitions`]: egui's defaults with JetBrains
/// Mono prepended to the monospace family.
pub fn font_definitions() -> egui::FontDefinitions {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "JetBrainsMono".to_owned(),
        Arc::new(egui::FontData::from_static(JETBRAINS_MONO_REGULAR)),
    );
    fonts
        .families
        .get_mut(&egui::FontFamily::Monospace)
        .expect("default FontDefinitions define the monospace family")
        .insert(0, "JetBrainsMono".to_owned());
    fonts
}

/// Install the bundled fonts on `ctx`. Call once before the first frame.
pub fn install(ctx: &egui::Context) {
    ctx.set_fonts(font_definitions());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jetbrains_mono_leads_the_monospace_family() {
        let fonts = font_definitions();
        assert!(fonts.font_data.contains_key("JetBrainsMono"));
        let mono = &fonts.families[&egui::FontFamily::Monospace];
        assert_eq!(mono.first().map(String::as_str), Some("JetBrainsMono"));
        // egui's built-in fonts stay behind it as fallbacks.
        assert!(mono.iter().any(|name| name == "Hack"));
    }

    #[test]
    fn rasterizes_with_monospace_metrics() {
        let ctx = egui::Context::default();
        install(&ctx);
        let font_id = egui::FontId::monospace(14.0);
        // egui only builds its font atlas inside a run() pass. glyph_width
        // forces the TTF to parse and rasterize — a corrupt or missing file
        // would panic here, not silently fall back.
        let mut metrics = (0.0f32, 0.0f32, 0.0f32);
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            metrics = ctx.fonts(|f| {
                (
                    f.glyph_width(&font_id, 'M'),
                    f.glyph_width(&font_id, 'i'),
                    f.row_height(&font_id),
                )
            })
        });
        let (m_width, i_width, height) = metrics;
        assert!(m_width > 0.0);
        assert!(height > 0.0);
        // Monospace contract: i and M share an advance width.
        assert!((m_width - i_width).abs() < 1e-6);
    }
}
