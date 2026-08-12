//! Crossterm → `EditorEvent` translation.
//!
//! The `EditorEvent` and `Movement` types live in `core::input` (see
//! ADR 0005). This module only contains the crossterm-specific glue.
//!
//! Clipboard-aware shortcuts (Ctrl+C/X/V) are NOT translated here — the
//! OS clipboard is a frontend concern, so they are handled directly in
//! [`crate::app::App::run`]. This module only emits plain
//! `EditorEvent`s.

use core::{Buffer, EditorEvent, Movement, Selection};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

use crate::app::App;

/// Translate a crossterm `KeyEvent` into an `EditorEvent`.
///
/// Returns `None` for keys we don't bind to anything (e.g. function keys,
/// modifier-only presses). The caller decides whether `None` is "ignore"
/// or "unhandled".
///
/// When the find bar is open, Enter / Esc / printable chars go to the
/// bar instead of the buffer. The caller passes the current App so
/// we can append to the live query.
pub fn translate_key(key: KeyEvent, app: Option<&App>) -> Option<EditorEvent> {
    // Quit and close are application-level shortcuts. Query dialogs must
    // not append their letters or otherwise consume them.
    if key.modifiers.contains(KeyModifiers::CONTROL)
        && !key.modifiers.contains(KeyModifiers::SHIFT)
        && matches!(key.code, KeyCode::Char('q' | 'Q' | 'w' | 'W'))
    {
        return translate_buffer_key(key);
    }

    if let Some(app) = app {
        if app.fuzzy_finder.open {
            if let Some(fuzzy_event) = fuzzy_finder_translate(key, app) {
                return Some(fuzzy_event);
            }
            // Unmapped keys fall through to the normal buffer bindings.
        } else if app.command_palette.open {
            if let Some(palette_event) = command_palette_translate(key, app) {
                return Some(palette_event);
            }
            // Unmapped keys fall through to the normal buffer bindings.
        } else if app.project_search.open {
            if let Some(search_event) = project_search_translate(key, app) {
                return Some(search_event);
            }
            // Unmapped keys fall through to the normal buffer bindings.
        } else if app.search.bar_open {
            if let Some(find_event) = find_bar_translate(key, app) {
                return Some(find_event);
            }
            // Unmapped keys (Ctrl+S, Ctrl+Z, Ctrl+Q, etc.) fall through
            // to the normal buffer bindings instead of being swallowed.
        } else if app.project_tree_open && app.project_tree.is_some() {
            // Only intercept keys for the project tree when it's open
            // AND a project is actually loaded. Without this guard, the
            // default project_tree_open=true silently swallows ALL keys
            // (typing, Ctrl+S, etc.) on startup before any project is
            // opened — making the editor appear non-functional.
            if let Some(tree_event) = project_tree_translate(key) {
                return Some(tree_event);
            }
            // Unmapped keys fall through to the normal buffer bindings.
        }
    }
    translate_buffer_key(key)
}

fn find_bar_translate(key: KeyEvent, app: &App) -> Option<EditorEvent> {
    // Replace bar takes priority — if it's open, all key events go to
    // the replace bar regardless of whether the find bar is also open.
    if app.search.replace_bar_open {
        return replace_bar_translate(key, app);
    }
    // Alt+R toggles regex mode before any char appends to the query.
    if key.code == KeyCode::Char('r') && key.modifiers.contains(KeyModifiers::ALT) {
        return Some(EditorEvent::ToggleFindRegex);
    }
    match key.code {
        KeyCode::Esc => Some(EditorEvent::FindClose),
        KeyCode::Enter => {
            if key.modifiers.contains(KeyModifiers::SHIFT) {
                Some(EditorEvent::FindPrev)
            } else {
                Some(EditorEvent::FindNext)
            }
        }
        KeyCode::Backspace => {
            // Pop the last char off the query.
            let mut q = app.search.query.clone();
            q.pop();
            Some(EditorEvent::FindQueryChanged(q))
        }
        KeyCode::Char(c) => {
            let mut q = app.search.query.clone();
            q.push(c);
            Some(EditorEvent::FindQueryChanged(q))
        }
        _ => None,
    }
}

