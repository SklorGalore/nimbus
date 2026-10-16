#!/usr/bin/env bash
# Stand-in archiver for scripts/clippy-cross.sh: writes an empty static
# library where the output points (`ar crs PATH ...`, or MSVC-style
# `-out:PATH`). Never use it for a real build.
out=""
for arg in "$@"; do
    case "$arg" in
        -out:*|/out:*|/OUT:*) out="${arg#*:}" ;;
    esac
done
if [[ -z "$out" ]]; then
    # ar-style: the first argument after the flags word is the archive.
    out="$2"
fi
[[ -n "$out" ]] && printf '!<arch>\n' > "$out"
exit 0
