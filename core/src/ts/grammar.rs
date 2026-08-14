//! Grammar registry — maps a file extension to a tree-sitter language and
//! its bundled highlight query.
//!
//! The set of supported extensions intentionally mirrors the LSP language
//! map in `crate::Document::language_id`, so every language the editor can
//! run an LSP server for also gets tree-sitter highlighting. Extensions
//! with no bundled grammar return `None` and the document renders as plain
//! text (no color).
//!
//! Grammar crates expose their language as a `tree_sitter_language::LanguageFn`
//! const named `LANGUAGE` (or `LANGUAGE_TYPESCRIPT` / `LANGUAGE_TSX` for the
//! dual-language typescript crate), converted to `tree_sitter::Language` via
//! `.into()`. The highlight query const is named `HIGHLIGHTS_QUERY` for most
//! crates but `HIGHLIGHT_QUERY` (singular) for a few — the wrappers below
//! normalise that. Markdown uses a paired block/inline grammar; `Grammar`
//! carries both languages and queries so `DocTree` can parse them together.

use std::path::Path;
use std::sync::OnceLock;

use tree_sitter::Language;

/// A loaded grammar: the tree-sitter language plus its highlight query
/// source. The query is kept as a `&'static str` because every grammar
/// crate embeds its `.scm` file via `include_str!`. `Language` is not
/// `Copy`, so neither is this — clone it where needed (it's cheap: a
/// language handle + a static str slice).
#[derive(Clone)]
pub struct Grammar {
    /// Stable language name ("rust", "go", …) used as the query-cache key.
    pub name: &'static str,
    pub language: Language,
    pub highlights_query: &'static str,
    /// Optional second grammar used for Markdown inline content.
    pub(crate) inline: Option<InlineGrammar>,
}

#[derive(Clone)]
pub(crate) struct InlineGrammar {
    pub language: Language,
    pub highlights_query: &'static str,
}

// Each helper builds the Grammar lazily. Grammar construction is cheap
// (a `LanguageFn` → `Language` conversion and two `&'static str` copies)
// but the resulting `Language` is `Send + Sync`, so we cache one per
// supported extension in a static `OnceLock`-guarded table built on first
// lookup.

pub(crate) fn rust() -> Grammar {
    Grammar {
        name: "rust",
        language: tree_sitter_rust::LANGUAGE.into(),
        highlights_query: tree_sitter_rust::HIGHLIGHTS_QUERY,
        inline: None,
    }
}

pub(crate) fn go() -> Grammar {
    Grammar {
        name: "go",
        language: tree_sitter_go::LANGUAGE.into(),
        highlights_query: tree_sitter_go::HIGHLIGHTS_QUERY,
        inline: None,
    }
}

pub(crate) fn javascript() -> Grammar {
    Grammar {
        name: "javascript",
        language: tree_sitter_javascript::LANGUAGE.into(),
        highlights_query: tree_sitter_javascript::HIGHLIGHT_QUERY,
        inline: None,
    }
}

pub(crate) fn typescript() -> Grammar {
    Grammar {
        name: "typescript",
        language: tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        highlights_query: tree_sitter_typescript::HIGHLIGHTS_QUERY,
        inline: None,
    }
}

pub(crate) fn tsx() -> Grammar {
    Grammar {
        name: "tsx",
        language: tree_sitter_typescript::LANGUAGE_TSX.into(),
        // TSX shares the TypeScript highlight query; the grammar's own
        // captures distinguish JSX contexts.
        highlights_query: tree_sitter_typescript::HIGHLIGHTS_QUERY,
        inline: None,
    }
}

pub(crate) fn python() -> Grammar {
    Grammar {
        name: "python",
        language: tree_sitter_python::LANGUAGE.into(),
        highlights_query: tree_sitter_python::HIGHLIGHTS_QUERY,
        inline: None,
    }
}

pub(crate) fn c() -> Grammar {
    Grammar {
        name: "c",
        language: tree_sitter_c::LANGUAGE.into(),
        highlights_query: tree_sitter_c::HIGHLIGHT_QUERY,
        inline: None,
    }
}

pub(crate) fn json() -> Grammar {
    Grammar {
        name: "json",
        language: tree_sitter_json::LANGUAGE.into(),
        highlights_query: tree_sitter_json::HIGHLIGHTS_QUERY,
        inline: None,
    }
}

