# Skerry — Implemented Features

> Snapshot of every feature currently shipped in Skerry. Every entry here is
> real, running code —
> no roadmap items, no aspirational checkboxes. Anything that exists
> only as a state field without visible behaviour is flagged.

## At a glance

Skerry is a dual-frontend text editor in Rust: a **GUI** (egui +
eframe) and a **TUI** (ratatui + crossterm) sharing the same `core`
engine. Both ship from day one with full feature parity (ADR 0005).
The engine is built around a **Piece Table** buffer backed by an
append-only edit log, so it scales from 1 KB source files to
multi-GB log files without a mode switch.

Workspace layout:

```
Skerry/
├── core/        # UI-agnostic text engine (Buffer, Document, search, undo)
├── skerry/      # egui + eframe GUI
├── skerry-tui/  # ratatui + crossterm TUI
└── docs/adr/    # architectural decision records
```

## Core engine (`core` crate)

- **`Buffer` trait** — UI-agnostic text manipulation contract. Both
  frontends operate against this; neither owns text state
  independently.
- **Piece Table** — base source (memory-mapped for files) plus an
  append-only edit buffer (the "delta") plus a descriptor array.
  O(1) insert / delete anywhere; undo is naturally append-only
  (ADR 0001).
- **Memory-mapped loading** for multi-GB files. The base source is
  the file itself via `memmap2`; unsaved edits land in the delta
  (ADR 0002).
- **Byte-primary positions** — addresses are UTF-8 byte offsets.
  Line / column pairs are derived through a side index, never stored
  as the primary form (ADR 0003).
- **Line index** — O(log n) (amortised O(1)) byte ↔ line/column
  conversion so multi-GB files don't break the latency promise.
- **Linear undo stack** with **edit groups**. Each undo step records
  text + cursor position; selection is intentionally not tracked
  (ADR 0004). `begin_edit_group` / `end_edit_group` collapse
  multi-keystroke actions (paste, line ops) into one undo entry.
- **`Document` type** — `Buffer` + file path + per-document view
  state. Owns its buffer exclusively; closing a `Document` drops it.
- **`ViewState`** — per-document scroll position (vertical +
  horizontal), cursor-history marker, scroll-margin, indent mode,
  tab width, soft-wrap toggle. Tab switches preserve each doc's
  state.
- **`Search`** — literal substring search via `memchr::memmem`
  (SIMD-accelerated). Lazy windowed match list, capped at
  `MAX_STORED_MATCHES = 10_000`. Tracks current match index;
  supports next / prev with wrap-around.
- **Frontend-agnostic `EditorEvent`** — the entire input surface is
  one enum consumed identically by both frontends (ADR 0005).
- **`Selection` model** — `anchor` + `head` byte offsets. Collapsed
  means `anchor == head`. Single selection in v1; multi-cursor
  deferred.

## Multi-buffer / tabs

- **`Vec<Document>` + active index.** Switching tabs preserves each
  document's cursor, scroll, indent mode, and scroll-margin.
- **Tab strip UI** in both frontends. Filename + dirty marker per
  tab, active tab highlighted with a coloured background. Click an
  inactive tab to focus it.
- **New document** (Cmd/Ctrl+T) — opens a fresh unsaved buffer.
- **Close document** (Cmd/Ctrl+W) — closes the active document.
  Triggers the close-confirm dialog if the buffer is dirty.
- **Next / previous document** (Cmd/Ctrl+Tab / Cmd/Ctrl+Shift+Tab) —
  wrap-around cycling through the open documents.
- **Active-file reveal** — opening a file or switching documents expands its
  ancestors in the project tree, selects it, and scrolls it into view in both
  frontends.
- **Go to line** (Cmd/Ctrl+G) — open a prompt, type a 1-based line
  number, and jump the cursor to the start of that line. Out-of-range
  numbers are clamped to the first or last line.
- **Fuzzy file finder** (Cmd/Ctrl+P) — quick-open any file under the
  active document's project root. Type a few characters; results are
  scored by ordered case-insensitive matches with bonuses for word
  starts, consecutive matches, and exact case. Recently-opened files
  are included when no project is active. Up/Down moves selection,
  Enter opens, Esc closes. Also reachable from the command palette.
