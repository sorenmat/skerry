#!/bin/bash
set -euo pipefail

PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
APP_DIR="$PROJECT_ROOT/target/Skerry.app"
BIN="${SKERRY_BINARY:-$PROJECT_ROOT/target/release/skerry}"
TUI_BIN="${SKERRY_TUI_BINARY:-$PROJECT_ROOT/target/release/skerry-tui}"
SCRIPT="$PROJECT_ROOT/scripts/Skerry.applescript"
ICON_SOURCE="$PROJECT_ROOT/assets/Skerry-icon.png"
ICONSET_DIR="$PROJECT_ROOT/target/Skerry.iconset"

if [ -n "${SKERRY_VERSION:-}" ]; then
    VERSION="$SKERRY_VERSION"
else
    PACKAGE_ID="$(cargo pkgid -p skerry --manifest-path "$PROJECT_ROOT/Cargo.toml")"
    VERSION="${PACKAGE_ID##*#}"
fi

if [ ! -x "$BIN" ]; then
    echo "error: Skerry binary not found or not executable: $BIN" >&2
    echo "       Run 'make build-release' first." >&2
    exit 1
fi

if [ ! -x "$TUI_BIN" ]; then
    echo "error: Skerry TUI binary not found or not executable: $TUI_BIN" >&2
    echo "       Run 'make build-release' first." >&2
    exit 1
fi

if [ ! -f "$ICON_SOURCE" ]; then
    echo "error: Skerry icon source not found: $ICON_SOURCE" >&2
    exit 1
fi

if [ -z "$VERSION" ]; then
    echo "error: Skerry version could not be read from Cargo.toml" >&2
    exit 1
fi

if [[ ! "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    echo "error: invalid Skerry bundle version: $VERSION" >&2
    exit 1
fi

rm -rf "$APP_DIR"
osacompile -o "$APP_DIR" "$SCRIPT"
install -m 755 "$BIN" "$APP_DIR/Contents/Resources/skerry"
install -m 755 "$TUI_BIN" "$APP_DIR/Contents/Resources/skerry-tui"

rm -rf "$ICONSET_DIR"
mkdir -p "$ICONSET_DIR"
sips -z 16 16 "$ICON_SOURCE" --out "$ICONSET_DIR/icon_16x16.png" >/dev/null
sips -z 32 32 "$ICON_SOURCE" --out "$ICONSET_DIR/icon_16x16@2x.png" >/dev/null
sips -z 32 32 "$ICON_SOURCE" --out "$ICONSET_DIR/icon_32x32.png" >/dev/null
sips -z 64 64 "$ICON_SOURCE" --out "$ICONSET_DIR/icon_32x32@2x.png" >/dev/null
sips -z 128 128 "$ICON_SOURCE" --out "$ICONSET_DIR/icon_128x128.png" >/dev/null
sips -z 256 256 "$ICON_SOURCE" --out "$ICONSET_DIR/icon_128x128@2x.png" >/dev/null
sips -z 256 256 "$ICON_SOURCE" --out "$ICONSET_DIR/icon_256x256.png" >/dev/null
sips -z 512 512 "$ICON_SOURCE" --out "$ICONSET_DIR/icon_256x256@2x.png" >/dev/null
sips -z 512 512 "$ICON_SOURCE" --out "$ICONSET_DIR/icon_512x512.png" >/dev/null
install -m 644 "$ICON_SOURCE" "$ICONSET_DIR/icon_512x512@2x.png"
iconutil -c icns "$ICONSET_DIR" -o "$APP_DIR/Contents/Resources/Skerry.icns"

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
    <string>com.smo.skerry</string>
    <key>CFBundleIconFile</key>
    <string>Skerry.icns</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>CFBundleName</key>
    <string>Skerry</string>
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

plutil -replace CFBundleShortVersionString -string "$VERSION" "$APP_DIR/Contents/Info.plist"
plutil -replace CFBundleVersion -string "$VERSION" "$APP_DIR/Contents/Info.plist"

codesign --force --sign - "$APP_DIR/Contents/Resources/skerry"
codesign --force --sign - "$APP_DIR/Contents/Resources/skerry-tui"
codesign --force --sign - "$APP_DIR"
codesign --verify --deep --strict "$APP_DIR"

echo "Created $APP_DIR"
