use core::{Buffer, EditorEvent, PieceTableBuffer};
use eframe::egui;

fn text_shape_rects(shapes: &[egui::epaint::ClippedShape], text: &str) -> Vec<egui::Rect> {
    shapes
        .iter()
        .filter_map(|shape| match &shape.shape {
            egui::epaint::Shape::Text(text_shape) if text_shape.galley.job.text == text => Some(
                egui::Rect::from_min_size(text_shape.pos, text_shape.galley.size()),
            ),
            _ => None,
        })
        .collect()
}

fn render_search(line: &str, query: &str) -> Vec<egui::epaint::ClippedShape> {
    render_search_with_navigation(line, query, false).0
}

fn render_search_with_navigation(
    line: &str,
    query: &str,
    next: bool,
) -> (Vec<egui::epaint::ClippedShape>, egui::Color32) {
    let buffer: Box<dyn Buffer> = Box::new(PieceTableBuffer::from_bytes(line.as_bytes().to_vec()));
    let mut app = skerry::app::EditorApp::new(buffer);
    app.handle_event(EditorEvent::FindQueryChanged(query.to_string()));
    if next {
        app.handle_event(EditorEvent::FindNext);
    }
    let current_match_text = app.theme.match_current_text;

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
    (shapes, current_match_text)
}

#[test]
fn find_highlight_keeps_surrounding_text_in_layout_order() {
    let shapes = render_search("猫\tprefix needle suffix", "needle");
    let prefix = text_shape_rects(&shapes, "猫\tprefix ")[0];
    let matched = text_shape_rects(&shapes, "needle")[0];
    let suffix = text_shape_rects(&shapes, " suffix")[0];

    assert!(
        prefix.right() <= matched.left() && matched.right() <= suffix.left(),
        "search segments overlap: prefix={prefix:?}, match={matched:?}, suffix={suffix:?}"
    );
}

#[test]
fn adjacent_find_matches_do_not_overlap() {
    let shapes = render_search("starthithitend", "hit");
    let prefix = text_shape_rects(&shapes, "start")[0];
    let mut matches = text_shape_rects(&shapes, "hit");
    matches.sort_by(|a, b| a.left().total_cmp(&b.left()));
    let suffix = text_shape_rects(&shapes, "end")[0];

    assert_eq!(matches.len(), 2);
    assert!(prefix.right() <= matches[0].left());
    assert!(matches[0].right() <= matches[1].left());
    assert!(matches[1].right() <= suffix.left());
}

#[test]
fn overlapping_case_insensitive_matches_do_not_duplicate_text() {
    let shapes = render_search("aaa", "aa");
    let first_match = text_shape_rects(&shapes, "aa");
    let overlap_remainder = text_shape_rects(&shapes, "a");

    assert_eq!(first_match.len(), 1);
    assert_eq!(overlap_remainder.len(), 1);
    assert!(first_match[0].right() <= overlap_remainder[0].left());
}

#[test]
fn complete_overlapping_match_stays_current_after_navigation() {
    let (shapes, current_match_text) = render_search_with_navigation("aaa", "aa", true);
    let current_match_shapes: Vec<_> = shapes
        .iter()
        .filter_map(|shape| match &shape.shape {
            egui::epaint::Shape::Text(text_shape)
                if text_shape.galley.job.text == "aa"
                    && text_shape.fallback_color == current_match_text =>
            {
                Some(text_shape)
            }
            _ => None,
        })
        .collect();

    assert_eq!(current_match_shapes.len(), 1);
}