- **Git gutter** — live, per-line change indicators relative to
  `HEAD`. Added lines, modified lines, and deleted-line blocks are
  shown in the gutter of both frontends. The gutter refreshes
  automatically ~500 ms after the user stops typing, plus on save,
  reload, and external file changes. A status-bar summary shows
  `+A ~M -D` counts. `NextHunk` / `PrevHunk` (`Ctrl+Shift+Down` /
  `Ctrl+Shift+Up`) jump the cursor between changed regions. Toggle
  per-document from the command palette; disabled when the file is not
  in a git repo.
- **Inline git blame** — per-line commit metadata (short hash, author,
  relative time) shown in a dedicated gutter column. Shells out to
  `git blame --line-porcelain HEAD`; debounced refresh (~500 ms after
  idle). Toggle via the command palette (off by default to keep the
  gutter uncluttered). Disabled for files over 5 MB and for untracked /
  non-repo files.

## Markdown

- Markdown source files (`.md` and `.markdown`) receive shared Tree-sitter
  syntax highlighting in both frontends, covering block structure and inline
  constructs such as headings, lists, quotes, code, emphasis, and links.
- Opening a `.md` or `.markdown` file exposes **Source**, **Split**, and
  **Preview** modes through a status-bar switcher beside the settings cog.
  The selected mode is persisted as the default and can also be changed in
  the Settings window's Markdown section.
- Split and preview render directly from the active in-memory buffer, so
  unsaved edits are reflected immediately. The split divider is resizable;
  Preview is read-only, while Source and Split remain editable.
- Parsed previews are cached by document identity and buffer revision, so an
  unchanged document is neither reconstructed nor reparsed on every GUI frame.
- Preview uses a centered, readable-width document column with increased line
  height, stronger heading hierarchy, hanging list markers, and distinct code
  and quotation surfaces.
- CommonMark headings, paragraphs, emphasis, strikethrough, inline and fenced
  code, ordered and unordered lists, task lists, block quotes, tables, rules,
  footnotes, links, and image references receive native GUI formatting.
- Preview is deliberately local and inert: embedded HTML is shown as text,
  image references are shown as alt text plus their URL, and no remote content
  is fetched or executed.
- The command palette includes **Cycle Markdown preview**. In the TUI the same
  command leaves the source view unchanged and reports that preview is GUI-only.

## CSV

- Opening a `.csv` file exposes **Source** and **Table** modes through a
  status-bar switcher beside the settings cog. The selected mode is persisted
  as the default and can also be changed in the Settings window's CSV section.
- Table mode parses RFC 4180-style quoting, escaped delimiters, and multiline
  fields from the active in-memory buffer, with the first record rendered as
  resizable column headings.
- The table is read-only, horizontally scrollable, and vertically virtualized
  so only visible rows are rendered. Parse errors are shown inline, and Source
  remains available for editing.
- Preview work is bounded to 32 MiB of source, 50,000 rows, 256 columns,
  1,000,000 stored cells, 16 MiB of displayed text, and 4,096 characters per
  displayed cell to keep pathological files responsive.

## Editing

- **Character insertion** — printable keys, paste (Cmd/Ctrl+V), Enter
  (newline), Tab (indent per doc mode).
- **Selection-aware editing** — every insert, paste, and Tab replaces
  a non-collapsed selection instead of inserting at the cursor
  head. Standard 1995-era editor behaviour.
- **Multi-cursor** — Cmd/Ctrl+D selects the next occurrence of the
  current word; Alt/Opt+click adds a cursor; Alt+drag selects a
  rectangular (column) region with a cursor per line. Typing, deleting,
  and moving all operate on every cursor simultaneously. Escape
  collapses to a single cursor.
- **Auto-pairing** — typing `(`, `[`, `{`, `"`, `'` auto-inserts the
  closing pair and leaves the cursor between them. Typing a closing
  char when the cursor is before its match skips over instead of
  doubling. Backspace between an empty pair deletes both.
- **Auto-indent** — Enter copies the current line's leading whitespace
  and adds one indent level if the line ends with `{`, `(`, `[`, or `=>`.
- **Smart Home** — Home toggles between column 0 and the first
  non-whitespace character.
- **Comment toggle** — Cmd/Ctrl+/ toggles line comments on the selected
  lines. Per-language syntax: `//` for Rust/C/Go/JS/TS, `#` for
  Python/TOML.
- **Code folding** — click ▶ in the gutter to fold a block; ▼ to
  unfold. Foldable ranges discovered via tree-sitter parse tree.
