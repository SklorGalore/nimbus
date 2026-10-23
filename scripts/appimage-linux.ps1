#!/usr/bin/env pwsh
# Make an AppImage from a built nimbus binary.
#
# usage: scripts/appimage-linux.ps1 BINARY VERSION OUT_DIR APPIMAGETOOL
#
# An AppImage is one executable file that carries an "AppDir": the
# program, its .desktop file and icon, and a small launcher (AppRun).
# Running it mounts the folder and starts AppRun; nothing is installed.
# APPIMAGETOOL is the path to appimagetool (the release workflow downloads
# it). Writes OUT_DIR/Nimbus-VERSION-x86_64.AppImage and prints its path.
# Run from the repository root.
#
# The binary opens libxkbcommon and libwayland-client (or the X11
# libraries) at startup; both are on every desktop Linux of the last
# decade, so they are not bundled.
#Requires -Version 7.4
param(
    [Parameter(Mandatory)] [string] $Binary,
    [Parameter(Mandatory)] [string] $Version,
    [Parameter(Mandatory)] [string] $OutDir,
    [Parameter(Mandatory)] [string] $AppImageTool
)
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
Set-StrictMode -Version Latest

$dir = Join-Path $OutDir 'Nimbus.AppDir'
Remove-Item -Recurse -Force -ErrorAction Ignore $dir
New-Item -ItemType Directory -Force "$dir/usr/bin", "$dir/usr/share/applications",
    "$dir/usr/share/icons/hicolor/1024x1024/apps" | Out-Null
install -m 755 $Binary "$dir/usr/bin/nimbus"
install -m 644 assets/linux/nimbus.desktop "$dir/nimbus.desktop"
install -m 644 assets/linux/nimbus.desktop "$dir/usr/share/applications/nimbus.desktop"
install -m 644 assets/icon/nimbus-1024.png "$dir/nimbus.png"
install -m 644 assets/icon/nimbus-1024.png "$dir/usr/share/icons/hicolor/1024x1024/apps/nimbus.png"
New-Item -ItemType SymbolicLink -Path "$dir/.DirIcon" -Target nimbus.png | Out-Null

# The launcher stays a tiny sh script: it runs on the user's machine,
# which has /bin/sh but may not have PowerShell. Single quotes in @'...'@
# keep its $ signs as they are.
$run = @'
#!/bin/sh
# Launcher: start the bundled binary from wherever the AppImage is mounted.
here=$(dirname "$(readlink -f "$0")")
exec "$here/usr/bin/nimbus" "$@"
'@
[IO.File]::WriteAllText(([IO.Path]::GetFullPath("$dir/AppRun", $PWD.Path)), $run + "`n")
chmod 755 "$dir/AppRun"

$image = Join-Path $OutDir "Nimbus-$Version-x86_64.AppImage"
Remove-Item -Force -ErrorAction Ignore $image
# appimagetool needs FUSE to run as an AppImage itself; the workflow's
# runner may lack it, so it is told to extract and run instead. Its
# output is kept quiet unless it fails, when a second run shows why on
# stderr (stdout carries only the image's path).
$env:ARCH = 'x86_64'
$env:APPIMAGE_EXTRACT_AND_RUN = '1'
try {
    & $AppImageTool --no-appstream $dir $image *> $null
} catch {
    & $AppImageTool --no-appstream $dir $image 2>&1 | ForEach-Object { [Console]::Error.WriteLine($_) }
}
Remove-Item -Recurse -Force $dir
$image