/// Translate a key event while the replace bar is open.
/// Enter = ReplaceOne (replace current + advance), Shift+Enter =
/// ReplaceAll, Tab = cycle focus between find and replace bars
/// (handled implicitly because of `bar_open` state — keeping it
/// simple for v1: Tab does nothing here, the user can Esc the find
/// bar to refocus on the buffer), Esc = close replace bar (find
/// bar stays open), Backspace / printable chars edit the replacement.
fn replace_bar_translate(key: KeyEvent, app: &App) -> Option<EditorEvent> {
    match key.code {
        KeyCode::Esc => Some(EditorEvent::ReplaceClose),
        KeyCode::Enter => {
            if key.modifiers.contains(KeyModifiers::SHIFT) {
                Some(EditorEvent::ReplaceAll)
            } else {
                Some(EditorEvent::ReplaceOne)
            }
        }
        KeyCode::Backspace => {
            let mut q = app.search.replace_query.clone();
            q.pop();
            Some(EditorEvent::ReplaceQueryChanged(q))
        }
        KeyCode::Char(c) => {
            let mut q = app.search.replace_query.clone();
            q.push(c);
            Some(EditorEvent::ReplaceQueryChanged(q))
        }
        _ => None,
    }
}

fn project_tree_translate(key: KeyEvent) -> Option<EditorEvent> {
    match key.code {
        KeyCode::Esc => Some(EditorEvent::ToggleProjectTree),
        KeyCode::Up => Some(EditorEvent::ProjectTreeMove { delta: -1 }),
        KeyCode::Down => Some(EditorEvent::ProjectTreeMove { delta: 1 }),
        KeyCode::Enter => Some(EditorEvent::ProjectTreeOpen),
        _ => None,
    }
}

/// Translate a key event while the command palette is open.
fn command_palette_translate(key: KeyEvent, app: &App) -> Option<EditorEvent> {
    match key.code {
        KeyCode::Esc => Some(EditorEvent::CommandPaletteClose),
        KeyCode::Enter => Some(EditorEvent::CommandPaletteExecute),
        KeyCode::Up => Some(EditorEvent::CommandPaletteMove { delta: -1 }),
        KeyCode::Down => Some(EditorEvent::CommandPaletteMove { delta: 1 }),
        KeyCode::Backspace => {
            let mut q = app.command_palette.query.clone();
            q.pop();
            Some(EditorEvent::CommandPaletteQueryChanged(q))
        }
        KeyCode::Char(c) => {
            if key.modifiers.contains(KeyModifiers::CONTROL) {
                return None;
            }
            let mut q = app.command_palette.query.clone();
            q.push(c);
            Some(EditorEvent::CommandPaletteQueryChanged(q))
        }
        _ => None,
    }
}

/// Translate a key event while the fuzzy file finder is open.
fn fuzzy_finder_translate(key: KeyEvent, app: &App) -> Option<EditorEvent> {
    match key.code {
        KeyCode::Esc => Some(EditorEvent::FuzzyFinderClose),
        KeyCode::Enter => Some(EditorEvent::FuzzyFinderExecute),
        KeyCode::Up => Some(EditorEvent::FuzzyFinderMove { delta: -1 }),
        KeyCode::Down => Some(EditorEvent::FuzzyFinderMove { delta: 1 }),
        KeyCode::Backspace => {
            let mut q = app.fuzzy_finder.query.clone();
            q.pop();
            Some(EditorEvent::FuzzyFinderQueryChanged(q))
        }
        KeyCode::Char(c) => {
            if key.modifiers.contains(KeyModifiers::CONTROL) {
                return None;
            }
            let mut q = app.fuzzy_finder.query.clone();
            q.push(c);
            Some(EditorEvent::FuzzyFinderQueryChanged(q))
        }
        _ => None,
    }
}

