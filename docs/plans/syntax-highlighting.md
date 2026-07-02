# Syntax Highlighting (v1)

> Plan updated 2026-06-29. Implementation now uses [syntect](https://crates.io/crates/syntect) for 200+ languages and built-in theme support.

## Goal

Render buffer text with semantic colors for every language syntect supports. Keeps every existing feature intact (find-match highlight, selection rect, cursor-line background). Both TUI and GUI see the same color segments (ADR 0005).

Out of v1 scope (intentional): incremental tokenization, user-switchable themes (one hardcoded dark theme for now), custom `.tmTheme` loading, tree-sitter or LSP-driven semantic coloring.

## Design at a glance

| Layer | Lives in | Responsibility |
| --- | --- | --- |
| Language defs + active theme | `core::SyntaxEngine` | Wraps syntect's `SyntaxSet` and `Theme`; created once at startup |
| Per-line colored segments | `core::syntax::ColorSegment` | Byte range + RGBA color from the theme |
| Per-line highlight cache | `Document` (existing field) | `HashMap<usize, Vec<ColorSegment>>`; lazy, invalidated on edit |
| Render-time walk | Frontends | Walk segments and convert syntect `Color` to native frontend color |

Key choices:

1. **syntect, not hand-written tokenizers.** It ships 200+ `.sublime-syntax` definitions and `.tmTheme` files. The old hand-written Rust/Markdown tokenizers were removed.
2. **Cache lives on `Document`, not on the frontend.** Same rationale as before — a doc-level cache survives tab switches and is shared by both frontends.
3. **Per-line, stateless highlighting.** Each visible line is highlighted independently so scrolling doesn't require sequential line access. Multi-line constructs (block comments spanning lines) won't carry state across lines in v1.

## `core::syntax` API

```rust
pub struct ColorSegment {
    pub range: std::ops::Range<usize>,  // byte offsets into the line text
    pub color: syntect::highlighting::Color,
}

pub struct SyntaxCache {
    pub lines: HashMap<usize, Vec<ColorSegment>>,
    pub dirty: bool,
}

pub struct SyntaxEngine {
    syntax_set: SyntaxSet,
    theme: Theme,
}

impl SyntaxEngine {
    pub fn default_dark() -> Self;
    pub fn syntax_for_path(&self, path: Option<&Path>) -> Option<&SyntaxReference>;
    pub fn highlight_line(&self, syntax: &SyntaxReference, line: &str) -> Vec<ColorSegment>;
}
```

The active theme is currently hardcoded to `base16-ocean.dark` from syntect's bundled defaults.

## Size gate

`SYNTAX_SIZE_LIMIT: usize = 2 * 1024 * 1024` — files above 2 MB skip syntax highlighting entirely. Same rationale as before: multi-GB log files and generated dumps shouldn't hang the editor.

## Frontend integration

Both frontends follow the same pattern:

1. Own a `SyntaxEngine` (created in `new_with_documents`).
2. Per visible line, call `get_syntax_segments(app, line_idx, &line_text)` which consults the per-document `SyntaxCache` and falls back to `SyntaxEngine::highlight_line` on cache miss.
3. Render each `ColorSegment` in its theme color; gaps (if any) use the default text color.

Precedence is unchanged:

- Selection > match highlights > syntax colors > default text
- Cursor line background is drawn underneath everything

Edit invalidation already happens in `handle_event` for any buffer-mutating event.

## What is implemented now

- `core/src/syntax.rs` — `SyntaxEngine`, `SyntaxCache`, `ColorSegment`, `SYNTAX_SIZE_LIMIT`, syntect integration, unit tests.
- `core/src/lib.rs` — re-exports the new syntax types.
- `frontend_gui/src/app.rs` — `EditorApp` owns a `SyntaxEngine`.
- `frontend_gui/src/ui.rs` — `get_syntax_segments` + per-line segment rendering in egui.
- `frontend_tui/src/app.rs` — `App` owns a `SyntaxEngine`.
- `frontend_tui/src/ui.rs` — `get_syntax_segments` + `push_syntax_spans` for ratatui.
- Pure-Rust regex backend (`regex-fancy`) to avoid C FFI / Oniguruma build dependencies.

## What is still missing for full "theming support"

The current implementation gives us **multi-language highlighting** and a single hardcoded dark theme. To make theming a real feature, these pieces are still needed:

1. **Theme selection.** Today only `base16-ocean.dark` is loaded. We need:
   - A list of bundled themes to choose from (`ThemeSet::load_defaults()` includes several).
   - An `EditorEvent` such as `CycleTheme` / `SetTheme` plus key bindings.
   - The active theme stored on `SyntaxEngine` and exposed so the status bar can show it.

2. **Custom theme files.** The `plist-load` feature is already enabled, so we can load `.tmTheme` files at runtime. We need:
   - A command/event to load a theme from a path.
   - Error handling for missing / malformed theme files.
   - A place to persist the user's last-chosen theme (e.g. a settings file — not built yet).

3. **Light/dark coordination.** The GUI (egui) and TUI each have their own default background color. A theme's background color is currently ignored; we only use the foreground syntax colors. If we want the editor chrome to adapt, we'd need to feed the theme's background to the frontend's visual style.

4. **Status-bar indicator.** Show the active language + theme in the status line, e.g. `Rust | base16-ocean.dark`.

5. **Theme-aware default text color.** Right now "plain" text segments use the frontend's default text color rather than the theme's plain foreground. This is usually fine but can make some themes look off.

## Testing

Run:

```bash
cargo test -p core --lib      # syntax engine unit tests
cargo test -p frontend_tui    # TUI render tests
cargo test -p frontend_gui    # GUI render tests
cargo clippy --workspace
```

## Files touched

- `core/src/syntax.rs`
- `core/src/lib.rs`
- `core/Cargo.toml`
- `frontend_gui/src/app.rs`
- `frontend_gui/src/ui.rs`
- `frontend_tui/src/app.rs`
- `frontend_tui/src/ui.rs`
- `docs/plans/syntax-highlighting.md` — this doc
