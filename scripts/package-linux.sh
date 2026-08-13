#!/bin/bash
set -euo pipefail

PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PACKAGE_DIR="$PROJECT_ROOT/target/Skerry-linux-x86_64"
ARCHIVE="$PROJECT_ROOT/Skerry-linux-x86_64.tar.gz"

if [ ! -x "$PROJECT_ROOT/target/release/skerry" ]; then
    echo "error: Skerry GUI binary not found: target/release/skerry" >&2
    echo "       Run 'cargo build --workspace --release --locked' first." >&2
    exit 1
fi

if [ ! -x "$PROJECT_ROOT/target/release/skerry-tui" ]; then
    echo "error: Skerry TUI binary not found: target/release/skerry-tui" >&2
    echo "       Run 'cargo build --workspace --release --locked' first." >&2
    exit 1
fi

rm -rf "$PACKAGE_DIR"
mkdir -p "$PACKAGE_DIR/bin"
install -m 755 "$PROJECT_ROOT/target/release/skerry" "$PACKAGE_DIR/bin/skerry"
install -m 755 "$PROJECT_ROOT/target/release/skerry-tui" "$PACKAGE_DIR/bin/skerry-tui"
ln -s skerry "$PACKAGE_DIR/bin/sky"

rm -f "$ARCHIVE"
tar -czf "$ARCHIVE" -C "$PROJECT_ROOT/target" Skerry-linux-x86_64

echo "Created $ARCHIVE"
