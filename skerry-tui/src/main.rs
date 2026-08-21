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
    let args: Vec<String> = env::args().skip(1).collect();
    let documents = if args.is_empty() && !config.recent_files.is_empty() {
        load_documents(
            &config
                .recent_files
                .iter()
                .map(|p| (p.clone(), None))
                .collect::<Vec<_>>(),
            &config,
        )?
    } else {
        load_documents(
            &args
                .iter()
                .map(|arg| {
                    let (path, line, col) = parse_position_arg(arg);
                    (path, line.map(|l| (l, col)))
                })
                .collect::<Vec<_>>(),
            &config,
        )?
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

/// Parse a CLI argument as `path`, `path:line`, or `path:line:col`
/// (1-based, `vim`/`code` style). A literal path always wins: if the
/// whole argument exists on disk it is treated as a plain path, so
/// files with colons in their names still open correctly. The numeric
/// suffixes must be all digits; anything else falls back to a literal
/// path too.
fn parse_position_arg(arg: &str) -> (PathBuf, Option<usize>, Option<usize>) {
    if Path::new(arg).exists() {
        return (PathBuf::from(arg), None, None);
    }
    if let Some((rest, col)) = arg.rsplit_once(':') {
        let digits = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
        if digits(col) {
            if let Some((path, line)) = rest.rsplit_once(':') {
                if digits(line) && !path.is_empty() {
                    return (
                        PathBuf::from(path),
                        Some(line.parse().unwrap_or(1)),
                        Some(col.parse().unwrap_or(1)),
                    );
                }
            } else if !rest.is_empty() {
                return (PathBuf::from(rest), Some(col.parse().unwrap_or(1)), None);
            }
        }
    }
    (PathBuf::from(arg), None, None)
}

/// Load each path into its own [`Document`]. Existing files are
/// memory-mapped via [`PieceTableBuffer::from_path`] (ADR 0002 — the
/// multi-GB-file path); paths that don't exist yet get a fresh empty
/// buffer that will save back to that path. With no paths, returns a
/// single empty document so the editor still has somewhere to land.
/// `path.1` optionally carries a 1-based `line`/`col` pair
/// (`file:line[:col]` CLI form) that becomes the initial cursor.
fn load_documents(
    paths: &[(PathBuf, Option<(usize, Option<usize>)>)],
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
        .map(|(path, position)| load_document(path, *position, config))
        .collect()
}

fn load_document(
    path: &Path,
    position: Option<(usize, Option<usize>)>,
    config: &core::Config,
) -> Result<Document, Box<dyn Error>> {
    let mut buffer: Box<dyn Buffer> = if path.exists() {
        Box::new(PieceTableBuffer::from_path(path.to_path_buf())?)
    } else {
        // Path was given but the file does not exist yet — open a new
        // buffer that will save to that path when the user hits Ctrl+S.
        Box::new(PieceTableBuffer::from_bytes_with_path(
            Vec::new(),
            path.to_path_buf(),
        ))
    };
    if let Some((line, col)) = position {
        let pos = core::clamped_line_charcol_to_pos(
            &*buffer,
            line.saturating_sub(1),
            col.unwrap_or(1).saturating_sub(1),
        );
        buffer.set_cursor(pos);
    }
    Ok(Document::new_with_config(buffer, config))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_path_has_no_position() {
        let (path, line, col) = parse_position_arg("src/main.rs");
        assert_eq!(path, PathBuf::from("src/main.rs"));
        assert_eq!((line, col), (None, None));
    }

    #[test]
    fn line_and_line_col_suffixes_parse() {
        let (path, line, col) = parse_position_arg("src/main.rs:42");
        assert_eq!(path, PathBuf::from("src/main.rs"));
        assert_eq!((line, col), (Some(42), None));

        let (path, line, col) = parse_position_arg("src/main.rs:42:7");
        assert_eq!(path, PathBuf::from("src/main.rs"));
        assert_eq!((line, col), (Some(42), Some(7)));
    }

    #[test]
    fn existing_file_with_colon_wins_as_literal() {
        let dir = std::env::temp_dir().join(format!("tui_args_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let weird = dir.join("weird:name.txt");
        std::fs::write(&weird, b"x").unwrap();
        let arg = weird.to_str().unwrap();
        let (path, line, col) = parse_position_arg(arg);
        assert_eq!(path, weird);
        assert_eq!((line, col), (None, None));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn non_numeric_suffix_stays_literal() {
        // URLs contain colons but no numeric suffix.
        let (path, line, col) = parse_position_arg("https://example.com/x");
        assert_eq!(path, PathBuf::from("https://example.com/x"));
        assert_eq!((line, col), (None, None));
        // Trailing colon with empty line.
        let (path, line, col) = parse_position_arg("foo:");
        assert_eq!(path, PathBuf::from("foo:"));
        assert_eq!((line, col), (None, None));
    }

    #[test]
    fn load_document_positions_cursor() {
        let path = std::env::temp_dir().join(format!("tui_pos_{}.txt", std::process::id()));
        std::fs::write(&path, b"one\ntwo\nthree").unwrap();
        let config = core::Config::default();
        let doc = load_document(&path, Some((3, None)), &config).unwrap();
        assert_eq!(
            doc.buffer.cursor(),
            doc.buffer.line_byte_range(2).unwrap().start
        );
        let _ = std::fs::remove_file(&path);
    }
}
