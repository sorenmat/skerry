//! Error types for the `Buffer` trait.

use std::ops::Range;

use crate::BytePos;

/// Errors returned by `Buffer::insert` and `Buffer::delete`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditError {
    /// The position or range was out of bounds.
    OutOfBounds { pos: BytePos, len: BytePos },

    /// The range was inverted (`start > end`).
    InvalidRange(Range<BytePos>),

    /// The text contained invalid UTF-8 at the given byte offset within
    /// the input. Should be unreachable in practice since `&str` is UTF-8;
    /// included for completeness when constructing an error from raw bytes.
    InvalidUtf8 { byte_in_input: usize },
}

impl std::fmt::Display for EditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EditError::OutOfBounds { pos, len } => {
                write!(f, "position {pos} out of bounds (buffer length {len})")
            }
            EditError::InvalidRange(r) => {
                write!(f, "invalid range {}..{}", r.start, r.end)
            }
            EditError::InvalidUtf8 { byte_in_input } => {
                write!(f, "invalid UTF-8 at byte {byte_in_input} of input")
            }
        }
    }
}

impl std::error::Error for EditError {}

/// Errors returned by `Buffer::save`.
#[derive(Debug)]
pub enum SaveError {
    /// Buffer has no source path (was never loaded from disk and
    /// no save target was set).
    NoSourcePath,

    /// Underlying I/O error.
    Io(std::io::Error),
}

impl std::fmt::Display for SaveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SaveError::NoSourcePath => write!(f, "buffer has no source path to save to"),
            SaveError::Io(e) => write!(f, "I/O error: {e}"),
        }
    }
}

impl std::error::Error for SaveError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SaveError::NoSourcePath => None,
            SaveError::Io(e) => Some(e),
        }
    }
}

impl From<std::io::Error> for SaveError {
    fn from(e: std::io::Error) -> Self {
        SaveError::Io(e)
    }
}