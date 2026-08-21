//! Hot-path benchmarks for the Skerry core.
//!
//! Run with `cargo bench -p core`. Numbers are reported in the commit
//! message of any change that touches these paths (repo convention).
//!
//! The three groups cover the per-frame / per-keystroke / per-search
//! costs:
//!
//! - `piece_table` — edit costs on a document with realistic piece
//!   counts (after a session of scattered edits), plus the O(n)
//!   `to_bytes` copy a fragmented buffer pays.
//! - `search_10mb` — find-bar refresh on a 10 MB haystack for each
//!   search mode (case-sensitive literal, case-insensitive literal,
//!   regex). The needle appears ~5k times so every path scans the full
//!   document without hitting the 10k match cap.
//! - `highlight` — tree-sitter parse and highlight costs on a 100k-line
//!   Rust file: initial parse (file open), per-line highlight (cache
//!   miss), 40-line viewport highlight (scroll), and the per-keystroke
//!   insert + incremental reparse cycle.

use std::path::{Path, PathBuf};

use core::ts::EditDelta;
use core::{Buffer, PieceTableBuffer};
use criterion::{black_box, criterion_group, criterion_main, BatchSize, Criterion};

// ---------- document builders ----------

/// Synthetic Rust source: `num_fns` small functions, 5 lines each
/// (including the blank separator).
fn rust_source(num_fns: usize) -> Vec<u8> {
    let mut out = String::new();
    out.reserve(num_fns * 72);
    for i in 0..num_fns {
        out.push_str(&format!(
            "fn item_{i}() -> u32 {{\n    let value = {i} * 2 + 1;\n    value * 3\n}}\n\n"
        ));
    }
    out.into_bytes()
}

/// A 100k-line Rust document for the highlight benches (~1.2 MB).
fn highlight_doc_content() -> Vec<u8> {
    rust_source(20_000)
}

/// Simulate an editing session: `edits` single-char inserts scattered
/// through the document, so the piece table has the piece count a real
/// session has (each insert fragments up to 3 pieces and the inserts
/// never coalesce with the original-source neighbors).
fn session_buffer(content: Vec<u8>, edits: usize) -> PieceTableBuffer {
    let mut buf = PieceTableBuffer::from_bytes(content);
    if buf.len() == 0 {
        return buf;
    }
    for i in 0..edits {
        let pos = (i * buf.len() / (edits + 1)).min(buf.len().saturating_sub(1));
        buf.insert(pos, "x").unwrap();
    }
    buf
}

/// Heavily fragmented buffer: ~`kb` KB of text with a one-char insert
/// every 250 bytes, so a half-document delete spans ~1000+ pieces.
fn fragmented_buffer(kb: usize) -> PieceTableBuffer {
    let mut content = String::new();
    for i in 0..kb {
        content.push_str(&format!(
            "line {:04} some padding text to fill the byte budget here\n",
            i
        ));
    }
    let mut buf = PieceTableBuffer::from_bytes(content.into_bytes());
    let mut pos = 250;
    while pos < buf.len() {
        buf.insert(pos, "x").unwrap();
        pos += 250;
    }
    buf
}

/// 10 MB haystack with the needle "zygote" once per ~2 KB (~5k
/// occurrences — under the 10k stored-match cap, so every search mode
/// scans the whole document).
fn search_haystack(target_bytes: usize) -> Vec<u8> {
    const NEEDLE: &str = " zygote ";
    let filler = "the quick brown fox jumps over the lazy dog; the quick brown fox jumps \
                  over the lazy dog again with only filler and a steady stream of harmless \
                  text and nothing of interest in sight at all, just more padding";
    let mut chunk = String::with_capacity(2048);
    while chunk.len() < 2040 {
        chunk.push_str(filler);
    }
    chunk.truncate(2040);
    chunk.insert_str(chunk.len() / 2, NEEDLE);
    let reps = target_bytes.div_ceil(chunk.len());
    let mut out = String::with_capacity(reps * chunk.len());
    for _ in 0..reps {
        out.push_str(&chunk);
    }
    out.into_bytes()
}

// ---------- piece table ----------

