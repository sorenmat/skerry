//! TestBackend-based screenshot generator for the TUI.
//!
//! Not an assertion test — run explicitly to dump the rendered cell grid
//! (text + colors, TSV) so the docs pipeline can rasterize a faithful
//! image of the real UI without a terminal:
//!
//! ```text
//! cargo test -p skerry-tui --bin skerry-tui dump_tui_cells -- --ignored
//! ```
//!
//! Output: one line per cell (row-major), `fg\tbg\tflags\tsymbol` where
//! colors are `#rrggbb` hex and flags mark bold (`b`), underlined (`u`),
//! italic (`i`). `default` means ratatui `Color::Reset` — rasterize with
//! the terminal defaults (Ocean Dark: bg #2b303b, fg #c0c5ce).

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use core::{Buffer, Document, PieceTableBuffer};
    use ratatui::backend::{Backend, TestBackend};
    use ratatui::style::{Color, Modifier};
    use ratatui::Terminal;

    use crate::app::App;
    use crate::ui;

    const WIDTH: u16 = 120;
    const HEIGHT: u16 = 34;

    fn color_hex(c: Color) -> String {
        match c {
            Color::Reset => "default".into(),
            Color::Black => "#000000".into(),
            Color::Red => "#cd0000".into(),
            Color::Green => "#00cd00".into(),
            Color::Yellow => "#cdcd00".into(),
            Color::Blue => "#0000ee".into(),
            Color::Magenta => "#cd00cd".into(),
            Color::Cyan => "#00cdcd".into(),
            Color::Gray => "#e5e5e5".into(),
            Color::DarkGray => "#7f7f7f".into(),
            Color::LightRed => "#ff0000".into(),
            Color::LightGreen => "#00ff00".into(),
            Color::LightYellow => "#ffff00".into(),
            Color::LightBlue => "#5c5cff".into(),
            Color::LightMagenta => "#ff00ff".into(),
            Color::LightCyan => "#00ffff".into(),
            Color::White => "#ffffff".into(),
            Color::Indexed(i) => format!("indexed:{i}"),
            Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        }
    }

    #[test]
    #[ignore = "docs screenshot generator; dumps TSV of rendered cells"]
    fn dump_tui_cells() {
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let root = manifest.parent().expect("workspace root");
        let config = core::Config::default();
        let make = |rel: &str| -> Document {
            let buffer: Box<dyn Buffer> =
                Box::new(PieceTableBuffer::from_path(root.join(rel)).unwrap());
            Document::new_with_config(buffer, &config)
        };
        let docs = vec![
            make("core/src/keymap.rs"),
            make("features.md"),
        ];
        let mut app = App::new_with_documents(docs, config);

        let backend = TestBackend::new(WIDTH, HEIGHT);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| ui::render(frame, &mut app)).unwrap();

        let buffer = terminal.backend().buffer();
        let mut out = format!("size\t{WIDTH}\t{HEIGHT}\n");
        for cell in &buffer.content {
            let fg = color_hex(cell.fg);
            let bg = color_hex(cell.bg);
            let mut flags = String::new();
            if cell.modifier.contains(Modifier::BOLD) {
                flags.push('b');
            }
            if cell.modifier.contains(Modifier::UNDERLINED) {
                flags.push('u');
            }
            if cell.modifier.contains(Modifier::ITALIC) {
                flags.push('i');
            }
            out.push_str(&format!("{fg}\t{bg}\t{flags}\t{}\n", cell.symbol()));
        }
        let dest = std::env::temp_dir().join("tui-cells.tsv");
        std::fs::write(&dest, out).unwrap();
        eprintln!("wrote {} cells to {}", WIDTH * HEIGHT, dest.display());
    }
}
