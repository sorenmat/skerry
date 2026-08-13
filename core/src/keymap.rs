//! Frontend-neutral Standard, Vim, and Emacs keybinding state machines.

use std::ops::Range;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{char_after, move_left_by_char, move_right_by_char, Buffer, EditorEvent, Movement};

const MAX_VIM_COUNT: usize = 10_000;
const MAX_VIM_PASTE_BYTES: usize = 16 * 1024 * 1024;

/// Persisted built-in keyboard preset.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KeybindingMode {
    #[default]
    Standard,
    Vim,
    Emacs,
}

impl KeybindingMode {
    pub const ALL: [Self; 3] = [Self::Standard, Self::Vim, Self::Emacs];

    pub fn label(self) -> &'static str {
        match self {
            Self::Standard => "Standard",
            Self::Vim => "Vim",
            Self::Emacs => "Emacs",
        }
    }
}

/// Modifiers normalized by each frontend.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KeyModifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    /// The platform application modifier (Command on macOS, Ctrl elsewhere).
    pub command: bool,
}

/// Non-printable keys used by built-in maps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyCode {
    Escape,
    Enter,
    Backspace,
    Delete,
    Tab,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    Space,
}

/// A frontend-neutral keystroke. Printable text is kept separate so GUI IME
/// input remains correct in Standard and insert-oriented modes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyInput {
    Char(char, KeyModifiers),
    Key(KeyCode, KeyModifiers),
}

impl KeyInput {
    fn modifiers(&self) -> KeyModifiers {
        match self {
            Self::Char(_, modifiers) | Self::Key(_, modifiers) => *modifiers,
        }
    }

    fn plain_char(&self) -> Option<char> {
        match self {
            Self::Char(ch, modifiers)
                if !modifiers.ctrl && !modifiers.alt && !modifiers.command =>
            {
                Some(*ch)
            }
            _ => None,
        }
    }
}

/// Vim's visible editor modes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum VimMode {
    #[default]
    Normal,
    Insert,
    Visual,
    VisualLine,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VimOperator {
    Delete,
    Change,
    Yank,
}

#[derive(Debug, Clone, Default)]
struct VimRegister {
    text: String,
    linewise: bool,
}

#[derive(Debug, Clone, Default)]
struct VimState {
    mode: VimMode,
    operator: Option<VimOperator>,
    operator_count: usize,
    count: usize,
    pending_g: bool,
    command_line: Option<String>,
    register: VimRegister,
    search_forward: bool,
    visual_line_anchor: Option<usize>,
    visual_line_head: Option<usize>,
}

#[derive(Debug, Clone, Default)]
struct EmacsState {
    ctrl_x: bool,
    mark: Option<usize>,
    kill_ring: Vec<String>,
    kill_index: usize,
    last_yank: Option<(usize, usize)>,
    last_was_kill: bool,
}

/// Result of a keymap lookup. Events are applied in order by the active
/// frontend through its ordinary `handle_event` path.
#[derive(Debug, Clone, Default)]
pub struct KeymapOutput {
    pub consumed: bool,
    pub events: Vec<EditorEvent>,
    pub status: Option<String>,
}

impl KeymapOutput {
    fn consumed() -> Self {
        Self {
            consumed: true,
            ..Self::default()
        }
    }

    fn event(event: EditorEvent) -> Self {
        Self {
            consumed: true,
            events: vec![event],
            status: None,
        }
    }
}

/// Process-global transient keymap state. Only `mode` is persisted by Config.
#[derive(Debug, Clone)]
pub struct KeymapState {
    mode: KeybindingMode,
    vim: VimState,
    emacs: EmacsState,
}

impl Default for KeymapState {
    fn default() -> Self {
        Self::new(KeybindingMode::Standard)
    }
}

impl KeymapState {
    pub fn new(mode: KeybindingMode) -> Self {
        let mut state = Self {
            mode,
            vim: VimState::default(),
            emacs: EmacsState::default(),
        };
        state.vim.search_forward = true;
        state
    }

    pub fn mode(&self) -> KeybindingMode {
        self.mode
    }

    pub fn vim_mode(&self) -> Option<VimMode> {
        (self.mode == KeybindingMode::Vim).then_some(self.vim.mode)
    }

    pub fn vim_register(&self) -> Option<(&str, bool)> {
        (self.mode == KeybindingMode::Vim)
            .then_some((self.vim.register.text.as_str(), self.vim.register.linewise))
    }

    pub fn emacs_kill_ring(&self) -> Option<&[String]> {
        (self.mode == KeybindingMode::Emacs).then_some(&self.emacs.kill_ring)
    }

    pub fn set_mode(&mut self, mode: KeybindingMode) {
        self.mode = mode;
        self.vim.operator = None;
        self.vim.operator_count = 0;
        self.vim.count = 0;
        self.vim.pending_g = false;
        self.vim.command_line = None;
        self.vim.mode = VimMode::Normal;
        self.vim.search_forward = true;
        self.vim.visual_line_anchor = None;
        self.vim.visual_line_head = None;
        self.emacs.ctrl_x = false;
        self.emacs.mark = None;
        self.emacs.last_yank = None;
        self.emacs.last_was_kill = false;
    }

    pub fn reset_transient(&mut self) {
        self.set_mode(self.mode);
    }

    pub fn status_label(&self) -> String {
        match self.mode {
            KeybindingMode::Standard => "STANDARD".into(),
            KeybindingMode::Vim => {
                if let Some(command) = &self.vim.command_line {
                    return format!("VIM :{command}");
                }
                let mode = match self.vim.mode {
                    VimMode::Normal => "NORMAL",
                    VimMode::Insert => "INSERT",
                    VimMode::Visual => "VISUAL",
                    VimMode::VisualLine => "VISUAL LINE",
                };
                let pending = if let Some(operator) = self.vim.operator {
                    match operator {
                        VimOperator::Delete => " d",
                        VimOperator::Change => " c",
                        VimOperator::Yank => " y",
                    }
                } else if self.vim.pending_g {
                    " g"
                } else {
                    ""
                };
                format!("VIM {mode}{pending}")
            }
            KeybindingMode::Emacs => {
                if self.emacs.ctrl_x {
                    "EMACS C-x".into()
                } else if self.emacs.mark.is_some() {
                    "EMACS MARK".into()
                } else {
                    "EMACS".into()
                }
            }
        }
    }

    pub fn handle(
        &mut self,
        input: &KeyInput,
        buffer: &dyn Buffer,
        viewport_lines: usize,
    ) -> KeymapOutput {
        // A pure platform Command chord belongs to the application's
        // ordinary shortcut layer on macOS in every preset.
        if input.modifiers().command && !input.modifiers().ctrl {
            return KeymapOutput::default();
        }
        match self.mode {
            KeybindingMode::Standard => KeymapOutput::default(),
            KeybindingMode::Vim => self.handle_vim(input, buffer, viewport_lines),
            KeybindingMode::Emacs => self.handle_emacs(input, buffer, viewport_lines),
        }
    }

