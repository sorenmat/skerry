# Install Skerry

## Homebrew

Add the Skerry Homebrew tap, then install the package for your platform.

On macOS, install the cask:

```sh
brew tap sorenmat/skerry
brew trust sorenmat/skerry
brew install --cask skerry
```

The current cask installs the application as `Skerry.app` and adds the `sky`
GUI command to Homebrew's binary directory:

```sh
sky path/to/file
```

The next release package also contains the `skerry-tui` terminal frontend.
It will become available from Homebrew when the matching cask update is
published.

Linux release archives contain `skerry`, `skerry-tui`, and `sky`:

```sh
skerry path/to/file
sky path/to/file
skerry-tui path/to/file
```

A Linux formula will be added to the tap with the next release. The current
tap does not yet support `brew install skerry` on Linux.

Upgrade or uninstall with:

```sh
brew upgrade --cask --greedy-latest skerry
brew uninstall --cask skerry
```

The macOS cask tracks the latest release, so macOS cask upgrades use
`--greedy-latest`. macOS releases are currently ad-hoc signed and not
Apple-notarized.

The canonical cask and formula are maintained in the
[sorenmat/homebrew-skerry](https://github.com/sorenmat/homebrew-skerry) tap.

## Opening an unsigned release

Current Skerry releases are ad-hoc signed but not Developer ID signed or
Apple-notarized, so macOS Gatekeeper may refuse to open them. If you installed
one from the Skerry project's official `sorenmat/skerry` tap and accept that
limitation, remove quarantine only from the installed Skerry application and
open it:

```sh
xattr -dr com.apple.quarantine /Applications/Skerry.app
open /Applications/Skerry.app
```

If the quarantine command reports a permission error, retry only that command
with elevated privileges:

```sh
sudo xattr -dr com.apple.quarantine /Applications/Skerry.app
```

This trust decision applies only to the current installed copy. A later
Homebrew upgrade may require it again until releases are Developer ID signed
and notarized.

Skerry publishes native Apple Silicon, Intel macOS, and Linux x86_64 builds. A
maintainer creates those release assets by pushing a version tag such as
`v0.1.0`.

The release workflow requires no Apple credentials. It publishes a warning on
every GitHub release and preserves that warning when release assets are
replaced. Developer ID signing and notarization can be reintroduced later as a
separately reviewed distribution change.

## Build locally

```sh
make app-bundle
open target/Skerry.app
```

The local bundle contains release builds of both the GUI and TUI executables
and can be moved without retaining a reference to the source checkout.