- **Snippets** — Tab-triggered text templates defined in config.json.
  `$0` marks the final cursor position; `${1:default}` inserts
  placeholder text.
- **Bracket match highlight** — cursor next to a bracket highlights
  both brackets. Handles nesting depth.
- **Indent guides** — thin vertical lines at each indent level.
- **Delete-left** (Backspace) and **delete-right** (Delete). Both
  selection-aware.
- **Delete-word-left** (Ctrl+Backspace) and **delete-word-right**
  (Ctrl+Delete). Word boundaries = char-class transitions between
  word chars (alphanumeric + `_`) and non-word chars.
- **Delete-line** (Cmd/Ctrl+K) — removes the line including its
  trailing newline; the next line shifts up.
- **Duplicate-line** (Shift+Cmd/Ctrl+D) — copies the current line below.
- **Move-line up / down** (Alt+Up / Alt+Down) — swaps the current
  line with its neighbour. Single undo entry.
- **Select all** (Cmd/Ctrl+A).

## Clipboard

- **Copy** (Cmd/Ctrl+C) — selection text to the OS clipboard.
  No-op when the selection is collapsed.
- **Cut** (Cmd/Ctrl+X) — selection text to the OS clipboard, then
  the selection is deleted.
- **Paste** (Cmd/Ctrl+V) — OS clipboard text inserted at the cursor,
  replacing any selection.
- TUI clipboard goes through `arboard`; GUI clipboard goes through
  `egui::Context::copy_text`. Both initialise lazily so the editor
  doesn't fail to start if the platform clipboard isn't available.

## Navigation

- **Arrow keys** — char-wise movement. Shift+Arrow extends the
  selection (Shift+Up / Shift+Down on TUI confirmed).
- **Word movement** (Ctrl+Left / Ctrl+Right) — Ctrl+Arrow on TUI;
  plain Ctrl+Arrow on GUI is the same word-move event.
- **Line start / end** (Home / End).
- **Page up / down** (PageUp / PageDown) — page size derived from
  the current viewport height.
- **Document start / end** (Ctrl+Home / Ctrl+End).
- **Horizontal scroll** (Shift+Left / Shift+Right) — scrolls the
  viewport one column at a time.
- **GUI: Shift+wheel** — horizontal scroll. egui's ScrollArea eats
  the wheel for vertical scroll; the modifier hijacks it to scroll
  horizontally using `char_width`.
- **TUI: mouse click** — `SetCursor` at the click position,
  collapses the selection.
- **TUI: mouse drag** — `SelectExtendTo` at the drag position.

## Vertical scrolling & auto-follow

- **GUI auto-scroll to cursor** — when the cursor leaves the
  viewport, the ScrollArea follows it. Custom implementation (not
  egui's `scroll_to_rect`) because that one accumulates extra delta
  per press.
- **Edge-stick semantics** — when the cursor crosses the last
  visible row, the view scrolls down by exactly one line per press.
  Same for upward.
- **Emacs `scroll-margin`** — per-document configurable margin (in
  lines). When the cursor enters the margin of the viewport's edge,
  the view pre-emptively scrolls so `margin` rows of buffer stay
  visible above / below the cursor. Lets you keep pressing Down near
  the bottom without the view jumping at the last row.
- **Scroll-margin fallback** — when `2 * margin + 1 > viewport_height`,
  the margin can't fit. Falls back to legacy edge-stick (scroll only
  on actual viewport exit). Same fallback in both frontends.
- **Scroll-margin default = 0** (legacy edge-stick). Set
  `view.scroll_margin_lines = 3` to opt into Emacs behaviour.
- **TUI viewport adjustment** — `App::adjust_viewport` runs each
  frame, keeping the cursor within the configured scroll-margin of
  the top / bottom rows.
- **Per-doc vertical scroll** — switching tabs preserves each doc's
  `scroll_top_line`. The GUI re-syncs its egui ScrollArea offset
  to this on tab switch so opening a doc doesn't reset its scroll.

## Find & replace

- **Find bar** (Cmd/Ctrl+F) — appears above the status bar.
  Incremental: matches refresh as the user types. Bar persists
  across multiple searches so the user can refine without re-opening.
- **Find Next** (Enter in the find bar) — jumps to the next match
  after the cursor; wraps to the first match at end of buffer.
- **Find Prev** (Shift+Enter) — previous match; wraps to the last
  match at start.
- **Match highlighting** — every match in the buffer is highlighted
  in the text. The currently-active match is rendered in a brighter
  amber so the eye lands on it at a glance.
