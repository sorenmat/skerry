//! GUI frontend entry point.
//!
//! Usage: `frontend_gui [PATH...]` — opens one document per PATH
//! argument. With no PATH, opens one unsaved buffer. Same feature
//! set as `frontend_tui` (ADR 0005).

use std::env;
use std::path::{Path, PathBuf};

use core::{Buffer, Document, PieceTableBuffer};
use eframe::egui;

use frontend_gui::app::EditorApp;

fn main() -> eframe::Result<()> {
    let paths: Vec<PathBuf> = env::args().skip(1).map(PathBuf::from).collect();
    let documents = load_documents(&paths);

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([800.0, 600.0])
            .with_title("the_editor"),
        ..Default::default()
    };

    eframe::run_native(
        "the_editor",
        native_options,
        Box::new(|_cc| Ok(Box::new(EditorApp::new_with_documents(documents)))),
    )
}

/// Load each path into its own [`Document`]. Existing files are
/// memory-mapped via [`PieceTableBuffer::from_path`] (ADR 0002 — the
/// multi-GB-file path); paths that don't exist yet get a fresh empty
/// buffer that will save back to that path. With no paths, returns a
/// single empty document so the editor still has somewhere to land.
fn load_documents(paths: &[PathBuf]) -> Vec<Document> {
    if paths.is_empty() {
        return vec![Document::empty()];
    }
    paths.iter().map(|p| load_document(p.as_path())).collect()
}

fn load_document(path: &Path) -> Document {
    let buffer: Box<dyn Buffer> = if path.exists() {
        Box::new(
            PieceTableBuffer::from_path(path.to_path_buf())
                .unwrap_or_else(|_| PieceTableBuffer::new()),
        )
    } else {
        Box::new(PieceTableBuffer::from_bytes_with_path(Vec::new(), path.to_path_buf()))
    };
    Document::new(buffer)
}