# ADR 0011: Publish ad-hoc-signed macOS releases

Status: accepted

## Context

Skerry does not yet have a Developer ID signing identity or Apple notarization
credentials, and obtaining them is not expected soon. Keeping a dormant
credential-dependent path in the release workflow has repeatedly prevented
otherwise complete macOS and Linux releases from being published.

An ad-hoc signature verifies bundle integrity but does not establish a trusted
developer identity or satisfy Gatekeeper for a quarantined download. Users must
make an explicit local trust decision before opening these builds.

## Decision

Publish macOS release archives with the ad-hoc signatures created by the app
bundle build script. Remove Developer ID signing, notarization, credential
detection, and signing-secret documentation from the release workflow.

Every GitHub release must prominently identify the macOS artifacts as
ad-hoc-signed and non-notarized, link to the manual Gatekeeper instructions,
and preserve that warning when assets are replaced. Homebrew does not remove
quarantine automatically.

## Consequences

- Releases no longer depend on unavailable Apple credentials.
- macOS and Linux artifacts can publish together from the same version tag.
- macOS users retain an explicit trust decision before running Skerry.
- The project does not provide an Apple-verified publisher identity.
- Developer ID signing and notarization require a future reviewed decision and
  corresponding workflow change.
