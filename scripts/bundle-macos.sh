#!/bin/sh
# Build a local development bundle; no installation or signing is performed.
set -eu
cd "$(dirname "$0")/.."
cargo build -p rowglide-ui --bin rowglide
bundle="target/Rowglide.app"
mkdir -p "$bundle/Contents/MacOS"
cp target/debug/rowglide "$bundle/Contents/MacOS/rowglide"
cat > "$bundle/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleName</key><string>Rowglide</string>
<key>CFBundleDisplayName</key><string>Rowglide</string>
<key>CFBundleIdentifier</key><string>dev.rowglide.app</string>
<key>CFBundleExecutable</key><string>rowglide</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleVersion</key><string>0.1.0</string>
<key>NSHighResolutionCapable</key><true/>
</dict></plist>
PLIST
printf '%s\n' "Built $bundle"