- **Match precedence** — selection preempts find-match highlight so
  the user can see their drag without matches painted over it.
  Same rule in both frontends.
- **Replace bar** (Cmd/Ctrl+R) — second row under the find bar.
  Coupled with find: closing find also closes replace; opening
  replace auto-opens find too.
- **Replace One** (Enter in replace bar) — replaces the current
  match with the replacement, then advances to the next match.
  Single undo entry per replace.
- **Replace All** (Shift+Enter in replace bar) — replaces every
  match, wrapped in one edit group so the whole batch is a single
  undo step. Iterates in reverse so earlier byte offsets stay valid
  as later ones are replaced.
- **Replace guards** — refuses to operate when the find query is
  empty, when the replacement is empty (refuses to silently
  delete), or when there's no current match. Reports via status bar.
- **Regex mode** — the `.*` button toggles regex search. Capture-group
  expansion (`$1`, `$2`) is supported in replace.
- **Case-sensitive** — the `Aa` button toggles case-sensitive matching.
  Default off (case-insensitive).
- **Whole-word** — the `W` button restricts matches to whole words
  (surrounded by non-word characters or boundaries).
- **Find in selection** — opening the find bar with a multi-line
  selection scopes the search to that selection's byte range.

## Project-wide find & replace

- **Project search** (Cmd/Ctrl+Shift+F) — searches all files under the
  active document's project root. Results show file path, line number,
  and the matching line. Enter opens the selected result.
- **Project replace preview** — typing a replacement shows a live
  preview of every line that will change. The preview reports both the
  number of affected lines and the total number of occurrences (so
  multiple matches on the same line are counted correctly).
- **Replace-all confirmation** — `Ctrl+Enter` does not replace
  immediately; it switches to a confirmation prompt showing the total
  occurrences and affected files. Confirm with `Enter`/`Y`, cancel with
  `Esc`/`N`.
- **Replace guards** — refuses to operate when the find or replacement
  query is empty, or when the preview is empty.
- **Open document reload** — after a confirmed replace, any open files
  that were modified are reloaded from disk so the buffer content stays
  in sync. Dirty open files are marked stale instead of being silently
  overwritten.

## Language Server Protocol (LSP)

- **`core::lsp::LspManager`** — synchronous, frontend-agnostic manager
  that owns a Tokio runtime and stdio language-server processes.
  Frontends call `poll()` once per frame; the manager hides all
  async I/O behind a blocking API.
- **JSON-RPC stdio transport** — full header/body framing, request/response
  routing, and notification handling in `core::lsp`.
- **Per-(root, language) server process** — language servers are started
  on demand. Currently wired: `rust-analyzer` (Rust), `gopls` (Go),
  `typescript-language-server --stdio` (JavaScript / TypeScript / JSX /
  TSX), `pylsp` (Python), and `clangd` (C / C++). Adding another server
  is a one-line change in `server_command`.
- **Document lifecycle** — `textDocument/didOpen`, debounced
  `textDocument/didChange` (full-text, 300 ms), `textDocument/didSave`,
  and `textDocument/didClose` keep the server in sync with the buffer.
- **Diagnostics** — `textDocument/publishDiagnostics` is rendered as
  colored underlines in the GUI and as `E`/`W`/`I`/`H` severity markers
  in the TUI gutter.
- **Completions** — `Ctrl+Space` fires `textDocument/completion`. A
  popup appears in both frontends; Up/Down navigate, Enter/Tab insert
  the selected item, Esc closes.
- **Hover** — `Shift+K` requests `textDocument/hover`. The GUI shows a
  tooltip near the pointer; the TUI shows the first line of the hover
  text in the status bar.
- **Go to definition** — `F12` requests `textDocument/definition`. If
  the target is in the current file, the cursor jumps to it; otherwise
  the editor opens the target file and then jumps. The jump is ignored
  if the cursor moved while the response was in flight.
- **macOS Command-click to definition** — holding `Cmd` in the GUI
  underlines the identifier under the pointer and turns the cursor into
  a pointing hand; clicking the underlined token requests
  `textDocument/definition` and jumps to the result.
- **LSP status indicator** — the status bar shows a filled circle
  (e.g. `● rust-analyzer`) when a language server is running and an empty
  circle (e.g. `○ rust-analyzer`) when the server failed to start.
