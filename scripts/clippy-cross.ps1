#!/usr/bin/env pwsh
# Run strict clippy for another target from macOS, where there is no C
# toolchain for Linux or Windows.
#
#   scripts/clippy-cross.ps1 x86_64-unknown-linux-gnu
#   scripts/clippy-cross.ps1 x86_64-pc-windows-msvc
#
# The tree-sitter runtime and grammars are C, compiled by build scripts.
# Clippy never links, so the build scripts only need *some* object files:
# this points the C compiler at a stand-in that writes empty ones. Never use
# it for a real build. Any arguments after the target go to cargo clippy.
#
# The stand-ins are started by cargo as programs, through their
# `#!/usr/bin/env pwsh` line, so this works on macOS and Linux only.
#Requires -Version 7.4
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

if ($args.Count -lt 1) {
    Write-Error 'usage: scripts/clippy-cross.ps1 TARGET [CLIPPY_ARGS...]'
    exit 2
}
$target = $args[0]
$rest = @($args | Select-Object -Skip 1)
$var = $target -replace '-', '_'
[Environment]::SetEnvironmentVariable("CC_$var", (Join-Path $PSScriptRoot 'fake-cc.ps1'))
[Environment]::SetEnvironmentVariable("AR_$var", (Join-Path $PSScriptRoot 'fake-ar.ps1'))
$env:RUSTFLAGS = '-D warnings'
cargo clippy --workspace --all-targets --target $target @rest
exit $LASTEXITCODE
