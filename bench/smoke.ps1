#!/usr/bin/env pwsh
# Startup check: build release, clear the OS file cache, then time one cold
# and several warm first frames with --smoke. Prints only the times.
#
#   bench/smoke.ps1        # 1 cold + 4 warm runs
#   bench/smoke.ps1 6      # 1 cold + 6 warm runs
#
# Clearing the cache needs sudo (`purge` on macOS, drop_caches on Linux), so
# expect a password prompt at the start. The 500 ms budget is checked
# against the warm median. A window opens for each run, so the screen must
# be unlocked; don't use the machine while it runs, input skews the numbers.
#Requires -Version 7.4
param([int] $Warm = 4)
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
Set-StrictMode -Version Latest

Set-Location (Join-Path $PSScriptRoot '..')
$budget = 500
$bin = './target/release/nimbus'

if (-not ($IsMacOS -or $IsLinux)) {
    Write-Error "don't know how to clear the file cache on this system" -ErrorAction Continue
    exit 1
}

# One smoke run; returns the first-frame-ms value or stops the script.
function Smoke {
    $line = & $bin --smoke 2>$null | Select-String -CaseSensitive '^first-frame-ms=(.*)$'
    if (-not $line) {
        Write-Error "no first-frame-ms from $bin --smoke (screen locked?)" -ErrorAction Continue
        exit 1
    }
    [double]$line.Matches[0].Groups[1].Value
}

# Ask for the password before the build so the prompt doesn't wait mid-run.
sudo -v

[Console]::Error.WriteLine('building release...')
cargo build --release --quiet

sync
if ($IsMacOS) {
    sudo purge
} else {
    '3' | sudo tee /proc/sys/vm/drop_caches | Out-Null
}

$cold = Smoke
$times = @(1..$Warm | ForEach-Object { Smoke })

'cold   {0,7:F1} ms' -f $cold
for ($i = 0; $i -lt $times.Count; $i++) {
    'warm {0} {1,7:F1} ms' -f ($i + 1), $times[$i]
}
$sorted = @($times | Sort-Object)
$n = $sorted.Count
$median = if ($n % 2) { $sorted[($n - 1) / 2] } else { ($sorted[$n / 2 - 1] + $sorted[$n / 2]) / 2 }
'warm median {0:F1} ms, min {1:F1}, max {2:F1} (budget {3} ms: {4})' -f $median, $sorted[0], $sorted[-1],
    $budget, $(if ($median -lt $budget) { 'ok' } else { 'OVER' })