- **Rename symbol** (F2) — `textDocument/rename`. A prompt pre-filled
  with the current word appears; Enter dispatches the rename and applies
  the resulting `WorkspaceEdit` to the buffer.
- **Format on save** — when saving, if the LSP server supports
  `documentFormattingProvider`, the editor formats the document and
  re-saves the formatted content.
- **Go to symbol** (Shift+Cmd/Ctrl+O) — queries
  `textDocument/documentSymbol` and shows a filterable symbol picker.
  Click or Enter to jump. Handles both hierarchical and flat formats.

## External formatting

- **Configurable formatters** — when no LSP server supports formatting,
  the editor falls back to configurable external tools (gofmt, rustfmt,
  prettier, etc.). Configured per-language in `config.json` under
  `formatters`. The formatter reads from stdin and writes to stdout.
  Defaults: rust→`rustfmt --emit stdout`, go→`gofmt`, python→`ruff format -`.
- **Format document** — the command palette's "Format document" runs
  the LSP formatter or the external fallback on demand.

## Minimap

- **Zoomed-out document overview** — toggle via command palette. Shows
  colored rects per syntax token at 2px per line. A viewport highlight
  rect indicates the current scroll position. Click/drag to scroll.
  Disabled for files over 5000 lines (performance guard).

## File I/O

- **Open via CLI args** — `skerry path1 path2` and
  `skerry-tui path1 path2` open one document per PATH. With no
  paths, opens a single empty document.
- **Open via dialog** (Cmd/Ctrl+O) — text-input dialog, Enter
  submits, Esc cancels. Ctrl/Alt-modified keys are filtered out so
  control chars don't pollute the path.
