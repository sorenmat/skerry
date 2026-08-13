# Skerry

A dual-frontend (GUI + TUI) text editor in Rust targeting Linux and macOS. Designed for mixed workloads in a single session: source files (1KB–100KB) and multi-GB files, with edits required in both regimes and no user-visible mode switch.

The installed macOS application is named **Skerry**. Its Homebrew-provided
command-line shortcut is **`sky`**; the packaged Rust executable remains
`skerry` internally.

On Linux, Homebrew installs release binaries instead of a cask: `skerry`,
`skerry-tui`, and the same `sky` shortcut to the GUI executable.

## Language

**Buffer**: An in-memory representation of a document being edited. Implements the `Buffer` trait; primary impl is a Piece Table. Holds no rendering state.

**Document**: A file path plus a Buffer plus view state (cursor, scroll). The Buffer is the source of truth for text; view state lives in the frontend.

**Frontend**: A presentation layer (GUI or TUI) that reads from a Buffer and forwards input back to it. Holds no independent text state — every character on screen is fetched from the Buffer.

**Core**: The UI-agnostic crate containing the `Buffer` trait, the Piece Table impl, and the public text API. Both frontends depend on it; it depends on neither.

**Piece Table**: A buffer structure using a base source (typically the original file, possibly mmap'd), an append-only edit buffer (the "delta"), and a descriptor array of `(source, start, length)` triples that compose the visible text. O(1) insert/delete anywhere; undo is naturally append-only.

**Delta**: The append-only edit buffer of a Piece Table — the writable layer between the (read-only) base source and the visible text. Carries all unsaved changes; reads of unmodified regions pass through to the base source.

**Position**: A location within a Buffer, expressed as a UTF-8 byte offset. Line/column pairs are derived via the line index, never stored as the primary form.

**Line index**: A side structure that maps byte offsets to `(line, column)` pairs and back, in O(log n) or amortised O(1). Required because naive byte↔line conversion is O(n) and would break the multi-GB latency promise.

**Undo entry**: A single step in the linear undo stack. Records the buffer delta and the cursor byte offset. Text + cursor only; selection is not tracked.

**Edit group**: A sequence of edits that collapse into one undo entry. Used for multi-character pastes and multi-key commands so the user undoes one logical action, not N keystrokes.

**Frontend**: One of two presentation layers (TUI via ratatui/crossterm, GUI via egui/eframe). Both consume the same `Buffer`/`Document` contract; both ship from day one with full feature parity.

**Document**: A `Buffer` plus file path plus per-view state (scroll position, viewport size). Cursor and selection live on `Buffer`, not on `Document` or `Frontend`.

**FrontendRenderer**: Trait that abstracts the rendering backend (currently egui; future raw-wgpu slot reserved). GUI behaviour is renderer-agnostic; concrete rendering is renderer-specific.

**EditorEvent**: Frontend-agnostic input event type that both TUI (crossterm) and GUI (egui/winit) translate into. Core handles input identically regardless of frontend.

**Keybinding preset**: One of the persistent, application-global `Standard`,
`Vim`, or `Emacs` input conventions. A preset chooses how normalized keys are
interpreted; it does not change document contents or the system clipboard.

**Keymap state**: Process-local modal or prefix state shared by both frontends,
including Vim mode/count/operator/register state and Emacs prefix/mark/kill-ring
state. Changing presets resets this transient state and collapses multi-cursors.

**Unnamed register**: Vim's application-global characterwise or linewise text
slot. Deletes and yanks replace it; `p` and `P` read it. It is deliberately
separate from the system clipboard.

**Kill ring**: Emacs's application-global, ordered collection of up to 60 text
kills. Consecutive compatible kills coalesce; `C-y` yanks and `M-y` rotates it.

**Gutter annotation**: Compact line metadata rendered beside the line number.
It can combine a Git change marker, inline Git blame ownership, and an LSP
diagnostic stripe. In the GUI, gutter hover expands every annotation affecting
that row into one tooltip; it does not issue a new LSP hover request.

**Selection**: A range on the `Buffer` represented as `Selection { anchor: usize, head: usize }`, both UTF-8 byte offsets. Single selection in v0.1; multi-cursor deferred.

**Workload (Mixed)**: The editor must handle source files (1KB–100KB) and multi-GB files in the same session with no mode switch. Optimised for the common (small-file) case without breaking the tail.

## Flagged ambiguities

- **"Buffer" vs "Document"**: the plan uses "Buffer" loosely for both the data structure and the in-memory state of a file. We distinguish them: the *Piece Table* is the data structure; the *Buffer* is the trait that wraps it; the *Document* is the file-plus-buffer-plus-view bundle.
