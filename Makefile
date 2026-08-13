# Skerry Makefile
#
# Thin convenience layer over Cargo. Run `make help` for the full list.
#
# Pass extra args to the binary via ARGS:
#   make run_tui ARGS="Cargo.toml"
#   make run_gui ARGS="src/lib.rs"

.PHONY: help run_tui run_gui build build-release test check clippy fmt clean app-bundle linux-package register-app

help:
	@echo "Skerry — convenience make targets"
	@echo ""
	@echo "  make run_tui        Run the TUI frontend (skerry-tui crate)"
	@echo "  make run_gui        Run the GUI frontend (skerry crate)"
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
	@echo ""
	@echo "macOS app bundle:"
	@echo "  make app-bundle     Build target/Skerry.app with GUI and TUI binaries"
	@echo "  make register-app   Build bundle and set it as default app for .rs, .go, .json"
	@echo ""
	@echo "Release packages:"
	@echo "  make linux-package  Build Skerry-linux-x86_64.tar.gz from release binaries"

run_tui:
	cargo run -p skerry-tui -- $(ARGS)

run_gui:
	cargo run -p skerry -- $(ARGS)

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

app-bundle: build-release
	@./scripts/build-app-bundle.sh

linux-package: build-release
	@bash scripts/package-linux.sh

register-app: app-bundle
	@./scripts/register-app.sh
	@echo "Set target/Skerry.app as default for .rs, .go, and .json"
