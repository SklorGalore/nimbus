#!/usr/bin/env bash
# Generate large test files for performance checks (DESIGN.md §5).
#
#   bench/gen.sh            10 MB and 100 MB source-like files
#   bench/gen.sh logs       also 1 GB and 4 GB log files (for M7)
#   bench/gen.sh longline   a single 100 MB line of minified JSON (for M7)
#   bench/gen.sh sizes      64, 128, 256 and 512 MB source files (for M7's
#                           rope-versus-mapped threshold; see threshold.sh)
#   bench/gen.sh diff       edited copies of the 10 MB and 100 MB files, one
#                           change per thousand lines (for M9's compare)
#   bench/gen.sh difflogs   the 1 GB log with 164 scattered edits and the
#                           4 GB log with 100,000 lines appended (M9; makes
#                           the logs first)
#
# Output goes to bench/out/, which git ignores. Files are reused if present.
set -euo pipefail

cd "$(dirname "$0")"
mkdir -p out

# Concatenate the project's own Rust sources until the target size is reached,
# so the text looks like real code: indentation, comments, long and short lines.
source_like() {
    local out="out/$1" size=$2
    [[ -f "$out" ]] && { echo "exists: $out"; return; }
    local seed
    seed=$(mktemp)
    find ../crates -name '*.rs' -print0 | xargs -0 cat > "$seed"
    : > "$out"
    while (( $(wc -c < "$out") < size )); do cat "$seed" >> "$out"; done
    # Cut to size, then drop the partial last line: a byte cut could split a
    # multi-byte character and make the file invalid UTF-8.
    head -c "$size" "$out" | LC_ALL=C sed '$d' > "$out.tmp"
    mv -f "$out.tmp" "$out"
    rm -f "$seed"
    echo "wrote $out ($(du -h "$out" | cut -f1))"
}

# Log lines: timestamp, level, component, message with a varying number.
log_file() {
    local out="out/$1" size=$2
    [[ -f "$out" ]] && { echo "exists: $out"; return; }
    LC_ALL=C awk -v size="$size" 'BEGIN {
        split("INFO WARN DEBUG ERROR TRACE", lv, " ");
        split("http db cache auth queue", cp, " ");
        n = 0; bytes = 0;
        while (bytes < size) {
            line = sprintf("2026-10-15T%02d:%02d:%02d.%03dZ %-5s %-5s request %d took %d ms",
                (n/3600000)%24, (n/60000)%60, (n/1000)%60, n%1000,
                lv[n%5+1], cp[n%5+1], n, (n*7919)%1000);
            print line; bytes += length(line) + 1; n++;
        }
    }' > "$out"
    echo "wrote $out ($(du -h "$out" | cut -f1))"
}

long_line() {
    local out="out/$1" size=$2
    [[ -f "$out" ]] && { echo "exists: $out"; return; }
    LC_ALL=C awk -v size="$size" 'BEGIN {
        printf "["; bytes = 1; n = 0;
        while (bytes < size) {
            s = sprintf("{\"id\":%d,\"name\":\"item%d\",\"tags\":[\"a\",\"b\"]},", n, n);
            printf "%s", s; bytes += length(s); n++;
        }
        print "{}]";
    }' > "$out"
    echo "wrote $out ($(du -h "$out" | cut -f1))"
}

# A copy of a file with a deterministic edit every `every` lines, cycling
# through a changed line, a deleted line and an inserted line, so a diff of
# the two has changes of every shape spread through the whole file.
edited_copy() {
    local src="out/$1" out="out/$2" every=$3
    [[ -f "$out" ]] && { echo "exists: $out"; return; }
    LC_ALL=C awk -v every="$every" '{
        if (NR % every == 0) {
            k = int(NR / every) % 3;
            if (k == 0) print "    let changed_" NR " = other();";
            else if (k == 2) { print "    // inserted line " NR; print; }
        } else print;
    }' "$src" > "$out"
    echo "wrote $out ($(du -h "$out" | cut -f1))"
}

# A copy of a file with `lines` log lines appended: a log that grew.
grown_copy() {
    local src="out/$1" out="out/$2" lines=$3
    [[ -f "$out" ]] && { echo "exists: $out"; return; }
    cp "$src" "$out"
    LC_ALL=C awk -v n="$lines" 'BEGIN {
        for (i = 0; i < n; i++)
            printf "2026-10-16T00:00:00.000Z INFO  tail  appended %d\n", i;
    }' >> "$out"
    echo "wrote $out ($(du -h "$out" | cut -f1))"
}

source_like src-10mb.rs $((10 * 1024 * 1024))
source_like src-100mb.rs $((100 * 1024 * 1024))

case "${1:-}" in
    logs)
        log_file log-1gb.log $((1024 * 1024 * 1024))
        log_file log-4gb.log $((4 * 1024 * 1024 * 1024))
        ;;
    longline)
        long_line longline-100mb.json $((100 * 1024 * 1024))
        ;;
    diff)
        edited_copy src-10mb.rs src-10mb-edited.rs 1000
        edited_copy src-100mb.rs src-100mb-edited.rs 1000
        ;;
    difflogs)
        log_file log-1gb.log $((1024 * 1024 * 1024))
        log_file log-4gb.log $((4 * 1024 * 1024 * 1024))
        edited_copy log-1gb.log log-1gb-edited.log 100000
        grown_copy log-4gb.log log-4gb-grown.log 100000
        ;;
    sizes)
        for mb in 64 128 256 512; do
            source_like "src-${mb}mb.rs" $((mb * 1024 * 1024))
        done
        ;;
    "") ;;
    *) echo "usage: $0 [logs|longline|sizes|diff|difflogs]" >&2; exit 2 ;;
esac
