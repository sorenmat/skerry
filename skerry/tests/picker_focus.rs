//! Regression tests for keyboard focus after overlay surfaces close.
//!
//! "Cmd+P opens a file, but the editor isn't the active surface — typing
//! goes nowhere until I click in the editor." The editor text surface now
//! carries a stable, focusable id, and every overlay that closes or
//! executes (fuzzy finder, command palette, symbol picker, …) hands
//! keyboard focus back to it. These tests drive the real pipeline —
//! `handle_input` + `render` inside `ctx.run` — and pin both ends: focus
//! sits on the overlay's query field while it's open, and lands on the
//! editor surface right after it closes.

use core::{Buffer, PieceTableBuffer};
use eframe::egui;

fn editor_surface_id(app: &skerry::app::EditorApp) -> egui::Id {
    egui::Id::new(("editor_surface", app.active))
}

fn app_with(content: &str) -> skerry::app::EditorApp {
    let buf: Box<dyn Buffer> = Box::new(PieceTableBuffer::from_bytes(content.as_bytes().to_vec()));
    skerry::app::EditorApp::new_with_documents(
        vec![core::Document::new(buf)],
        core::Config::default(),
    )
}

fn frame(
    ctx: &egui::Context,
    app: &mut skerry::app::EditorApp,
    events: Vec<egui::Event>,
    time: f64,
) {
    let raw = egui::RawInput {
        time: Some(time),
        predicted_dt: 1.0 / 60.0,
        events,
        ..Default::default()
    };
    let _ = ctx.run(raw, |ctx| {
        app.handle_input(ctx);
        skerry::ui::render(ctx, app);
    });
}

fn key(key: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
    egui::Event::Key {
        key,
        physical_key: Some(key),
        pressed: true,
        repeat: false,
        modifiers,
    }
}

fn settle(ctx: &egui::Context, app: &mut skerry::app::EditorApp, time: f64) {
    frame(ctx, app, vec![], time);
}

#[test]
fn focus_sits_on_finder_while_open_then_returns_to_editor() {
    let ctx = egui::Context::default();
    let mut app = app_with("hello\n");

    settle(&ctx, &mut app, 0.0);

    // Cmd+P opens the finder; the next frame its query field grabs focus.
    frame(
        &ctx,
        &mut app,
        vec![key(egui::Key::P, egui::Modifiers::COMMAND)],
        0.1,
    );
    assert!(app.fuzzy_finder.open);
    settle(&ctx, &mut app, 0.2);
    let focus_while_open = ctx.memory(|m| m.focused());
    assert!(focus_while_open.is_some());
    assert_ne!(
        focus_while_open,
        Some(editor_surface_id(&app)),
        "while the finder is open, focus belongs to its query field"
    );

    // Esc closes it; the frame after, focus must be on the editor surface.
    frame(
        &ctx,
        &mut app,
        vec![key(egui::Key::Escape, egui::Modifiers::NONE)],
        0.3,
    );
    assert!(!app.fuzzy_finder.open);
    settle(&ctx, &mut app, 0.4);
    assert_eq!(
        ctx.memory(|m| m.focused()),
        Some(editor_surface_id(&app)),
        "after the finder closes, the editor surface takes keyboard focus"
    );

    // And typed text lands in the buffer (cursor sits after "hello").
    app.active_buffer_mut().set_cursor(5);
    frame(&ctx, &mut app, vec![egui::Event::Text("!".into())], 0.5);
    assert_eq!(app.active_buffer().to_bytes(), b"hello!\n".to_vec());
}

#[test]
fn executing_the_finder_moves_focus_to_the_newly_opened_editor() {
    // A tiny project on disk so the finder has a real file to open.
    let dir = std::env::temp_dir().join(format!("sky_focus_{}", std::process::id()));
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("Cargo.toml"), "[package]").unwrap();
    std::fs::write(dir.join("src").join("main.rs"), b"fn main() {}\n").unwrap();

    let ctx = egui::Context::default();
    let mut app = app_with("start\n");
    app.documents[0].project = core::Project::from_path(&dir);

    settle(&ctx, &mut app, 0.0);

    // Cmd+P, type a query that matches, Enter.
    frame(
        &ctx,
        &mut app,
        vec![key(egui::Key::P, egui::Modifiers::COMMAND)],
        0.1,
    );
    frame(&ctx, &mut app, vec![egui::Event::Text("main".into())], 0.2);
    frame(
        &ctx,
        &mut app,
        vec![key(egui::Key::Enter, egui::Modifiers::NONE)],
        0.3,
    );
    assert!(!app.fuzzy_finder.open);
    assert_eq!(app.documents.len(), 2);

    settle(&ctx, &mut app, 0.4);
    assert_eq!(
        ctx.memory(|m| m.focused()),
        Some(editor_surface_id(&app)),
        "after Cmd+P opens a file, the editor surface owns keyboard focus"
    );

    // Typing immediately edits the newly opened document.
    frame(&ctx, &mut app, vec![egui::Event::Text("x".into())], 0.5);
    assert_eq!(app.active_buffer().to_bytes(), b"xfn main() {}\n".to_vec());

    let _ = std::fs::remove_dir_all(&dir);
}
