#!/bin/sh
# Make a .dmg around Nimbus.app, with an Applications shortcut beside it.
#
# usage: scripts/dmg-macos.sh APP_DIR VERSION OUT_DIR
#
# A .dmg is a disk image: opening it mounts a volume, and dragging the
# app onto the "Applications" symlink inside copies it to /Applications.
# `hdiutil` (part of macOS) builds the image from a staging folder; UDZO
# is the compressed, read-only format releases use. Writes
# OUT_DIR/Nimbus-VERSION-macos.dmg.
#
# The app inside is unsigned: the first time, open it with right-click >
# Open, or run `xattr -d com.apple.quarantine /Applications/Nimbus.app`.
set -eu

app=$1
version=$2
out=$3

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
cp -R "$app" "$stage/Nimbus.app"
ln -s /Applications "$stage/Applications"

dmg="$out/Nimbus-$version-macos.dmg"
rm -f "$dmg"
hdiutil create -quiet -volname "Nimbus $version" -srcfolder "$stage" -ov -format UDZO "$dmg"
echo "$dmg"
