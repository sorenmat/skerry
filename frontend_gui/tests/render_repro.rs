//! Offscreen reproduction of the selection-rendering bug.
//!
//! Run with: cargo test -p frontend_gui --test render_repro -- --nocapture
//!
//! Loads the workspace Makefile, sets the same multi-line selection
//! the user reported (anchor at line 21 col 15, head at line 27 col 14),
//! and runs the editor's render code inside an egui::Context.
//! It dumps font metrics and per-line selection math so we can see
//! exactly what gets drawn.

use core::{Buffer, PieceTableBuffer, Selection};
use eframe::egui;

#[test]
fn reproduce_selection_bug() {
    // The workspace Makefile is one level up from the gui crate.
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
    let path = std::path::PathBuf::from(&manifest)
        .parent()
        .map(|p| p.join("Makefile"))
        .unwrap_or_else(|| std::path::PathBuf::from("Makefile"));
    eprintln!("loading: {}", path.display());

    let buf: Box<dyn Buffer> = Box::new(
        PieceTableBuffer::from_path(path).expect("load Makefile"),
    );
    let mut app = frontend_gui::app::EditorApp::new(buf);

    // Selection matching the user's reported scenario:
    //   anchor at line 21 col 15 (1-indexed)
    //   head   at line 27 col 14 (1-indexed)
    let anchor = app
        .buffer
        .linecol_to_pos(21 - 1, 15 - 1)
        .expect("anchor pos");
    let head = app
        .buffer
        .linecol_to_pos(27 - 1, 14 - 1)
        .expect("head pos");

    app.buffer.set_cursor(head);
    app.buffer.set_selection(Selection { anchor, head });

    eprintln!("=== STATE ===");
    eprintln!("anchor byte = {anchor}, head byte = {head}");
    eprintln!("line 27 text = {:?}", app.buffer.line_text(26).map(|c| c.into_owned()));
    eprintln!("selection = {:?}", app.buffer.selection());
    eprintln!(
        "anchor (line,col) = {:?}, head (line,col) = {:?}",
        app.buffer.pos_to_linecol(anchor),
        app.buffer.pos_to_linecol(head),
    );

    let ctx = egui::Context::default();
    let raw_input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::Vec2::new(800.0, 600.0),
        )),
        ..Default::default()
    };

    let output = ctx.run(raw_input, |ctx| {
        frontend_gui::ui::render(ctx, &mut app);
    });

    eprintln!("\n=== PAINT SHAPES (first 80) ===");
    eprintln!("shapes count: {}", output.shapes.len());
    for (i, shape) in output.shapes.iter().enumerate().take(80) {
        eprintln!("  shape[{i:3}]: {shape:?}");
    }
}