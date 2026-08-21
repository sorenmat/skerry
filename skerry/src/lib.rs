//! GUI frontend library — exposes the editor app + render code so tests
//! and binaries can both use it.

// Crate-wide: no unsafe. The only exception is the macOS self-capture FFI
// in screenshot.rs (see Cargo.toml lints note).
#![deny(unsafe_code)]

pub mod app;
mod csv_preview;
pub mod event;
pub mod fonts;
mod markdown;
#[cfg(target_os = "macos")]
pub mod screenshot;
pub mod theme;
pub mod ui;
