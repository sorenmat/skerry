//! egui → `EditorEvent` translation.
//!
//! `EditorEvent` and `Movement` come from `core::input`. This module
//! only contains the egui-specific glue.
//!
//! Clipboard-aware shortcuts (Ctrl/Cmd+C/X/V) are NOT translated here —
//! the OS clipboard is a frontend concern, so they are handled directly
//! in [`crate::app::EditorApp::handle_input`]. This module only emits
//! plain `EditorEvent`s.

use core::{Buffer, EditorEvent, Movement, Selection};
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
        // OS paste: egui already pulled the clipboard text for us.
        Event::Paste(text) => Some(EditorEvent::Paste(text.clone())),
        Event::Key {
            key,
            pressed: true,
            modifiers,
            ..
        } => translate_key(*key, *modifiers),
        _ => None,
    }
}

/// `true` for the platform "primary editing modifier" (Cmd on macOS,
/// Ctrl elsewhere). Used by Edit-menu shortcuts so Mac users get
/// Cmd+C/X/V as God intended, while Linux/Windows users get Ctrl.
fn is_primary_modifier(modifiers: &Modifiers) -> bool {
    // `Modifiers::command` is egui's portable alias for the OS primary
    // edit-modifier — Cmd on Mac, Ctrl on other platforms.
    modifiers.command
}

fn translate_key(key: Key, modifiers: Modifiers) -> Option<EditorEvent> {
    let primary = is_primary_modifier(&modifiers);
    let shift = modifiers.shift;

    // Cmd/Ctrl-modified keys first. Clipboard shortcuts are NOT
    // returned here — they need OS access and are handled by
    // `EditorApp::handle_input` directly.
    if primary && !shift {
        return match key {
            Key::S => Some(EditorEvent::Save),
            Key::Z => Some(EditorEvent::Undo),
            Key::Y => Some(EditorEvent::Redo),
            Key::Q => Some(EditorEvent::Quit),
            Key::F => Some(EditorEvent::FindOpen),
            Key::R => Some(EditorEvent::ReplaceOpen),
            Key::K => Some(EditorEvent::DeleteLine),
            Key::D => Some(EditorEvent::DuplicateLine),
            Key::T => Some(EditorEvent::NewDoc),
            Key::W => Some(EditorEvent::CloseDoc),
            Key::O => Some(EditorEvent::OpenFile(None)),
            Key::Tab => Some(EditorEvent::NextDoc),
            Key::Backspace => Some(EditorEvent::DeleteWordLeft),
            Key::Delete => Some(EditorEvent::DeleteWordRight),
            Key::ArrowLeft => Some(movement(Movement::WordLeft, false)),
            Key::ArrowRight => Some(movement(Movement::WordRight, false)),
            // C / X / V fall through to clipboard handling.
            _ => None,
        };
    }

    // Ctrl+Shift-modified keys. Tab cycles the other direction.
    if primary && shift {
        return match key {
            Key::Tab => Some(EditorEvent::PrevDoc),
            _ => None,
        };
    }

    // Alt-modified keys (line move).
    if modifiers.alt && !primary && !modifiers.ctrl && !shift {
        return match key {
            Key::ArrowUp => Some(EditorEvent::MoveLineUp),
            Key::ArrowDown => Some(EditorEvent::MoveLineDown),
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
        Key::ArrowLeft if shift => Some(EditorEvent::ScrollLeft),
        Key::ArrowRight if shift => Some(EditorEvent::ScrollRight),
        Key::ArrowLeft => Some(movement(Movement::Left, select)),
        Key::ArrowRight => Some(movement(Movement::Right, select)),
        Key::ArrowUp => Some(movement(Movement::Up, select)),
        Key::ArrowDown => Some(movement(Movement::Down, select)),
        Key::Home => Some(movement(Movement::LineStart, select)),
        Key::End => Some(movement(Movement::LineEnd, select)),
        Key::PageUp => Some(movement(Movement::PageUp, select)),
        Key::PageDown => Some(movement(Movement::PageDown, select)),
        _ => None,
    }
}

/// `true` if the key event is the platform's "copy" shortcut
/// (Cmd/Ctrl+C). Used by `EditorApp::handle_input` to route to OS
/// clipboard instead of through `translate_key`.
pub fn is_copy_shortcut(key: Key, modifiers: &Modifiers) -> bool {
    key == Key::C && is_primary_modifier(modifiers) && !modifiers.shift
}

/// `true` if the key event is the platform's "cut" shortcut
/// (Cmd/Ctrl+X).
pub fn is_cut_shortcut(key: Key, modifiers: &Modifiers) -> bool {
    key == Key::X && is_primary_modifier(modifiers) && !modifiers.shift
}

/// A side-effect-producing clipboard action that needs OS access and
/// therefore can't be a plain `EditorEvent`. Carries the text to write
/// to the clipboard (for copy/cut) so the input phase doesn't need a
/// `&Context`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardAction {
    /// Cmd/Ctrl+C: copy the selection text to the OS clipboard.
    Copy(String),
    /// Cmd/Ctrl+X: copy the selection text to the OS clipboard, then
    /// delete the selection.
    Cut(String),
}

