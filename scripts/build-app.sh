#!/bin/sh
# Build Docket.app (release) in target/Docket.app.
# Install it with:  ditto target/Docket.app /Applications/Docket.app
# The icon (assets/icon/Docket.icns) is made from assets/icon/docket.svg; see the README.
set -e
cd "$(dirname "$0")/.."
cargo build --release
APP=target/Docket.app
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/docket "$APP/Contents/MacOS/docket"
cp assets/icon/Docket.icns "$APP/Contents/Resources/Docket.icns"
cat > "$APP/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Docket</string>
  <key>CFBundleDisplayName</key><string>Docket</string>
  <key>CFBundleIdentifier</key><string>local.docket.Docket</string>
  <key>CFBundleExecutable</key><string>docket</string>
  <key>CFBundleIconFile</key><string>Docket</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>LSMinimumSystemVersion</key><string>12.0</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.productivity</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSCalendarsFullAccessUsageDescription</key><string>Docket shows your calendar events next to review and decision deadlines. It only reads them, on this Mac.</string>
  <key>NSCalendarsUsageDescription</key><string>Docket shows your calendar events next to review and decision deadlines. It only reads them, on this Mac.</string>
  <key>NSAppTransportSecurity</key><dict><key>NSAllowsLocalNetworking</key><true/></dict>
</dict>
</plist>
EOF
# Ad-hoc signature (no developer certificate): enough to run on this Mac.
codesign --force --sign - "$APP"
echo "$APP"
