#!/usr/bin/env bash
# Run strict clippy for another target from macOS, where there is no C
# toolchain for Linux or Windows.
#
#   scripts/clippy-cross.sh x86_64-unknown-linux-gnu
#   scripts/clippy-cross.sh x86_64-pc-windows-msvc
#
# The tree-sitter runtime and grammars are C, compiled by build scripts.
# Clippy never links, so the build scripts only need *some* object files:
# this points the C compiler at a stand-in that writes empty ones. Never use
# it for a real build.
set -euo pipefail
target=${1:?usage: scripts/clippy-cross.sh TARGET}
here=$(cd "$(dirname "$0")" && pwd)
var=$(echo "$target" | tr '-' '_')
export "CC_${var}=${here}/fake-cc.sh"
export "AR_${var}=${here}/fake-ar.sh"
export RUSTFLAGS="-D warnings"
exec cargo clippy --workspace --all-targets --target "$target" "${@:2}"
