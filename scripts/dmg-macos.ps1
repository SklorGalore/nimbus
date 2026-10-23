#!/usr/bin/env pwsh
# Make a .dmg around Nimbus.app, with an Applications shortcut beside it.
#
# usage: scripts/dmg-macos.ps1 APP_DIR VERSION OUT_DIR
#
# A .dmg is a disk image: opening it mounts a volume, and dragging the
# app onto the "Applications" symlink inside copies it to /Applications.
# `hdiutil` (part of macOS) builds the image from a staging folder; UDZO
# is the compressed, read-only format releases use. Writes
# OUT_DIR/Nimbus-VERSION-macos.dmg and prints its path.
#
# The app inside is unsigned: the first time, open it with right-click >
# Open, or run `xattr -d com.apple.quarantine /Applications/Nimbus.app`.
#Requires -Version 7.4
param(
    [Parameter(Mandatory)] [string] $AppDir,
    [Parameter(Mandatory)] [string] $Version,
    [Parameter(Mandatory)] [string] $OutDir
)
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
Set-StrictMode -Version Latest

# A fresh, empty staging folder, removed however the script ends.
$stage = Join-Path ([IO.Path]::GetTempPath()) "nimbus-dmg-$([IO.Path]::GetRandomFileName())"
New-Item -ItemType Directory $stage | Out-Null
try {
    # cp -R keeps the bundle's symlinks and permissions as they are.
    cp -R $AppDir "$stage/Nimbus.app"
    New-Item -ItemType SymbolicLink -Path "$stage/Applications" -Target /Applications | Out-Null

    $dmg = Join-Path $OutDir "Nimbus-$Version-macos.dmg"
    Remove-Item -Force -ErrorAction Ignore $dmg
    hdiutil create -quiet -volname "Nimbus $Version" -srcfolder $stage -ov -format UDZO $dmg
    $dmg
} finally {
    Remove-Item -Recurse -Force -ErrorAction Ignore $stage
}
