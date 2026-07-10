#!/bin/bash
set -e
PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
APP_DIR="$PROJECT_ROOT/target/the_editor.app"
BIN="$PROJECT_ROOT/target/release/frontend_gui"
SCRIPT="$PROJECT_ROOT/scripts/the_editor.applescript"

cat > "$SCRIPT" <<APPLESCRIPT
on open fileList
	repeat with f in fileList
		set posixPath to POSIX path of f
		do shell script "$BIN " & quoted form of posixPath & " >/dev/null 2>&1 &"
	end repeat
end open

on run
	do shell script "$BIN >/dev/null 2>&1 &"
end run
APPLESCRIPT

rm -rf "$APP_DIR"
osacompile -o "$APP_DIR" "$SCRIPT"

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
    <string>com.smo.the-editor</string>
    <key>CFBundleIconFile</key>
    <string>droplet</string>
    <key>CFBundleIconName</key>
    <string>droplet</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>CFBundleName</key>
    <string>the_editor</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleSignature</key>
    <string>dplt</string>
    <key>LSMinimumSystemVersionByArchitecture</key>
    <dict>
        <key>x86_64</key>
        <string>10.6</string>
    </dict>
    <key>LSRequiresCarbon</key>
    <true/>
    <key>OSAAppletShowStartupScreen</key>
    <false/>
</dict>
</plist>
PLIST

echo "Created $APP_DIR"