    fn handle_vim(
        &mut self,
        input: &KeyInput,
        buffer: &dyn Buffer,
        viewport_lines: usize,
    ) -> KeymapOutput {
        if self.vim.command_line.is_some() {
            return self.handle_vim_command_line(input);
        }
        if self.vim.mode == VimMode::Insert {
            if matches!(input, KeyInput::Key(KeyCode::Escape, _))
                || matches!(input, KeyInput::Char('[', m) if m.ctrl)
            {
                self.vim.mode = VimMode::Normal;
                return KeymapOutput::consumed();
            }
            return KeymapOutput::default();
        }

        let modifiers = input.modifiers();
        // Shifted Ctrl chords are reserved for application shortcuts.
        if modifiers.ctrl && modifiers.shift {
            return KeymapOutput::default();
        }
        if modifiers.ctrl {
            if matches!(input, KeyInput::Char('r' | 'R', _)) {
                return KeymapOutput::event(EditorEvent::Redo);
            }
            let movement = match input {
                KeyInput::Char('f' | 'F', _) => Some(Movement::PageDown),
                KeyInput::Char('b' | 'B', _) => Some(Movement::PageUp),
                KeyInput::Char('d' | 'D', _) => Some(Movement::PageDown),
                KeyInput::Char('u' | 'U', _) => Some(Movement::PageUp),
                _ => None,
            };
            if let Some(movement) = movement {
                return self.vim_motion(buffer, movement, viewport_lines, false);
            }
        }

        if matches!(input, KeyInput::Key(KeyCode::Escape, _)) {
            self.vim.mode = VimMode::Normal;
            self.clear_vim_pending();
            return KeymapOutput {
                consumed: true,
                events: vec![EditorEvent::SetCursor {
                    pos: buffer.cursor(),
                }],
                status: None,
            };
        }

        if let Some(ch) = input.plain_char() {
            if ch.is_ascii_digit() && !(ch == '0' && self.vim.count == 0) {
                self.vim.count = self
                    .vim
                    .count
                    .saturating_mul(10)
                    .saturating_add(ch.to_digit(10).unwrap_or(0) as usize)
                    .min(MAX_VIM_COUNT);
                return KeymapOutput::consumed();
            }
            if self.vim.pending_g {
                self.vim.pending_g = false;
                if ch == 'g' {
                    return self.vim_motion(buffer, Movement::DocumentStart, viewport_lines, false);
                }
                self.clear_vim_pending();
                return KeymapOutput::consumed();
            }
            if let Some(operator) = self.vim.operator {
                if operator_key(operator) == ch {
                    return self.vim_line_operator(operator, buffer);
                }
                if matches!(ch, '^' | 'e') {
                    return self.vim_special_motion(ch, buffer, viewport_lines);
                }
                if let Some((movement, inclusive)) = vim_char_motion(ch) {
                    return self.vim_motion(buffer, movement, viewport_lines, inclusive);
                }
                self.clear_vim_pending();
                return KeymapOutput::consumed();
            }
            if matches!(self.vim.mode, VimMode::Visual | VimMode::VisualLine) {
                return self.handle_vim_visual_char(ch, buffer, viewport_lines);
            }
            return self.handle_vim_normal_char(ch, buffer, viewport_lines);
        }

        let movement = match input {
            KeyInput::Key(KeyCode::Left, _) => Some(Movement::Left),
            KeyInput::Key(KeyCode::Right, _) => Some(Movement::Right),
            KeyInput::Key(KeyCode::Up, _) => Some(Movement::Up),
            KeyInput::Key(KeyCode::Down, _) => Some(Movement::Down),
            KeyInput::Key(KeyCode::Home, _) => Some(Movement::LineStart),
            KeyInput::Key(KeyCode::End, _) => Some(Movement::LineEnd),
            KeyInput::Key(KeyCode::PageUp, _) => Some(Movement::PageUp),
            KeyInput::Key(KeyCode::PageDown, _) => Some(Movement::PageDown),
            _ => None,
        };
        movement
            .map(|movement| self.vim_motion(buffer, movement, viewport_lines, false))
            .unwrap_or_default()
    }

    fn handle_vim_normal_char(
        &mut self,
        ch: char,
        buffer: &dyn Buffer,
        viewport_lines: usize,
    ) -> KeymapOutput {
        if matches!(ch, '^' | 'e') {
            return self.vim_special_motion(ch, buffer, viewport_lines);
        }
        if let Some((movement, inclusive)) = vim_char_motion(ch) {
            return self.vim_motion(buffer, movement, viewport_lines, inclusive);
        }
        match ch {
            'g' => {
                self.vim.pending_g = true;
                KeymapOutput::consumed()
            }
            'G' => self.vim_motion(buffer, Movement::DocumentEnd, viewport_lines, false),
            'd' | 'c' | 'y' => {
                self.vim.operator = Some(match ch {
                    'd' => VimOperator::Delete,
                    'c' => VimOperator::Change,
                    _ => VimOperator::Yank,
                });
                self.vim.operator_count = self.take_vim_count();
                KeymapOutput::consumed()
            }
            'i' => self.enter_vim_insert(None),
            'a' => self.enter_vim_insert(Some(move_right_by_char(buffer, buffer.cursor()))),
            'I' => self.enter_vim_insert(Some(first_non_whitespace(buffer, buffer.cursor()))),
            'A' => self.enter_vim_insert(Some(line_end(buffer, buffer.cursor()))),
            'o' => {
                self.vim.mode = VimMode::Insert;
                let pos = line_end(buffer, buffer.cursor());
                KeymapOutput {
                    consumed: true,
                    events: vec![
                        EditorEvent::SetCursor { pos },
                        EditorEvent::Paste("\n".into()),
                    ],
                    status: None,
                }
            }
            'O' => {
                self.vim.mode = VimMode::Insert;
                let pos = line_start(buffer, buffer.cursor());
                KeymapOutput {
                    consumed: true,
                    events: vec![
                        EditorEvent::SetCursor { pos },
                        EditorEvent::Paste("\n".into()),
                        EditorEvent::SetCursor { pos },
                    ],
                    status: None,
                }
            }
            'v' => {
                self.vim.mode = VimMode::Visual;
                KeymapOutput {
                    consumed: true,
                    events: vec![EditorEvent::SelectExtendTo {
                        pos: move_right_by_char(buffer, buffer.cursor()),
                    }],
                    status: None,
                }
            }
            'V' => {
                self.vim.mode = VimMode::VisualLine;
                let line = buffer
                    .pos_to_linecol(buffer.cursor())
                    .map(|(line, _)| line)
                    .unwrap_or(0);
                self.vim.visual_line_anchor = Some(line);
                self.vim.visual_line_head = Some(line);
                let range = line_span(buffer, buffer.cursor(), 1);
                select_range(range)
            }
            'x' | 's' => {
                let count = self.take_vim_count();
                let range = char_span_right(buffer, buffer.cursor(), count);
                let text = buffer.slice(range.clone()).unwrap_or_default();
                self.set_vim_register(text, false);
                if ch == 's' {
                    self.vim.mode = VimMode::Insert;
                }
                delete_range(range)
            }
            'X' => {
                let count = self.take_vim_count();
                let range = char_span_left(buffer, buffer.cursor(), count);
                let text = buffer.slice(range.clone()).unwrap_or_default();
                self.set_vim_register(text, false);
                delete_range(range)
            }
            'D' | 'C' => {
                let range = buffer.cursor()..line_end(buffer, buffer.cursor());
                let text = buffer.slice(range.clone()).unwrap_or_default();
                self.set_vim_register(text, false);
                if ch == 'C' {
                    self.vim.mode = VimMode::Insert;
                }
                delete_range(range)
            }
            'u' => KeymapOutput::event(EditorEvent::Undo),
            'p' | 'P' => self.vim_paste(ch == 'p', buffer),
            '/' | '?' => {
                self.vim.search_forward = ch == '/';
                KeymapOutput::event(if ch == '/' {
                    EditorEvent::FindOpen
                } else {
                    EditorEvent::FindOpenBackward
                })
            }
            'n' => KeymapOutput::event(if self.vim.search_forward {
                EditorEvent::FindNext
            } else {
                EditorEvent::FindPrev
            }),
            'N' => KeymapOutput::event(if self.vim.search_forward {
                EditorEvent::FindPrev
            } else {
                EditorEvent::FindNext
            }),
            ':' => {
                self.vim.command_line = Some(String::new());
                KeymapOutput::consumed()
            }
            _ => KeymapOutput::consumed(),
        }
    }

