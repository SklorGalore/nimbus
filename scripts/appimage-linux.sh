#!/bin/sh
# Make an AppImage from a built nimbus binary.
#
# usage: scripts/appimage-linux.sh BINARY VERSION OUT_DIR APPIMAGETOOL
#
# An AppImage is one executable file that carries an "AppDir": the
# program, its .desktop file and icon, and a small launcher (AppRun).
# Running it mounts the folder and starts AppRun; nothing is installed.
# APPIMAGETOOL is the path to appimagetool (the release workflow downloads
# it). Writes OUT_DIR/Nimbus-VERSION-x86_64.AppImage. Run from the
# repository root.
#
# The binary opens libxkbcommon and libwayland-client (or the X11
# libraries) at startup; both are on every desktop Linux of the last
# decade, so they are not bundled.
set -eu

bin=$1
version=$2
out=$3
tool=$4

dir="$out/Nimbus.AppDir"
rm -rf "$dir"
mkdir -p "$dir/usr/bin" "$dir/usr/share/applications" \
  "$dir/usr/share/icons/hicolor/1024x1024/apps"
install -m 755 "$bin" "$dir/usr/bin/nimbus"
install -m 644 assets/linux/nimbus.desktop "$dir/nimbus.desktop"
install -m 644 assets/linux/nimbus.desktop "$dir/usr/share/applications/nimbus.desktop"
install -m 644 assets/icon/nimbus-1024.png "$dir/nimbus.png"
install -m 644 assets/icon/nimbus-1024.png "$dir/usr/share/icons/hicolor/1024x1024/apps/nimbus.png"
ln -sf nimbus.png "$dir/.DirIcon"
cat > "$dir/AppRun" <<'RUN'
#!/bin/sh
# Launcher: start the bundled binary from wherever the AppImage is mounted.
here=$(dirname "$(readlink -f "$0")")
exec "$here/usr/bin/nimbus" "$@"
RUN
chmod 755 "$dir/AppRun"

image="$out/Nimbus-$version-x86_64.AppImage"
rm -f "$image"
# appimagetool needs FUSE to run as an AppImage itself; the workflow's
# runner may lack it, so it is told to extract and run instead.
ARCH=x86_64 APPIMAGE_EXTRACT_AND_RUN=1 "$tool" --no-appstream "$dir" "$image" >/dev/null 2>&1 \
  || ARCH=x86_64 APPIMAGE_EXTRACT_AND_RUN=1 "$tool" --no-appstream "$dir" "$image"
rm -rf "$dir"
echo "$image"
