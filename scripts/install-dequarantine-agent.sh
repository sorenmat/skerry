#!/bin/bash
# Install a per-user LaunchAgent that strips Gatekeeper quarantine from
# Skerry.app whenever it reappears. Skerry's release builds are ad-hoc
# signed (not Apple-notarized), so every reinstall or `brew upgrade`
# re-quarantines the app and macOS blocks the first launch.
#
# If you only ever install via Homebrew, `HOMEBREW_CASK_OPTS=--no-quarantine`
# (see INSTALL.md) is enough; this agent additionally covers drag-and-drop
# installs from a downloaded release tarball.
#
# Why polling instead of WatchPaths: macOS does not deliver filesystem
# events on /Applications to user LaunchAgents (verified empirically), so
# the agent runs every 15 seconds. The check walks the app bundle's
# extended attributes and exits in milliseconds when nothing is
# quarantined.
#
# Re-runnable: overwrites the script and agent, then reloads.
# Uninstall:
#   launchctl bootout "gui/$(id -u)"/com.smo.skerry.dequarantine
#   rm -f "$HOME/Library/LaunchAgents/com.smo.skerry.dequarantine.plist" \
#         "$HOME/Library/Application Support/Skerry/strip-quarantine.sh"
set -euo pipefail

LABEL="com.smo.skerry.dequarantine"
APP="${SKERRY_APP:-/Applications/Skerry.app}"
APP_SUPPORT="$HOME/Library/Application Support/Skerry"
SCRIPT_PATH="$APP_SUPPORT/strip-quarantine.sh"
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"
UID_="$(id -u)"

mkdir -p "$APP_SUPPORT" "$HOME/Library/LaunchAgents" "$HOME/Library/Logs"

cat > "$SCRIPT_PATH" <<'SCRIPT'
#!/bin/bash
# Strip Gatekeeper quarantine from Skerry after reinstall/upgrade.
# Managed by install-dequarantine-agent.sh — edit there, not here.
APP="${1:-/Applications/Skerry.app}"
LOG="$HOME/Library/Logs/skerry-dequarantine.log"

[ -d "$APP" ] || exit 0
# Quarantine can land on the bundle root or nested files; check the
# whole tree so a partial strip never leaves Gatekeeper armed.
if xattr -r -l "$APP" 2>/dev/null | grep -q com.apple.quarantine; then
    xattr -dr com.apple.quarantine "$APP" 2>/dev/null
    echo "$(date '+%Y-%m-%d %H:%M:%S') stripped quarantine from $APP" >>"$LOG"
fi
SCRIPT
chmod +x "$SCRIPT_PATH"

cat > "$PLIST" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>$LABEL</string>
    <key>ProgramArguments</key>
    <array>
        <string>/bin/bash</string>
        <string>$SCRIPT_PATH</string>
    </array>
    <key>StartInterval</key>
    <integer>15</integer>
    <key>RunAtLoad</key>
    <true/>
</dict>
</plist>
PLIST

launchctl bootout "gui/$UID_/$LABEL" 2>/dev/null || true
launchctl bootstrap "gui/$UID_" "$PLIST"

echo "Installed $LABEL"
echo "  watches:  $APP (checked every 15s + at login)"
echo "  log:      ~/Library/Logs/skerry-dequarantine.log"
