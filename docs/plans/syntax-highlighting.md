# Syntax Highlighting (v1)

> Plan authored 2026-06-29. Scope: token-based syntax coloring for both
> frontends, v1 ships with **Rust + Markdown + plain-text fallback**.

## Goal

Render buffer text with semantic colors (keywords, strings, comments,
numbers, types, functions) instead of a single foreground color. Keeps
every existing feature intact (find-match highlight, selection rect,
cursor-line background). Both TUI and GUI see the same token stream
(ADR 0005).

Out of v1 scope (intentional): incremental tokenization, languages
beyond Rust + Markdown, user-configurable themes, tree-sitter or other
external parsers, LSP-driven semantic coloring.

## Design at a glance

| Layer | Lives in | Responsibility |
| --- | --- | --- |
| Token data + tokenizers | `core::syntax` (new module) | Pure functions `&[u8] -> Vec<Token>`; dispatcher picks by extension |
| TokenKind → color palette | Frontends | Each frontend maps `TokenKind` to its own theme color |
| Per-line token cache | `Document` (new field) | `HashMap<usize /*line*/, Vec<Token>>`; lazy, invalidated on edit |
| Render-time walk | Frontends | Same shape as the existing match-highlight walker |

Two key choices to lock in:

1. **`core::syntax` is stateless.** It doesn't know about the buffer,
   about edits, or about caching. Pure functions make it trivial to
   unit-test and reuse (LSP, status bar, anything).
2. **Cache lives on `Document`, not on the frontend.** Tabs hold
   different docs in different languages — a doc-level cache survives
   tab switches correctly, and both frontends share the same shape via
   the `Document` type already defined in `core`.

## Token types

```rust
pub struct Token {
    pub range: std::ops::Range<usize>,  // byte offsets into the line text
    pub kind: TokenKind,
}

pub enum TokenKind {
    Keyword,    // `fn`, `let`, `if`, `pub`, ...
    Type,       // `i32`, `Vec<T>`, `String`, user types
    Function,   // call sites + definitions (best-effort)
    String,     // "..." / r#"..."# / 'c'
    Comment,    // // ...  /* ... */
    Number,     // 42, 0xFF, 1.0e-3, 0b1010
    Punctuation,// brackets, operators — usually default-colored
    Identifier, // default foreground
}
```

Punctuation and Identifier exist so the renderer can use them as
"plain" segments without an `Option`. The frontend is free to ignore
either (e.g. just paint them with the default text color).

## Tokenizer: Rust

Handwritten single-pass scanner. State machine tracks:

- `Normal` — looking for the next interesting token
- `LineComment` — until `\n`
- `BlockComment { depth: usize }` — Rust nests, so we track depth
- `String { terminated: bool }` — handle `\"`, `\\`, and `\` line continuation
- `Char` — single-quoted, similar to String but 1-char body
- `RawString { hashes: usize }` — `r#"..."#`

Identifier scan in `Normal` consumes `[A-Za-z_][A-Za-z0-9_]*` and
classifies via two static sets:

- `KEYWORDS` — `as, break, const, continue, crate, else, enum, extern,
  false, fn, for, if, impl, in, let, loop, match, mod, move, mut, pub,
  ref, return, self, Self, static, struct, super, trait, true, type,
  unsafe, use, where, while, async, await, dyn, box, do, final, macro,
  override, priv, try, typeof, unsized, virtual, yield`
- `PRIMITIVE_TYPES` — `i8 i16 i32 i64 i128 isize u8 u16 u32 u64 u128
  usize f32 f64 bool char str String Vec Option Result Box Rc Arc`
  (small fixed list; full coverage deferred)

Numbers consume `[0-9][0-9a-fA-F_.oxb]*` with light validation —
malformed input falls back to emitting a `Number` token of whatever
the scanner ate, which is fine for highlighting purposes.

`Function` classification is a deliberate punt for v1: any identifier
followed by `(` that isn't a keyword or primitive type is marked
`Function`. False positives in generic-arg positions are acceptable
v1 trade-off.

