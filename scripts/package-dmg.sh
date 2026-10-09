#!/usr/bin/env bash
# Build an Apple Silicon .app and a drag-to-Applications DMG.
# Ad-hoc signed so an M-series Mac can launch it. Not notarized.
set -euo pipefail

cd "$(dirname "$0")/.."

version="$(awk -F '"' '/^version[[:space:]]*=/ { print $2; exit }' Cargo.toml)"
app_name="EZD Studio"
identifier="com.byellokore.ezdstudio"
dist="dist"
app="$dist/$app_name.app"
stage="$dist/dmg-root"
dmg="$dist/EZD-Studio-${version}-apple-silicon.dmg"

export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-11.0}"

echo "Building release for this Mac (Apple Silicon)…"
cargo build --release

binary="target/release/ezd-studio"
if [[ ! -f "$binary" ]]; then
  echo "missing $binary" >&2
  exit 1
fi

rm -rf "$app" "$stage"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"

cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key>
  <string>en</string>
  <key>CFBundleDisplayName</key>
  <string>${app_name}</string>
  <key>CFBundleExecutable</key>
  <string>ezd-studio</string>
  <key>CFBundleIdentifier</key>
  <string>${identifier}</string>
  <key>CFBundleInfoDictionaryVersion</key>
  <string>6.0</string>
  <key>CFBundleName</key>
  <string>${app_name}</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
  <key>CFBundleShortVersionString</key>
  <string>${version}</string>
  <key>CFBundleVersion</key>
  <string>${version}</string>
  <key>LSApplicationCategoryType</key>
  <string>public.app-category.graphics-design</string>
  <key>LSMinimumSystemVersion</key>
  <string>${MACOSX_DEPLOYMENT_TARGET}</string>
  <key>NSHighResolutionCapable</key>
  <true/>
  <key>NSSupportsAutomaticGraphicsSwitching</key>
  <true/>
</dict>
</plist>
PLIST
printf 'APPL????' > "$app/Contents/PkgInfo"

ditto "$binary" "$app/Contents/MacOS/ezd-studio"
chmod +x "$app/Contents/MacOS/ezd-studio"

# Apple Silicon refuses an unsigned arm64 app. "-" is an ad-hoc signature.
codesign --force --sign - --identifier "$identifier" "$app"
codesign --verify --strict --verbose=2 "$app"

mkdir -p "$stage"
ditto "$app" "$stage/$app_name.app"
ln -s /Applications "$stage/Applications"

rm -f "$dmg"
hdiutil create \
  -volname "$app_name" \
  -srcfolder "$stage" \
  -ov \
  -format UDZO \
  "$dmg"
rm -rf "$stage"

echo
echo "App: $app"
echo "DMG: $dmg"
file "$app/Contents/MacOS/ezd-studio"
