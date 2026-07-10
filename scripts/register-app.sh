#!/bin/bash
# Set target/the_editor.app as the default handler for .rs, .go, and .json.
#
# macOS will not let an app claim a default extension just by declaring it in
# Info.plist. We register the bundle with Launch Services, launch it once so the
# bundle id binds, then use duti to actually claim the default role. duti is
# auto-installed via Homebrew if it is missing.

set -e

PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
APP_DIR="$PROJECT_ROOT/target/the_editor.app"
BUNDLE_ID="com.smo.the-editor"
EXTENSIONS="rs go json"

LSREGISTER="/System/Library/Frameworks/CoreServices.framework/Versions/A/Frameworks/LaunchServices.framework/Versions/A/Support/lsregister"

if [ ! -d "$APP_DIR" ]; then
	echo "error: $APP_DIR not found. Run 'make app-bundle' first." >&2
	exit 1
fi

# 1. Register the bundle with Launch Services.
"$LSREGISTER" -u "$APP_DIR" 2>/dev/null || true
"$LSREGISTER" -f "$APP_DIR"

# 2. Launch once so Launch Services fully binds the bundle identifier.
open -a "$APP_DIR" 2>/dev/null || true

# 3. Ensure duti is available; install it via Homebrew if missing.
if ! command -v duti >/dev/null 2>&1; then
	if ! command -v brew >/dev/null 2>&1; then
		echo "error: duti is not installed and Homebrew is unavailable." >&2
		echo "       Install duti manually:  brew install duti" >&2
		exit 1
	fi
	echo "Installing duti via Homebrew…"
	brew install duti
fi

# 4. Claim the default role for each extension.
for ext in $EXTENSIONS; do
	if duti -s "$BUNDLE_ID" ".$ext" all; then
		echo "  default for .$ext -> $BUNDLE_ID"
	else
		echo "  warning: could not set default for .$ext" >&2
	fi
done
