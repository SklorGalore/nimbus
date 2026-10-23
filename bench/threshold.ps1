#!/usr/bin/env pwsh
# Measure opening the M7 size files with each buffer backend (macOS:
# /usr/bin/time -l), to choose the size at which Nimbus switches from the
# rope to the memory-mapped buffer (io::MAX_ROPE_BYTES).
#
#   bench/gen.ps1 sizes; cargo build --release; bench/threshold.ps1
#
# Rule (M7): the largest size where the rope draws its first frame in
# under 1 s and its memory stays within 1.5x the file. Headless, so no
# window opens; NIMBUS_CONFIG_DIR keeps the run from touching the real
# config or sidebar state.
#Requires -Version 7.4
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
Set-StrictMode -Version Latest

if (-not $IsMacOS) {
    Write-Error 'threshold.ps1 reads the memory figures of macOS /usr/bin/time -l; run it on a Mac' -ErrorAction Continue
    exit 1
}
Set-Location (Join-Path $PSScriptRoot '..')
$cfg = Join-Path ([IO.Path]::GetTempPath()) "nimbus-threshold-$([IO.Path]::GetRandomFileName())"
New-Item -ItemType Directory $cfg | Out-Null
$shot = "$cfg.bmp"
$env:NIMBUS_CONFIG_DIR = $cfg
try {
    '{0,-8} {1,-7} {2,8} {3,10} {4,12}' -f 'size', 'backend', 'seconds', 'rss MB', 'footprint MB'
    foreach ($mb in 64, 128, 256, 512) {
        $file = "bench/out/src-${mb}mb.rs"
        if (-not (Test-Path $file)) {
            Write-Error "missing ${file}: run bench/gen.ps1 sizes" -ErrorAction Continue
            exit 1
        }
        foreach ($backend in 'rope', 'mapped') {
            # time -l reports on stderr, one figure per line with the number
            # first. With 2>&1 the stderr lines arrive as ErrorRecords and
            # the program's own stdout as strings, which are dropped.
            $report = & /usr/bin/time -l ./target/release/nimbus $file --backend $backend `
                --headless 1400x700 --screenshot $shot 2>&1 |
                Where-Object { $_ -is [Management.Automation.ErrorRecord] } |
                ForEach-Object { "$_" }
            $real = $rss = $peak = 0
            foreach ($line in $report) {
                $first = ($line.Trim() -split '\s+')[0]
                if ($line -match 'real') { $real = [double]$first }
                if ($line -match 'maximum resident') { $rss = [double]$first }
                if ($line -match 'peak memory footprint') { $peak = [double]$first }
            }
            '{0,-8} {1,-7} {2,8:F2} {3,10:F0} {4,12:F0}' -f "$mb MB", $backend, $real,
                [math]::Floor($rss / 1MB), [math]::Floor($peak / 1MB)
        }
    }
} finally {
    Remove-Item -Recurse -Force -ErrorAction Ignore $cfg, $shot
}