fn piece_table_benches(c: &mut Criterion) {
    let mut group = c.benchmark_group("piece_table");

    // One keystroke mid-document in a file with a realistic piece count
    // (100k lines after 2k scattered edits).
    let content = rust_source(20_000);
    let mut buf = session_buffer(content, 2_000);
    let pos = buf.len() / 2;
    group.bench_function("insert_mid_100k_lines", |b| {
        b.iter(|| {
            let p = buf.insert(black_box(pos), "x").unwrap();
            black_box(p)
        })
    });

    // Deleting half a heavily fragmented document — the O(k^2) case
    // (k = pieces spanned).
    group.bench_function("delete_multipiece", |b| {
        b.iter_batched(
            || fragmented_buffer(100),
            |mut buf| {
                let half = buf.len() / 2;
                let p = buf.delete(0..half).unwrap();
                black_box(p)
            },
            BatchSize::NumIterations(1),
        )
    });

    // Full-document copy of a fragmented buffer — what per-keystroke
    // callers used to pay for search.
    group.bench_function("to_bytes_fragmented", |b| {
        b.iter_batched(
            || fragmented_buffer(100),
            |buf| black_box(buf.to_bytes()),
            BatchSize::NumIterations(1),
        )
    });

    group.finish();
}

// ---------- search ----------

fn search_benches(c: &mut Criterion) {
    let haystack = search_haystack(10 * 1024 * 1024);
    let mut group = c.benchmark_group("search_10mb");

    group.bench_function("literal_case_sensitive", |b| {
        let mut s = core::Search::new();
        s.query = "zygote".to_string();
        s.case_sensitive = true;
        b.iter(|| {
            s.refresh(black_box(&haystack));
            black_box(s.matches.len())
        })
    });

    group.bench_function("literal_case_insensitive", |b| {
        let mut s = core::Search::new();
        s.query = "zygote".to_string();
        s.case_sensitive = false;
        b.iter(|| {
            s.refresh(black_box(&haystack));
            black_box(s.matches.len())
        })
    });

    group.bench_function("regex_mode", |b| {
        let mut s = core::Search::new();
        s.query = "zygote".to_string();
        s.regex_mode = true;
        b.iter(|| {
            s.refresh(black_box(&haystack));
            black_box(s.matches.len())
        })
    });

    group.finish();
}

// ---------- tree-sitter highlighting ----------

fn make_doc(content: &[u8]) -> core::Document {
    let buf =
        PieceTableBuffer::from_bytes_with_path(content.to_vec(), PathBuf::from("bench_fixture.rs"));
    core::Document::new(Box::new(buf))
}

fn highlight_benches(c: &mut Criterion) {
    let content = highlight_doc_content();
    let grammar = core::ts::grammar_for_path(Some(Path::new("fixture.rs"))).unwrap();
    let theme = core::ts::bundled_themes()[0];

    // Initial full parse (file-open cost).
    c.bench_function("highlight/initial_parse", |b| {
        b.iter(|| {
            let mut tree = core::ts::DocTree::new(grammar.clone()).unwrap();
            tree.parse(black_box(&content));
            black_box(tree.tree().is_some())
        })
    });

    let mut doc = make_doc(&content);

    // One line's highlight (the per-cache-miss cost the renderer pays).
    c.bench_function("highlight/per_line", |b| {
        b.iter(|| {
            let (segs, complete) = doc.highlight_lines_ts(50_000, 50_001, &theme);
            black_box((segs, complete))
        })
    });

    // A 40-line viewport highlight (scroll cost).
    c.bench_function("highlight/viewport_40_lines", |b| {
        b.iter(|| {
            let (segs, complete) = doc.highlight_lines_ts(50_000, 50_040, &theme);
            black_box((segs, complete))
        })
    });

    // Per-keystroke cost: buffer edit + incremental reparse. Alternates
    // insert/delete of one char at a fixed position so the document
    // size stays steady.
    let pos = {
        let (line, col) = doc.buffer.pos_to_linecol(600_000).unwrap();
        let _ = (line, col);
        600_000
    };
    let line = doc.buffer.pos_to_linecol(pos).unwrap().0;
    let col = doc.buffer.pos_to_linecol(pos).unwrap().1;
    let mut have_extra = false;
    c.bench_function("highlight/keystroke_reparse", |b| {
        b.iter(|| {
            if !have_extra {
                doc.buffer.insert(pos, "x").unwrap();
                doc.apply_ts_edit(EditDelta::single_line(line, col, pos, 1));
            } else {
                doc.buffer.delete(pos..pos + 1).unwrap();
                doc.apply_ts_edit(EditDelta::single_line(line, col, pos, -1));
            }
            have_extra = !have_extra;
            black_box(doc.buffer.revision())
        })
    });
}

criterion_group!(
    benches,
    piece_table_benches,
    search_benches,
    highlight_benches
);
criterion_main!(benches);
