#!/bin/sh
# Make Nimbus.app around a built nimbus binary, and zip it.
#
# usage: scripts/bundle-macos.sh BINARY VERSION OUT_DIR
#
# A macOS app is a folder: Contents/Info.plist says what it is,
# Contents/MacOS holds the program, Contents/Resources the icon. Run from
# the repository root. Writes OUT_DIR/Nimbus.app and
# OUT_DIR/Nimbus-VERSION-macos.zip (made with ditto, which keeps the
# bundle's structure and permissions as Finder expects).
#
# CFBundleDocumentTypes lists what Finder may hand the app: text of any
# kind (source code, scripts, Markdown, JSON all conform to public.text)
# and folders. Rank Alternate puts Nimbus in Open With without taking
# over any file type's default app. The files arrive through the app
# delegate in crates/app/src/open_with.rs.
#
# The app is not signed: the first time, open it with right-click > Open,
# or run `xattr -d com.apple.quarantine Nimbus.app`.
set -eu

bin=$1
version=$2
out=$3

app="$out/Nimbus.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$bin" "$app/Contents/MacOS/nimbus"
cp assets/icon/nimbus.icns "$app/Contents/Resources/nimbus.icns"
cp LICENSE "$app/Contents/Resources/LICENSE"
cp crates/render/assets/HackNerdFont-LICENSE.md "$app/Contents/Resources/"

cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Nimbus</string>
  <key>CFBundleDisplayName</key><string>Nimbus</string>
  <key>CFBundleIdentifier</key><string>io.github.sklorgalore.nimbus</string>
  <key>CFBundleExecutable</key><string>nimbus</string>
  <key>CFBundleIconFile</key><string>nimbus</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>CFBundleVersion</key><string>$version</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>CFBundleDocumentTypes</key>
  <array>
    <dict>
      <key>CFBundleTypeName</key><string>Text</string>
      <key>CFBundleTypeRole</key><string>Editor</string>
      <key>LSHandlerRank</key><string>Alternate</string>
      <key>LSItemContentTypes</key>
      <array>
        <string>public.text</string>
        <string>public.plain-text</string>
        <string>public.source-code</string>
        <string>public.script</string>
      </array>
    </dict>
    <dict>
      <key>CFBundleTypeName</key><string>Folder</string>
      <key>CFBundleTypeRole</key><string>Viewer</string>
      <key>LSHandlerRank</key><string>Alternate</string>
      <key>LSItemContentTypes</key>
      <array><string>public.folder</string></array>
    </dict>
  </array>
</dict>
</plist>
PLIST
plutil -lint "$app/Contents/Info.plist" >/dev/null

(cd "$out" && rm -f "Nimbus-$version-macos.zip" && ditto -c -k --norsrc --noextattr --keepParent Nimbus.app "Nimbus-$version-macos.zip")
echo "$out/Nimbus-$version-macos.zip"
