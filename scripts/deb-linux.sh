#!/bin/sh
# Make a Debian package from a built nimbus binary.
#
# usage: scripts/deb-linux.sh BINARY VERSION OUT_DIR
#
# A .deb is an archive with a control file (name, version, dependencies)
# and the files to install, laid out as they land on disk: the binary in
# /usr/bin, the .desktop file and icon where desktop menus look for them.
# `dpkg-deb` builds it; no cargo plugin is needed. Writes
# OUT_DIR/nimbus_VERSION_amd64.deb. Run from the repository root.
#
# The dependencies are the libraries winit opens at startup with dlopen
# (so `ldd` does not list them): libxkbcommon for keyboard layouts and
# libwayland-client for the Wayland backend. The X11 libraries are opened
# only on X11, so they are recommended, not required.
set -eu

bin=$1
version=$2
out=$3

name="nimbus_${version}_amd64"
root="$out/$name"
rm -rf "$root"
mkdir -p "$root/DEBIAN" "$root/usr/bin" "$root/usr/share/applications" \
  "$root/usr/share/icons/hicolor/1024x1024/apps" "$root/usr/share/doc/nimbus"
install -m 755 "$bin" "$root/usr/bin/nimbus"
install -m 644 assets/linux/nimbus.desktop "$root/usr/share/applications/nimbus.desktop"
install -m 644 assets/icon/nimbus-1024.png "$root/usr/share/icons/hicolor/1024x1024/apps/nimbus.png"
install -m 644 LICENSE "$root/usr/share/doc/nimbus/copyright"
install -m 644 crates/render/assets/HackNerdFont-LICENSE.md "$root/usr/share/doc/nimbus/"

size=$(du -sk "$root/usr" | cut -f1)
cat > "$root/DEBIAN/control" <<CONTROL
Package: nimbus
Version: $version
Section: editors
Priority: optional
Architecture: amd64
Depends: libc6, libxkbcommon0, libwayland-client0
Recommends: libx11-6, libxcursor1, libxi6, libxrandr2
Installed-Size: $size
Maintainer: SklorGalore <168158960+SklorGalore@users.noreply.github.com>
Homepage: https://github.com/SklorGalore/nimbus
Description: A fast, lightweight, modeless text editor
 Nimbus is a text editor with its own GPU text renderer. It opens a 4 GB
 log in a fraction of a second, edits with several cursors, and
 highlights nine languages.
CONTROL

dpkg-deb --build --root-owner-group "$root" "$out/$name.deb" >/dev/null
rm -rf "$root"
echo "$out/$name.deb"
