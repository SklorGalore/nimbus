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
# ImageMagick to turn the BMP frames into PNG. The languages grid needs
# ImageMagick, and the huge-file shot bench/out/log-4gb.log
# (bench/gen.sh logs); each is skipped, with a note, when missing.
set -euo pipefail

cd "$(dirname "$0")/.."
out=assets/screenshots
mkdir -p "$out"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

cargo build --release --quiet
nimbus=./target/release/nimbus

# Two config folders: one with the sidebar showing, one without.
mkdir -p "$tmp/sidebar" "$tmp/bare" "$tmp/wide" "$tmp/guides"
printf 'sidebar_visible = true\nsidebar_width = 260.0\nsessions = []\n' >"$tmp/sidebar/state.toml"
printf 'sidebar_visible = true\nsidebar_width = 340.0\nsessions = []\n' >"$tmp/wide/state.toml"
printf 'sidebar_visible = false\nsessions = []\n' >"$tmp/bare/state.toml"
cp "$tmp/bare/state.toml" "$tmp/guides/state.toml"
printf 'render_whitespace = "all"\nindent_guides = true\n' >"$tmp/guides/config.toml"

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
	want "$name" || return 0
	NIMBUS_CONFIG_DIR="$tmp/$config" "$nimbus" "$@" --no-session \
		--screenshot "$tmp/$name.bmp" >/dev/null 2>"$tmp/$name.log" ||
		{ cat "$tmp/$name.log" >&2; exit 1; }
	to_png "$tmp/$name.bmp" "$out/$name.png"
	echo "$out/$name.png"
}

# want NAME: whether NAME is among the shots asked for (all when none are).
want() {
	[ ${#wanted[@]} -eq 0 ] || [[ " ${wanted[*]} " == *" $1 "* ]]
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

# Reading and layout aids.
shot workspace sidebar crates/core crates/syntax crates/syntax/src/symbols.rs --headless 2400x1400
shot guides guides "$undo" --cursors "$at" --headless 2400x1000
shot wrap bare docs/manual/getting-started.md --wrap --headless 2000x1200
shot symbols sidebar . "${tabs[@]}" --cursors "$at" --picker symbols:record --headless $big
shot debug bare "$undo" --cursors "$at" --debug-overlay --headless 2400x1200

# Folder compare: crates/core/src at the v0.2.0 tag against a fixed later
# commit (nimbus 0.9.1), so the list does not change as main moves on.
if want folders; then
	for rev in v0.2.0:0.2.0 d265e7c:0.9.1; do
		mkdir -p "$tmp/folders/${rev#*:}"
		git archive "${rev%%:*}" crates/core/src | tar -x -C "$tmp/folders/${rev#*:}" --strip-components 3
	done
fi
shot folders wide --diff "$tmp/folders/0.2.0" "$tmp/folders/0.9.1" --headless 2000x1200
# Only the sidebar: a folder compare leaves the editor an empty tab. The
# wide sidebar is 340 points, 680 pixels plus its divider at 2x.
if want folders; then
	magick "$out/folders.png" -crop 682x1200+0+0 +repage "$out/folders.png"
fi

# A 4 GB log, every line counted, with the cursor on the last line.
log=bench/out/log-4gb.log
if [ -f "$log" ]; then
	shot hugefile bare "$log" --cursors "$(wc -c <"$log" | tr -d ' ')" --wait-index --headless 1800x1000
elif want hugefile; then
	echo "skipping hugefile: run bench/gen.sh logs first" >&2
fi

# Four languages in a grid: each rendered alone, then joined.
if want languages; then
	if command -v magick >/dev/null; then
		for file in scripts/make-icons.py bench/gen.sh Cargo.toml .github/workflows/ci.yml; do
			name=lang-$(basename "$file" | tr . -)
			wanted+=("$name")
			shot "$name" bare "$file" --headless 1400x900 >/dev/null
			parts+=("$out/$name.png")
		done
		gap=(-bordercolor '#101114' -border 8)
		magick \( "${parts[0]}" "${parts[1]}" "${gap[@]}" +append \) \
			\( "${parts[2]}" "${parts[3]}" "${gap[@]}" +append \) -append "$out/languages.png"
		rm "${parts[@]}"
		echo "$out/languages.png"
	else
		echo "skipping languages: needs ImageMagick (magick)" >&2
	fi
fi
