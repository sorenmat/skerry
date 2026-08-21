//! Offscreen regression test for selection rendering over syntax-
//! highlighted lines.
//!
//! A line that has BOTH a selection and tree-sitter segments must draw
//! its colored chunks left-to-right, each offset by the width of the
//! text before it. The original bug drew every chunk at the line's
//! left edge, stacking them on top of each other and garbling the
//! page the moment anything was selected.

use core::{Buffer, PieceTableBuffer, Selection};
use eframe::egui;

const LINE: &str = "let alpha = beta;";

fn render_selected_line() -> Vec<egui::epaint::ClippedShape> {
    let path = std::env::temp_dir().join(format!("skerry_sel_render_{}.rs", std::process::id()));
    std::fs::write(&path, format!("{LINE}\n")).expect("write temp .rs file");
    let buf: Box<dyn Buffer> = Box::new(PieceTableBuffer::from_path(path.clone()).expect("load"));
    let mut app = skerry::app::EditorApp::new(buf);
    // Select the whole line so render_text takes the selection branch.
    app.active_buffer_mut().set_selection(Selection {
        anchor: 0,
        head: LINE.len(),
    });

    let ctx = egui::Context::default();
    let shapes = ctx
        .run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 300.0),
                )),
                ..Default::default()
            },
            |ctx| skerry::ui::render(ctx, &mut app),
        )
        .shapes;
    let _ = std::fs::remove_file(&path);
    shapes
}

#[test]
fn selected_syntax_line_draws_segments_left_to_right() {
    let shapes = render_selected_line();

    // Text shapes whose text is a proper fragment of the line, sorted
    // by row then x. The editor line is the row with the most fragments.
    let mut frags: Vec<(f32, f32, String)> = shapes
        .iter()
        .filter_map(|s| match &s.shape {
            egui::epaint::Shape::Text(t) => {
                let text = t.galley.job.text.clone();
                if !text.is_empty() && text != LINE && LINE.contains(&text) {
                    Some((t.pos.x, t.pos.y, text))
                } else {
                    None
                }
            }
            _ => None,
        })
        .collect();
    frags.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.total_cmp(&b.0)));

    let mut rows: Vec<Vec<(f32, f32, String)>> = Vec::new();
    for f in frags {
        match rows.last_mut() {
            Some(row) if (row[0].1 - f.1).abs() < 0.5 => row.push(f),
            _ => rows.push(vec![f]),
        }
    }
    let line_row = rows
        .into_iter()
        .max_by_key(|r| r.len())
        .expect("at least one fragment of the line should be drawn");

    assert!(
        line_row.len() >= 2,
        "expected syntax segments on the selected line, got: {line_row:?}"
    );
    for w in line_row.windows(2) {
        assert!(
            w[0].0 < w[1].0,
            "fragments stacked at the same x (garbled selection): {line_row:?}"
        );
    }
}