/// Translate a key event while the project-search dialog is open.
/// Tab toggles focus between the find and replace fields. Typing and
/// Backspace edit the focused field. Ctrl+Enter replaces all.
fn project_search_translate(key: KeyEvent, app: &App) -> Option<EditorEvent> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    if app.project_search.confirm_replace {
        return match key.code {
            KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                Some(EditorEvent::ProjectSearchReplaceAllConfirm)
            }
            KeyCode::Esc
            | KeyCode::Char('n')
            | KeyCode::Char('N')
            | KeyCode::Char('q')
            | KeyCode::Char('Q') => Some(EditorEvent::ProjectSearchReplaceAllCancel),
            _ => None,
        };
    }
    match key.code {
        KeyCode::Esc => Some(EditorEvent::ProjectSearchClose),
        KeyCode::Enter if ctrl => Some(EditorEvent::ProjectSearchReplaceAll),
        KeyCode::Enter => Some(EditorEvent::ProjectSearchOpenResult),
        KeyCode::Up => Some(EditorEvent::ProjectSearchMove { delta: -1 }),
        KeyCode::Down => Some(EditorEvent::ProjectSearchMove { delta: 1 }),
        KeyCode::Tab => Some(EditorEvent::ProjectSearchToggleFocus),
        KeyCode::Backspace => {
            if app.project_search.replace_focused {
                let mut q = app.project_search.replace_query.clone();
                q.pop();
                Some(EditorEvent::ProjectSearchReplaceQueryChanged(q))
            } else {
                let mut q = app.project_search.query.clone();
                q.pop();
                Some(EditorEvent::ProjectSearchQueryChanged(q))
            }
        }
        KeyCode::Char(c) => {
            if app.project_search.replace_focused {
                let mut q = app.project_search.replace_query.clone();
                q.push(c);
                Some(EditorEvent::ProjectSearchReplaceQueryChanged(q))
            } else {
                let mut q = app.project_search.query.clone();
                q.push(c);
                Some(EditorEvent::ProjectSearchQueryChanged(q))
            }
        }
        _ => None,
    }
}

fn translate_buffer_key(key: KeyEvent) -> Option<EditorEvent> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);

    // Ctrl-modified keys first. Clipboard shortcuts (C / X / V) are
    // handled separately — see [`classify_clipboard_key`].
    if ctrl && !shift {
        return match key.code {
            KeyCode::Char('s') => Some(EditorEvent::Save),
            KeyCode::Char('z') => Some(EditorEvent::Undo),
            KeyCode::Char('y') => Some(EditorEvent::Redo),
            KeyCode::Char('q') => Some(EditorEvent::Quit),
            KeyCode::Char('a') => Some(EditorEvent::SelectAll),
            KeyCode::Char('f') => Some(EditorEvent::FindOpen),
            KeyCode::Char('r') => Some(EditorEvent::ReplaceOpen),
            KeyCode::Char('i') => Some(EditorEvent::CycleIndentMode),
            KeyCode::Char('k') => Some(EditorEvent::DeleteLine),
            KeyCode::Char('d') => Some(EditorEvent::SelectNextOccurrence),
            KeyCode::Char('t') => Some(EditorEvent::NewDoc),
            KeyCode::Char('w') => Some(EditorEvent::CloseDoc),
            KeyCode::Char('o') => Some(EditorEvent::OpenFile(None)),
            KeyCode::Char('g') => Some(EditorEvent::GoToLine(None)),
            KeyCode::Char('p') => Some(EditorEvent::FuzzyFinder(None)),
            KeyCode::Tab => Some(EditorEvent::NextDoc),
            KeyCode::Char('/') => Some(EditorEvent::ToggleComment),
            KeyCode::Backspace => Some(EditorEvent::DeleteWordLeft),
            KeyCode::Delete => Some(EditorEvent::DeleteWordRight),
            KeyCode::Left => Some(movement(Movement::WordLeft, false)),
            KeyCode::Right => Some(movement(Movement::WordRight, false)),
            KeyCode::Home => Some(movement(Movement::DocumentStart, false)),
            KeyCode::End => Some(movement(Movement::DocumentEnd, false)),
            KeyCode::Char(' ') => Some(EditorEvent::LspCompletion),
            _ => None,
        };
    }

    // Ctrl+Shift-modified keys. Tab cycles the other direction; Shift+W
    // toggles soft-wrap (W alone is close); Shift+F opens project search;
    // Shift+P opens the command palette.
    if ctrl && shift {
        return match key.code {
            KeyCode::Tab => Some(EditorEvent::PrevDoc),
            KeyCode::Char('W') => Some(EditorEvent::ToggleSoftWrap),
            KeyCode::Char('F') => Some(EditorEvent::ProjectSearch(None)),
            KeyCode::Char('P') => Some(EditorEvent::CommandPalette(None)),
            KeyCode::Char('S') => Some(EditorEvent::SaveAs(None)),
            KeyCode::Char('R') => Some(EditorEvent::ReloadFile),
            KeyCode::Up => Some(EditorEvent::PrevHunk),
            KeyCode::Down => Some(EditorEvent::NextHunk),
            KeyCode::Char('D') => Some(EditorEvent::DuplicateLine),
            _ => None,
        };
    }

    // Alt-modified keys (line move).
    if key.modifiers.contains(KeyModifiers::ALT) && !ctrl && !shift {
        return match key.code {
            KeyCode::Up => Some(EditorEvent::MoveLineUp),
            KeyCode::Down => Some(EditorEvent::MoveLineDown),
            _ => None,
        };
    }

    let select = shift;

    match key.code {
        KeyCode::Char('K') if shift && !ctrl && !key.modifiers.contains(KeyModifiers::ALT) => {
            Some(EditorEvent::LspHover)
        }
        KeyCode::Char(c) => Some(EditorEvent::Insert(c)),
        KeyCode::Backspace => Some(EditorEvent::DeleteLeft),
        KeyCode::Delete => Some(EditorEvent::DeleteRight),
        KeyCode::Enter => Some(EditorEvent::Insert('\n')),
        KeyCode::Tab => Some(EditorEvent::InsertTab),
        KeyCode::Esc => Some(EditorEvent::CollapseCursors),
        KeyCode::F(2) => Some(EditorEvent::RenameSymbol),
        KeyCode::F(5) => Some(EditorEvent::CycleTheme),
        KeyCode::F(8) => Some(EditorEvent::ToggleProjectTree),
        KeyCode::F(12) => Some(EditorEvent::LspGoToDefinition),
        KeyCode::Left if shift => Some(EditorEvent::ScrollLeft),
        KeyCode::Right if shift => Some(EditorEvent::ScrollRight),
        KeyCode::Left => Some(movement(Movement::Left, select)),
        KeyCode::Right => Some(movement(Movement::Right, select)),
        KeyCode::Up => Some(movement(Movement::Up, select)),
        KeyCode::Down => Some(movement(Movement::Down, select)),
        KeyCode::Home => Some(movement(Movement::LineStart, select)),
        KeyCode::End => Some(movement(Movement::LineEnd, select)),
        KeyCode::PageUp => Some(movement(Movement::PageUp, select)),
        KeyCode::PageDown => Some(movement(Movement::PageDown, select)),
        _ => None,
    }
}

