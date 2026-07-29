# the_editor — Roadmap to world-class

Prioritized plan based on the current feature set and the biggest gaps next to modern editors (VS Code, Zed, Sublime).

## Phase 1 — Editing power (biggest daily impact)

- [x] Multi-cursor / multiple selections
- [x] Auto-pairing brackets and quotes
- [x] Auto-indent on `{`, `}`, `Enter`
- [x] Smart Home (toggle column 0 ↔ first non-whitespace)
- [x] Block / column selection (Alt+drag or middle-click)
- [x] Bracket matching highlight

## Phase 2 — Language smarts (the "world-class" leap)

- [x] LSP client integration
  - [x] Autocomplete
  - [x] Go to definition
  - [x] Hover documentation
  - [x] Diagnostics / error squiggles
  - [x] Rename symbol
  - [x] Format on save
- [x] Tree-sitter parser for accurate highlighting and local-variable colors
- [x] Basic symbol outline / breadcrumbs
- [x] External formatters (gofmt, rustfmt, prettier) as LSP fallback

## Phase 3 — Search & project workflow

- [x] Project-wide regex search
- [x] Find bar options: match case, whole word, regex
- [ ] Keep project-search window open while jumping results
- [ ] Search result highlight navigation
- [x] Find in selection

## Phase 4 — UI/UX polish

- [ ] Split editor panes (side-by-side / stacked)
- [ ] Drag-and-drop tab reorder / drag to split
- [x] Minimap
- [x] Indent guides
- [x] Inline git blame
- [ ] Git diff / merge-conflict view
- [ ] Configurable keybindings and settings file
- [ ] Load custom `.tmTheme` and UI theme files
- [x] Code folding
- [x] Comment toggle (Cmd/Ctrl+/)

## Phase 5 — Conveniences

- [x] Snippets
- [x] Auto-save
- [ ] Integrated terminal panel
- [ ] TUI soft-wrap rendering parity

## What "done" means

For each item:
1. Implementation lands in `core` or the relevant frontend(s).
2. Both GUI and TUI have parity unless the feature is GUI-only by nature.
3. Unit/integration tests added.
4. `features.md` updated.
5. `cargo test --workspace` passes.
