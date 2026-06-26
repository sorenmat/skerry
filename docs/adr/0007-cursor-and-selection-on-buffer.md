# Cursor and selection live on Buffer

Cursor (byte offset) and selection (`Selection { anchor, head }`) are owned by `Buffer`, not by `Document` or `Frontend`. `Document` becomes a thin handle around `Buffer` plus file path plus per-view scroll state.

## Why

- ADR 0004 already locks cursor tracking on the undo path. Keeping cursor on `Buffer` means undo restores text + cursor atomically with no coordination between `Document` and `Buffer`.
- Cursor and selection are part of "what's being edited", not "how it's being viewed". Scroll is view state and lives elsewhere.
- Full frontend parity (ADR 0005) requires both TUI and GUI to read the same cursor/selection. Owning it on `Buffer` makes that automatic.
- Putting cursor on `Frontend` would silently break undo parity: GUI undo would restore the GUI's cursor but not the TUI's, even when both are editing the same document.

## Considered Options

- **On Document** — rejected for now. Cleaner separation (text vs. view state), more abstraction, and would let `Buffer` be reused outside this editor. None of those benefits justify the extra layer for v0.1. Revisit if a non-editor consumer of `Buffer` ever materialises.
- **On Frontend** — rejected. Breaks undo parity (see above) and breaks ADR 0004.

## Consequences

- `Buffer::cursor()`, `Buffer::set_cursor()`, `Buffer::selection()`, `Buffer::set_selection()` are part of the trait.
- `Document` is `Buffer + file_path + scroll_state`. No own cursor/selection.
- `Selection` is a struct with two byte offsets. Single selection for v0.1; multi-cursor deferred behind the trait (cannot be added without a trait revision).
- The trait shape sketched earlier (cursor-only, no selection) is now obsolete. The synthesized trait below replaces it.