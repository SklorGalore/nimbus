#!/bin/sh
# Fill in the Homebrew cask for one release.
#
# usage: scripts/manifests.sh VERSION DIST_DIR
#
# DIST_DIR holds the release's packages (downloaded back from GitHub, so
# the hash is of the file people get). Writes DIST_DIR/nimbus.rb with the
# version and the SHA-256 of the .dmg filled in. The template is in
# packaging/homebrew; it is generated from the tag so it cannot drift
# from what was released. (The Chocolatey package is made by
# .github/workflows/chocolatey.yml, on Windows.)
set -eu

version=$1
dist=$2

hash() { sha256sum "$1" | cut -d' ' -f1; }

dmg=$(hash "$dist/Nimbus-$version-macos.dmg")
sed -e "s/@VERSION@/$version/g" -e "s/@SHA256@/$dmg/g" \
  packaging/homebrew/nimbus.rb.in > "$dist/nimbus.rb"
echo "$dist/nimbus.rb"
