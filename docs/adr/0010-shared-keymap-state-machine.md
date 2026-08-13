# ADR 0010: Use one shared keymap state machine

## Status

Accepted.

## Context

Skerry has first-class GUI and TUI frontends (ADR 0005). Vim and Emacs
bindings are stateful: Vim interprets modes, counts, and operators across
keystrokes, while Emacs interprets prefixes, marks, and a kill ring. Repeating
those state machines in egui and crossterm would make behavior drift and would
force every bug fix to be implemented twice.

Native frontend events also differ. The GUI distinguishes text input from key
events for IME support, while the terminal reports crossterm key events. The
editor still needs one definition of what a binding means.

## Decision

Core owns a frontend-neutral `KeyInput`, modifier model, and `KeymapState`.
Each frontend normalizes its native input, gives dialogs and text overlays the
first opportunity to consume it, then asks the shared state machine to emit
ordinary `EditorEvent`s. Unrecognized input falls through to the existing
frontend shortcut translation.

The selected `KeybindingMode` is application-global and persisted. Modal and
prefix state is process-local. Vim's unnamed register and Emacs's kill ring are
application-global but intentionally independent of the system clipboard.
Switching presets clears pending operations, marks, prefixes, and multi-cursor
selection, and Vim always begins in Normal mode.

The first release provides fixed Standard, Vim, and Emacs presets. User-defined
remapping is outside this decision and can be layered over normalized input in
a later ADR.

## Consequences

- GUI and TUI receive the same mode transitions and editing events for the same
  normalized key sequence.
- Keymap logic can be tested without either rendering framework.
- Frontends remain responsible for input ownership, platform clipboard
  shortcuts, visual status, and caret presentation.
- Adding an event is justified only when existing `EditorEvent` composition
  cannot express a mode-safe action, such as force-closing a dirty document.
- Full Vim or Emacs emulation is not implied; supported bindings are an
  explicit product surface documented in `features.md`.