    fn handle_vim_visual_char(
        &mut self,
        ch: char,
        buffer: &dyn Buffer,
        viewport_lines: usize,
    ) -> KeymapOutput {
        match ch {
            'v' if self.vim.mode == VimMode::Visual => {
                self.vim.mode = VimMode::Normal;
                KeymapOutput::event(EditorEvent::SetCursor {
                    pos: buffer.cursor(),
                })
            }
            'V' => {
                self.vim.mode = VimMode::VisualLine;
                let selection = buffer.selection();
                let start = line_start(buffer, selection.anchor);
                let end_line = buffer
                    .pos_to_linecol(selection.head)
                    .map(|(line, _)| line)
                    .unwrap_or(0);
                let start_line = buffer
                    .pos_to_linecol(start)
                    .map(|(line, _)| line)
                    .unwrap_or(0);
                self.vim.visual_line_anchor = Some(start_line);
                self.vim.visual_line_head = Some(end_line);
                let end = line_span_from_lines(buffer, end_line, end_line).end;
                select_range(start..end)
            }
            'd' | 'c' | 'y' | 'x' => {
                let range = buffer.selection().range();
                let linewise = self.vim.mode == VimMode::VisualLine;
                let text = buffer.slice(range.clone()).unwrap_or_default();
                self.set_vim_register(text, linewise);
                self.vim.mode = if ch == 'c' {
                    VimMode::Insert
                } else {
                    VimMode::Normal
                };
                self.vim.visual_line_anchor = None;
                self.vim.visual_line_head = None;
                if ch == 'y' {
                    KeymapOutput::event(EditorEvent::SetCursor { pos: range.start })
                } else {
                    delete_range(range)
                }
            }
            _ => {
                if matches!(ch, '^' | 'e') {
                    return self.vim_special_motion(ch, buffer, viewport_lines);
                }
                if let Some((movement, inclusive)) = vim_char_motion(ch) {
                    self.vim_motion(buffer, movement, viewport_lines, inclusive)
                } else {
                    KeymapOutput::consumed()
                }
            }
        }
    }

    fn vim_motion(
        &mut self,
        buffer: &dyn Buffer,
        movement: Movement,
        viewport_lines: usize,
        inclusive: bool,
    ) -> KeymapOutput {
        let count = self.take_vim_count();
        let effective_count = count
            .saturating_mul(self.vim.operator_count.max(1))
            .min(MAX_VIM_COUNT);
        if self.vim.mode == VimMode::VisualLine {
            let anchor_line = self.vim.visual_line_anchor.unwrap_or_else(|| {
                buffer
                    .pos_to_linecol(buffer.cursor())
                    .map(|(line, _)| line)
                    .unwrap_or(0)
            });
            let head_line = self.vim.visual_line_head.unwrap_or(anchor_line);
            let last = buffer.line_count().saturating_sub(1);
            let target_line = match movement {
                Movement::Up => head_line.saturating_sub(effective_count),
                Movement::Down => head_line.saturating_add(effective_count).min(last),
                Movement::PageUp => {
                    head_line.saturating_sub(effective_count.saturating_mul(viewport_lines.max(1)))
                }
                Movement::PageDown => head_line
                    .saturating_add(effective_count.saturating_mul(viewport_lines.max(1)))
                    .min(last),
                Movement::DocumentStart => 0,
                Movement::DocumentEnd => last,
                _ => head_line,
            };
            self.vim.visual_line_anchor = Some(anchor_line);
            self.vim.visual_line_head = Some(target_line);
            return select_range(line_span_from_lines(buffer, anchor_line, target_line));
        }
        let origin = buffer.cursor();
        let mut target = origin;
        for _ in 0..effective_count {
            let next = resolve_movement(buffer, target, movement, viewport_lines);
            if next == target {
                break;
            }
            target = next;
        }
        if let Some(operator) = self.vim.operator.take() {
            self.vim.operator_count = 0;
            if inclusive && target >= origin {
                target = move_right_by_char(buffer, target);
            }
            let range = origin.min(target)..origin.max(target);
            let text = buffer.slice(range.clone()).unwrap_or_default();
            self.set_vim_register(text, false);
            return match operator {
                VimOperator::Yank => KeymapOutput::event(EditorEvent::SetCursor { pos: origin }),
                VimOperator::Delete => delete_range(range),
                VimOperator::Change => {
                    self.vim.mode = VimMode::Insert;
                    delete_range(range)
                }
            };
        }
        if self.vim.mode == VimMode::Visual {
            KeymapOutput::event(EditorEvent::SelectExtendTo { pos: target })
        } else if self.vim.mode == VimMode::VisualLine {
            let anchor_line = buffer
                .pos_to_linecol(buffer.selection().anchor)
                .map(|(line, _)| line)
                .unwrap_or(0);
            let target_line = buffer
                .pos_to_linecol(target)
                .map(|(line, _)| line)
                .unwrap_or(anchor_line);
            select_range(line_span_from_lines(buffer, anchor_line, target_line))
        } else {
            KeymapOutput::event(EditorEvent::SetCursor { pos: target })
        }
    }

    fn vim_special_motion(
        &mut self,
        ch: char,
        buffer: &dyn Buffer,
        _viewport_lines: usize,
    ) -> KeymapOutput {
        let count = self.take_vim_count();
        let effective_count = count
            .saturating_mul(self.vim.operator_count.max(1))
            .min(MAX_VIM_COUNT);
        let origin = buffer.cursor();
        let mut target = origin;
        for _ in 0..effective_count {
            let next = match ch {
                '^' => first_non_whitespace(buffer, target),
                'e' => word_end_right(buffer, target),
                _ => target,
            };
            if next == target {
                break;
            }
            target = next;
        }
        if ch == 'e' {
            self.finish_vim_target(buffer, origin, target, true)
        } else {
            self.finish_vim_target(buffer, origin, target, false)
        }
    }

