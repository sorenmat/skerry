# Install Skerry

## Homebrew

Add the Skerry Homebrew tap, then install the package for your platform.

On macOS, install the cask:

```sh
brew tap sorenmat/skerry
brew trust sorenmat/skerry
brew install --cask skerry
```

The cask installs the application as `Skerry.app` and adds the `sky` GUI
command and `skerry-tui` terminal frontend to Homebrew's binary directory:

```sh
sky path/to/file
skerry-tui path/to/file
```

On x86_64 Linux, install the formula:

```sh
brew tap sorenmat/skerry
brew install skerry
```

The formula installs `skerry`, `skerry-tui`, and `sky`:

```sh
skerry path/to/file
sky path/to/file
skerry-tui path/to/file
```

Upgrade or uninstall with:

```sh
brew upgrade --cask skerry
brew uninstall --cask skerry
brew upgrade skerry
brew uninstall skerry
```

macOS releases are currently ad-hoc signed and not Apple-notarized.

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

### Avoiding the quarantine dance on every upgrade

If you install or upgrade via Homebrew, let brew strip the flag as part of
the install instead of doing it by hand:

```sh
echo 'export HOMEBREW_CASK_OPTS="--no-quarantine"' >> ~/.zshrc
```

New shells (and the `brew upgrade --cask skerry` you run from them) no
longer apply quarantine to Skerry — or to any other cask, so skip this if
you would rather keep Gatekeeper checks for other apps.

To also cover manual installs from a downloaded release tarball, the repo
ships a per-user LaunchAgent installer. It generates a launchd job that
checks Skerry.app every 15 seconds and strips quarantine whenever it
reappears (macOS does not deliver filesystem events on `/Applications` to
user agents, so event-driven watching is not possible):

```sh
git clone https://github.com/sorenmat/skerry
./skerry/scripts/install-dequarantine-agent.sh
```

Each strip is recorded in `~/Library/Logs/skerry-dequarantine.log`. Undo
with the `launchctl bootout` command printed by the installer.

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
