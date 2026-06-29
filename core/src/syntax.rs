//! Syntax highlighting tokenizers.
//!
//! Pure functions: `&[u8]` (one line) → `Vec<Token>`. Stateless,
//! trivial to unit-test, reusable by anything (frontends, search,
//! status bar). The dispatcher `tokenize_for_path` picks the right
//! tokenizer by file extension.
//!
//! v1 ships Rust + Markdown + plain-text passthrough. See
//! `docs/plans/syntax-highlighting.md` for the full design.

use std::collections::HashMap;
use std::path::Path;

/// Maximum file size (in bytes) for which syntax highlighting is
/// enabled. Files above this skip tokenization entirely — a 2 MB+
/// source file is almost certainly generated code or a data dump, and
/// multi-GB log files would make every keystroke lag.
pub const SYNTAX_SIZE_LIMIT: usize = 2 * 1024 * 1024;

/// A classified byte range within a line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    /// Byte range within the line's text (NOT the document — line-local).
    pub range: std::ops::Range<usize>,
    pub kind: TokenKind,
}

/// Semantic category for highlighting. Frontends map each variant to
/// a concrete color; `Punctuation` and `Identifier` exist so the
/// renderer can treat them as "default foreground" without an
/// `Option`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TokenKind {
    Keyword,
    Type,
    Function,
    String,
    Comment,
    Number,
    Punctuation,
    Identifier,
}

