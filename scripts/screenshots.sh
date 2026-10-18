#!/usr/bin/env bash
# Remake the screenshots in assets/screenshots/ that the README and the
# user manual show. Run it after a change to how Nimbus looks, then look at
# the images before committing them.
#
#   scripts/screenshots.sh          all of them
#   scripts/screenshots.sh hero     only the named ones
#
# Every shot is rendered with --headless, so no window opens and the screen
# may be locked. Shots are at a Retina scale (2x): a 2560x1600 image is a
# 1280x800 window. A throwaway config folder (NIMBUS_CONFIG_DIR) holds the
# sidebar's state, so your own config and state.toml are neither read nor
# changed.
#
# Cursor positions are found by searching the shown files rather than by
# fixed byte offsets, so the shots survive small edits to those files. If a
# pattern stops matching, the script stops and names it.
#
# Needs python3 (to find cursor positions), and sips (macOS) or
# ImageMagick to turn the BMP frames into PNG.
set -euo pipefail

cd "$(dirname "$0")/.."
out=assets/screenshots
mkdir -p "$out"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

cargo build --release --quiet
nimbus=./target/release/nimbus

# Two config folders: one with the sidebar showing, one without.
mkdir -p "$tmp/sidebar" "$tmp/bare" "$tmp/wide"
printf 'sidebar_visible = true\nsidebar_width = 260.0\nsessions = []\n' >"$tmp/sidebar/state.toml"
printf 'sidebar_visible = true\nsidebar_width = 340.0\nsessions = []\n' >"$tmp/wide/state.toml"
printf 'sidebar_visible = false\nsessions = []\n' >"$tmp/bare/state.toml"

# Byte offset just after NEEDLE in FILE, searching from the first line that
# contains AFTER (or from the start). Nimbus's --cursors takes byte offsets.
offset() {
	python3 - "$@" <<'PY'
import sys
path, needle, after = sys.argv[1], sys.argv[2].encode(), (sys.argv[3] if len(sys.argv) > 3 else "").encode()
text = open(path, "rb").read()
start = text.find(after) if after else 0
at = text.find(needle, max(start, 0)) if start >= 0 else -1
if at < 0:
    sys.exit(f"screenshots.sh: {needle.decode()!r} not found in {path}")
print(at + len(needle))
PY
}

to_png() {
	if command -v sips >/dev/null; then
		sips -s format png "$1" --out "$2" >/dev/null
	else
		magick "$1" "$2"
	fi
}

# shot NAME CONFIG ARGS...: render one frame with the given config folder.
shot() {
	local name=$1 config=$2
	shift 2
	if [ ${#wanted[@]} -gt 0 ] && [[ " ${wanted[*]} " != *" $name "* ]]; then
		return
	fi
	NIMBUS_CONFIG_DIR="$tmp/$config" "$nimbus" "$@" --no-session \
		--screenshot "$tmp/$name.bmp" >/dev/null 2>"$tmp/$name.log" ||
		{ cat "$tmp/$name.log" >&2; exit 1; }
	to_png "$tmp/$name.bmp" "$out/$name.png"
	echo "$out/$name.png"
}

wanted=("$@")
undo=crates/core/src/undo.rs
tabs=("$undo" crates/core/src/search.rs crates/app/src/action.rs README.md)
# The cursor on `record_batch`, which scrolls History's methods into view.
at=$(offset "$undo" "pub fn " "pub fn record_batch")
# Five cursors after the field names of the Group pushed in History::record.
# The last offset is the primary cursor, which the view scrolls to: the
# bottom one, so the four above it stay in view.
group="self.undo.push(Group {"
many=$(for field in id edits batch_ends before after; do
	offset "$undo" "                $field" "$group"
done | sort -n | paste -sd, -)
# Split so this script is not among the matches.
pattern="COALESCE""_WINDOW"
big=2560x1600

shot hero sidebar . "${tabs[@]}" --cursors "$at" --headless $big
shot light sidebar . "${tabs[@]}" --cursors "$at" --theme light --headless $big
shot palette sidebar . "${tabs[@]}" --cursors "$at" --picker palette --headless $big
shot files sidebar . "${tabs[@]}" --cursors "$at" --picker files:buf --headless $big
shot find bare "$undo" --cursors "$at" --find edits --headless 2400x1000
shot multicursor bare "$undo" --cursors "$many" --headless 2000x1000
shot search wide . "$undo" --search "$pattern" --headless $big
shot diff bare --diff examples/merge/local.rs examples/merge/remote.rs --headless 2560x1400
shot merge bare --merge examples/merge/{base,local,remote}.rs -o "$tmp/merged.rs" --headless 3200x1500
