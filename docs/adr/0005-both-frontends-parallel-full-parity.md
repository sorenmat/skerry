# Frontend scope: both in parallel, full feature parity

Both TUI and GUI ship from day one and must achieve full feature parity. The core trait is the contract — both frontends work against it independently and achieve parity by satisfying the same API, not by synchronising feature work.

## Why

Stated user preference. The architectural assumption that makes this work: core is the single source of truth for behaviour, both frontends are interchangeable renderers of the same document.

## Consequences

- **Selection, save/dirty, multi-buffer/workspace, and frontend-agnostic input events move into core scope from day one.** They are no longer "later if needed" — full parity forces them in.
- The `Buffer` trait must grow over time to host these. Sketches done before this decision (e.g. cursor-less trait) need to be revised.
- The TUI is no longer a throwaway test bed. It is a first-class product surface with the same feature requirements as the GUI.
- Solo maintainer risk: this is two-frontends' worth of work. If scope pressure builds, the escape hatch is to drop the TUI to read-only / search-only, but that violates parity — flag it as a real trade-off, not a free option.