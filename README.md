# Skerry

Skerry is a fast, dual-frontend text editor written in Rust. Its GUI and TUI
share the same editing core and are designed to handle ordinary source files
and multi-gigabyte files in the same session.

## Install on macOS

```sh
brew tap sorenmat/skerry
brew trust sorenmat/skerry
brew install --cask skerry
```

Homebrew installs `Skerry.app` and the `sky` command:

```sh
sky path/to/file
```

The cask is maintained in the
[sorenmat/homebrew-skerry](https://github.com/sorenmat/homebrew-skerry) tap.

See [INSTALL.md](INSTALL.md) for release requirements, upgrades, uninstallation,
and local app-bundle builds.

## Build from source

```sh
cargo build --workspace
cargo test --workspace
```

Run the graphical or terminal frontend with:

```sh
cargo run -p skerry -- path/to/file
cargo run -p skerry-tui -- path/to/file
```

See [features.md](features.md) for the implemented feature set and
[CONTEXT.md](CONTEXT.md) for the architecture and shared terminology.
