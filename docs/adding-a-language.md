# Adding a language (file type) to Skerry

How to add support for a new file type — both **syntax highlighting** (tree-sitter)
and **language-server (LSP)** features. The two are independent: you can add one
without the other. Most of the time you want both.

A "file type" in Skerry is really two things:

1. A **tree-sitter grammar** that produces colored `ColorSegment`s for the
   viewport. Without it the document renders as plain uncolored text.
2. An **LSP language id** (`"rust"`, `"yaml"`, …) so an LSP server can be
   started for the file. Without it you still get highlighting, but no
   diagnostics/completion/formatting.

There are **three** code locations for highlighting (not two — this is the easy
mistake) and **one** for LSP. This guide walks through all of them using YAML as
a worked example.

## Prerequisites

- A working C toolchain. Every `tree-sitter-*` crate compiles a small C parser
  via `cc` (Xcode Command Line Tools on macOS). `cargo build` will fail without it.
- The workspace MSRV is Rust 1.75; `lsp-types` is pinned to a version that
  builds against it. Verify a new grammar crate builds under the MSRV before
  pinning it.
- Check the grammar crate's public API on <https://docs.rs/tree-sitter-<lang>>
  **before** wiring it up. The const names are not consistent across crates
  (see [Gotchas](#gotchas)).

## How highlighting flows

So you know which piece you're touching:

```
Document::highlight_lines_ts        (core/src/document.rs)
  └─ ts::highlight_doc_range         (core/src/ts/highlight.rs)
       └─ highlight_range_with_query
            └─ compiled_query(grammar)   ← compiled from all_grammars()
            └─ theme.color_for(capture)
       ⇒ Vec<ColorSegment>
```

`compiled_query` is the trap: it looks up the grammar's query in a table that is
populated **only** from `all_grammars()`. If your grammar isn't in that list,
`compiled_query` returns `None` and highlighting silently emits nothing — the
document falls back to plain text with no error.

## Step-by-step

### 1. Add the grammar dependency — `core/Cargo.toml`

Add the crate next to the other grammar dependencies:

```toml
tree-sitter-yaml = "0.7"
```

### 2. Add a grammar constructor — `core/src/ts/grammar.rs`

Mirror the existing simple constructors (e.g. `json()`). A grammar is
`name` + `language` + `highlights_query` + `inline: None`:

```rust
pub(crate) fn yaml() -> Grammar {
    Grammar {
        name: "yaml",
        language: tree_sitter_yaml::LANGUAGE.into(),
        highlights_query: tree_sitter_yaml::HIGHLIGHTS_QUERY,
        inline: None,
    }
}
```

The `name` is the query-cache key — it must be stable and unique across grammars.

### 3. Register the extension(s) — `core/src/ts/grammar.rs`

In `grammar_for_extension`, add one row per extension to the `REGISTRY` `vec!`:

```rust
("yaml", yaml),
("yml", yaml),
```

Multiple extensions can share one constructor (see `js`/`jsx`/`mjs`/`cjs`, or
C++ reusing the C grammar). Add the new extension(s) to the
`known_extensions_resolve` test below so resolution is covered.

### 4. Register the grammar for query compilation — `core/src/ts/highlight.rs` ⚠️

**This is the step that's easy to forget.** In `all_grammars()`, add your
constructor to **both** the `use` list and the `vec!`:

```rust
fn all_grammars() -> Vec<Grammar> {
    use super::grammar::{
        c, go, javascript, json, markdown, python, rust, tsx, typescript, yaml,
    };
    vec![
        // ...
        json(),
        yaml(),     // ← added
        markdown(),
    ]
}
```

Without this, the grammar resolves (step 3 passes) but `compiled_query` returns
`None` and **no color is produced**. There is no compile error or warning — the
only signal is uncolored output, which is why an end-to-end test (step 6) matters.

### 5. (Optional) Add the LSP language id — `core/src/document.rs`

In `language_id_from_extension`, add a match arm. Only needed if you want LSP:

```rust
"yaml" | "yml" => Some("yaml"),
```

The returned string is the LSP `languageId` and must match what an LSP server
expects (e.g. `"typescriptreact"` for TSX, `"cpp"` for C++). Note this map and
the grammar registry are deliberately separate — they usually mirror each other
but don't have to (today `toml` and `csv` have an LSP id but no grammar, so they
highlight as plain text).

### 6. Add an end-to-end highlight test — `core/src/ts/highlight.rs`

Mirror `highlight_json_keys_and_values`. This is what catches the step-4
omission — it asserts the pipeline actually produces segments, not just that the
grammar resolves:

```rust
#[test]
fn highlight_yaml_keys_and_values() {
    let src = "name: skerry\nversion: 42\n";
    let (tree, g) = parse(src, "yaml");
    let segs = highlight_range(&tree, &g, &OCEAN_DARK, 0..src.len(), src.as_bytes());
    assert!(!segs.is_empty(), "yaml should highlight");
    for s in &segs {
        assert!(s.range.start < s.range.end, "segment must be non-empty");
        assert!(s.range.end <= src.len(), "segment must be in bounds");
    }
}
```

## Verify

```sh
cargo test -p core --lib ts::           # grammar + highlight tests
cargo test -p core --lib document::     # language-id resolution
cargo run -p skerry-tui -- path/to/file.<ext>   # see it live in the TUI
cargo run -p skerry     -- path/to/file.<ext>   # …or the GUI
```

To inspect the exact colors the theme assigns to each token, run a one-off
test that prints each segment's RGB (run with `--nocapture`):

```rust
let segs = highlight_range(&tree, &g, &OCEAN_DARK, 0..src.len(), src.as_bytes());
for s in &segs {
    let text = &src[s.range.clone()];
    println!("rgb({},{},{}) {:?}", s.color.r, s.color.g, s.color.b, text);
}
```

## Gotchas

- **Const names vary per crate.** The language entry is a
  `tree_sitter_language::LanguageFn` const named `LANGUAGE`, converted to
  `tree_sitter::Language` via `.into()`. The highlight query is usually
  `HIGHLIGHTS_QUERY` (plural) but is `HIGHLIGHT_QUERY` (singular) for some
  crates — JavaScript and C use the singular form. Always confirm on docs.rs.
- **Special-case crates.** `tree-sitter-typescript` exposes two grammars,
  `LANGUAGE_TYPESCRIPT` and `LANGUAGE_TSX`. `tree-sitter-md` (Markdown) is
  paired: a block grammar plus an inline grammar, and needs an `InlineGrammar`
  on the `Grammar` (see `markdown()`). Don't use these as your template; copy a
  simple one like `json()` or `yaml()`.
- **Three locations for highlighting, one for LSP.** Steps 2, 3, **and 4** are
  all required for highlighting. Step 4 is the silent-failure trap.
- **`Grammar` is `Clone`, not `Copy`.** Clone it where you need to hand it off;
  it's cheap (a language handle plus a `&'static str` slice).
- **LSP id ≠ grammar name.** They often coincide but aren't required to. The LSP
  id must match what servers expect; the grammar `name` is an internal cache key.

## Checklist

- [ ] `core/Cargo.toml` — `tree-sitter-<lang>` dependency added
- [ ] `core/src/ts/grammar.rs` — constructor added; extension(s) registered in
      `REGISTRY`; `known_extensions_resolve` updated
- [ ] `core/src/ts/highlight.rs` — constructor in **both** the `use` list and
      the `vec!` of `all_grammars()`
- [ ] `core/src/document.rs` — LSP language id (if LSP wanted)
- [ ] `core/src/ts/highlight.rs` — end-to-end highlight test added
- [ ] `cargo test -p core --lib ts::` passes
- [ ] Opened a real `.<ext>` file in the TUI/GUI and confirmed colors