- **Memory-mapped loading** — existing files are loaded via
  `PieceTableBuffer::from_path` (mmap'd base source).
- **New-file path** — a path that doesn't exist yet opens as an
  empty buffer with the path remembered; the next Save writes there.
- **Save** (Cmd/Ctrl+S) — writes the buffer back to its
  `source_path`. Reports "Saved." or "Save error: ..." in the status
  bar.
- **New document** (Cmd/Ctrl+T) — opens a fresh empty buffer.

## Close-on-dirty

- **Close-confirm prompt** — triggered when the user closes a
  document with unsaved edits. Three choices: Save / Discard /
  Cancel.
- **Tab navigation** — Tab / Shift+Tab / Left / Right cycle the
  focused choice. Vim-style `h` / `l` also cycle on the TUI.
- **Enter** confirms the focused choice.
- **`y`** is a one-key shortcut for "Discard" on both frontends.
- **`n` / Esc** cancel.
- Closing the last document always quits (regardless of dirty
  state).

## Modal prompts (shared model)

Both frontends implement the same modal intercept pattern: a modal
"up" state (`Some(...)`) routes incoming keys through a
`dispatch_modal_*` function before they reach normal key
translation. Stray keystrokes don't reach the buffer while a modal
is open.

| Modal | Trigger | Keys |
| --- | --- | --- |
| Find bar | Cmd/Ctrl+F | Esc closes; printable chars / Backspace edit; Enter = next; Shift+Enter = prev |
| Replace bar | Cmd/Ctrl+R | Esc closes (find bar stays); Enter = replace one; Shift+Enter = replace all; printable chars / Backspace edit |
| Close-confirm | dirty close | Tab / arrows cycle; Enter confirms; `y` discards; `n` / Esc cancel |
| Open-file dialog | Cmd/Ctrl+O | printable chars append; Backspace pops; Enter submits; Esc cancels |
| Go-to-line dialog | Cmd/Ctrl+G | digits append; Backspace pops; Enter jumps; Esc cancels |
| Fuzzy file finder | Cmd/Ctrl+P | printable chars filter; Backspace pops; Up/Down move; Enter opens; Esc closes |
| Git gutter | (always visible when enabled) | updates live; `Ctrl+Shift+Up/Down` jumps hunks; toggle via command palette |

## Display & rendering

- **Header / tab strip** — single-doc mode shows the filename + a
  dirty marker (`[+]`). Multi-doc mode becomes a tab strip with all
  open docs, active tab highlighted, click to switch.
- **Gutter with line numbers** — right-aligned to the maximum
  digit count (minimum 2). Separator character `│` between number
  and text.
- **Cursor line background** — dim background behind the line the
  cursor is on.
- **Selection rectangle** — semi-transparent overlay over the
  selected byte range. Three-segment text drawing (before / inside
  / after) eliminates sub-pixel ghosting artifacts.
- **Find-match highlight** — amber rectangles for every match;
  current match in a brighter shade. Suppressed when a selection is
  active.
- **Syntax highlighting** — powered by [tree-sitter](https://tree-sitter.github.io/);
  incremental parsing with per-document parse trees reparsed on edits
  via `InputEdit`, and viewport-scoped highlight queries
  (`QueryCursor::set_byte_range`) so only visible lines are tokenised.
  Languages: Rust, Go, JavaScript/TypeScript (incl. TSX/JSX), Python,
  C/C++, JSON. Files over 32 MB skip the tree to keep load fast;
  queries carry a 15 ms cancellation budget so a pathological region
  can't stall a frame.
- **Settings window** — the GUI status bar keeps a cog button in its
  lower-right corner, preceded by a presentation switcher for Markdown and
  CSV documents. The cog opens a dedicated settings surface for
  coordinated interface and syntax themes, indentation, wrapping, scroll margin,
  caret animation, git annotations, auto-save, project-tree visibility,
  Markdown presentation, and keyboard-shortcut help. Changes apply
  immediately and persist in the Skerry config.
- **Expanded built-in themes** — the GUI includes Dark, Light, One Dark,
  Fjord Night, Aubergine, Sandstone, and High Contrast palettes, each pairing
  interface chrome with syntax colors selected for the same background.
  The shared highlighter also retains Gruvbox Dark for the TUI. F5 cycles
  complete themes in the GUI and syntax colors in the TUI; changing colors
  invalidates the highlight cache so they update immediately.
- **Status bar** — single line at the bottom showing the latest
  status message and the cursor position `L{line}:{col} / L{total}`.
- **GUI: monospace 14 pt font**, 2 px caret. Tab characters render
  with a 4× char-width advance (per egui's default monospace
  behaviour — see ADR-context below).
- **TUI: ANSI color spans via ratatui.** Selection renders as
  reverse-video. Match highlights get distinct fg/bg styles.
- **Tab advance accounting** — both renderers use per-glyph advance
  widths (`char_width` for normal chars, `tab_width` ≈ 4× `char_width`
  for `\t`) when computing text x positions, so segments after a tab
  don't drift right. Fixes a real bug we hit — without it,
  selection-after-tab text overlapped the selection rectangle.

## macOS installation and file associations

- **`make app-bundle`** builds the relocatable `target/Skerry.app`. The
  app contains the release `skerry` executable and an AppleScript entry
  point that handles macOS "open document" events.
- **Homebrew** installs the app as **Skerry** on macOS and as Linux binaries
  on Linux. Both packages provide the **`sky`** command. See `INSTALL.md` for
  the tap and install commands.
- **`make register-app`** registers the bundle with Launch Services as
  the default editor for `.rs`, `.go`, and `.json` files.
- Double-clicking one of those files in Finder opens it in the GUI
  frontend. The wrapper forwards the file path to the binary as a
  command-line argument. `sky PATH...` opens the same GUI from a shell.

## Per-document settings

Every setting below lives on `Document::view` so different docs can
use different conventions without re-configuring.

- **`use_spaces`** — true = Tab inserts spaces; false = Tab inserts
  a literal `\t`. Default: `true`.
- **`tab_width`** — number of spaces per indent level when
  `use_spaces` is true. Clamped to 1..=16. Default: 4.
- **`Cycle indent mode** (Cmd/Ctrl+I) — walks through
  `spaces:2 → spaces:4 → spaces:8 → tabs → spaces:2`. Other widths
  collapse to the first preset so the cycle always lands somewhere
  sane. Reports the new mode in the status bar.
- **`scroll_margin_lines`** — see Vertical scrolling above.
  Default: 0.
- **`soft_wrap`** — true = long lines wrap on multiple visual rows
  without inserting newlines; false = horizontal scroll.
  - **GUI**: render behaviour implemented.
  - **TUI**: state is set and reported ("Soft-wrap: on / off
    (horizontal scroll)"), but the renderer does not yet produce
    wrapped visual rows. Toggle is wired through; the renderer is
    deferred. *(planned: parity with GUI; tracked but not yet
    shipped)*
- **`scroll_x_cols`** — per-doc horizontal scroll offset.
- **`scroll_top_line`** — per-doc vertical scroll offset.
- **`git_gutter_enabled`** — whether the git gutter is shown for
  this document. Default comes from `config.git_gutter` (`true`).
  Toggle at runtime with the "Toggle git gutter" command.
- **`last_seen_cursor`** — tracks the last cursor position the
  renderer observed. Used by the GUI to detect "cursor actually
  moved" so auto-scroll fires only on fresh motion (not on every
  frame, and not when tab-switching to a doc whose cursor happens
  to be at a position the renderer just rendered).

## GUI-only (`skerry`)

- **Backend: egui 0.30 + eframe 0.30**, immediate-mode GUI.
- **Default window**: 800 × 600, title "Skerry".
- **Swappable renderer** — GUI behaviour is renderer-agnostic
  (ADR 0006). The concrete renderer is egui today; a future raw-wgpu
  slot is reserved.
- **Click + drag selection** — left mouse sets cursor (collapses
  selection) or extends it during drag.
- **Shift+wheel horizontal scroll** — egui's ScrollArea eats wheel
  for vertical scroll; the shift modifier hijacks it.
- **Offscreen render tests** — `egui::Context::run` with
  `RawInput::screen_rect` lets tests inspect paint commands
  (`output.shapes`) without a display server. Used to reproduce and
  fix the tab-advance selection bug.

## TUI-only (`skerry-tui`)

- **Backend: ratatui 0.29 + crossterm 0.28**.
- **Mouse capture enabled** at startup so crossterm delivers
  `MouseEvent`s for click / drag.
- **arboard** for cross-platform OS clipboard access (X11 / Wayland
  / Cocoa / Win32). Lazy initialisation — clipboard failures don't
  prevent the editor from starting.
- **Panic-safe terminal restore** — `TerminalGuard` (Drop impl)
  restores the terminal even if a panic happens inside the event
  loop. Without this, a panic leaves the user's shell in raw mode.
- **Terminal resize handled implicitly** — ratatui recomputes
  layout on the next render; no manual work.
- **`TestBackend`-based smoke tests** — `ui_tests.rs` renders into
  ratatui's `TestBackend` and asserts on the produced buffer text.

## Engineering / non-feature properties

- **Three-crate Cargo workspace**: `core`, `skerry-tui`,
  `skerry`. Frontends depend on `core`; `core` depends on
  neither.
- **Rust 1.75**, edition 2021. `unsafe_code = "deny"` workspace-wide.
- **9 ADRs** in `docs/adr/` document the architectural decisions
  (Piece Table, mmap+delta, byte positions, linear undo, full
  frontend parity, swappable GUI renderer, cursor/selection on
  Buffer, product naming, and the unsigned v0.1.2 release exception).
- **Unit tests in every crate** — `cargo test --workspace`.
- **Integration render tests**:
  - `skerry/tests/auto_scroll.rs` — offscreen egui render
    tests for the auto-scroll path.
  - `skerry/tests/render_repro.rs` — reproduces the
    tab-advance selection bug and asserts on paint commands.
  - `skerry-tui/src/ui_tests.rs` — ratatui TestBackend smoke
    tests covering header, dirty marker, status bar, tab strip, find
    match highlight, etc.

## Known limitations (shipped today)

- **No custom theme files.** The bundled interface and tree-sitter themes
  can be selected at runtime, but loading a user theme file is not
  implemented.
- **TUI soft-wrap rendering is deferred** — the toggle changes
  state and the status bar reports the new value, but long lines do
  not yet wrap visually in the TUI. The state is preserved
  per-document so the implementation can land later without losing
  user settings.
- **Single selection only.** Multi-cursor is explicitly out of v1
  scope (CONTEXT.md).
- **Project-wide search is literal only.** Single-file search supports
  regex; project-wide search still uses `memmem` substring matching.
- **LSP is opt-in per language.** Rust, Go, JavaScript/TypeScript,
  Python, and C/C++ are wired. Other languages need a one-line addition
  in `core::lsp::manager::server_command` plus an extension mapping in
  `core::Document::language_id`.
- **LSP sync is full-text.** Every debounced change sends the whole
  document, which is fine for source files but not optimised for
  multi-GB files.
- **LSP columns use character columns.** The editor maps its internal
  character columns directly to LSP positions; languages with complex
  Unicode or non-UTF-8 content may see minor position drift.
- **Search matches capped at 10 000.** Beyond that, the oldest
  matches drop off. For 1 KB–100 KB source files this is more than
  enough; multi-GB log files are not the target use case for find.