    fn finish_vim_target(
        &mut self,
        buffer: &dyn Buffer,
        origin: usize,
        mut target: usize,
        inclusive: bool,
    ) -> KeymapOutput {
        if let Some(operator) = self.vim.operator.take() {
            self.vim.operator_count = 0;
            if inclusive && target >= origin {
                target = move_right_by_char(buffer, target);
            }
            let range = origin.min(target)..origin.max(target);
            let text = buffer.slice(range.clone()).unwrap_or_default();
            self.set_vim_register(text, false);
            return match operator {
                VimOperator::Yank => KeymapOutput::event(EditorEvent::SetCursor { pos: origin }),
                VimOperator::Delete => delete_range(range),
                VimOperator::Change => {
                    self.vim.mode = VimMode::Insert;
                    delete_range(range)
                }
            };
        }
        if self.vim.mode == VimMode::Visual {
            KeymapOutput::event(EditorEvent::SelectExtendTo { pos: target })
        } else if self.vim.mode == VimMode::VisualLine {
            let anchor_line = buffer
                .pos_to_linecol(buffer.selection().anchor)
                .map(|(line, _)| line)
                .unwrap_or(0);
            let target_line = buffer
                .pos_to_linecol(target)
                .map(|(line, _)| line)
                .unwrap_or(anchor_line);
            select_range(line_span_from_lines(buffer, anchor_line, target_line))
        } else {
            KeymapOutput::event(EditorEvent::SetCursor { pos: target })
        }
    }

    fn vim_line_operator(&mut self, operator: VimOperator, buffer: &dyn Buffer) -> KeymapOutput {
        let count = self
            .take_vim_count()
            .saturating_mul(self.vim.operator_count.max(1))
            .min(MAX_VIM_COUNT);
        self.vim.operator = None;
        self.vim.operator_count = 0;
        let range = line_span(buffer, buffer.cursor(), count);
        let text = buffer.slice(range.clone()).unwrap_or_default();
        self.set_vim_register(text, true);
        match operator {
            VimOperator::Yank => KeymapOutput::event(EditorEvent::SetCursor { pos: range.start }),
            VimOperator::Delete => delete_range(range),
            VimOperator::Change => {
                self.vim.mode = VimMode::Insert;
                delete_range(range)
            }
        }
    }

    fn vim_paste(&mut self, after: bool, buffer: &dyn Buffer) -> KeymapOutput {
        if self.vim.register.text.is_empty() {
            return KeymapOutput::consumed();
        }
        let count = self.take_vim_count();
        let mut payload = self.vim.register.text.clone();
        if self.vim.register.linewise && !payload.ends_with('\n') {
            payload.push('\n');
        }
        let Some(repeated_len) = payload.len().checked_mul(count) else {
            return KeymapOutput {
                consumed: true,
                events: Vec::new(),
                status: Some("Vim paste is too large".into()),
            };
        };
        if repeated_len.saturating_add(usize::from(self.vim.register.linewise))
            > MAX_VIM_PASTE_BYTES
        {
            return KeymapOutput {
                consumed: true,
                events: Vec::new(),
                status: Some("Vim paste is limited to 16 MiB".into()),
            };
        }
        let mut text = payload.repeat(count);
        let pos = if self.vim.register.linewise {
            let start = line_start(buffer, buffer.cursor());
            if buffer.is_empty() {
                0
            } else if after {
                let pos = line_span(buffer, buffer.cursor(), 1).end;
                if pos == buffer.len()
                    && char_after(buffer, move_left_by_char(buffer, pos)) != Some('\n')
                {
                    text.insert(0, '\n');
                }
                pos
            } else {
                start
            }
        } else if after {
            move_right_by_char(buffer, buffer.cursor())
        } else {
            buffer.cursor()
        };
        KeymapOutput {
            consumed: true,
            events: vec![EditorEvent::SetCursor { pos }, EditorEvent::Paste(text)],
            status: None,
        }
    }

    fn handle_vim_command_line(&mut self, input: &KeyInput) -> KeymapOutput {
        match input {
            KeyInput::Key(KeyCode::Escape, _) => {
                self.vim.command_line = None;
                KeymapOutput::consumed()
            }
            KeyInput::Key(KeyCode::Backspace, _) => {
                if let Some(command) = self.vim.command_line.as_mut() {
                    command.pop();
                }
                KeymapOutput::consumed()
            }
            KeyInput::Key(KeyCode::Enter, _) => {
                let command = self.vim.command_line.take().unwrap_or_default();
                let mut output = KeymapOutput::consumed();
                output.events = match command.trim() {
                    "w" => vec![EditorEvent::Save],
                    "q" => vec![EditorEvent::CloseDoc],
                    "q!" => vec![EditorEvent::ForceCloseDoc],
                    "wq" => vec![EditorEvent::Save, EditorEvent::CloseDoc],
                    "x" => vec![EditorEvent::Save, EditorEvent::CloseDoc],
                    command if command.starts_with("e ") => vec![EditorEvent::OpenFile(Some(
                        PathBuf::from(command[2..].trim()),
                    ))],
                    _ => {
                        output.status = Some(format!("Unknown Vim command: :{command}"));
                        Vec::new()
                    }
                };
                output
            }
            KeyInput::Char(ch, modifiers)
                if !modifiers.ctrl && !modifiers.alt && !modifiers.command =>
            {
                if let Some(command) = self.vim.command_line.as_mut() {
                    command.push(*ch);
                }
                KeymapOutput::consumed()
            }
            _ => KeymapOutput::consumed(),
        }
    }

