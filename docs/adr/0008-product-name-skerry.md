# ADR 0008: Use Skerry as the product name

Status: accepted

## Context

The editor previously shipped as Nova. That name is already used by an
established macOS code editor, which creates ambiguity in application search,
Homebrew instructions, release assets, and user conversations.

The replacement name needs to be distinct from existing editors, suitable for
both the GUI and TUI, and usable consistently as a macOS application, Rust
crate, executable, repository, and Homebrew cask.

## Decision

The product is named **Skerry**. Public identifiers use `skerry`, including the
GUI executable and crate, `skerry-tui`, `Skerry.app`, the `skerry` Homebrew
cask, and `com.smo.skerry`. Homebrew also installs `sky` as the short command
for opening the GUI from a terminal. The canonical cask lives in the separate
`sorenmat/homebrew-skerry` tap repository so `brew tap sorenmat/skerry`
follows Homebrew's repository naming convention.

Skerry reads configuration from `~/.config/skerry`. When that file is absent,
it falls back to the previous `nova` and `the_editor` locations in that order
so existing settings and session state survive the rename.

## Consequences

- The product no longer collides by name with the existing Nova editor.
- Release, packaging, documentation, and source identifiers share one name.
- Existing scripts using `nova`, `nova-tui`, or `nv` must switch to `skerry`,
  `skerry-tui`, or `sky`.
- A new GitHub release is required because previously published asset names do
  not match the Skerry cask URL.
