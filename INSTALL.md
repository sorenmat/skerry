# Install Skerry

## Homebrew

Add the Skerry Homebrew tap, then install its cask:

```sh
brew tap sorenmat/skerry
brew trust sorenmat/skerry
brew install --cask skerry
```

Homebrew installs the application as `Skerry.app` and adds `sky` to its
binary directory. Open files from a terminal with:

```sh
sky path/to/file
```

Upgrade or uninstall with:

```sh
brew upgrade --cask --greedy-latest skerry
brew uninstall --cask skerry
```

The cask tracks the latest signed release, so upgrades use
`--greedy-latest`.

The canonical cask is maintained in the
[sorenmat/homebrew-skerry](https://github.com/sorenmat/homebrew-skerry) tap.

Skerry publishes native Apple Silicon and Intel builds. A maintainer creates
those release assets by pushing a version tag such as `v0.1.0`.

Release builds require these GitHub Actions secrets so Homebrew downloads pass
Gatekeeper:

- `APPLE_CERTIFICATE_BASE64`: base64-encoded Developer ID Application `.p12`
- `APPLE_CERTIFICATE_PASSWORD`: password for that `.p12`
- `APPLE_DEVELOPER_ID`: full `Developer ID Application: …` signing identity
- `APPLE_ID`: Apple account used for notarization
- `APPLE_APP_PASSWORD`: app-specific password for that account
- `APPLE_TEAM_ID`: Apple Developer team identifier

The release workflow fails before publishing if signing, notarization,
stapling, or Gatekeeper assessment fails.

## Build locally

```sh
make app-bundle
open target/Skerry.app
```

The local bundle contains the release build of the GUI executable and can be
moved without retaining a reference to the source checkout.