    fn handle_emacs(
        &mut self,
        input: &KeyInput,
        buffer: &dyn Buffer,
        _viewport_lines: usize,
    ) -> KeymapOutput {
        if input.modifiers().ctrl && input.modifiers().shift {
            return KeymapOutput::default();
        }
        if input.modifiers().ctrl && matches!(input, KeyInput::Char('g' | 'G', _)) {
            self.emacs.ctrl_x = false;
            self.emacs.mark = None;
            self.emacs.last_yank = None;
            self.emacs.last_was_kill = false;
            return KeymapOutput::event(EditorEvent::SetCursor {
                pos: buffer.cursor(),
            });
        }
        if self.emacs.ctrl_x {
            self.emacs.ctrl_x = false;
            return self.handle_emacs_ctrl_x(input, buffer);
        }
        let modifiers = input.modifiers();
        let is_kill = matches!(
            input,
            KeyInput::Char('k' | 'K' | 'w' | 'W', m) if m.ctrl
        ) || matches!(input, KeyInput::Char('d' | 'D', m) if m.alt)
            || matches!(input, KeyInput::Key(KeyCode::Backspace, m) if m.alt);
        let is_yank = matches!(input, KeyInput::Char('y' | 'Y', m) if m.ctrl || m.alt);
        if !is_kill {
            self.emacs.last_was_kill = false;
        }
        if !is_yank {
            self.emacs.last_yank = None;
        }
        if modifiers.ctrl && matches!(input, KeyInput::Char('x' | 'X', _)) {
            self.emacs.ctrl_x = true;
            return KeymapOutput::consumed();
        }
        if modifiers.ctrl {
            return match input {
                KeyInput::Char('f' | 'F', _) => self.emacs_move(buffer, Movement::Right),
                KeyInput::Char('b' | 'B', _) => self.emacs_move(buffer, Movement::Left),
                KeyInput::Char('n' | 'N', _) => self.emacs_move(buffer, Movement::Down),
                KeyInput::Char('p' | 'P', _) => self.emacs_move(buffer, Movement::Up),
                KeyInput::Char('a' | 'A', _) => self.emacs_move(buffer, Movement::LineStart),
                KeyInput::Char('e' | 'E', _) => self.emacs_move(buffer, Movement::LineEnd),
                KeyInput::Char('v' | 'V', _) => self.emacs_move(buffer, Movement::PageDown),
                KeyInput::Char('d' | 'D', _) => KeymapOutput::event(EditorEvent::DeleteRight),
                KeyInput::Char('k' | 'K', _) => self.emacs_kill_line(buffer),
                KeyInput::Char('o' | 'O', _) => {
                    let pos = buffer.cursor();
                    KeymapOutput {
                        consumed: true,
                        events: vec![
                            EditorEvent::Paste("\n".into()),
                            EditorEvent::SetCursor { pos },
                        ],
                        status: None,
                    }
                }
                KeyInput::Char('t' | 'T', _) => transpose_chars(buffer),
                KeyInput::Char('w' | 'W', _) => self.emacs_copy_or_kill(buffer, true),
                KeyInput::Char('y' | 'Y', _) => self.emacs_yank(buffer),
                KeyInput::Char('s' | 'S', _) => KeymapOutput::event(EditorEvent::FindOpen),
                KeyInput::Char('r' | 'R', _) => KeymapOutput::event(EditorEvent::FindOpenBackward),
                KeyInput::Char('/' | '_', _) => KeymapOutput::event(EditorEvent::Undo),
                KeyInput::Key(KeyCode::Space, _) | KeyInput::Char(' ', _) => {
                    self.emacs.mark = Some(buffer.cursor());
                    KeymapOutput::consumed()
                }
                _ => KeymapOutput::default(),
            };
        }
        if modifiers.alt {
            return match input {
                KeyInput::Char('f' | 'F', _) => self.emacs_move(buffer, Movement::WordRight),
                KeyInput::Char('b' | 'B', _) => self.emacs_move(buffer, Movement::WordLeft),
                KeyInput::Char('<', _) => self.emacs_move(buffer, Movement::DocumentStart),
                KeyInput::Char('>', _) => self.emacs_move(buffer, Movement::DocumentEnd),
                KeyInput::Char('v' | 'V', _) => self.emacs_move(buffer, Movement::PageUp),
                KeyInput::Char('d' | 'D', _) => self.emacs_kill_word(buffer, true),
                KeyInput::Char('w' | 'W', _) => self.emacs_copy_or_kill(buffer, false),
                KeyInput::Char('y' | 'Y', _) => self.emacs_yank_pop(buffer),
                KeyInput::Key(KeyCode::Backspace, _) => self.emacs_kill_word(buffer, false),
                _ => KeymapOutput::default(),
            };
        }
        self.emacs.last_was_kill = false;
        self.emacs.last_yank = None;
        KeymapOutput::default()
    }

    fn handle_emacs_ctrl_x(&mut self, input: &KeyInput, buffer: &dyn Buffer) -> KeymapOutput {
        match input {
            KeyInput::Char('s' | 'S', modifiers) if modifiers.ctrl => {
                KeymapOutput::event(EditorEvent::Save)
            }
            KeyInput::Char('w' | 'W', modifiers) if modifiers.ctrl => {
                KeymapOutput::event(EditorEvent::SaveAs(None))
            }
            KeyInput::Char('f' | 'F', modifiers) if modifiers.ctrl => {
                KeymapOutput::event(EditorEvent::OpenFile(None))
            }
            KeyInput::Char('c' | 'C', modifiers) if modifiers.ctrl => {
                KeymapOutput::event(EditorEvent::Quit)
            }
            KeyInput::Char('u' | 'U', _) => KeymapOutput::event(EditorEvent::Undo),
            KeyInput::Char('k' | 'K', _) => KeymapOutput::event(EditorEvent::CloseDoc),
            KeyInput::Key(KeyCode::Right, _) => KeymapOutput::event(EditorEvent::NextDoc),
            KeyInput::Key(KeyCode::Left, _) => KeymapOutput::event(EditorEvent::PrevDoc),
            KeyInput::Char('x' | 'X', modifiers) if modifiers.ctrl => {
                if let Some(mark) = self.emacs.mark {
                    // Exchange point and mark, preserving the mark so the
                    // chord can be repeated just like Emacs.
                    self.emacs.mark = Some(buffer.cursor());
                    KeymapOutput {
                        consumed: true,
                        events: vec![EditorEvent::SetCursor { pos: mark }],
                        status: None,
                    }
                } else {
                    KeymapOutput::consumed()
                }
            }
            _ => KeymapOutput {
                consumed: true,
                events: Vec::new(),
                status: Some("Undefined Emacs C-x prefix".into()),
            },
        }
    }

    fn emacs_move(&mut self, _buffer: &dyn Buffer, movement: Movement) -> KeymapOutput {
        let event = if self.emacs.mark.is_some() {
            EditorEvent::SelectExtend(movement)
        } else {
            EditorEvent::Move(movement)
        };
        self.emacs.last_was_kill = false;
        self.emacs.last_yank = None;
        KeymapOutput::event(event)
    }

    fn emacs_kill_line(&mut self, buffer: &dyn Buffer) -> KeymapOutput {
        let pos = buffer.cursor();
        let end = line_end(buffer, pos);
        let range = if pos == end && end < buffer.len() {
            pos..move_right_by_char(buffer, end)
        } else {
            pos..end
        };
        self.push_kill(buffer.slice(range.clone()).unwrap_or_default(), false);
        delete_range(range)
    }

    fn emacs_kill_word(&mut self, buffer: &dyn Buffer, forward: bool) -> KeymapOutput {
        let pos = buffer.cursor();
        let target = if forward {
            resolve_movement(buffer, pos, Movement::WordRight, 1)
        } else {
            resolve_movement(buffer, pos, Movement::WordLeft, 1)
        };
        let range = pos.min(target)..pos.max(target);
        self.push_kill(buffer.slice(range.clone()).unwrap_or_default(), !forward);
        delete_range(range)
    }

    fn emacs_copy_or_kill(&mut self, buffer: &dyn Buffer, kill: bool) -> KeymapOutput {
        let selection = buffer.selection();
        if selection.is_collapsed() {
            return KeymapOutput::consumed();
        }
        let range = selection.range();
        self.push_kill(buffer.slice(range.clone()).unwrap_or_default(), false);
        self.emacs.mark = None;
        if kill {
            delete_range(range)
        } else {
            KeymapOutput::event(EditorEvent::SetCursor {
                pos: buffer.cursor(),
            })
        }
    }

    fn emacs_yank(&mut self, buffer: &dyn Buffer) -> KeymapOutput {
        let Some(text) = self.emacs.kill_ring.first().cloned() else {
            return KeymapOutput::consumed();
        };
        let start = buffer.cursor();
        self.emacs.kill_index = 0;
        self.emacs.last_yank = Some((start, text.len()));
        self.emacs.last_was_kill = false;
        KeymapOutput::event(EditorEvent::Paste(text))
    }

