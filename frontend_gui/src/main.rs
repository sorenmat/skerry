//! GUI frontend entry point.
//!
//! Usage: `frontend_gui [PATH]` — opens PATH if given, otherwise an
//! unsaved buffer. Same feature set as `frontend_tui` (ADR 0005).

use std::env;
use std::path::PathBuf;

use core::{Buffer, PieceTableBuffer};
use eframe::egui;

use frontend_gui::app::EditorApp;

fn main() -> eframe::Result<()> {
    let path = env::args().nth(1).map(PathBuf::from);
    let buffer = load_buffer(path.as_ref());

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([800.0, 600.0])
            .with_title("the_editor"),
        ..Default::default()
    };

    eframe::run_native(
        "the_editor",
        native_options,
        Box::new(|_cc| Ok(Box::new(EditorApp::new(buffer)))),
    )
}

fn load_buffer(path: Option<&PathBuf>) -> Box<dyn Buffer> {
    match path {
        Some(p) if p.exists() => {
            // from_path memory-maps the file — multi-GB logs never load
            // into RAM as a whole. ADR 0002's payoff.
            Box::new(PieceTableBuffer::from_path(p.clone())
                .unwrap_or_else(|_| PieceTableBuffer::new()))
        }
        Some(p) => Box::new(PieceTableBuffer::from_bytes_with_path(Vec::new(), p.clone())),
        None => Box::new(PieceTableBuffer::new()),
    }
}