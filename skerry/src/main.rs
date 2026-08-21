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
    let args: Vec<String> = env::args().skip(1).collect();
    let documents = if args.is_empty() && !config.recent_files.is_empty() {
        config
            .recent_files
            .iter()
            .map(|p| load_document(p, None, &config))
            .collect()
    } else {
        args.iter()
            .map(|arg| {
                let (path, line, col) = parse_position_arg(arg);
                load_document(&path, line.map(|l| (l, col)), &config)
            })
            .collect()
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
        Box::new(|cc| {
            skerry::fonts::install(&cc.egui_ctx);
            Ok(Box::new(EditorApp::new_with_documents(documents, config)))
        }),
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
        .map(|p| load_document(p, None, config))
        .collect()
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

/// Convert 1-based `line`/`col` CLI arguments into a 0-based byte
/// position, ready to be set as the buffer's initial cursor. The
/// clamped helper tolerates out-of-range values (e.g. `file:99999`).
fn load_document(
    path: &Path,
    position: Option<(usize, Option<usize>)>,
    config: &core::Config,
) -> Document {
    let mut buffer: Box<dyn Buffer> = if path.exists() {
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
    if let Some((line, col)) = position {
        let pos = core::clamped_line_charcol_to_pos(
            &*buffer,
            line.saturating_sub(1),
            col.unwrap_or(1).saturating_sub(1),
        );
        buffer.set_cursor(pos);
    }
    Document::new_with_config(buffer, config)
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
        let dir = std::env::temp_dir().join(format!("sky_args_{}", std::process::id()));
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
        let path = std::env::temp_dir().join(format!("sky_pos_{}.txt", std::process::id()));
        std::fs::write(&path, b"one\ntwo\nthree").unwrap();
        let config = core::Config::default();
        let doc = load_document(&path, Some((3, None)), &config);
        let text = doc.buffer.line_text(2).unwrap().into_owned();
        assert_eq!(text, "three");
        assert_eq!(
            doc.buffer.cursor(),
            doc.buffer.line_byte_range(2).unwrap().start
        );
        let _ = std::fs::remove_file(&path);
    }
}
