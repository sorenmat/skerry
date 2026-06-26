# Linear undo with text + cursor tracking

Undo is a linear stack (single redo branch, new edits clear the redo stack). Each undo entry records the buffer delta and the cursor byte offset. Selection state is not tracked in v0.1 — the frontend owns selection; undo only restores the cursor.

## Why

- Piece Table's append-only delta gives linear undo nearly for free.
- Tracking the cursor byte offset is one extra `usize` per entry. It avoids the UX-hostile "cursor jumps to byte 0 after undo" of text-only undo, at trivial cost.
- Skipping selection tracking avoids forcing a `Selection` type into the `Buffer` trait API before either frontend needs it. Can be added later without breaking changes.

## Consequences

- `Buffer::undo()` restores both text and cursor atomically.
- An edit-group concept (`begin_edit_group` / `end_edit_group`) is required so that a paste of N characters or a multi-key command collapses into one undo step, not N keystrokes.
- Tree undo is explicitly deferred — pull it in if anyone actually asks, don't pay the UX-design cost up front.