/// A side-effect-producing clipboard action that needs OS access and
/// therefore can't be a plain `EditorEvent`. The caller
/// ([`crate::app::App::run`]) reads the actual clipboard via arboard
/// when it sees one of these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardAction {
    /// Ctrl+C: copy the selection text to the OS clipboard.
    Copy(String),
    /// Ctrl+X: copy the selection text to the OS clipboard, then delete
    /// the selection from the buffer.
    Cut(String),
    /// Ctrl+V: paste clipboard text into the buffer at the cursor.
    Paste,
}

/// If the key event is a clipboard shortcut (Ctrl+C / Ctrl+X / Ctrl+V),
/// return the corresponding [`ClipboardAction`]. Otherwise `None`.
///
/// Copy and Cut require a non-empty selection. Paste works regardless
/// of selection state — the handler replaces the selection if one
/// exists, or inserts at the cursor if collapsed.
pub fn classify_clipboard_key(key: KeyEvent, buffer: &dyn Buffer) -> Option<ClipboardAction> {
    if !key.modifiers.contains(KeyModifiers::CONTROL) {
        return None;
    }
    if key.modifiers.contains(KeyModifiers::SHIFT) {
        return None;
    }
    // Paste doesn't need a selection — handle it before the selection
    // check below.
    if key.code == KeyCode::Char('v') {
        return Some(ClipboardAction::Paste);
    }
    let target = match key.code {
        KeyCode::Char('c') => 'c',
        KeyCode::Char('x') => 'x',
        _ => return None,
    };
    let sel: Selection = buffer.selection();
    if sel.is_collapsed() {
        return None;
    }
    let text = buffer.slice(sel.range())?;
    if target == 'c' {
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

/// Translate a crossterm `MouseEvent` into an `EditorEvent`.
///
/// Supports:
/// - Left-button press → `SetCursor` at the click position (collapses selection)
/// - Left-button drag → `SelectExtendTo` at the drag position
/// - Left-button release → ignored (selection already finalized at last drag)
///
/// Scroll wheel and other buttons are ignored. The click position is
/// resolved against the current `App` viewport via
/// [`App::click_to_byte_pos`].
pub fn translate_mouse(mouse: MouseEvent, app: &App) -> Option<EditorEvent> {
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            let pos = app.click_to_byte_pos(mouse.column, mouse.row)?;
            Some(EditorEvent::SetCursor { pos })
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            let pos = app.click_to_byte_pos(mouse.column, mouse.row)?;
            Some(EditorEvent::SelectExtendTo { pos })
        }
        // Up and ScrollUp/ScrollDown are ignored for v1.
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn key_ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn key_shift(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::SHIFT)
    }

    fn key_alt(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::ALT)
    }

    #[test]
    fn printable_char_inserts() {
        assert_eq!(
            translate_key(key(KeyCode::Char('a')), None),
            Some(EditorEvent::Insert('a'))
        );
    }

    #[test]
    fn enter_inserts_newline() {
        assert_eq!(
            translate_key(key(KeyCode::Enter), None),
            Some(EditorEvent::Insert('\n'))
        );
    }

    #[test]
    fn backspace_deletes_left() {
        assert_eq!(
            translate_key(key(KeyCode::Backspace), None),
            Some(EditorEvent::DeleteLeft)
        );
    }

    #[test]
    fn delete_deletes_right() {
        assert_eq!(
            translate_key(key(KeyCode::Delete), None),
            Some(EditorEvent::DeleteRight)
        );
    }

    #[test]
    fn arrows_move() {
        assert_eq!(
            translate_key(key(KeyCode::Left), None),
            Some(EditorEvent::Move(Movement::Left))
        );
        assert_eq!(
            translate_key(key(KeyCode::Right), None),
            Some(EditorEvent::Move(Movement::Right))
        );
        assert_eq!(
            translate_key(key(KeyCode::Up), None),
            Some(EditorEvent::Move(Movement::Up))
        );
        assert_eq!(
            translate_key(key(KeyCode::Down), None),
            Some(EditorEvent::Move(Movement::Down))
        );
    }

    #[test]
    fn shift_arrows_extend_selection() {
        // Shift+Up/Down still extend selection.
        assert_eq!(
            translate_key(key_shift(KeyCode::Up), None),
            Some(EditorEvent::SelectExtend(Movement::Up))
        );
        assert_eq!(
            translate_key(key_shift(KeyCode::Down), None),
            Some(EditorEvent::SelectExtend(Movement::Down))
        );
    }

    #[test]
    fn shift_left_right_scrolls_horizontally() {
        assert_eq!(
            translate_key(key_shift(KeyCode::Left), None),
            Some(EditorEvent::ScrollLeft)
        );
        assert_eq!(
            translate_key(key_shift(KeyCode::Right), None),
            Some(EditorEvent::ScrollRight)
        );
    }

    #[test]
    fn ctrl_s_saves() {
        assert_eq!(translate_key(key_ctrl('s'), None), Some(EditorEvent::Save));
    }

    #[test]
    fn ctrl_q_quits() {
        assert_eq!(translate_key(key_ctrl('q'), None), Some(EditorEvent::Quit));
    }

    #[test]
    fn ctrl_z_undoes() {
        assert_eq!(translate_key(key_ctrl('z'), None), Some(EditorEvent::Undo));
    }

    #[test]
    fn ctrl_y_redoes() {
        assert_eq!(translate_key(key_ctrl('y'), None), Some(EditorEvent::Redo));
    }

    #[test]
    fn escape_collapses_cursor_state() {
        assert_eq!(
            translate_key(key(KeyCode::Esc), None),
            Some(EditorEvent::CollapseCursors)
        );
    }

    #[test]
    fn unmapped_key_returns_none() {
        assert_eq!(translate_key(key(KeyCode::F(1)), None), None);
    }

    #[test]
    fn alt_r_in_find_bar_toggles_regex() {
        let mut app = app_with("hello world");
        app.handle_event(EditorEvent::FindOpen);
        assert_eq!(
            translate_key(key_alt(KeyCode::Char('r')), Some(&app)),
            Some(EditorEvent::ToggleFindRegex)
        );
    }

    #[test]
    fn text_dialogs_do_not_swallow_quit_or_close_shortcuts() {
        let mut app = app_with("hello world");

        app.handle_event(EditorEvent::FindOpen);
        assert_eq!(
            translate_key(key_ctrl('q'), Some(&app)),
            Some(EditorEvent::Quit)
        );

        app.handle_event(EditorEvent::ReplaceOpen);
        assert_eq!(
            translate_key(key_ctrl('w'), Some(&app)),
            Some(EditorEvent::CloseDoc)
        );

        app.handle_event(EditorEvent::FindClose);
        app.handle_event(EditorEvent::ProjectSearch(None));
        assert_eq!(
            translate_key(key_ctrl('q'), Some(&app)),
            Some(EditorEvent::Quit)
        );
    }

    // ----- mouse event translation -----

    use crate::app::App;
    use core::PieceTableBuffer;

    fn app_with(content: &str) -> App {
        let buf: Box<dyn core::Buffer> =
            Box::new(PieceTableBuffer::from_bytes(content.as_bytes().to_vec()));
        App::new(buf)
    }

    fn mouse_event(kind: MouseEventKind, col: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    #[test]
    fn left_click_translates_to_set_cursor() {
        let mut app = app_with("hello\nworld");
        app.active_doc_mut().view.scroll_top_line = 0;
        app.viewport_height = 10;
        // Click on row 2 ("world"), col 4 (gutter end), expect byte 6.
        let ev = translate_mouse(
            mouse_event(MouseEventKind::Down(MouseButton::Left), 4, 2),
            &app,
        );
        match ev {
            Some(EditorEvent::SetCursor { pos }) => assert_eq!(pos, 6),
            _ => panic!("expected SetCursor, got {ev:?}"),
        }
    }

    #[test]
    fn left_drag_translates_to_select_extend_to() {
        let mut app = app_with("hello\nworld");
        app.active_doc_mut().view.scroll_top_line = 0;
        app.viewport_height = 10;
        // First click sets the anchor.
        app.handle_event(EditorEvent::SetCursor {
            pos: app.click_to_byte_pos(0, 2).unwrap(),
        });
        // Drag to col 8 (gutter=5 → text col 3 = 'r' in 'world' = byte 9).
        let ev = translate_mouse(
            mouse_event(MouseEventKind::Drag(MouseButton::Left), 8, 2),
            &app,
        );
        match ev {
            Some(EditorEvent::SelectExtendTo { pos }) => assert_eq!(pos, 9),
            _ => panic!("expected SelectExtendTo, got {ev:?}"),
        }
    }

    #[test]
    fn right_click_returns_none() {
        let app = app_with("hello");
        let ev = translate_mouse(
            mouse_event(MouseEventKind::Down(MouseButton::Right), 5, 1),
            &app,
        );
        assert!(ev.is_none());
    }

    #[test]
    fn scroll_returns_none() {
        let app = app_with("hello");
        let ev = translate_mouse(mouse_event(MouseEventKind::ScrollUp, 5, 1), &app);
        assert!(ev.is_none());
    }

    #[test]
    fn release_returns_none() {
        // Up is intentionally ignored — selection is finalized at last drag.
        let app = app_with("hello");
        let ev = translate_mouse(
            mouse_event(MouseEventKind::Up(MouseButton::Left), 5, 1),
            &app,
        );
        assert!(ev.is_none());
    }

    // ----- clipboard key classification -----

    use core::Selection;

    fn buf_with(text: &str) -> core::PieceTableBuffer {
        core::PieceTableBuffer::from_bytes(text.as_bytes().to_vec())
    }

    #[test]
    fn ctrl_c_with_selection_returns_copy_action() {
        let mut buf = buf_with("hello world");
        buf.set_selection(Selection { anchor: 0, head: 5 });
        let ev = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(
            classify_clipboard_key(ev, &buf),
            Some(ClipboardAction::Copy("hello".to_string()))
        );
    }

    #[test]
    fn ctrl_x_with_selection_returns_cut_action() {
        let mut buf = buf_with("hello world");
        buf.set_selection(Selection { anchor: 0, head: 5 });
        let ev = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL);
        assert_eq!(
            classify_clipboard_key(ev, &buf),
            Some(ClipboardAction::Cut("hello".to_string()))
        );
    }

    #[test]
    fn ctrl_c_without_selection_returns_none() {
        let buf = buf_with("hello");
        let ev = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(classify_clipboard_key(ev, &buf), None);
    }

    #[test]
    fn plain_c_does_not_match_clipboard_shortcut() {
        let mut buf = buf_with("hello");
        buf.set_selection(Selection { anchor: 0, head: 5 });
        let ev = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE);
        assert_eq!(classify_clipboard_key(ev, &buf), None);
    }

    #[test]
    fn ctrl_shift_c_is_not_clipboard_shortcut() {
        let mut buf = buf_with("hello");
        buf.set_selection(Selection { anchor: 0, head: 5 });
        let ev = KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        );
        assert_eq!(classify_clipboard_key(ev, &buf), None);
    }

    #[test]
    fn ctrl_v_classifies_as_paste() {
        // Ctrl+V returns ClipboardAction::Paste regardless of
        // selection state. The event loop reads the OS clipboard and
        // fires EditorEvent::Paste.
        let mut buf = buf_with("hello");
        buf.set_selection(Selection { anchor: 0, head: 5 });
        let ev = KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL);
        assert_eq!(
            classify_clipboard_key(ev, &buf),
            Some(ClipboardAction::Paste)
        );
        // Paste also works with a collapsed (empty) selection.
        buf.set_selection(Selection::collapsed(3));
        let ev2 = KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL);
        assert_eq!(
            classify_clipboard_key(ev2, &buf),
            Some(ClipboardAction::Paste)
        );
    }

    // ----- multi-buffer key bindings -----

    #[test]
    fn ctrl_t_opens_new_doc() {
        assert_eq!(
            translate_key(key_ctrl('t'), None),
            Some(EditorEvent::NewDoc)
        );
    }

    #[test]
    fn ctrl_w_closes_active_doc() {
        assert_eq!(
            translate_key(key_ctrl('w'), None),
            Some(EditorEvent::CloseDoc)
        );
    }

    #[test]
    fn ctrl_tab_cycles_next_doc() {
        assert_eq!(
            translate_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::CONTROL), None),
            Some(EditorEvent::NextDoc)
        );
    }

    #[test]
    fn ctrl_shift_tab_cycles_prev_doc() {
        assert_eq!(
            translate_key(
                KeyEvent::new(KeyCode::Tab, KeyModifiers::CONTROL | KeyModifiers::SHIFT),
                None
            ),
            Some(EditorEvent::PrevDoc)
        );
    }

    #[test]
    fn plain_tab_inserts_indent_not_doc_event() {
        // Plain Tab (no modifier) emits InsertTab so the App can
        // resolve it per the active document's indent mode (spaces
        // vs tab character). Used to emit Insert('\t'); refactored
        // when indent settings landed.
        assert_eq!(
            translate_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), None),
            Some(EditorEvent::InsertTab)
        );
    }

    #[test]
    fn ctrl_i_translates_to_cycle_indent_mode() {
        assert_eq!(
            translate_key(
                KeyEvent::new(KeyCode::Char('i'), KeyModifiers::CONTROL),
                None
            ),
            Some(EditorEvent::CycleIndentMode)
        );
    }

    #[test]
    fn f5_translates_to_cycle_theme() {
        assert_eq!(
            translate_key(KeyEvent::new(KeyCode::F(5), KeyModifiers::NONE), None),
            Some(EditorEvent::CycleTheme)
        );
    }

    #[test]
    fn ctrl_g_opens_go_to_line() {
        assert_eq!(
            translate_key(
                KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL),
                None
            ),
            Some(EditorEvent::GoToLine(None))
        );
    }

    #[test]
    fn ctrl_arrows_move_by_word() {
        assert_eq!(
            translate_key(KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL), None),
            Some(EditorEvent::Move(Movement::WordLeft))
        );
        assert_eq!(
            translate_key(KeyEvent::new(KeyCode::Right, KeyModifiers::CONTROL), None),
            Some(EditorEvent::Move(Movement::WordRight))
        );
    }

    #[test]
    fn ctrl_home_end_jump_to_document_edges() {
        assert_eq!(
            translate_key(KeyEvent::new(KeyCode::Home, KeyModifiers::CONTROL), None),
            Some(EditorEvent::Move(Movement::DocumentStart))
        );
        assert_eq!(
            translate_key(KeyEvent::new(KeyCode::End, KeyModifiers::CONTROL), None),
            Some(EditorEvent::Move(Movement::DocumentEnd))
        );
    }
}