/// If the event is a clipboard shortcut that should produce a
/// [`ClipboardAction`] instead of an `EditorEvent`, return the action.
/// Returns `None` for plain text input, paste events, or non-clipboard
/// keys — those go through `translate_event` as usual.
///
/// `Paste` events are translated to `EditorEvent::Paste` directly, so
/// they never reach this function in practice (they're handled by
/// `translate_event`); included here for completeness if a caller wants
/// to inspect clipboard-related events independently.
pub fn classify_clipboard_event(event: &Event, buffer: &dyn Buffer) -> Option<ClipboardAction> {
    let Event::Key {
        key,
        pressed: true,
        modifiers,
        ..
    } = event
    else {
        return None;
    };
    if !is_primary_modifier(modifiers) || modifiers.shift {
        return None;
    }
    if *key != Key::C && *key != Key::X {
        return None;
    }
    let sel: Selection = buffer.selection();
    if sel.is_collapsed() {
        return None;
    }
    let text = buffer.slice(sel.range())?;
    if *key == Key::C {
        Some(ClipboardAction::Copy(text))
    } else {
        Some(ClipboardAction::Cut(text))
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

    /// The platform's "primary edit modifier" — Cmd on Mac, Ctrl
    /// elsewhere. Real egui events set both `ctrl` (on non-Mac) and
    /// `command`; we set both here so the tests are platform-agnostic.
    fn primary() -> Modifiers {
        Modifiers {
            ctrl: true,
            command: true,
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
    fn paste_event_translates_to_paste() {
        let ev = Event::Paste("hello".to_string());
        assert_eq!(
            translate_event(&ev),
            Some(EditorEvent::Paste("hello".to_string()))
        );
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
        // Shift+Up / Shift+Down still extend selection.
        assert_eq!(
            translate_event(&key_event(Key::ArrowUp, true, shift())),
            Some(EditorEvent::SelectExtend(Movement::Up))
        );
        assert_eq!(
            translate_event(&key_event(Key::ArrowDown, true, shift())),
            Some(EditorEvent::SelectExtend(Movement::Down))
        );
    }

    #[test]
    fn shift_left_right_scrolls_horizontally() {
        // Shift+Left / Shift+Right now scroll horizontally rather
        // than extending the selection (which was ambiguous anyway —
        // Ctrl+Left/Right already does word-wise movement).
        assert_eq!(
            translate_event(&key_event(Key::ArrowLeft, true, shift())),
            Some(EditorEvent::ScrollLeft)
        );
        assert_eq!(
            translate_event(&key_event(Key::ArrowRight, true, shift())),
            Some(EditorEvent::ScrollRight)
        );
    }

    #[test]
    fn primary_s_saves() {
        assert_eq!(
            translate_event(&key_event(Key::S, true, primary())),
            Some(EditorEvent::Save)
        );
    }

    #[test]
    fn primary_r_opens_replace_bar() {
        assert_eq!(
            translate_event(&key_event(Key::R, true, primary())),
            Some(EditorEvent::ReplaceOpen)
        );
    }

    #[test]
    fn primary_q_quits() {
        assert_eq!(
            translate_event(&key_event(Key::Q, true, primary())),
            Some(EditorEvent::Quit)
        );
    }

    #[test]
    fn primary_z_undoes() {
        assert_eq!(
            translate_event(&key_event(Key::Z, true, primary())),
            Some(EditorEvent::Undo)
        );
    }

    #[test]
    fn primary_y_redoes() {
        assert_eq!(
            translate_event(&key_event(Key::Y, true, primary())),
            Some(EditorEvent::Redo)
        );
    }

    #[test]
    fn primary_c_does_not_emit_quit() {
        // Cmd/Ctrl+C is the OS clipboard shortcut, not Quit. It is
        // handled separately by classify_clipboard_event, so
        // translate_event should return None for it.
        assert_eq!(
            translate_event(&key_event(Key::C, true, primary())),
            None
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

    // ----- classify_clipboard_event -----

    fn buffer_with(text: &str) -> core::PieceTableBuffer {
        core::PieceTableBuffer::from_bytes(text.as_bytes().to_vec())
    }

    #[test]
    fn copy_shortcut_with_selection_returns_copy_action() {
        let mut buf = buffer_with("hello world");
        buf.set_selection(Selection {
            anchor: 0,
            head: 5,
        });
        let action = classify_clipboard_event(&key_event(Key::C, true, primary()), &buf);
        assert_eq!(action, Some(ClipboardAction::Copy("hello".to_string())));
    }

    #[test]
    fn cut_shortcut_with_selection_returns_cut_action() {
        let mut buf = buffer_with("hello world");
        buf.set_selection(Selection {
            anchor: 0,
            head: 5,
        });
        let action = classify_clipboard_event(&key_event(Key::X, true, primary()), &buf);
        assert_eq!(action, Some(ClipboardAction::Cut("hello".to_string())));
    }

    #[test]
    fn clipboard_shortcut_without_selection_returns_none() {
        let buf = buffer_with("hello");
        // Selection is collapsed by default.
        assert!(classify_clipboard_event(&key_event(Key::C, true, primary()), &buf).is_none());
        assert!(classify_clipboard_event(&key_event(Key::X, true, primary()), &buf).is_none());
    }

    #[test]
    fn clipboard_shortcut_with_shift_is_ignored() {
        // Shift+Cmd/Ctrl+C is a different action (in macOS: some apps
        // use it for "copy formatting"). Don't intercept.
        let mut buf = buffer_with("hello");
        buf.set_selection(Selection {
            anchor: 0,
            head: 5,
        });
        let mut mods = primary();
        mods.shift = true;
        assert!(classify_clipboard_event(&key_event(Key::C, true, mods), &buf).is_none());
    }

    #[test]
    fn plain_c_key_does_not_match_clipboard_shortcut() {
        // Without Cmd/Ctrl, 'C' is just a printable char.
        let mut buf = buffer_with("hello");
        buf.set_selection(Selection {
            anchor: 0,
            head: 5,
        });
        assert!(classify_clipboard_event(&key_event(Key::C, true, no_mods()), &buf).is_none());
    }

    #[test]
    fn released_clipboard_key_returns_none() {
        let mut buf = buffer_with("hello");
        buf.set_selection(Selection {
            anchor: 0,
            head: 5,
        });
        assert!(
            classify_clipboard_event(&key_event(Key::C, false, primary()), &buf).is_none()
        );
    }

    #[test]
    fn paste_event_is_not_classified_as_clipboard_action() {
        // Paste events go through translate_event, not classify_clipboard_event.
        let mut buf = buffer_with("hello");
        buf.set_selection(Selection {
            anchor: 0,
            head: 5,
        });
        assert!(classify_clipboard_event(&Event::Paste("x".into()), &buf).is_none());
    }

    // ----- multi-buffer key bindings -----

    #[test]
    fn primary_t_opens_new_doc() {
        assert_eq!(
            translate_event(&key_event(Key::T, true, primary())),
            Some(EditorEvent::NewDoc)
        );
    }

    #[test]
    fn primary_w_closes_active_doc() {
        assert_eq!(
            translate_event(&key_event(Key::W, true, primary())),
            Some(EditorEvent::CloseDoc)
        );
    }

    #[test]
    fn primary_tab_cycles_next_doc() {
        assert_eq!(
            translate_event(&key_event(Key::Tab, true, primary())),
            Some(EditorEvent::NextDoc)
        );
    }

    #[test]
    fn primary_shift_tab_cycles_prev_doc() {
        let mods = Modifiers {
            ctrl: true,
            command: true,
            shift: true,
            ..Default::default()
        };
        assert_eq!(
            translate_event(&key_event(Key::Tab, true, mods)),
            Some(EditorEvent::PrevDoc)
        );
    }

    #[test]
    fn plain_tab_inserts_indent_not_doc_event() {
        // Without primary modifier, Tab is still the indent key.
        assert_eq!(
            translate_event(&key_event(Key::Tab, true, no_mods())),
            Some(EditorEvent::Insert('\t'))
        );
    }
}