#!/usr/bin/env pwsh
# M21 soak: 100,000 keystrokes with line breaks, undo, find and saves on the
# 100 MB file, memory sampled every 1,000 keystrokes. Fails (exit 1) if the
# resident size grows more than 20 MB from the first sample to the last.
#
#   bench/soak.ps1            # 100,000 keystrokes
#   bench/soak.ps1 20000      # a shorter run
#
# The soak saves into the file it types in, so it works on a scratch copy.
# It opens a window: run it with the screen unlocked and leave the machine
# alone while it types.
#Requires -Version 7.4
param([long] $Count = 100000)
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
Set-StrictMode -Version Latest

Set-Location (Join-Path $PSScriptRoot '..')
$src = 'bench/out/src-100mb.rs'
if (-not (Test-Path $src)) { & bench/gen.ps1 }
cargo build --release
$nimbus = if ($IsWindows) { './target/release/nimbus.exe' } else { './target/release/nimbus' }
$tmp = Join-Path ([IO.Path]::GetTempPath()) "nimbus-soak-$([IO.Path]::GetRandomFileName())"
New-Item -ItemType Directory $tmp | Out-Null
try {
    Copy-Item $src "$tmp/soak.rs"
    # The soak's own verdict is its exit code: hand it on.
    $PSNativeCommandUseErrorActionPreference = $false
    & $nimbus "$tmp/soak.rs" --no-session --bench-typing $Count --soak
    $code = $LASTEXITCODE
} finally {
    Remove-Item -Recurse -Force -ErrorAction Ignore $tmp
}
exit $code
