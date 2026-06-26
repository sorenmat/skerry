//! TUI frontend entry point.
//!
//! Usage: `the_editor_tui [PATH]` — opens PATH if given, otherwise opens
//! an unsaved buffer. Saves go back to PATH if set (Ctrl+S); unsaved
//! buffers with no path can't be saved (yet).
//!
//! Terminal state is restored via a Drop guard so that panics during the
//! event loop don't leave the user's terminal in raw mode.

use std::env;
use std::error::Error;
use std::path::PathBuf;
use std::process;

use core::{Buffer, PieceTableBuffer};

mod app;
mod event;
mod ui;

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
        ratatui::restore();
    }
}

fn main() {
    if let Err(e) = run() {
        eprintln!("the_editor_tui: {e}");
        process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let path = parse_path_arg();

    let buffer = load_buffer(path.as_ref())?;

    let _guard = TerminalGuard;
    let mut terminal = ratatui::init();

    // Enable mouse capture so we get MouseEvent on click/drag. Without
    // this, crossterm never delivers mouse events.
    if let Err(e) = crossterm::execute!(
        std::io::stdout(),
        crossterm::event::EnableMouseCapture
    ) {
        eprintln!("warning: failed to enable mouse capture: {e}");
    }

    let mut app = App::new(buffer);
    let result = app.run(&mut terminal);

    // Drop the terminal explicitly before the guard runs so the guard's
    // restore() runs against a clean Terminal state.
    drop(terminal);

    result
}

fn parse_path_arg() -> Option<PathBuf> {
    let args: Vec<String> = env::args().collect();
    if args.len() > 1 {
        Some(PathBuf::from(&args[1]))
    } else {
        None
    }
}

fn load_buffer(path: Option<&PathBuf>) -> Result<Box<dyn Buffer>, Box<dyn Error>> {
    match path {
        Some(p) if p.exists() => {
            // from_path memory-maps the file — multi-GB logs never load
            // into RAM as a whole. ADR 0002's payoff.
            let buf = PieceTableBuffer::from_path(p.clone())?;
            Ok(Box::new(buf))
        }
        Some(p) => {
            // Path was given but the file does not exist yet — open a new
            // buffer that will save to that path when the user hits Ctrl+S.
            Ok(Box::new(PieceTableBuffer::from_bytes_with_path(
                Vec::new(),
                p.clone(),
            )))
        }
        None => Ok(Box::new(PieceTableBuffer::new())),
    }
}