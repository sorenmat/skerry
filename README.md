# Skerry

Skerry is a fast, dual-frontend text editor written in Rust. Its GUI and TUI
share the same editing core and are designed to handle ordinary source files
and multi-gigabyte files in the same session.

Visit the [Skerry website](https://sorenmat.github.io/skerry/) for an overview,
or continue below for installation and development details.

## Install with Homebrew

### macOS

```sh
brew tap sorenmat/skerry
brew trust sorenmat/skerry
brew install --cask skerry
```

The cask installs `Skerry.app`, the `sky` GUI command, and the `skerry-tui`
terminal frontend:

```sh
sky path/to/file
skerry-tui path/to/file
```

The cask is maintained in the
[sorenmat/homebrew-skerry](https://github.com/sorenmat/homebrew-skerry) tap.

### Linux

On x86_64 Linux, install the formula:

```sh
brew tap sorenmat/skerry
brew install skerry
```

The formula installs the GUI, the terminal frontend, and the same `sky` GUI
command:

```sh
sky path/to/file
skerry-tui path/to/file
```

### Opening an unsigned release

Current Skerry releases are ad-hoc signed but not Developer ID signed or
Apple-notarized. If you installed one from the Skerry project's official
`sorenmat/skerry` tap and accept that limitation, remove quarantine from this
copy and open it with:

```sh
xattr -dr com.apple.quarantine /Applications/Skerry.app
open /Applications/Skerry.app
```

Use `sudo xattr -dr com.apple.quarantine /Applications/Skerry.app` only if the
first command reports a permission error.

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
