# the_editor Makefile
#
# Thin convenience layer over Cargo. Run `make help` for the full list.
#
# Pass extra args to the binary via ARGS:
#   make run_tui ARGS="Cargo.toml"
#   make run_gui ARGS="src/lib.rs"

.PHONY: help run_tui run_gui build build-release test check clippy fmt clean

help:
	@echo "the_editor — convenience make targets"
	@echo ""
	@echo "  make run_tui        Run the TUI frontend (frontend_tui crate)"
	@echo "  make run_gui        Run the GUI frontend (frontend_gui crate)"
	@echo ""
	@echo "  Pass extra args to the binary via ARGS:"
	@echo "      make run_tui ARGS=\"Cargo.toml\""
	@echo ""
	@echo "Build / test / lint:"
	@echo "  make build          cargo build --workspace (debug)"
	@echo "  make build-release  cargo build --workspace --release"
	@echo "  make test           cargo test --workspace"
	@echo "  make check          cargo check --workspace --all-targets"
	@echo "  make clippy         cargo clippy --workspace --all-targets -- -D warnings"
	@echo "  make fmt            cargo fmt --all"
	@echo "  make clean          cargo clean"

run_tui:
	cargo run -p frontend_tui -- $(ARGS)

run_gui:
	cargo run -p frontend_gui -- $(ARGS)

build:
	cargo build --workspace

build-release:
	cargo build --workspace --release

test:
	cargo test --workspace

check:
	cargo check --workspace --all-targets

clippy:
	cargo clippy --workspace --all-targets -- -D warnings

fmt:
	cargo fmt --all

clean:
	cargo clean