    fn emacs_yank_pop(&mut self, _buffer: &dyn Buffer) -> KeymapOutput {
        let Some((start, old_len)) = self.emacs.last_yank else {
            return KeymapOutput::consumed();
        };
        if self.emacs.kill_ring.len() < 2 {
            return KeymapOutput::consumed();
        }
        self.emacs.kill_index = (self.emacs.kill_index + 1) % self.emacs.kill_ring.len();
        let text = self.emacs.kill_ring[self.emacs.kill_index].clone();
        self.emacs.last_yank = Some((start, text.len()));
        KeymapOutput {
            consumed: true,
            events: vec![
                EditorEvent::SetCursor { pos: start },
                EditorEvent::SelectExtendTo {
                    pos: start.saturating_add(old_len),
                },
                EditorEvent::Paste(text),
            ],
            status: None,
        }
    }

    fn push_kill(&mut self, text: String, prepend: bool) {
        if text.is_empty() {
            return;
        }
        if self.emacs.last_was_kill && !self.emacs.kill_ring.is_empty() {
            if prepend {
                self.emacs.kill_ring[0].insert_str(0, &text);
            } else {
                self.emacs.kill_ring[0].push_str(&text);
            }
        } else {
            self.emacs.kill_ring.insert(0, text);
            self.emacs.kill_ring.truncate(60);
        }
        self.emacs.kill_index = 0;
        self.emacs.last_yank = None;
        self.emacs.last_was_kill = true;
    }

    fn set_vim_register(&mut self, text: String, linewise: bool) {
        self.vim.register = VimRegister { text, linewise };
    }

    fn enter_vim_insert(&mut self, pos: Option<usize>) -> KeymapOutput {
        self.vim.mode = VimMode::Insert;
        let events = pos
            .map(|pos| vec![EditorEvent::SetCursor { pos }])
            .unwrap_or_default();
        KeymapOutput {
            consumed: true,
            events,
            status: None,
        }
    }

    fn take_vim_count(&mut self) -> usize {
        let count = self.vim.count.clamp(1, MAX_VIM_COUNT);
        self.vim.count = 0;
        count
    }

    fn clear_vim_pending(&mut self) {
        self.vim.operator = None;
        self.vim.operator_count = 0;
        self.vim.count = 0;
        self.vim.pending_g = false;
    }
}

fn operator_key(operator: VimOperator) -> char {
    match operator {
        VimOperator::Delete => 'd',
        VimOperator::Change => 'c',
        VimOperator::Yank => 'y',
    }
}

fn vim_char_motion(ch: char) -> Option<(Movement, bool)> {
    match ch {
        'h' => Some((Movement::Left, false)),
        'l' => Some((Movement::Right, false)),
        'j' => Some((Movement::Down, false)),
        'k' => Some((Movement::Up, false)),
        'w' => Some((Movement::WordRight, false)),
        'b' => Some((Movement::WordLeft, false)),
        '0' => Some((Movement::LineStart, false)),
        '$' => Some((Movement::LineEnd, false)),
        _ => None,
    }
}

fn resolve_movement(
    buffer: &dyn Buffer,
    pos: usize,
    movement: Movement,
    viewport_lines: usize,
) -> usize {
    match movement {
        Movement::Left => move_left_by_char(buffer, pos),
        Movement::Right => move_right_by_char(buffer, pos),
        Movement::WordLeft => skip_word_left(buffer, pos),
        Movement::WordRight => skip_word_right(buffer, pos),
        Movement::LineStart => line_start(buffer, pos),
        Movement::LineEnd => line_end(buffer, pos),
        Movement::DocumentStart => 0,
        Movement::DocumentEnd => buffer.len(),
        Movement::Up | Movement::Down | Movement::PageUp | Movement::PageDown => {
            let (line, col) = buffer.pos_to_linecol(pos).unwrap_or((0, 0));
            let delta = match movement {
                Movement::Up => -1,
                Movement::Down => 1,
                Movement::PageUp => -(viewport_lines.max(1) as isize),
                Movement::PageDown => viewport_lines.max(1) as isize,
                _ => 0,
            };
            let target_line = line
                .saturating_add_signed(delta)
                .min(buffer.line_count().saturating_sub(1));
            let text_len = buffer.line_text(target_line).map(|s| s.len()).unwrap_or(0);
            buffer
                .linecol_to_pos(target_line, col.min(text_len))
                .unwrap_or(pos)
        }
    }
}

fn skip_word_right(buffer: &dyn Buffer, mut pos: usize) -> usize {
    let len = buffer.len();
    if pos >= len {
        return len;
    }
    let class = char_after(buffer, pos).map(is_word).unwrap_or(false);
    while pos < len && char_after(buffer, pos).map(is_word).unwrap_or(false) == class {
        pos = move_right_by_char(buffer, pos);
    }
    while pos < len && !char_after(buffer, pos).map(is_word).unwrap_or(false) {
        pos = move_right_by_char(buffer, pos);
    }
    pos
}

fn skip_word_left(buffer: &dyn Buffer, mut pos: usize) -> usize {
    if pos == 0 {
        return 0;
    }
    pos = move_left_by_char(buffer, pos);
    while pos > 0 && !char_after(buffer, pos).map(is_word).unwrap_or(false) {
        pos = move_left_by_char(buffer, pos);
    }
    while pos > 0 {
        let previous = move_left_by_char(buffer, pos);
        if !char_after(buffer, previous).map(is_word).unwrap_or(false) {
            break;
        }
        pos = previous;
    }
    pos
}

fn word_end_right(buffer: &dyn Buffer, mut pos: usize) -> usize {
    let len = buffer.len();
    if pos >= len {
        return len;
    }
    if char_after(buffer, pos).map(is_word).unwrap_or(false) {
        pos = move_right_by_char(buffer, pos);
    }
    while pos < len && !char_after(buffer, pos).map(is_word).unwrap_or(false) {
        pos = move_right_by_char(buffer, pos);
    }
    while pos < len {
        let next = move_right_by_char(buffer, pos);
        if next >= len || !char_after(buffer, next).map(is_word).unwrap_or(false) {
            break;
        }
        pos = next;
    }
    pos
}

