#!/usr/bin/env bash
# Measure opening the M7 size files with each buffer backend (macOS:
# /usr/bin/time -l), to choose the size at which Nimbus switches from the
# rope to the memory-mapped buffer (io::MAX_ROPE_BYTES).
#
#   bench/gen.sh sizes && cargo build --release && bench/threshold.sh
#
# Rule (M7): the largest size where the rope draws its first frame in
# under 1 s and its memory stays within 1.5x the file. Headless, so no
# window opens; NIMBUS_CONFIG_DIR keeps the run from touching the real
# config or sidebar state.
set -euo pipefail
cd "$(dirname "$0")/.."
cfg=$(mktemp -d)
shot=$(mktemp -t nimbus-threshold).bmp
trap 'rm -rf "$cfg" "$shot"' EXIT
printf '%-8s %-7s %8s %10s %12s\n' size backend seconds "rss MB" "footprint MB"
for mb in 64 128 256 512; do
    file="bench/out/src-${mb}mb.rs"
    [[ -f "$file" ]] || { echo "missing $file: run bench/gen.sh sizes" >&2; exit 1; }
    for backend in rope mapped; do
        /usr/bin/time -l env NIMBUS_CONFIG_DIR="$cfg" ./target/release/nimbus "$file" \
            --backend "$backend" --headless 1400x700 --screenshot "$shot" 2>&1 >/dev/null |
            awk -v s="${mb} MB" -v b="$backend" '
                /real/ {r=$1} /maximum resident/ {m=$1} /peak memory footprint/ {p=$1}
                END {printf "%-8s %-7s %8.2f %10d %12d\n", s, b, r, m/1048576, p/1048576}'
    done
done
