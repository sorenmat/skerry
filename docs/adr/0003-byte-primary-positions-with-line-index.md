# Core API positions: byte-primary with line index

The core API exposes positions as UTF-8 byte offsets internally, with conversion helpers to/from `(line, column)` pairs backed by a side index. Frontends pick whichever representation is convenient; the core never deals in characters, only in bytes.

## Why

- Byte offset is the natural unit for the Piece Table's descriptor array — `insert`/`delete` already work in bytes, no conversion needed at the hot path.
- Line/column is what humans and frontends care about. Naive byte→line conversion is O(n) on the file, which kills the multi-GB promise. The line index buys O(log n) (or amortised O(1)) conversion.
- UTF-8 byte offsets are unambiguous on disk and in memory; Unicode scalar offsets would force conversion at every edit and re-introduce the O(n) cost.

## Consequences

- The line index is a load-bearing side structure. It must be kept consistent with the delta on every edit. Design choice: incremental update vs. lazy rebuild on demand — deferred.
- Frontends handle grapheme clusters themselves if they care about emoji and combining marks; the core does not. This matches what `&str` slicing naturally supports and avoids forcing the core to maintain a grapheme index.
- Any future feature that wants "character offsets" (e.g. column-based selection in a frontend) has to convert at the boundary; the core never exposes them.