fn is_word(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

fn line_start(buffer: &dyn Buffer, pos: usize) -> usize {
    let line = buffer
        .pos_to_linecol(pos)
        .map(|(line, _)| line)
        .unwrap_or(0);
    buffer.line_byte_range(line).map(|r| r.start).unwrap_or(0)
}

fn first_non_whitespace(buffer: &dyn Buffer, pos: usize) -> usize {
    let line = buffer
        .pos_to_linecol(pos)
        .map(|(line, _)| line)
        .unwrap_or(0);
    let Some(range) = buffer.line_byte_range(line) else {
        return pos;
    };
    let offset = buffer
        .line_text(line)
        .and_then(|text| text.find(|ch: char| !ch.is_whitespace()))
        .unwrap_or(0);
    range.start + offset
}

fn line_end(buffer: &dyn Buffer, pos: usize) -> usize {
    let line = buffer
        .pos_to_linecol(pos)
        .map(|(line, _)| line)
        .unwrap_or(0);
    buffer
        .line_byte_range(line)
        .map(|range| range.end)
        .unwrap_or(buffer.len())
}

fn line_span(buffer: &dyn Buffer, pos: usize, count: usize) -> Range<usize> {
    let line = buffer
        .pos_to_linecol(pos)
        .map(|(line, _)| line)
        .unwrap_or(0);
    let end_line = line
        .saturating_add(count.saturating_sub(1))
        .min(buffer.line_count().saturating_sub(1));
    line_span_from_lines(buffer, line, end_line)
}

fn line_span_from_lines(buffer: &dyn Buffer, first: usize, second: usize) -> Range<usize> {
    let start_line = first.min(second);
    let end_line = first.max(second);
    let start = buffer
        .line_byte_range(start_line)
        .map(|r| r.start)
        .unwrap_or(0);
    let raw_end = buffer
        .line_byte_range(end_line)
        .map(|r| r.end)
        .unwrap_or(buffer.len());
    let end = if raw_end < buffer.len() {
        move_right_by_char(buffer, raw_end)
    } else {
        raw_end
    };
    start..end
}

fn char_span_right(buffer: &dyn Buffer, pos: usize, count: usize) -> Range<usize> {
    let mut end = pos;
    for _ in 0..count {
        end = move_right_by_char(buffer, end);
    }
    pos..end
}

fn char_span_left(buffer: &dyn Buffer, pos: usize, count: usize) -> Range<usize> {
    let mut start = pos;
    for _ in 0..count {
        start = move_left_by_char(buffer, start);
    }
    start..pos
}

fn select_range(range: Range<usize>) -> KeymapOutput {
    KeymapOutput {
        consumed: true,
        events: vec![
            EditorEvent::SetCursor { pos: range.start },
            EditorEvent::SelectExtendTo { pos: range.end },
        ],
        status: None,
    }
}

fn delete_range(range: Range<usize>) -> KeymapOutput {
    if range.is_empty() {
        return KeymapOutput::consumed();
    }
    KeymapOutput {
        consumed: true,
        events: vec![
            EditorEvent::SetCursor { pos: range.start },
            EditorEvent::SelectExtendTo { pos: range.end },
            EditorEvent::DeleteSelection,
        ],
        status: None,
    }
}

fn transpose_chars(buffer: &dyn Buffer) -> KeymapOutput {
    let pos = buffer.cursor();
    if pos == 0 || buffer.len() < 2 {
        return KeymapOutput::consumed();
    }
    let right_end = if pos == buffer.len() {
        pos
    } else {
        move_right_by_char(buffer, pos)
    };
    let middle = if pos == buffer.len() {
        move_left_by_char(buffer, pos)
    } else {
        pos
    };
    let left = move_left_by_char(buffer, middle);
    let Some(a) = char_after(buffer, left) else {
        return KeymapOutput::consumed();
    };
    let Some(b) = char_after(buffer, middle) else {
        return KeymapOutput::consumed();
    };
    KeymapOutput {
        consumed: true,
        events: vec![
            EditorEvent::SetCursor { pos: left },
            EditorEvent::SelectExtendTo { pos: right_end },
            EditorEvent::Paste(format!("{b}{a}")),
        ],
        status: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PieceTableBuffer;

    fn plain(ch: char) -> KeyInput {
        KeyInput::Char(ch, KeyModifiers::default())
    }

    fn ctrl(ch: char) -> KeyInput {
        KeyInput::Char(
            ch,
            KeyModifiers {
                ctrl: true,
                ..KeyModifiers::default()
            },
        )
    }

    fn alt(ch: char) -> KeyInput {
        KeyInput::Char(
            ch,
            KeyModifiers {
                alt: true,
                ..KeyModifiers::default()
            },
        )
    }

    #[test]
    fn existing_configs_default_to_standard() {
        assert_eq!(KeybindingMode::default(), KeybindingMode::Standard);
    }

    #[test]
    fn vim_transitions_and_counts_are_shared_state() {
        let buffer = PieceTableBuffer::from_bytes(b"one two three".to_vec());
        let mut map = KeymapState::new(KeybindingMode::Vim);
        assert_eq!(map.status_label(), "VIM NORMAL");
        map.handle(&plain('i'), &buffer, 20);
        assert_eq!(map.vim_mode(), Some(VimMode::Insert));
        map.handle(
            &KeyInput::Key(KeyCode::Escape, KeyModifiers::default()),
            &buffer,
            20,
        );
        map.handle(&plain('3'), &buffer, 20);
        let output = map.handle(&plain('w'), &buffer, 20);
        assert!(matches!(output.events.as_slice(), [EditorEvent::SetCursor { pos }] if *pos == 13));
    }

    #[test]
    fn vim_operator_captures_register_before_delete() {
        let buffer = PieceTableBuffer::from_bytes(b"one two".to_vec());
        let mut map = KeymapState::new(KeybindingMode::Vim);
        map.handle(&plain('d'), &buffer, 20);
        let output = map.handle(&plain('w'), &buffer, 20);
        assert_eq!(map.vim.register.text, "one ");
        assert!(matches!(
            output.events.last(),
            Some(EditorEvent::DeleteSelection)
        ));
    }

    #[test]
    fn vim_operator_and_motion_counts_multiply() {
        let buffer = PieceTableBuffer::from_bytes(b"one two three four five six seven".to_vec());
        let mut map = KeymapState::new(KeybindingMode::Vim);
        for ch in ['3', 'd', '2', 'w'] {
            let output = map.handle(&plain(ch), &buffer, 20);
            if ch == 'w' {
                assert_eq!(
                    map.vim_register(),
                    Some(("one two three four five six ", false))
                );
                assert!(matches!(
                    output.events.last(),
                    Some(EditorEvent::DeleteSelection)
                ));
            }
        }
    }

    #[test]
    fn vim_visual_line_yank_is_linewise_and_switch_resets_state() {
        let buffer = PieceTableBuffer::from_bytes(b"one\ntwo\n".to_vec());
        let mut map = KeymapState::new(KeybindingMode::Vim);
        map.handle(&plain('V'), &buffer, 20);
        // Simulate the frontend applying the selection emitted by V.
        let mut buffer = buffer;
        buffer.set_selection(crate::Selection { anchor: 0, head: 4 });
        map.handle(&plain('y'), &buffer, 20);
        assert_eq!(map.vim_register(), Some(("one\n", true)));
        map.set_mode(KeybindingMode::Emacs);
        assert_eq!(map.status_label(), "EMACS");
        assert_eq!(map.vim_register(), None);
    }

    #[test]
    fn vim_colon_commands_cover_file_lifecycle() {
        let buffer = PieceTableBuffer::new();
        let cases = [
            ("w", EditorEvent::Save),
            ("q", EditorEvent::CloseDoc),
            ("q!", EditorEvent::ForceCloseDoc),
        ];
        for (command, expected) in cases {
            let mut map = KeymapState::new(KeybindingMode::Vim);
            map.handle(&plain(':'), &buffer, 20);
            for ch in command.chars() {
                map.handle(&plain(ch), &buffer, 20);
            }
            let output = map.handle(
                &KeyInput::Key(KeyCode::Enter, KeyModifiers::default()),
                &buffer,
                20,
            );
            assert_eq!(output.events, vec![expected]);
        }
    }

    #[test]
    fn vim_adversarial_counts_are_bounded() {
        let buffer = PieceTableBuffer::from_bytes(b"x".to_vec());
        let mut map = KeymapState::new(KeybindingMode::Vim);
        for _ in 0..100 {
            map.handle(&plain('9'), &buffer, 20);
        }
        let output = map.handle(&plain('h'), &buffer, 20);
        assert_eq!(output.events, vec![EditorEvent::SetCursor { pos: 0 }]);

        map.set_vim_register("x".repeat(2_000), false);
        for _ in 0..100 {
            map.handle(&plain('9'), &buffer, 20);
        }
        let output = map.handle(&plain('p'), &buffer, 20);
        assert!(output.events.is_empty());
        assert_eq!(
            output.status.as_deref(),
            Some("Vim paste is limited to 16 MiB")
        );
    }

    #[test]
    fn vim_linewise_paste_preserves_unterminated_line_boundaries() {
        let buffer = PieceTableBuffer::from_bytes(b"one".to_vec());
        for (after, expected_pos, expected_text) in [(true, 3, "\none\n"), (false, 0, "one\n")] {
            let mut map = KeymapState::new(KeybindingMode::Vim);
            map.set_vim_register("one".into(), true);
            let output = map.vim_paste(after, &buffer);
            assert_eq!(
                output.events,
                vec![
                    EditorEvent::SetCursor { pos: expected_pos },
                    EditorEvent::Paste(expected_text.into()),
                ]
            );
        }

        let multi = PieceTableBuffer::from_bytes(b"first\nlast".to_vec());
        let mut map = KeymapState::new(KeybindingMode::Vim);
        map.set_vim_register("last".into(), true);
        assert_eq!(
            map.vim_paste(true, &multi).events,
            vec![
                EditorEvent::SetCursor { pos: 6 },
                EditorEvent::Paste("last\n".into()),
            ]
        );

        let mut counted = KeymapState::new(KeybindingMode::Vim);
        counted.set_vim_register("one".into(), true);
        counted.vim.count = 3;
        assert_eq!(
            counted.vim_paste(true, &buffer).events,
            vec![
                EditorEvent::SetCursor { pos: 3 },
                EditorEvent::Paste("\none\none\none\n".into()),
            ]
        );

        let mut terminated = KeymapState::new(KeybindingMode::Vim);
        terminated.set_vim_register("one\n".into(), true);
        assert_eq!(
            terminated.vim_paste(true, &buffer).events,
            vec![
                EditorEvent::SetCursor { pos: 3 },
                EditorEvent::Paste("\none\n".into()),
            ]
        );
    }

    #[test]
    fn shifted_control_chords_fall_through_for_application_shortcuts() {
        let buffer = PieceTableBuffer::new();
        let input = KeyInput::Char(
            'P',
            KeyModifiers {
                ctrl: true,
                shift: true,
                command: true,
                ..KeyModifiers::default()
            },
        );
        for mode in [KeybindingMode::Vim, KeybindingMode::Emacs] {
            assert!(!KeymapState::new(mode).handle(&input, &buffer, 20).consumed);
        }
    }

    #[test]
    fn backward_search_reverses_repeat_direction() {
        let buffer = PieceTableBuffer::new();
        let mut vim = KeymapState::new(KeybindingMode::Vim);
        assert_eq!(
            vim.handle(&plain('?'), &buffer, 20).events,
            vec![EditorEvent::FindOpenBackward]
        );
        assert_eq!(
            vim.handle(&plain('n'), &buffer, 20).events,
            vec![EditorEvent::FindPrev]
        );
        assert_eq!(
            vim.handle(&plain('N'), &buffer, 20).events,
            vec![EditorEvent::FindNext]
        );

        let mut emacs = KeymapState::new(KeybindingMode::Emacs);
        assert_eq!(
            emacs.handle(&ctrl('r'), &buffer, 20).events,
            vec![EditorEvent::FindOpenBackward]
        );
    }

    #[test]
    fn emacs_prefix_waits_and_emits_save() {
        let buffer = PieceTableBuffer::from_bytes(Vec::new());
        let mut map = KeymapState::new(KeybindingMode::Emacs);
        assert!(map.handle(&ctrl('x'), &buffer, 20).consumed);
        assert_eq!(map.status_label(), "EMACS C-x");
        let output = map.handle(&ctrl('s'), &buffer, 20);
        assert_eq!(output.events, vec![EditorEvent::Save]);
        assert_eq!(map.status_label(), "EMACS");
    }

    #[test]
    fn emacs_cancel_clears_mark_and_pending_prefix() {
        let buffer = PieceTableBuffer::from_bytes(b"text".to_vec());
        let mut map = KeymapState::new(KeybindingMode::Emacs);
        map.handle(
            &KeyInput::Key(
                KeyCode::Space,
                KeyModifiers {
                    ctrl: true,
                    ..KeyModifiers::default()
                },
            ),
            &buffer,
            20,
        );
        map.handle(&ctrl('x'), &buffer, 20);
        assert_eq!(map.status_label(), "EMACS C-x");
        map.handle(&ctrl('g'), &buffer, 20);
        assert_eq!(map.status_label(), "EMACS");
    }

    #[test]
    fn emacs_movement_and_editing_bindings_emit_shared_events() {
        let buffer = PieceTableBuffer::from_bytes("one βeta".as_bytes().to_vec());
        let cases = [
            (ctrl('f'), EditorEvent::Move(Movement::Right)),
            (ctrl('b'), EditorEvent::Move(Movement::Left)),
            (ctrl('n'), EditorEvent::Move(Movement::Down)),
            (ctrl('p'), EditorEvent::Move(Movement::Up)),
            (alt('f'), EditorEvent::Move(Movement::WordRight)),
            (alt('b'), EditorEvent::Move(Movement::WordLeft)),
            (ctrl('d'), EditorEvent::DeleteRight),
        ];
        for (input, expected) in cases {
            let mut map = KeymapState::new(KeybindingMode::Emacs);
            assert_eq!(map.handle(&input, &buffer, 20).events, vec![expected]);
        }
    }

    #[test]
    fn emacs_consecutive_kills_coalesce() {
        let buffer = PieceTableBuffer::from_bytes(b"one two three".to_vec());
        let mut map = KeymapState::new(KeybindingMode::Emacs);
        map.handle(&alt('d'), &buffer, 20);
        // The real frontend has deleted "one ", leaving the next kill at
        // the same point. Use that resulting buffer to verify coalescing.
        let second = PieceTableBuffer::from_bytes(b"two three".to_vec());
        map.handle(&alt('d'), &second, 20);
        assert_eq!(map.emacs_kill_ring().unwrap()[0], "one two ");
    }

    #[test]
    fn emacs_kill_ring_is_bounded() {
        let mut map = KeymapState::new(KeybindingMode::Emacs);
        for index in 0..70 {
            map.emacs.last_was_kill = false;
            map.push_kill(index.to_string(), false);
        }
        assert_eq!(map.emacs.kill_ring.len(), 60);
    }
}
