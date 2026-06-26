# GUI tech: egui with swappable renderer backend

We use egui via eframe for the initial GUI. Code is structured so the rendering backend is swappable behind a `FrontendRenderer` trait, allowing a future migration to raw winit + wgpu + glyphon without rewriting GUI behaviour.

## Why

- egui ships a working GUI in days, not months. The TUI is the primary test bed for core; the GUI is a parallel frontend and shouldn't gate core work on a custom renderer.
- egui's hidden cost — re-laying out the whole UI every frame — is real but only matters once a multi-GB file is open and frame-rate becomes a problem. We measure before optimising.
- The swappable-backend discipline is cheap to add now (one trait, one impl per backend) and expensive to add later (retrofit a renderer seam across every GUI component).

## Considered Options

- **Raw winit + wgpu + glyphon** — rejected for v0.1. Maximum control and steady-state performance, but months of plumbing before the first glyph. Revisit if egui's per-frame re-layout shows up in profiling on large files.
- **iced** — not considered in depth. Worth re-evaluating only if egui's immediate-mode model proves incompatible with viewport-only redraw on huge files.

## Consequences

- The GUI crate exposes a `FrontendRenderer` trait (or analogous) and the egui implementation lives behind it. Concrete `winit + wgpu + glyphon` impl is deferred, not built speculatively.
- File-level rendering decisions (e.g. "only redraw visible lines on huge files") become renderer-impl concerns, not frontend-behaviour concerns.
- egui's font/text machinery is used for v0.1; any future renderer must match its behaviour at the trait boundary, not at the egui API surface.