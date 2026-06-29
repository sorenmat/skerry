//! Regression test: pressing the cursor DOWN past the visible viewport
//! must scroll the view so the cursor stays on-screen. Other editors do
//! this transparently — this test guards the cursor-following
//! `scroll_to_rect` call in `render_text` from regressing to the
//! broken visibility check that only fired when the cursor went past
//! the *entire document*, not just past the viewport.
//!
//! The previous logic compared the cursor's content-y against the
//! content rect (rect.top()/rect.bottom()) instead of the visible
//! viewport, so neither the above- nor below-check was ever true
//! inside the document — the cursor simply walked off the bottom of
//! the screen with no view response. With `scroll_to_rect(rect, None)`
//! egui does the visibility check itself and scrolls only when
//! needed; this test exercises that path end-to-end through
//! `ctx.run`.
//!
//! egui scroll is animated by default (the offset interpolates over a
//! few hundred ms). To get a deterministic check, this test disables
//! the animation via the `Style` and runs enough frames for the
//! scroll to settle.
//!
//! Run with:
//!   cargo test -p frontend_gui --test auto_scroll -- --nocapture

use core::{Buffer, PieceTableBuffer};
use eframe::egui;

/// Build an editor with `lines` lines of content. Lines are made
/// distinct so the paint output identifies which line the cursor was
/// drawn at.
fn app_with_lines(lines: usize) -> frontend_gui::app::EditorApp {
    let body: String = (1..=lines)
        .map(|i| format!("line_{i:03}\n"))
        .collect();
    let body = body.trim_end_matches('\n').to_string();
    let buf: Box<dyn Buffer> = Box::new(PieceTableBuffer::from_bytes(body.into_bytes()));
    frontend_gui::app::EditorApp::new(buf)
}

/// Render one frame at the configured screen size and return the
/// resulting paint output. Each call uses `time` from `RawInput` so
/// that successive frames advance the global animation clock —
/// without it, egui's scroll animation would never progress in a
/// tight test loop.
fn render_frame(
    ctx: &egui::Context,
    app: &mut frontend_gui::app::EditorApp,
    screen_w: f32,
    screen_h: f32,
    time: f64,
) -> Vec<egui::epaint::ClippedShape> {
    let raw_input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::Vec2::new(screen_w, screen_h),
        )),
        time: Some(time),
        predicted_dt: 1.0 / 60.0,
        ..Default::default()
    };
    let output = ctx.run(raw_input, |ctx| {
        frontend_gui::ui::render(ctx, app);
    });
    output.shapes
}

/// Find the cursor caret shape in the paint output. The render code
/// paints the caret as a thin (CARET_WIDTH = 2 px) filled rect using
/// the editor's foreground `visuals.text_color()`. We narrow on that
/// width to avoid noise from selection rectangles and gutter
/// backgrounds, which also paint as filled rects but at different
/// widths.
fn find_caret_rect(shapes: &[egui::epaint::ClippedShape]) -> Option<egui::Rect> {
    for shape in shapes.iter() {
        if let egui::epaint::Shape::Rect(r) = &shape.shape {
            if (r.rect.width() - 2.0).abs() < 0.5 {
                return Some(r.rect);
            }
        }
    }
    None
}

/// Run enough render passes for egui's scroll animation (if any) to
/// settle. With `ScrollAnimation::none()` applied above the offset
/// jumps to target instantly, but we still loop to keep the test
/// resilient if that style override regresses.
fn settle_scroll(
    ctx: &egui::Context,
    app: &mut frontend_gui::app::EditorApp,
    screen_w: f32,
    screen_h: f32,
    start_time: f64,
) -> Vec<egui::epaint::ClippedShape> {
    let mut shapes = Vec::new();
    for frame in 0..10 {
        let t = start_time + frame as f64 * 0.05; // 50ms per simulated frame
        shapes = render_frame(ctx, app, screen_w, screen_h, t);
    }
    shapes
}

#[test]
fn cursor_down_past_viewport_scrolls_view_so_cursor_stays_visible() {
    // Disable scroll animation for deterministic tests — the production
    // code uses the default smooth animation but for an offscreen test
    // we want the offset to land on the target value in one step.
    let ctx = egui::Context::default();
    ctx.style_mut(|s| {
        s.scroll_animation = egui::style::ScrollAnimation::none();
    });

    // 200 lines, viewport tall enough to show ~20 lines at FONT_SIZE=14.
    // The exact font in offscreen render may differ from a real display,
    // but the relative math (cursor at line 150 must NOT sit at content-y
    // ≈ 150 * line_height with no offset applied) is robust.
    let mut app = app_with_lines(200);
    let screen_w = 800.0;
    let screen_h = 400.0;

    // Frame 1: cursor at line 0 (default). Establish baseline: caret
    // should be visible somewhere near the top of the editor area.
    let shapes_top = settle_scroll(&ctx, &mut app, screen_w, screen_h, 0.0);
    let caret_top = find_caret_rect(&shapes_top)
        .expect("caret should be painted when cursor at top of doc");
    let caret_top_y = caret_top.min.y;
    assert!(
        caret_top_y < screen_h,
        "caret at top should sit inside the screen height, got y={caret_top_y} (screen_h={screen_h})"
    );

    // Move cursor to line 150 — far past the visible viewport.
    let far_pos = app.active_buffer().linecol_to_pos(150, 0).unwrap();
    app.handle_event(core::EditorEvent::SetCursor { pos: far_pos });

    // Frame 2 (and a few more for animation): with auto-scroll fixed,
    // the view should now be scrolled so the caret line is inside the
    // viewport. With the previous broken check, no scroll happens and
    // the caret remains at content-y ≈ 150 * line_height — off-screen.
    let shapes_far = settle_scroll(&ctx, &mut app, screen_w, screen_h, 10.0);
    let caret_far = find_caret_rect(&shapes_far)
        .expect("caret should still be painted (the rect is just clipped later)");
    let caret_far_y = caret_far.min.y;

    assert!(
        caret_far_y < screen_h,
        "BUG: caret at line 150 is at y={caret_far_y} which is at or past screen height {screen_h} — view did not scroll to follow the cursor"
    );

    // The caret should also be visibly below where it was at the top
    // of the document — confirming the view actually moved (not just
    // a no-op aliasing of the same coordinates).
    assert!(
        caret_far_y > caret_top_y,
        "after scrolling down, caret y should be larger than at top: top={caret_top_y} far={caret_far_y}"
    );
}
