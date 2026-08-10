#!/bin/bash
set -euo pipefail

PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
APP_DIR="$PROJECT_ROOT/target/Nova.app"
BIN="${NOVA_BINARY:-$PROJECT_ROOT/target/release/nova}"
SCRIPT="$PROJECT_ROOT/scripts/Nova.applescript"

if [ ! -x "$BIN" ]; then
    echo "error: Nova binary not found or not executable: $BIN" >&2
    echo "       Run 'make build-release' first." >&2
    exit 1
fi

rm -rf "$APP_DIR"
osacompile -o "$APP_DIR" "$SCRIPT"
install -m 755 "$BIN" "$APP_DIR/Contents/Resources/nova"

cat > "$APP_DIR/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleAllowMixedLocalizations</key>
    <true/>
    <key>CFBundleDevelopmentRegion</key>
    <string>en</string>
    <key>CFBundleDocumentTypes</key>
    <array>
        <dict>
            <key>CFBundleTypeName</key>
            <string>Rust source code</string>
            <key>CFBundleTypeExtensions</key>
            <array>
                <string>rs</string>
            </array>
            <key>CFBundleTypeRole</key>
            <string>Editor</string>
        </dict>
        <dict>
            <key>CFBundleTypeName</key>
            <string>Go source code</string>
            <key>CFBundleTypeExtensions</key>
            <array>
                <string>go</string>
            </array>
            <key>CFBundleTypeRole</key>
            <string>Editor</string>
        </dict>
        <dict>
            <key>CFBundleTypeName</key>
            <string>JSON file</string>
            <key>CFBundleTypeExtensions</key>
            <array>
                <string>json</string>
            </array>
            <key>CFBundleTypeRole</key>
            <string>Editor</string>
        </dict>
    </array>
    <key>CFBundleExecutable</key>
    <string>droplet</string>
    <key>CFBundleIdentifier</key>
    <string>com.smo.nova</string>
    <key>CFBundleIconFile</key>
    <string>droplet</string>
    <key>CFBundleIconName</key>
    <string>droplet</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>CFBundleName</key>
    <string>Nova</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleSignature</key>
    <string>dplt</string>
    <key>LSMinimumSystemVersion</key>
    <string>11.0</string>
    <key>OSAAppletShowStartupScreen</key>
    <false/>
</dict>
</plist>
PLIST

codesign --force --sign - "$APP_DIR/Contents/Resources/nova"
codesign --force --sign - "$APP_DIR"
codesign --verify --deep --strict "$APP_DIR"

echo "Created $APP_DIR"
