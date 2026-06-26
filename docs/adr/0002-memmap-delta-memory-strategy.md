# memmap + delta as memory strategy

We chose memmap+delta over pure in-memory, threshold switch, and chunked Piece Table. The original file is memory-mapped via `memmap2`; only unsaved edits enter the Piece Table's append buffer. Reads are zero-copy from the mmap; writes go to the delta. On save, concatenate the mmap slice and the delta.

## Why

Piece Table is already shaped as "original source + append buffer" — mmap'ing the original source is a near-zero architectural change. The "edit overlay" of memmap+delta is exactly the Piece Table's existing edit logic. This promotes `memmap2` from Phase 5 (original plan) to Phase 1 architecture.

## Considered Options

- **Pure in-memory** — rejected. Fights "limited resources" for any file larger than the RAM budget.
- **Threshold switch** — rejected. Pays the boundary tax twice (testing, reasoning, user-visible behavior) without buying anything the `Buffer` trait boundary doesn't already give us.
- **Chunked Piece Table** — rejected for now. Most uniform but interacts poorly with `&str`-style access APIs; revisit if profiling shows memmap+delta isn't enough.

## Consequences

- The `Buffer` trait must distinguish the read-only base source (the mmap) from the writable delta. Frontends read through the trait and never see the difference.
- Save semantics need explicit design: concatenate mmap + delta, or rewrite the whole file? Deferred.
- `memmap2` is now a Phase 1 dependency in `core`, not a Phase 5 optimisation.