# Piece Table as primary buffer structure

We chose Piece Table as the initial `Buffer` impl over Gap Buffer and Rope because it gives O(1) insert/delete anywhere with the smallest code surface, and naturally append-only undo. The decision is locked behind a `Buffer` trait so future impls (`ChunkedPieceTable`, mmap-backed variants, etc.) can be added without rewriting frontends.

## Considered Options

- **Gap Buffer** — rejected. Per-keystroke byte shift is proportional to file size, so the multi-GB file requirement makes it an algorithmic mismatch, not just a complexity tax.
- **Rope** — rejected for now. Genuinely better for multi-GB-heavy workloads and used by Zed/JetBrains, but larger code surface and weaker undo story. Revisit if profiling shows Piece Table doesn't scale for the target workload.

## Consequences

- The original plan's Phase 5 `memmap2` step is no longer an optimization — it's a Phase 1 architecture concern (see follow-up decision on memory strategy).
- The `Buffer` trait becomes the load-bearing abstraction. Frontends must never read text state through anything other than `Buffer` methods.