Algorithm is O(n) in the input bytes, no per-char allocation, no
regex engine — should run well under 5 ms for a 100 KB Rust file on
modern hardware.

## Tokenizer: Markdown

Line-oriented (each line starts in `Normal`):

| Pattern at line start | Emit |
| --- | --- |
| `#{1,6} ` | `Keyword` over the `#` prefix |
| `> ` | `Keyword` over the `>` |
| `[-*+] ` | `Keyword` over the marker |
| `\d+\. ` | `Keyword` over the number + dot |
| `` ` `` ... `` ` `` | `String` over the code span (single line only) |
| `**...**` or `*...*` | `Keyword` over the asterisks |

After handling the line-start pattern, scan for inline `**...**`,
`*...*`, `` `...` ``, and `[text](url)` — bracket text → `Type`, URL
→ `String`. Unmatched markers fall through to `Identifier`.

Code fences (``` ``` ``` blocks) toggle a "code block" state — the
contents render as `String` until the closing fence. v1 treats any
line with only whitespace + backticks as a fence boundary.

## Dispatcher

```rust
pub fn tokenize_for_path(path: &Path, bytes: &[u8]) -> Vec<Token>;
```

Extension map:

| Extension(s) | Tokenizer |
| --- | --- |
| `rs` | `tokenize_rust` |
| `md`, `markdown` | `tokenize_markdown` |
| anything else (incl. no extension) | passthrough — returns an empty `Vec` |

The passthrough is **not** `Vec::new()` — to keep the frontend
unaware of the dispatcher logic, the passthrough returns one
`Identifier`-range token covering the whole input. Frontends that
don't care about syntax just treat `Identifier` and `Punctuation` as
default-colored.

Actually, simpler: passthrough returns `Vec::new()`. Frontends check
"is there a cache for this document?" and treat empty cache as
"no syntax highlighting." Frontend handles this naturally already
for unsaved / non-matching files. **Going with empty Vec.**

## Per-document cache

Add to `Document`:

```rust
pub struct Document {
    pub buffer: Box<dyn Buffer>,
    pub view: ViewState,
    pub syntax: SyntaxCache,        // new
}

pub struct SyntaxCache {
    /// Tokens per line. Empty when syntax highlighting is disabled
    /// (file too large, no matching extension, or dispatcher returned
    /// nothing).
    pub lines: HashMap<usize, Vec<Token>>,
    /// Set to `true` when the buffer has been edited since the cache
    /// was last populated. Frontend re-tokenizes affected lines on
    /// next render.
    pub dirty: bool,
}
```

**Invalidation**: on every `EditorEvent` that mutates the buffer
(insert, delete, replace, undo, redo), set `dirty = true`. No need
to be clever about which lines are affected — the cache is
**lazy**: lines are computed only when the renderer asks for them, and
when `dirty` is true the affected lines are re-tokenized.

The simplest correct version: **on every edit, drop the entire cache.**
Re-tokenize cost on next render is amortized over the visible lines
only (the renderer only asks for tokens on lines it's drawing). For
files under the size gate, this is well under frame budget.

When `dirty` is true and a line is requested, the renderer asks
`tokenize_for_path(doc.path(), &line_bytes)` and stores the result.

## Size gate

`SYNTAX_SIZE_LIMIT: usize = 2 * 1024 * 1024` — files above 2 MB skip
syntax highlighting entirely. The Document's `syntax.lines` stays
empty. Rationale: a 2 MB+ Rust file is a generated code dump or a
binary blob, not something a user wants colored. Multi-GB log files
definitely don't. Avoids the "I opened a 100 MB log and now the
editor hangs on every keystroke" failure mode that would otherwise be
the natural v1.5 upgrade path.

The gate is checked once at cache-population time. The constant lives
in `core::syntax`; the frontend just reads it.

## Frontend integration — GUI (`frontend_gui`)

In `ui.rs::render_text`, inside the per-line loop, after computing
`line_text`:

1. If `doc.syntax.lines.get(line_idx)` is `Some` and not stale → use
   it.
2. Otherwise, if `doc.syntax.dirty` or the cache misses → tokenize
   the line's bytes via `core::syntax::tokenize_for_path(doc.path(),
   &line_bytes_as_bytes)`, store in `doc.syntax.lines`, return.
3. If `doc.buffer.len() > SYNTAX_SIZE_LIMIT` → skip entirely.

When tokens exist, walk the line in the same "plain / styled /
plain / styled" shape as the existing `match_highlights` walker. For
each token, compute its char-col range via `byte_to_char_col` (already
imported and used by the match-highlight code), draw a colored text
segment.

**Precedence** (matches existing rules in `ui.rs`):
- Selection > tokens (selection wins visually; tokens suppressed on
  selected ranges so the user sees their drag without colored
  characters underneath)
- Match highlights > tokens (search results win so the user sees what
  they're navigating to)
- Cursor line background < tokens (background draws first, tokens
  draw on top of it)

**Edit invalidation hook**: in `EditorApp::handle_event`, set
`doc.syntax.dirty = true` for any event that mutates the buffer
(`Insert`, `InsertTab`, `DeleteLeft`, `DeleteRight`,
`DeleteSelection`, `DeleteWordLeft`, `DeleteWordRight`, `DeleteLine`,
`DuplicateLine`, `MoveLineUp`, `MoveLineDown`, `Paste`, `Undo`,
`Redo`, `OpenFile`, `NewDoc`, `CloseDoc`).

## Frontend integration — TUI (`frontend_tui`)

Mirror of the GUI plan. `render_content` (in `ui.rs`) per-line:
tokenize via the same path, walk segments as `Span`s with `Style`
mapping `TokenKind` → `Color`. Same precedence rules. Same edit
invalidation hook in `App::handle_event`.

The existing TUI render path already walks per-line with a
`push_highlight_spans` helper for match highlights — the syntax
variant should mirror that shape (`push_syntax_spans`) to keep the
file readable.

## Testing

### Core unit tests (`core/src/syntax.rs`)

- **Rust** — hand-picked snippets covering:
  - Plain identifier + keyword (`let x = 42;` → `Keyword` over `let`,
    `Number` over `42`)
  - Line comment (`// hello` → `Comment` over whole line)
  - Block comment, including nested (`/* /* inner */ */`)
  - String with escapes (`"a\"b"` → `String` over whole literal)
  - Char literal (`'x'`, `'\n'`)
  - Raw string (`r#"x"#`)
  - Number variants (`0xFF`, `1.0e-3`, `0b1010`)
  - Function call (`foo(bar)` → `Function` over `foo`)
  - No-extension file → empty `Vec`
- **Markdown** — hand-picked snippets covering:
  - H1, H2, H3 (`#`, `##`, `###`)
  - Bullet list (`- item`, `* item`)
  - Ordered list (`1. item`)
  - Inline code (`` `code` ``)
  - Bold + italic (`**bold**`, `*italic*`)
  - Link (`[text](url)`)
  - Code fence (``` ```rust ... ``` ```)
- **Dispatcher** — `.rs` → Rust, `.md` → Markdown, `.txt` → empty,
  no extension → empty.

### Frontend integration tests

- **GUI** (`frontend_gui/tests/`): offscreen render of a 3-line Rust
  snippet, assert that `output.shapes` contains text with non-default
  colors for at least one keyword. Skip if running headless fails
  (already proven workable by `render_repro.rs`).
- **TUI** (`frontend_tui/src/ui_tests.rs`): render a Rust snippet,
  inspect the produced `Buffer` for ANSI color codes on keyword
  spans. The existing tests render plain text — extend the same
  pattern to inspect styled spans.

Existing `ui_tests.rs` tests must still pass unchanged (plain text
files still render plain; only `.rs`/`.md` get new colors).

## Performance budget

- 100 KB Rust file, full tokenize: < 5 ms (measured on M-series, the
  primary target).
- 100 KB Markdown file: < 3 ms (line-oriented, cheaper).
- 2 MB+ file: 0 ms (gate skips).
- Per-frame render overhead for syntax walk: ~`O(visible_lines *
  tokens_per_line)`. Typical is < 50 tokens per visible line; well
  under 1 ms.

If real-world perf on a 1 MB Rust file exceeds 16 ms (one frame), the
mitigation is to debounce the cache population: only re-tokenize
visible lines if the last tokenize was > 200 ms ago. v1 skips this
optimization — measure first.

## Implementation order (small commits)

1. `core::syntax` module skeleton: `Token`, `TokenKind`, empty
   `tokenize_for_path` returning `Vec::new()`. Re-export from
   `lib.rs`. No tests yet pass.
2. Rust tokenizer + unit tests.
3. Markdown tokenizer + unit tests.
4. Dispatcher wired in (`tokenize_for_path` returns the right
   tokenizer's output).
5. `Document::syntax` field added (struct + ctor).
6. GUI integration: cache + render hook + one integration test.
7. TUI integration: cache + render hook + one integration test.
8. Polish: status bar mention of language (`Rust` / `Markdown` /
   `text`); editor_event for `ToggleSyntaxHighlight` (deferred — not
   in v1 unless trivial).

Each commit compiles + tests pass independently. The GUI and TUI
integrations can land in either order — they're symmetric per ADR
0005, but landing them in the same release is required (no partial
parity).

## Files touched

**New**:
- `core/src/syntax.rs` — tokenizers, `Token`, `TokenKind`,
  `SYNTAX_SIZE_LIMIT`, `tokenize_for_path`
- `docs/plans/syntax-highlighting.md` — this doc
- `frontend_gui/tests/syntax_render.rs` — offscreen render smoke test
- `frontend_tui/...` — new tests inline in `ui_tests.rs`

**Modified**:
- `core/src/lib.rs` — `pub mod syntax;` + re-exports
- `core/src/document.rs` — add `syntax: SyntaxCache` field on
  `Document`, default-initialized
- `frontend_gui/src/app.rs` — `syntax.dirty = true` on edit events;
  cache lookup helpers
- `frontend_gui/src/ui.rs` — per-line token walk, palette mapping
- `frontend_tui/src/app.rs` — symmetric to GUI app
- `frontend_tui/src/ui.rs` — symmetric to GUI ui

## Verification

- `cargo test -p core` — all tokenizer unit tests pass
- `cargo test -p frontend_tui` — render smoke + new syntax tests pass
- `cargo test -p frontend_gui` — offscreen render + new syntax tests
  pass
- `cargo clippy --workspace -- -D warnings` — clean
- Manual: open `frontend_gui/src/ui.rs` in the GUI → keywords
  (e.g. `fn`, `let`, `pub`) colored, `//` comments gray, `"string"`
  green. Open `Cargo.toml` → no syntax highlight (not `.rs`/`.md`).
  Open a 5 MB log → no syntax highlight, no keystroke lag.

## Risks / open questions

- **Tab advance**: tokens carry byte ranges, but the GUI renderer
  positions by char col × `char_width` (with `'\t'` advance = 4× —
  see existing fix in `ui.rs:392-405`). Token char-col conversion is
  the same `byte_to_char_col` the match-highlight code uses, so this
  composes correctly. Confirmed not a new risk.
- **Edit invalidation granularity**: v1 drops the entire cache on
  every edit. If a 1 MB file shows real lag on edits, debounce or
  fine-grained invalidation becomes a v1.1. Not blocking.
- **Rust tokenizer corner cases**: lifetimes (`'a`), turbofish
  (`::<T>`), attribute syntax (`#[..]`) — v1 just treats these as
  `Identifier` / `Punctuation`. Cosmetic only, doesn't break
  highlighting of the rest.
- **Theme palette disagreement**: GUI uses egui's default colors
  (dark mode); TUI uses ratatui's. They won't match visually, and
  that's fine — ADR 0005 is about behavior parity, not visual
  identity. If we want them to converge later, both palettes can
  move to a shared `core::syntax::Palette` struct. Defer.
- **No new ADR**: this is an additive feature using the existing
  pieces (Buffer trait, Document type, frontend parity rule). No
  architectural decision is being made that warrants ADR 0008. If
  during implementation we discover the cache-invalidation strategy
  or the size-gate threshold needs codifying, capture it as a
  follow-up ADR then.