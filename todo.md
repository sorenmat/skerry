# the_editor — Roadmap to world-class

Prioritized plan based on the current feature set and the biggest gaps next to modern editors (VS Code, Zed, Sublime).

## Phase 1 — Editing power (biggest daily impact)

- [ ] Multi-cursor / multiple selections
- [ ] Auto-pairing brackets and quotes
- [ ] Auto-indent on `{`, `}`, `Enter`
- [ ] Smart Home (toggle column 0 ↔ first non-whitespace)
- [ ] Block / column selection (Alt+drag or middle-click)
- [ ] Bracket matching highlight

## Phase 2 — Language smarts (the "world-class" leap)

- [ ] LSP client integration
  - [ ] Autocomplete
  - [ ] Go to definition
  - [ ] Hover documentation
  - [ ] Diagnostics / error squiggles
  - [ ] Rename symbol
  - [ ] Format on save
- [ ] Tree-sitter parser for accurate highlighting and local-variable colors
- [ ] Basic symbol outline / breadcrumbs

## Phase 3 — Search & project workflow

- [ ] Project-wide regex search
- [ ] Find bar options: match case, whole word, regex
- [ ] Keep project-search window open while jumping results
- [ ] Search result highlight navigation

## Phase 4 — UI/UX polish

- [ ] Split editor panes (side-by-side / stacked)
- [ ] Drag-and-drop tab reorder / drag to split
- [ ] Minimap
- [ ] Indent guides
- [ ] Inline git blame
- [ ] Git diff / merge-conflict view
- [ ] Configurable keybindings and settings file
- [ ] Load custom `.tmTheme` and UI theme files

## Phase 5 — Conveniences

- [ ] Snippets
- [ ] Auto-save
- [ ] Integrated terminal panel
- [ ] TUI soft-wrap rendering parity

## What "done" means

For each item:
1. Implementation lands in `core` or the relevant frontend(s).
2. Both GUI and TUI have parity unless the feature is GUI-only by nature.
3. Unit/integration tests added.
4. `features.md` updated.
5. `cargo test --workspace` passes.
