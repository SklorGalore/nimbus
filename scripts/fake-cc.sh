#!/usr/bin/env bash
# Stand-in C compiler for scripts/clippy-cross.sh: writes an empty object
# file where the output flag points (`-o PATH`, or MSVC-style `-FoPATH`,
# which the `cc` crate falls back to when it cannot tell which compiler this
# is), and nothing else. Never use it for a real build.
out=""
while (( $# )); do
    case "$1" in
        -o) out="$2"; shift ;;
        -Fo*|/Fo*) out="${1:3}" ;;
    esac
    shift
done
[[ -n "$out" ]] && : > "$out"
exit 0
