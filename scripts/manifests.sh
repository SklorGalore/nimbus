#!/bin/sh
# Fill in the package-manager manifests for one release.
#
# usage: scripts/manifests.sh VERSION DIST_DIR
#
# DIST_DIR holds the release's packages (downloaded back from GitHub, so
# the hashes are of the files people get). Writes DIST_DIR/nimbus.rb
# (Homebrew cask) and DIST_DIR/winget/*.yaml (winget manifest), with the
# version and the SHA-256 of the .dmg and the Windows installer filled
# in. The templates are in packaging/; they are generated from the tag
# so they cannot drift from what was released.
set -eu

version=$1
dist=$2

hash() { sha256sum "$1" | cut -d' ' -f1; }

dmg=$(hash "$dist/Nimbus-$version-macos.dmg")
sed -e "s/@VERSION@/$version/g" -e "s/@SHA256@/$dmg/g" \
  packaging/homebrew/nimbus.rb.in > "$dist/nimbus.rb"

setup=$(hash "$dist/nimbus-$version-windows-x86_64-setup.exe")
mkdir -p "$dist/winget"
for t in packaging/winget/*.yaml.in; do
  out="$dist/winget/$(basename "$t" .in)"
  sed -e "s/@VERSION@/$version/g" -e "s/@SHA256@/$setup/g" "$t" > "$out"
done
echo "$dist/nimbus.rb"
ls "$dist"/winget/*.yaml
