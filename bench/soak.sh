#!/bin/sh
# M21 soak: 100,000 keystrokes with line breaks, undo, find and saves on the
# 100 MB file, memory sampled every 1,000 keystrokes. Fails (exit 1) if the
# resident size grows more than 20 MB from the first sample to the last.
#
#   bench/soak.sh            # 100,000 keystrokes
#   bench/soak.sh 20000      # a shorter run
#
# The soak saves into the file it types in, so it works on a scratch copy.
# It opens a window: run it with the screen unlocked and leave the machine
# alone while it types.
set -eu
cd "$(dirname "$0")/.."
count=${1:-100000}
src=bench/out/src-100mb.rs
[ -f "$src" ] || bench/gen.sh
cargo build --release
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
cp "$src" "$tmp/soak.rs"
./target/release/nimbus "$tmp/soak.rs" --no-session --bench-typing "$count" --soak
