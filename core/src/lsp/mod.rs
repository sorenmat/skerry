//! Language Server Protocol (LSP) client support.
//!
//! The `lsp` module is intentionally frontend-agnostic. Both the GUI
//! and the TUI talk to the same synchronous `LspManager`, which hides
//! a Tokio runtime and stdio language-server processes underneath.

mod client;
mod manager;
mod protocol;

pub use client::{LspClient, LspError};
pub use manager::{hover_text, LspManager, MissingServerInfo, ServerStatus};