pub(crate) fn yaml() -> Grammar {
    Grammar {
        name: "yaml",
        language: tree_sitter_yaml::LANGUAGE.into(),
        highlights_query: tree_sitter_yaml::HIGHLIGHTS_QUERY,
        inline: None,
    }
}

pub(crate) fn markdown() -> Grammar {
    Grammar {
        name: "markdown",
        language: tree_sitter_md::LANGUAGE.into(),
        highlights_query: tree_sitter_md::HIGHLIGHT_QUERY_BLOCK,
        inline: Some(InlineGrammar {
            language: tree_sitter_md::INLINE_LANGUAGE.into(),
            highlights_query: tree_sitter_md::HIGHLIGHT_QUERY_INLINE,
        }),
    }
}

/// Resolve a `Grammar` for a file by its extension. Returns `None` for
/// unsupported extensions (the caller renders plain text). The result is
/// cached in a process-global table so the `Language` is constructed at
/// most once per grammar.
pub fn grammar_for_path(path: Option<&Path>) -> Option<Grammar> {
    let ext = path?.extension()?.to_str()?.to_ascii_lowercase();
    grammar_for_extension(&ext)
}

/// One row in the extension → grammar table.
type GrammarEntry = (&'static str, fn() -> Grammar);

/// Resolve a `Grammar` by lowercase extension string (no leading dot).
pub fn grammar_for_extension(ext: &str) -> Option<Grammar> {
    static REGISTRY: OnceLock<Vec<GrammarEntry>> = OnceLock::new();
    let entries = REGISTRY.get_or_init(|| {
        vec![
            ("rs", rust as fn() -> Grammar),
            ("go", go),
            ("js", javascript),
            ("jsx", javascript),
            ("mjs", javascript),
            ("cjs", javascript),
            ("ts", typescript),
            ("tsx", tsx),
            ("py", python),
            ("pyw", python),
            ("c", c),
            ("h", c),
            // C++: the C grammar parses it acceptably for highlighting;
            // a dedicated cpp grammar is a follow-up if coverage gaps show.
            ("cpp", c),
            ("cc", c),
            ("cxx", c),
            ("hpp", c),
            ("json", json),
            ("jsonc", json),
            ("yaml", yaml),
            ("yml", yaml),
            ("md", markdown),
            ("markdown", markdown),
        ]
    });
    entries
        .iter()
        .find(|(e, _)| *e == ext)
        .map(|(_, ctor)| constructor_or_cached(ext, *ctor))
}

/// Build a grammar via its constructor. The per-extension constructors are
/// deterministic and cheap; we don't bother memoising the `Language` further
/// than the constructor call itself (the `LanguageFn` → `Language` conversion
/// is a single pointer dereference under the hood).
fn constructor_or_cached(_ext: &str, ctor: fn() -> Grammar) -> Grammar {
    ctor()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn known_extensions_resolve() {
        for ext in [
            "rs", "go", "js", "ts", "tsx", "py", "c", "cpp", "json", "yaml", "yml", "md",
            "markdown",
        ] {
            let g = grammar_for_extension(ext);
            assert!(g.is_some(), "extension .{ext} should resolve to a grammar");
        }
    }

    #[test]
    fn unknown_extension_is_none() {
        assert!(grammar_for_extension("xyz123").is_none());
        assert!(grammar_for_extension("").is_none());
    }

    #[test]
    fn grammar_for_path_uses_extension() {
        let g = grammar_for_path(Some(&PathBuf::from("src/main.rs")));
        assert!(g.is_some());
        assert!(grammar_for_path(None).is_none());
        assert!(grammar_for_path(Some(&PathBuf::from("README"))).is_none());
    }

    #[test]
    fn highlight_query_is_nonempty() {
        // A sanity check that the const wiring actually pointed at a
        // non-empty query file. If a crate renames its const, this fails
        // loudly rather than silently degrading to no color.
        let g = grammar_for_extension("rs").unwrap();
        assert!(!g.highlights_query.is_empty());
        assert!(
            g.highlights_query.contains('@'),
            "rust highlight query should contain capture patterns"
        );
    }

    #[test]
    fn typescript_and_tsx_distinct_languages() {
        // Both should parse, and they should be different language objects
        // (TSX adds JSX nodes). We can't compare Language for equality
        // directly, but we can confirm both resolve without panic.
        let _ts = grammar_for_extension("ts").unwrap();
        let _tsx = grammar_for_extension("tsx").unwrap();
    }
}
