#!/usr/bin/env pwsh
# Fill in the Homebrew cask for one release.
#
# usage: scripts/manifests.ps1 VERSION DIST_DIR
#
# DIST_DIR holds the release's packages (downloaded back from GitHub, so
# the hash is of the file people get). Writes DIST_DIR/nimbus.rb with the
# version and the SHA-256 of the .dmg filled in, and prints its path. The
# template is in packaging/homebrew; it is generated from the tag so it
# cannot drift from what was released. (The Chocolatey package is made by
# .github/workflows/chocolatey.yml, on Windows.) Run from the repository
# root.
#Requires -Version 7.4
param(
    [Parameter(Mandatory)] [string] $Version,
    [Parameter(Mandatory)] [string] $DistDir
)
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
Set-StrictMode -Version Latest

$dmg = (Get-FileHash -Algorithm SHA256 (Join-Path $DistDir "Nimbus-$Version-macos.dmg")).Hash.ToLowerInvariant()
$text = [IO.File]::ReadAllText((Resolve-Path packaging/homebrew/nimbus.rb.in))
$text = $text.Replace('@VERSION@', $Version).Replace('@SHA256@', $dmg)
$out = Join-Path $DistDir 'nimbus.rb'
[IO.File]::WriteAllText([IO.Path]::GetFullPath($out, $PWD.Path), $text)
$out
