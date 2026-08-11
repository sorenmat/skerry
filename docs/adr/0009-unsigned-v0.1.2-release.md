# ADR 0009: Publish v0.1.2 without Apple notarization

Status: accepted

## Context

Skerry needs its first release under the new product name so the public
Homebrew tap can resolve architecture-specific application archives. The
project does not yet have a Developer ID signing identity or Apple
notarization credentials, and the product owner explicitly requested that
v0.1.2 be published without them.

An ad-hoc signature verifies bundle integrity but does not establish a trusted
developer identity. It also does not satisfy Gatekeeper after downloading a
quarantined application from the internet.

## Decision

Publish v0.1.2 as a one-off unsigned release. Both macOS archives remain
ad-hoc signed by the bundle build script, and the GitHub release prominently
states that they are not Developer ID signed or Apple-notarized.

The automated release workflow continues to require the Apple credentials.
Future releases should use that signed and notarized path rather than treating
v0.1.2 as a new default.

## Consequences

- Homebrew can discover and download v0.1.2 for Apple Silicon and Intel Macs.
- Gatekeeper may block the downloaded application until the user makes an
  explicit local trust decision.
- The release does not provide Apple-verified publisher identity.
- A later signed release can replace this exception without changing the
  cask or artifact naming contract.
