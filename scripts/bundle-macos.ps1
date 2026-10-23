#!/usr/bin/env pwsh
# Make Nimbus.app around a built nimbus binary, and zip it.
#
# usage: scripts/bundle-macos.ps1 BINARY VERSION OUT_DIR
#
# A macOS app is a folder: Contents/Info.plist says what it is,
# Contents/MacOS holds the program, Contents/Resources the icon. Run from
# the repository root. Writes OUT_DIR/Nimbus.app and
# OUT_DIR/Nimbus-VERSION-macos.zip (made with ditto, which keeps the
# bundle's structure and permissions as Finder expects), and prints the
# zip's path, which is all it prints.
#
# CFBundleDocumentTypes lists what Finder may hand the app: text of any
# kind (source code, scripts, Markdown, JSON all conform to public.text)
# and folders. Rank Alternate puts Nimbus in Open With without taking
# over any file type's default app. The files arrive through the app
# delegate in crates/app/src/open_with.rs.
#
# The app is not signed: the first time, open it with right-click > Open,
# or run `xattr -d com.apple.quarantine Nimbus.app`.
#Requires -Version 7.4
param(
    [Parameter(Mandatory)] [string] $Binary,
    [Parameter(Mandatory)] [string] $Version,
    [Parameter(Mandatory)] [string] $OutDir
)
$ErrorActionPreference = 'Stop'
# A program that exits non-zero (plutil, ditto) stops the script, as
# `set -e` does in sh.
$PSNativeCommandUseErrorActionPreference = $true
Set-StrictMode -Version Latest

$app = Join-Path $OutDir 'Nimbus.app'
Remove-Item -Recurse -Force -ErrorAction Ignore $app
New-Item -ItemType Directory -Force "$app/Contents/MacOS", "$app/Contents/Resources" | Out-Null
Copy-Item $Binary "$app/Contents/MacOS/nimbus"
Copy-Item assets/icon/nimbus.icns "$app/Contents/Resources/nimbus.icns"
Copy-Item LICENSE "$app/Contents/Resources/LICENSE"
Copy-Item crates/render/assets/HackNerdFont-LICENSE.md "$app/Contents/Resources/"

# A here-string: the text between @" and "@, with $Version filled in.
$plist = @"
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
  <key>CFBundleShortVersionString</key><string>$Version</string>
  <key>CFBundleVersion</key><string>$Version</string>
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
"@
[IO.File]::WriteAllText(([IO.Path]::GetFullPath("$app/Contents/Info.plist", $PWD.Path)), $plist + "`n")
plutil -lint "$app/Contents/Info.plist" | Out-Null

$zip = "Nimbus-$Version-macos.zip"
Push-Location $OutDir
try {
    Remove-Item -Force -ErrorAction Ignore $zip
    ditto -c -k --norsrc --noextattr --keepParent Nimbus.app $zip
} finally {
    Pop-Location
}
Join-Path $OutDir $zip
