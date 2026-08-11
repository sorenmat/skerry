//! GUI frontend entry point.
//!
//! Usage: `skerry [PATH...]` — opens one document per PATH
//! argument. With no PATH, opens one unsaved buffer. Same feature
//! set as `skerry-tui` (ADR 0005).

use std::env;
use std::path::{Path, PathBuf};

use core::{Buffer, Document, PieceTableBuffer};
use eframe::egui;

use skerry::app::EditorApp;

fn main() -> eframe::Result<()> {
    let config = core::Config::load();
    let args: Vec<PathBuf> = env::args().skip(1).map(PathBuf::from).collect();
    let documents = if args.is_empty() && !config.recent_files.is_empty() {
        config
            .recent_files
            .iter()
            .map(|p| load_document(p.as_path(), &config))
            .collect()
    } else {
        load_documents(&args, &config)
    };

    let initial_size = match (config.window_width, config.window_height) {
        (Some(w), Some(h)) => [w as f32, h as f32],
        _ => [800.0, 600.0],
    };
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size(initial_size)
            .with_title("Skerry"),
        ..Default::default()
    };

    eframe::run_native(
        "Skerry",
        native_options,
        Box::new(|_cc| Ok(Box::new(EditorApp::new_with_documents(documents, config)))),
    )
}

/// Load each path into its own [`Document`]. Existing files are
/// memory-mapped via [`PieceTableBuffer::from_path`] (ADR 0002 — the
/// multi-GB-file path); paths that don't exist yet get a fresh empty
/// buffer that will save back to that path. With no paths, returns a
/// single empty document so the editor still has somewhere to land.
fn load_documents(paths: &[PathBuf], config: &core::Config) -> Vec<Document> {
    if paths.is_empty() {
        return vec![Document::new_with_config(
            Box::new(PieceTableBuffer::new()),
            config,
        )];
    }
    paths
        .iter()
        .map(|p| load_document(p.as_path(), config))
        .collect()
}

fn load_document(path: &Path, config: &core::Config) -> Document {
    let buffer: Box<dyn Buffer> = if path.exists() {
        Box::new(
            PieceTableBuffer::from_path(path.to_path_buf())
                .unwrap_or_else(|_| PieceTableBuffer::new()),
        )
    } else {
        Box::new(PieceTableBuffer::from_bytes_with_path(
            Vec::new(),
            path.to_path_buf(),
        ))
    };
    Document::new_with_config(buffer, config)
}
