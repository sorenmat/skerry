//! External formatter support — shells out to tools like `gofmt`,
//! `rustfmt`, and `prettier` as a fallback when LSP formatting isn't
//! available.
//!
//! Formatters read the file content from stdin and write the formatted
//! output to stdout. The command is configured per language ID in
//! [`crate::Config::formatters`] (e.g. `"go" → "gofmt"`).

use std::io::Write;
use std::process::{Command, Stdio};

use crate::Config;

/// Maximum input size (in bytes) for external formatting. Guards
/// against spawning a formatter on a multi-GB file.
const MAX_FORMAT_BYTES: usize = 5 * 1024 * 1024;

/// Run an external formatter command, feeding `input` via stdin and
/// returning the formatted output from stdout.
///
/// `command` is a whitespace-delimited command string (e.g.
/// `"rustfmt --emit stdout"`). The first token is the binary; the rest
/// are arguments.
///
/// Returns `None` on spawn failure, non-zero exit, empty output (no
/// changes), or if the input exceeds [`MAX_FORMAT_BYTES`].
pub fn run_external_formatter(command: &str, input: &str) -> Option<String> {
    if input.len() > MAX_FORMAT_BYTES {
        return None;
    }
    let mut parts = command.split_whitespace();
    let binary = parts.next()?;
    let args: Vec<&str> = parts.collect();

    let mut child = Command::new(binary)
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    // Write the input to stdin.
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(input.as_bytes());
        // stdin is dropped here, signalling EOF to the formatter.
    }

    let output = child.wait_with_output().ok()?;
    if !output.status.success() {
        return None;
    }
    let formatted = String::from_utf8(output.stdout).ok()?;
    // If the formatter produced no output or identical content, treat
    // it as "no change needed" — return None so the caller skips the
    // replace.
    if formatted.is_empty() || formatted == input {
        return None;
    }
    Some(formatted)
}

/// Look up the configured formatter command for a language ID.
/// Returns `None` if no formatter is configured for that language.
pub fn formatter_for_language<'a>(config: &'a Config, language_id: &str) -> Option<&'a str> {
    config.formatters.get(language_id).map(|s| s.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formatter_for_language_finds_configured() {
        let config = Config::default();
        assert!(formatter_for_language(&config, "rust").is_some());
        assert!(formatter_for_language(&config, "go").is_some());
        assert!(formatter_for_language(&config, "python").is_some());
    }

    #[test]
    fn formatter_for_language_missing() {
        let config = Config::default();
        assert!(formatter_for_language(&config, "json").is_none());
        assert!(formatter_for_language(&config, "nonexistent").is_none());
    }

    #[test]
    fn run_external_formatter_spawn_failure_returns_none() {
        // A binary that almost certainly doesn't exist.
        let result = run_external_formatter("definitely_not_a_real_binary_xyz123", "hello");
        assert!(result.is_none());
    }

    #[test]
    fn run_external_formatter_identical_output_returns_none() {
        // `cat` echoes input unchanged — should return None (no change).
        let result = run_external_formatter("cat", "hello\n");
        assert_eq!(result, None);
    }

    #[test]
    fn run_external_formatter_overlarge_input_returns_none() {
        let huge = "x".repeat(MAX_FORMAT_BYTES + 1);
        assert!(run_external_formatter("cat", &huge).is_none());
    }

    #[test]
    fn default_formatters_have_common_languages() {
        let config = Config::default();
        assert_eq!(
            config.formatters.get("go").map(|s| s.as_str()),
            Some("gofmt")
        );
    }
}
