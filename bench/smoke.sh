#!/usr/bin/env bash
# Startup check: build release, clear the OS file cache, then time one cold
# and several warm first frames with --smoke. Prints only the times.
#
#   bench/smoke.sh        # 1 cold + 4 warm runs
#   bench/smoke.sh 6      # 1 cold + 6 warm runs
#
# Clearing the cache needs sudo (`purge` on macOS, drop_caches on Linux), so
# expect a password prompt at the start. The 500 ms budget is checked
# against the warm median. A window opens for each run, so the screen must
# be unlocked; don't use the machine while it runs, input skews the numbers.
set -euo pipefail
cd "$(dirname "$0")/.."

warm=${1:-4}
budget=500
bin=./target/release/nimbus

# One smoke run; prints the first-frame-ms value or fails.
smoke() {
    local ms
    ms=$("$bin" --smoke 2>/dev/null | sed -n 's/^first-frame-ms=//p')
    [[ -n "$ms" ]] || { echo "no first-frame-ms from $bin --smoke (screen locked?)" >&2; exit 1; }
    echo "$ms"
}

# Ask for the password before the build so the prompt doesn't wait mid-run.
sudo -v

echo "building release..." >&2
cargo build --release --quiet

sync
case "$(uname -s)" in
    Darwin) sudo purge ;;
    Linux) echo 3 | sudo tee /proc/sys/vm/drop_caches >/dev/null ;;
    *) echo "don't know how to clear the file cache on $(uname -s)" >&2; exit 1 ;;
esac

cold=$(smoke)
times=()
for _ in $(seq "$warm"); do
    times+=("$(smoke)")
done

printf 'cold   %7.1f ms\n' "$cold"
for i in "${!times[@]}"; do
    printf 'warm %d %7.1f ms\n' "$((i + 1))" "${times[$i]}"
done
printf '%s\n' "${times[@]}" | sort -n | awk -v b="$budget" '
    { t[NR] = $1 }
    END {
        med = (NR % 2) ? t[(NR + 1) / 2] : (t[NR / 2] + t[NR / 2 + 1]) / 2
        printf "warm median %.1f ms, min %.1f, max %.1f (budget %d ms: %s)\n",
            med, t[1], t[NR], b, (med < b) ? "ok" : "OVER"
    }'