/// Pick a tokenizer by file extension and run it on `line`. Returns
/// an empty `Vec` for unrecognized extensions or files with no
/// extension — the frontend treats empty as "no highlighting".
pub fn tokenize_line(path: Option<&Path>, line: &[u8]) -> Vec<Token> {
    let Some(ext) = path.and_then(|p| p.extension()).and_then(|e| e.to_str()) else {
        return Vec::new();
    };
    match ext {
        "rs" => tokenize_rust(line),
        "md" | "markdown" => tokenize_markdown(line),
        _ => Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// Per-document cache
// ---------------------------------------------------------------------------

/// Lazily-populated, edit-invalidated per-line token cache. Lives on
/// `Document` so it survives tab switches. Both frontends share the
/// same shape via `core::Document`.
///
/// v1 invalidation is simple: **any** edit sets `dirty = true` and
/// clears the cache. The renderer re-tokenizes only the visible lines
/// on the next frame. For files under the size gate this is well under
/// frame budget.
#[derive(Clone, Debug, Default)]
pub struct SyntaxCache {
    /// Tokens keyed by line index. Empty when highlighting is disabled
    /// (file too large, no matching extension, or cache was just
    /// invalidated).
    pub lines: HashMap<usize, Vec<Token>>,
    /// `true` after any buffer edit; the renderer checks this and
    /// re-populates affected lines.
    pub dirty: bool,
}

impl SyntaxCache {
    /// Mark the cache as stale. Called on every buffer-mutating event.
    /// Drops the entire cache — v1 is not clever about which lines
    /// are affected.
    pub fn invalidate(&mut self) {
        self.lines.clear();
        self.dirty = true;
    }
}

// ---------------------------------------------------------------------------
// Rust tokenizer
// ---------------------------------------------------------------------------

/// Rust keyword set (including reserved words from the 2024 edition).
const KEYWORDS: &[&str] = &[
    "as", "async", "await", "box", "break", "const", "continue", "crate", "do", "dyn", "else",
    "enum", "extern", "false", "final", "fn", "for", "if", "impl", "in", "let", "loop", "macro",
    "match", "mod", "move", "mut", "override", "priv", "pub", "ref", "return", "self", "Self",
    "static", "struct", "super", "trait", "true", "try", "type", "typeof", "union", "unsafe",
    "unsized", "use", "virtual", "where", "while", "yield",
];

/// Primitive + common stdlib types for v1 highlighting.
const TYPES: &[&str] = &[
    "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64", "u128", "usize", "f32",
    "f64", "bool", "char", "str", "String", "Vec", "Option", "Result", "Box", "Rc", "Arc", "Cell",
    "RefCell", "HashMap", "HashSet", "BTreeMap", "BTreeSet",
];

fn is_ident_start(b: u8) -> bool {
    b == b'_' || b.is_ascii_alphabetic()
}

fn is_ident_cont(b: u8) -> bool {
    b == b'_' || b.is_ascii_alphanumeric()
}

fn classify_ident(word: &str) -> TokenKind {
    if KEYWORDS.contains(&word) {
        TokenKind::Keyword
    } else if TYPES.contains(&word) {
        TokenKind::Type
    } else {
        TokenKind::Identifier
    }
}

/// Tokenize a single line of Rust source code.
///
/// Block comments that span multiple lines are tracked via the
/// caller — v1 operates per-line, so a `/*` without a matching `*/`
/// on the same line consumes the rest of the line as `Comment`, and
/// subsequent lines are handled on their own (the missing `*/` on a
/// later line will produce tokens as normal, which is cosmetically
/// acceptable for v1).
pub fn tokenize_rust(line: &[u8]) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut i = 0;
    let n = line.len();

    while i < n {
        let start = i;
        let b = line[i];

        // Whitespace — skip (gaps between tokens are implicit).
        if b.is_ascii_whitespace() {
            i += 1;
            continue;
        }

        // Line comment
        if b == b'/' && i + 1 < n && line[i + 1] == b'/' {
            tokens.push(Token { range: start..n, kind: TokenKind::Comment });
            break;
        }

        // Block comment (single-line portion — `/* ... */` or `/* ...` without close)
        if b == b'/' && i + 1 < n && line[i + 1] == b'*' {
            let mut j = i + 2;
            let mut depth = 1;
            while j < n && depth > 0 {
                if line[j] == b'/' && j + 1 < n && line[j + 1] == b'*' {
                    depth += 1;
                    j += 2;
                } else if line[j] == b'*' && j + 1 < n && line[j + 1] == b'/' {
                    depth -= 1;
                    j += 2;
                } else {
                    j += 1;
                }
            }
            tokens.push(Token { range: start..j, kind: TokenKind::Comment });
            i = j;
            continue;
        }

        // String literal: "..."
        if b == b'"' {
            i += 1;
            while i < n {
                match line[i] {
                    b'\\' => i += 2, // skip escaped char
                    b'"' => { i += 1; break; }
                    _ => i += 1,
                }
            }
            tokens.push(Token { range: start..i, kind: TokenKind::String });
            continue;
        }

        // Raw string: r"..." or r#"..."#
        if b == b'r' && i + 1 < n && (line[i + 1] == b'"' || line[i + 1] == b'#') {
            let mut j = i + 1;
            let mut hashes = 0;
            while j < n && line[j] == b'#' {
                hashes += 1;
                j += 1;
            }
            if j < n && line[j] == b'"' {
                j += 1;
                // Find closing " followed by `hashes` #'s
                while j < n {
                    if line[j] == b'"' {
                        let mut k = j + 1;
                        let mut seen = 0;
                        while k < n && line[k] == b'#' && seen < hashes {
                            seen += 1;
                            k += 1;
                        }
                        if seen == hashes {
                            j = k;
                            break;
                        }
                    }
                    j += 1;
                }
                tokens.push(Token { range: start..j, kind: TokenKind::String });
                i = j;
                continue;
            }
            // Not a raw string — `r` falls through to identifier scan.
        }

        // Byte string: b"..."  (b'...' falls through to the char
        // literal handler below — the `'` starts the scan there).
        if b == b'b' && i + 1 < n && line[i + 1] == b'"' {
            let mut j = i + 2;
            while j < n {
                match line[j] {
                    b'\\' => j += 2,
                    b'"' => { j += 1; break; }
                    _ => j += 1,
                }
            }
            tokens.push(Token { range: start..j, kind: TokenKind::String });
            i = j;
            continue;
        }

        // Char literal: '...'
        // Distinguish from lifetime labels: lifetime is `'` followed by
        // an identifier start and no closing `'` within 1-2 chars.
        // For v1, treat `'x'` as a char (has closing quote quickly)
        // and `'abc` (no close) as punctuation + identifier.
        if b == b'\'' && i + 1 < n {
            // Look for a closing quote within a small window.
            let mut j = i + 1;
            let mut found_close = false;
            while j < n {
                match line[j] {
                    b'\\' => j += 2,
                    b'\'' => { found_close = true; j += 1; break; }
                    _ if j - start > 6 => break, // too long for a char literal
                    _ => j += 1,
                }
            }
            if found_close {
                tokens.push(Token { range: start..j, kind: TokenKind::String });
                i = j;
                continue;
            }
            // No close — lifetime or malformed. Treat `'` as punctuation.
        }

        // Number: [0-9][0-9a-fA-F_.oxbeE+\-]*
        // (loose — highlights the whole numeric expression)
        if b.is_ascii_digit() {
            let mut j = i + 1;
            // Hex / octal / binary prefix: 0x, 0o, 0b
            if line[i] == b'0' && j < n
                && matches!(line[j], b'x' | b'X' | b'o' | b'O' | b'b' | b'B')
            {
                j += 1;
            }
            while j < n {
                let c = line[j];
                let is_exp_sign = (c == b'+' || c == b'-')
                    && j > 0
                    && (line[j - 1] == b'e' || line[j - 1] == b'E');
                if c.is_ascii_hexdigit() || c == b'_' || c == b'.' || is_exp_sign {
                    j += 1;
                } else {
                    break;
                }
            }
            tokens.push(Token { range: start..j, kind: TokenKind::Number });
            i = j;
            continue;
        }

        // Identifier or keyword
        if is_ident_start(b) {
            let mut j = i + 1;
            while j < n && is_ident_cont(line[j]) {
                j += 1;
            }
            let word = std::str::from_utf8(&line[start..j]).unwrap_or("");
            let kind = classify_ident(word);

            // Function detection: identifier followed by `(` and not a
            // keyword/type. Best-effort — false positives on macro-like
            // syntax are acceptable for v1.
            let kind = if kind == TokenKind::Identifier
                && j < n
                && line[j] == b'('
            {
                TokenKind::Function
            } else {
                kind
            };

            tokens.push(Token { range: start..j, kind });
            i = j;
            continue;
        }

        // Everything else: single-char punctuation
        tokens.push(Token { range: start..start + 1, kind: TokenKind::Punctuation });
        i += 1;
    }

    tokens
}

// ---------------------------------------------------------------------------
// Markdown tokenizer
// ---------------------------------------------------------------------------

/// Tokenize a single line of Markdown.
///
/// Line-oriented: each line starts in `Normal` state. Block-level
/// patterns (headings, blockquotes, list items, code fences) are
/// detected at line start. Inline patterns (bold, italic, code spans,
/// links) are scanned after the block-level prefix.
pub fn tokenize_markdown(line: &[u8]) -> Vec<Token> {
    let mut tokens = Vec::new();
    let n = line.len();
    if n == 0 {
        return tokens;
    }

    // Skip leading whitespace for block-level detection.
    let mut i = 0;
    while i < n && (line[i] == b' ' || line[i] == b'\t') {
        i += 1;
    }
    let indent = i;

    // Code fence: ``` or ~~~
    if i + 2 < n && (line[i] == b'`' || line[i] == b'~') && line[i] == line[i + 1] && line[i] == line[i + 2] {
        // Consume the fence marker and any trailing chars (language hint)
        let mut j = i;
        while j < n && (line[j] == line[i]) {
            j += 1;
        }
        tokens.push(Token { range: indent..n, kind: TokenKind::Keyword });
        return tokens;
    }

    // Heading: #{1,6}
    if line[i] == b'#' {
        let mut j = i;
        while j < n && line[j] == b'#' && j - i < 6 {
            j += 1;
        }
        if j < n && line[j] == b' ' {
            tokens.push(Token { range: indent..j, kind: TokenKind::Keyword });
            // Rest of the line: scan for inline patterns
            scan_markdown_inline(&mut tokens, line, j);
            return tokens;
        }
    }

    // Blockquote: >
    if line[i] == b'>' && (i + 1 >= n || line[i + 1] == b' ' || line[i + 1] == b'\t') {
        tokens.push(Token { range: indent..i + 1, kind: TokenKind::Keyword });
        scan_markdown_inline(&mut tokens, line, i + 1);
        return tokens;
    }

    // Unordered list: - * +
    if (line[i] == b'-' || line[i] == b'*' || line[i] == b'+')
        && i + 1 < n
        && line[i + 1] == b' '
    {
        tokens.push(Token { range: indent..i + 1, kind: TokenKind::Keyword });
        scan_markdown_inline(&mut tokens, line, i + 1);
        return tokens;
    }

    // Ordered list: \d+.
    if line[i].is_ascii_digit() {
        let mut j = i;
        while j < n && line[j].is_ascii_digit() {
            j += 1;
        }
        if j < n && line[j] == b'.' && j + 1 < n && line[j + 1] == b' ' {
            tokens.push(Token { range: indent..j + 1, kind: TokenKind::Keyword });
            scan_markdown_inline(&mut tokens, line, j + 1);
            return tokens;
        }
    }

    // Regular line — scan for inline patterns from the start
    scan_markdown_inline(&mut tokens, line, 0);
    tokens
}

/// Scan inline Markdown patterns starting at `start`.
///
/// Recognizes:
/// - `` `code` `` → `String`
/// - `**bold**` → `Keyword` over markers, rest as `Identifier`
/// - `*italic*` → `Keyword` over markers
/// - `[text](url)` → `Type` over text, `String` over URL
///
/// Unrecognized text falls through as `Identifier` (default foreground).
fn scan_markdown_inline(tokens: &mut Vec<Token>, line: &[u8], start: usize) {
    let n = line.len();
    let mut i = start;
    let mut plain_start = start;

    while i < n {
        let b = line[i];

        // Inline code span: `...`
        if b == b'`' {
            if i > plain_start {
                tokens.push(Token { range: plain_start..i, kind: TokenKind::Identifier });
            }
            let mut j = i + 1;
            while j < n && line[j] != b'`' {
                j += 1;
            }
            let end = if j < n { j + 1 } else { j };
            tokens.push(Token { range: i..end, kind: TokenKind::String });
            i = end;
            plain_start = i;
            continue;
        }

        // Bold: **...**
        if b == b'*' && i + 1 < n && line[i + 1] == b'*' {
            if let Some(close) = find_marker(line, i + 2, b'*', 2) {
                if i > plain_start {
                    tokens.push(Token { range: plain_start..i, kind: TokenKind::Identifier });
                }
                tokens.push(Token { range: i..i + 2, kind: TokenKind::Keyword });
                tokens.push(Token { range: i + 2..close, kind: TokenKind::Identifier });
                tokens.push(Token { range: close..close + 2, kind: TokenKind::Keyword });
                i = close + 2;
                plain_start = i;
                continue;
            }
        }

        // Italic: *...*  (single asterisk, not part of **)
        if b == b'*' && (i + 1 >= n || line[i + 1] != b'*') {
            if let Some(close) = find_marker(line, i + 1, b'*', 1) {
                if i > plain_start {
                    tokens.push(Token { range: plain_start..i, kind: TokenKind::Identifier });
                }
                tokens.push(Token { range: i..i + 1, kind: TokenKind::Keyword });
                tokens.push(Token { range: i + 1..close, kind: TokenKind::Identifier });
                tokens.push(Token { range: close..close + 1, kind: TokenKind::Keyword });
                i = close + 1;
                plain_start = i;
                continue;
            }
        }

        // Link: [text](url)
        if b == b'[' {
            if let Some(close_bracket) = find_byte(line, i + 1, b']') {
                if close_bracket + 1 < n
                    && line[close_bracket + 1] == b'('
                {
                    if let Some(close_paren) = find_byte(line, close_bracket + 2, b')') {
                        if i > plain_start {
                            tokens.push(Token { range: plain_start..i, kind: TokenKind::Identifier });
                        }
                        tokens.push(Token { range: i..i + 1, kind: TokenKind::Punctuation });
                        tokens.push(Token { range: i + 1..close_bracket, kind: TokenKind::Type });
                        tokens.push(Token { range: close_bracket..close_bracket + 1, kind: TokenKind::Punctuation });
                        tokens.push(Token { range: close_bracket + 1..close_paren + 1, kind: TokenKind::String });
                        i = close_paren + 1;
                        plain_start = i;
                        continue;
                    }
                }
            }
        }

        i += 1;
    }

    if i > plain_start {
        tokens.push(Token { range: plain_start..i, kind: TokenKind::Identifier });
    }
}

/// Find the position of `byte` in `line` starting from `from`, or `None`.
fn find_byte(line: &[u8], from: usize, byte: u8) -> Option<usize> {
    let mut j = from;
    while j < line.len() {
        if line[j] == byte {
            return Some(j);
        }
        j += 1;
    }
    None
}

/// Find a repeated marker (`count` copies of `marker`) starting at or
/// after `from`. Returns the byte index of the first byte of the
/// marker run, or `None`.
fn find_marker(line: &[u8], from: usize, marker: u8, count: usize) -> Option<usize> {
    let mut j = from;
    while j + count <= line.len() {
        let mut all_match = true;
        for k in 0..count {
            if line[j + k] != marker {
                all_match = false;
                break;
            }
        }
        if all_match {
            return Some(j);
        }
        j += 1;
    }
    None
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: tokenize a string and return (kind, text) pairs.
    fn kinds(input: &str) -> Vec<(TokenKind, &str)> {
        tokenize_rust(input.as_bytes())
            .iter()
            .map(|t| (t.kind, &input[t.range.clone()]))
            .collect()
    }

    fn md_kinds(input: &str) -> Vec<(TokenKind, &str)> {
        tokenize_markdown(input.as_bytes())
            .iter()
            .map(|t| (t.kind, &input[t.range.clone()]))
            .collect()
    }

    // --- Rust: keywords and identifiers ---

    #[test]
    fn rust_let_binding() {
        let t = kinds("let x = 42;");
        assert!(t.contains(&(TokenKind::Keyword, "let")));
        assert!(t.contains(&(TokenKind::Identifier, "x")));
        assert!(t.contains(&(TokenKind::Number, "42")));
    }

    #[test]
    fn rust_pub_fn() {
        let t = kinds("pub fn main() {}");
        assert!(t.contains(&(TokenKind::Keyword, "pub")));
        assert!(t.contains(&(TokenKind::Keyword, "fn")));
        assert!(t.contains(&(TokenKind::Function, "main")));
    }

    #[test]
    fn rust_primitive_type() {
        let t = kinds("let x: i32 = 0;");
        assert!(t.contains(&(TokenKind::Type, "i32")));
    }

    #[test]
    fn rust_stdlib_type() {
        let t = kinds("let v = Vec::new();");
        assert!(t.contains(&(TokenKind::Type, "Vec")));
        assert!(t.contains(&(TokenKind::Function, "new")));
    }

    // --- Rust: comments ---

    #[test]
    fn rust_line_comment() {
        let t = kinds("// hello world");
        assert_eq!(t, vec![(TokenKind::Comment, "// hello world")]);
    }

    #[test]
    fn rust_line_comment_after_code() {
        let t = kinds("let x = 1; // comment");
        let comment = t.iter().find(|(k, _)| *k == TokenKind::Comment);
        assert_eq!(comment, Some(&(TokenKind::Comment, "// comment")));
    }

    #[test]
    fn rust_block_comment_single_line() {
        let t = kinds("/* inline */");
        assert_eq!(t, vec![(TokenKind::Comment, "/* inline */")]);
    }

    #[test]
    fn rust_nested_block_comment() {
        let t = kinds("/* outer /* inner */ */");
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].0, TokenKind::Comment);
    }

    // --- Rust: strings ---

    #[test]
    fn rust_string_literal() {
        let t = kinds(r#"let s = "hello";"#);
        assert!(t.contains(&(TokenKind::String, r#""hello""#)));
    }

    #[test]
    fn rust_string_with_escapes() {
        let t = kinds(r#"let s = "a\"b\n";"#);
        assert!(t.contains(&(TokenKind::String, r#""a\"b\n""#)));
    }

    #[test]
    fn rust_raw_string() {
        let t = kinds(r##"let s = r#"inner"#;"##);
        assert!(t.iter().any(|(k, s)| *k == TokenKind::String && s.starts_with("r#")));
    }

    #[test]
    fn rust_char_literal() {
        let t = kinds("let c = 'x';");
        assert!(t.contains(&(TokenKind::String, "'x'")));
    }

    #[test]
    fn rust_escaped_char_literal() {
        let t = kinds("let c = '\\n';");
        assert!(t.contains(&(TokenKind::String, "'\\n'")));
    }

    #[test]
    fn rust_byte_string() {
        let t = kinds(r#"let b = b"bytes";"#);
        assert!(t.contains(&(TokenKind::String, r#"b"bytes""#)));
    }

    // --- Rust: numbers ---

    #[test]
    fn rust_decimal() {
        let t = kinds("let n = 42;");
        assert!(t.contains(&(TokenKind::Number, "42")));
    }

    #[test]
    fn rust_hex() {
        let t = kinds("let n = 0xFF;");
        assert!(t.contains(&(TokenKind::Number, "0xFF")));
    }

    #[test]
    fn rust_binary() {
        let t = kinds("let n = 0b1010;");
        assert!(t.contains(&(TokenKind::Number, "0b1010")));
    }

    #[test]
    fn rust_float_with_exponent() {
        let t = kinds("let n = 1.0e-3;");
        assert!(t.contains(&(TokenKind::Number, "1.0e-3")));
    }

    // --- Rust: function detection ---

    #[test]
    fn rust_function_call() {
        let t = kinds("foo(bar)");
        assert!(t.contains(&(TokenKind::Function, "foo")));
    }

    #[test]
    fn rust_keyword_not_function() {
        let t = kinds("if (x)");
        assert!(t.contains(&(TokenKind::Keyword, "if")));
        assert!(!t.iter().any(|(k, s)| *k == TokenKind::Function && *s == "if"));
    }

    // --- Markdown ---

    #[test]
    fn md_heading_h1() {
        let t = md_kinds("# Title");
        assert!(t.contains(&(TokenKind::Keyword, "#")));
    }

    #[test]
    fn md_heading_h3() {
        let t = md_kinds("### Section");
        assert!(t.iter().any(|(k, s)| *k == TokenKind::Keyword && s.starts_with("###")));
    }

    #[test]
    fn md_unordered_list_dash() {
        let t = md_kinds("- item");
        assert!(t.contains(&(TokenKind::Keyword, "-")));
    }

    #[test]
    fn md_unordered_list_star() {
        let t = md_kinds("* item");
        assert!(t.contains(&(TokenKind::Keyword, "*")));
    }

    #[test]
    fn md_ordered_list() {
        let t = md_kinds("1. item");
        assert!(t.contains(&(TokenKind::Keyword, "1.")));
    }

    #[test]
    fn md_blockquote() {
        let t = md_kinds("> quote");
        assert!(t.contains(&(TokenKind::Keyword, ">")));
    }

    #[test]
    fn md_inline_code() {
        let t = md_kinds("use `code` here");
        assert!(t.contains(&(TokenKind::String, "`code`")));
    }

    #[test]
    fn md_bold() {
        let t = md_kinds("**bold**");
        assert!(t.iter().any(|(k, s)| *k == TokenKind::Keyword && *s == "**"));
    }

    #[test]
    fn md_italic() {
        let t = md_kinds("*italic*");
        assert!(t.iter().any(|(k, s)| *k == TokenKind::Keyword && *s == "*"));
    }

    #[test]
    fn md_link() {
        let t = md_kinds("[text](url)");
        assert!(t.iter().any(|(k, s)| *k == TokenKind::Type && *s == "text"));
        assert!(t.iter().any(|(k, s)| *k == TokenKind::String && s.contains("url")));
    }

    #[test]
    fn md_code_fence() {
        let t = md_kinds("```rust");
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].0, TokenKind::Keyword);
    }

    // --- Dispatcher ---

    #[test]
    fn dispatch_rust() {
        let path = Path::new("main.rs");
        let tokens = tokenize_line(Some(path), b"let x = 1;");
        assert!(!tokens.is_empty());
        assert!(tokens.iter().any(|t| t.kind == TokenKind::Keyword));
    }

    #[test]
    fn dispatch_markdown() {
        let path = Path::new("README.md");
        let tokens = tokenize_line(Some(path), b"# Title");
        assert!(!tokens.is_empty());
    }

    #[test]
    fn dispatch_unknown_extension() {
        let path = Path::new("config.toml");
        let tokens = tokenize_line(Some(path), b"[package]");
        assert!(tokens.is_empty());
    }

    #[test]
    fn dispatch_no_extension() {
        let path = Path::new("Makefile");
        let tokens = tokenize_line(Some(path), b"all: build");
        assert!(tokens.is_empty());
    }

    #[test]
    fn dispatch_no_path() {
        let tokens = tokenize_line(None, b"let x = 1;");
        assert!(tokens.is_empty());
    }
}
