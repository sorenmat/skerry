# AGENTS.md

## Engineering rules

- **Zero warnings.** The workspace must build with no compiler or clippy
  warnings: `cargo check --workspace --all-targets` and
  `cargo clippy --workspace --all-targets` stay silent. Any change that
  would introduce a warning (dead code, an over-complex type, anything
  else) fixes it in the same change — delete the dead code, name the
  type, or address the lint. Never leave a warning for the next commit.
