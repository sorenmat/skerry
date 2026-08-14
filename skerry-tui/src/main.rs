//! TUI frontend entry point.
//!
//! Usage: `skerry-tui [PATH...]` — opens one document per PATH
//! argument. With no PATH, opens one unsaved buffer. Saves go back to
//! the original PATH for each document (Ctrl+S); unsaved buffers with
//! no path can't be saved (yet).
//!
//! Terminal state is restored via a Drop guard so that panics during the
//! event loop don't leave the user's terminal in raw mode.

use std::env;
use std::error::Error;
use std::path::{Path, PathBuf};
use std::process;

use core::{Buffer, Document, PieceTableBuffer};

mod app;
mod event;
mod ui;

#[cfg(test)]
mod screenshot;
#[cfg(test)]
mod ui_tests;

use app::App;

/// Drop guard that restores the terminal even on panic. The TUI puts
/// the terminal into raw mode at start; if a panic happens during the
/// event loop and we don't restore, the user's shell is broken until
/// they reset it manually.
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        // Disable mouse capture BEFORE ratatui::restore().  The restore
        // function only leaves the alternate screen and disables raw
        // mode — it does NOT send DisableMouseCapture.  Without this,
        // the user's terminal is left with mouse capture enabled after
        // exit, breaking mouse clicks in their shell until `reset`.
        let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableMouseCapture);
        let _ = crossterm::execute!(
            std::io::stdout(),
            crossterm::cursor::SetCursorStyle::DefaultUserShape
        );
        ratatui::restore();
    }
}

fn main() {
    if let Err(e) = run() {
        eprintln!("skerry-tui: {e}");
        process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let config = core::Config::load();
    let args = parse_path_args();
    let documents = if args.is_empty() && !config.recent_files.is_empty() {
        load_documents(&config.recent_files, &config)?
    } else {
        load_documents(&args, &config)?
    };

    let _guard = TerminalGuard;
    let mut terminal = ratatui::init();

    // Enable mouse capture so we get MouseEvent on click/drag. Without
    // this, crossterm never delivers mouse events.
    if let Err(e) = crossterm::execute!(std::io::stdout(), crossterm::event::EnableMouseCapture) {
        eprintln!("warning: failed to enable mouse capture: {e}");
    }

    let mut app = App::new_with_documents(documents, config);
    let result = app.run(&mut terminal);

    // Drop the terminal explicitly before the guard runs so the guard's
    // restore() runs against a clean Terminal state.
    drop(terminal);

    result
}

fn parse_path_args() -> Vec<PathBuf> {
    env::args().skip(1).map(PathBuf::from).collect()
}

/// Load each path into its own [`Document`]. Existing files are
/// memory-mapped via [`PieceTableBuffer::from_path`] (ADR 0002 — the
/// multi-GB-file path); paths that don't exist yet get a fresh empty
/// buffer that will save back to that path. With no paths, returns a
/// single empty document so the editor still has somewhere to land.
fn load_documents(
    paths: &[PathBuf],
    config: &core::Config,
) -> Result<Vec<Document>, Box<dyn Error>> {
    if paths.is_empty() {
        return Ok(vec![Document::new_with_config(
            Box::new(PieceTableBuffer::new()),
            config,
        )]);
    }
    paths
        .iter()
        .map(|p| load_document(p.as_path(), config))
        .collect()
}

fn load_document(path: &Path, config: &core::Config) -> Result<Document, Box<dyn Error>> {
    let buffer: Box<dyn Buffer> = if path.exists() {
        Box::new(PieceTableBuffer::from_path(path.to_path_buf())?)
    } else {
        // Path was given but the file does not exist yet — open a new
        // buffer that will save to that path when the user hits Ctrl+S.
        Box::new(PieceTableBuffer::from_bytes_with_path(
            Vec::new(),
            path.to_path_buf(),
        ))
    };
    Ok(Document::new_with_config(buffer, config))
}
