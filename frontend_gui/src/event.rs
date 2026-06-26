//! egui → `EditorEvent` translation.
//!
//! `EditorEvent` and `Movement` come from `core::input`. This module
//! only contains the egui-specific glue.

use core::{EditorEvent, Movement};
use eframe::egui::{Event, Key, Modifiers};

/// Translate an egui `Event` into an `EditorEvent`.
///
/// Returns `None` for events we don't bind to anything (mouse moves,
/// scroll, etc.). The caller decides whether `None` is "ignore" or
/// "unhandled".
pub fn translate_event(event: &Event) -> Option<EditorEvent> {
    match event {
        // Printable characters come through as Text events (handles IME
        // and dead keys correctly; a `Key::A` event alone doesn't tell
        // us the resulting character once shift/IME is applied).
        Event::Text(text) => text.chars().next().map(EditorEvent::Insert),
        Event::Key {
            key,
            pressed: true,
            modifiers,
            ..
        } => translate_key(*key, *modifiers),
        _ => None,
    }
}

fn translate_key(key: Key, modifiers: Modifiers) -> Option<EditorEvent> {
    let ctrl = modifiers.ctrl;
    let shift = modifiers.shift;

    // Ctrl-modified keys first.
    if ctrl && !shift {
        return match key {
            Key::S => Some(EditorEvent::Save),
            Key::Z => Some(EditorEvent::Undo),
            Key::Y => Some(EditorEvent::Redo),
            Key::Q | Key::C => Some(EditorEvent::Quit),
            _ => None,
        };
    }

    let select = shift;

    match key {
        Key::Backspace => Some(EditorEvent::DeleteLeft),
        Key::Delete => Some(EditorEvent::DeleteRight),
        Key::Enter => Some(EditorEvent::Insert('\n')),
        Key::Tab => Some(EditorEvent::Insert('\t')),
        Key::Escape => Some(EditorEvent::Quit),
        Key::ArrowLeft => Some(movement(Movement::Left, select)),
        Key::ArrowRight => Some(movement(Movement::Right, select)),
        Key::ArrowUp => Some(movement(Movement::Up, select)),
        Key::ArrowDown => Some(movement(Movement::Down, select)),
        Key::Home => Some(movement(Movement::LineStart, select)),
        Key::End => Some(movement(Movement::LineEnd, select)),
        Key::PageUp => Some(movement(Movement::DocumentStart, select)),
        Key::PageDown => Some(movement(Movement::DocumentEnd, select)),
        _ => None,
    }
}

fn movement(m: Movement, select: bool) -> EditorEvent {
    if select {
        EditorEvent::SelectExtend(m)
    } else {
        EditorEvent::Move(m)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::{Event, Key, Modifiers};

    fn key_event(key: Key, pressed: bool, modifiers: Modifiers) -> Event {
        Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers,
        }
    }

    fn no_mods() -> Modifiers {
        Modifiers::default()
    }

    fn ctrl() -> Modifiers {
        Modifiers {
            ctrl: true,
            ..Default::default()
        }
    }

    fn shift() -> Modifiers {
        Modifiers {
            shift: true,
            ..Default::default()
        }
    }

    #[test]
    fn text_event_inserts_char() {
        let ev = Event::Text("x".to_string());
        assert_eq!(translate_event(&ev), Some(EditorEvent::Insert('x')));
    }

    #[test]
    fn text_event_empty_returns_none() {
        let ev = Event::Text(String::new());
        assert_eq!(translate_event(&ev), None);
    }

    #[test]
    fn released_key_returns_none() {
        let ev = key_event(Key::ArrowLeft, false, no_mods());
        assert_eq!(translate_event(&ev), None);
    }

    #[test]
    fn arrows_move() {
        assert_eq!(
            translate_event(&key_event(Key::ArrowLeft, true, no_mods())),
            Some(EditorEvent::Move(Movement::Left))
        );
        assert_eq!(
            translate_event(&key_event(Key::ArrowRight, true, no_mods())),
            Some(EditorEvent::Move(Movement::Right))
        );
    }

    #[test]
    fn shift_arrow_extends_selection() {
        assert_eq!(
            translate_event(&key_event(Key::ArrowRight, true, shift())),
            Some(EditorEvent::SelectExtend(Movement::Right))
        );
    }

    #[test]
    fn ctrl_s_saves() {
        assert_eq!(
            translate_event(&key_event(Key::S, true, ctrl())),
            Some(EditorEvent::Save)
        );
    }

    #[test]
    fn ctrl_q_quits() {
        assert_eq!(
            translate_event(&key_event(Key::Q, true, ctrl())),
            Some(EditorEvent::Quit)
        );
    }

    #[test]
    fn ctrl_z_undoes() {
        assert_eq!(
            translate_event(&key_event(Key::Z, true, ctrl())),
            Some(EditorEvent::Undo)
        );
    }

    #[test]
    fn ctrl_y_redoes() {
        assert_eq!(
            translate_event(&key_event(Key::Y, true, ctrl())),
            Some(EditorEvent::Redo)
        );
    }

    #[test]
    fn escape_quits() {
        assert_eq!(
            translate_event(&key_event(Key::Escape, true, no_mods())),
            Some(EditorEvent::Quit)
        );
    }

    #[test]
    fn backspace_deletes_left() {
        assert_eq!(
            translate_event(&key_event(Key::Backspace, true, no_mods())),
            Some(EditorEvent::DeleteLeft)
        );
    }

    #[test]
    fn enter_inserts_newline() {
        assert_eq!(
            translate_event(&key_event(Key::Enter, true, no_mods())),
            Some(EditorEvent::Insert('\n'))
        );
    }

    #[test]
    fn unmapped_key_returns_none() {
        assert_eq!(
            translate_event(&key_event(Key::F1, true, no_mods())),
            None
        );
    }
}