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
`--greedy-latest`. Releases are intended to be signed; release notes identify
any unsigned exception.

The canonical cask and formula are maintained in the
[sorenmat/homebrew-skerry](https://github.com/sorenmat/homebrew-skerry) tap.

## Opening an unsigned release

Unsigned Skerry releases, including v0.1.2 and v0.1.6, are ad-hoc signed but
not Developer ID signed or Apple-notarized, so macOS Gatekeeper may refuse to
open them. If you installed one from the Skerry project's official
`sorenmat/skerry` tap and accept that limitation, remove quarantine only from
the installed Skerry application and open it:

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

Signed release builds use these GitHub Actions secrets so Homebrew downloads
pass Gatekeeper:

- `APPLE_CERTIFICATE_BASE64`: base64-encoded Developer ID Application `.p12`
- `APPLE_CERTIFICATE_PASSWORD`: password for that `.p12`
- `APPLE_DEVELOPER_ID`: full `Developer ID Application: …` signing identity
- `APPLE_ID`: Apple account used for notarization
- `APPLE_APP_PASSWORD`: app-specific password for that account
- `APPLE_TEAM_ID`: Apple Developer team identifier

When all six secrets are configured, the workflow fails before publishing if
signing, notarization, stapling, or Gatekeeper assessment fails. v0.1.6 is an
explicitly labelled exception that may publish ad-hoc-signed when none are
configured. A partial credential configuration, or missing credentials for a
later release, always fails.

## Build locally

```sh
make app-bundle
open target/Skerry.app
```

The local bundle contains release builds of both the GUI and TUI executables
and can be moved without retaining a reference to the source checkout.
