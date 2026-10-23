#!/usr/bin/env pwsh
# Sign a release's SHA256SUMS with the release key, on the owner's Mac.
#
# usage: scripts/sign-release.ps1 vX.Y.Z [SECRET_KEY_FILE]
#
# The release workflow builds a draft release with every package and a
# SHA256SUMS file listing their hashes, but it cannot sign: the secret
# key never goes into GitHub (docs/milestones/m26-security.md), so a
# hijacked account or workflow cannot make a release Nimbus will trust.
# This script is the step between the draft and publishing it:
#
#   1. download the release's packages and SHA256SUMS from GitHub;
#   2. check every package against SHA256SUMS, and that SHA256SUMS lists
#      every package and nothing else;
#   3. sign SHA256SUMS with minisign (it asks for the key's password),
#      with the trusted comment "Nimbus X.Y.Z SHA256SUMS" that the
#      updater reads the version from;
#   4. check the signature against minisign.pub in this repository;
#   5. upload SHA256SUMS.minisig to the release.
#
# It works on a draft or an already published release (1.0.0 and 1.0.1
# shipped unsigned). A release that already has a signature is refused,
# so a good signature is never replaced by accident; delete the
# .minisig asset on GitHub first to re-sign on purpose.
#
# SECRET_KEY_FILE defaults to minisign's own default,
# ~/.minisign/minisign.key. Needs gh (signed in) and minisign.
# NIMBUS_REPO overrides the repository (default SklorGalore/nimbus).
#Requires -Version 7.4
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
Set-StrictMode -Version Latest

$repo = if ($env:NIMBUS_REPO) { $env:NIMBUS_REPO } else { 'SklorGalore/nimbus' }
$pubkey = Join-Path (Resolve-Path (Join-Path $PSScriptRoot '..')) 'minisign.pub'

# Print a message and stop with exit code 1.
function Stop-Script([string] $message) {
    [Console]::Error.WriteLine("sign-release: $message")
    exit 1
}

# Run a program, returning its output, or $null if it failed: for the
# programs whose failure has a message of our own.
function Invoke-Checked {
    $PSNativeCommandUseErrorActionPreference = $false
    $out = & $args[0] @($args | Select-Object -Skip 1)
    if ($LASTEXITCODE -ne 0) { return $null }
    , @($out)
}

if ($args.Count -lt 1 -or $args.Count -gt 2) {
    Stop-Script 'usage: scripts/sign-release.ps1 vX.Y.Z [SECRET_KEY_FILE]'
}
$tag = [string]$args[0]
if ($tag -cnotmatch '^v[0-9]+\.[0-9]+\.[0-9]+') { Stop-Script "the tag must look like v1.2.3, not '$tag'" }
$version = $tag.Substring(1)
$seckey = if ($args.Count -eq 2) { [string]$args[1] } else { Join-Path $HOME '.minisign/minisign.key' }

foreach ($tool in 'gh', 'minisign') {
    if (-not (Get-Command $tool -ErrorAction Ignore)) { Stop-Script "$tool is not installed (brew install $tool)" }
}
if (-not (Test-Path $pubkey)) { Stop-Script "no minisign.pub at $pubkey" }
if (-not (Test-Path $seckey)) { Stop-Script "no secret key at $seckey (pass its path as the second argument)" }
$seckey = (Resolve-Path $seckey).Path

# What the release has. A signature already there is left alone.
$assets = Invoke-Checked gh release view $tag --repo $repo --json assets --jq '.assets[].name'
if ($null -eq $assets) { Stop-Script "no release $tag in $repo (is gh signed in?)" }
if ($assets -ccontains 'SHA256SUMS.minisig') {
    Stop-Script "$tag already has SHA256SUMS.minisig; delete that asset on GitHub first to re-sign"
}
if ($assets -cnotcontains 'SHA256SUMS') { Stop-Script "$tag has no SHA256SUMS yet; wait for the workflow to finish" }

$work = Join-Path ([IO.Path]::GetTempPath()) "nimbus-sign-$version.$([IO.Path]::GetRandomFileName())"
New-Item -ItemType Directory $work | Out-Null
Push-Location $work
try {
    "Downloading $tag from $repo into $work"
    gh release download $tag --repo $repo `
        --pattern '*.zip' --pattern '*.tar.gz' --pattern '*.dmg' `
        --pattern '*.deb' --pattern '*.AppImage' --pattern '*-setup.exe' `
        --pattern 'SHA256SUMS'

    # Each SHA256SUMS line is "HASH  NAME" (or "HASH *NAME", binary mode).
    $sums = foreach ($line in [IO.File]::ReadAllLines("$work/SHA256SUMS")) {
        if ($line -cmatch '^([0-9a-fA-F]{64}) [ *](.+)$') {
            [pscustomobject]@{ Hash = $Matches[1].ToLowerInvariant(); Name = $Matches[2] }
        } elseif ($line.Trim()) {
            Stop-Script "SHA256SUMS has a line that is not a hash and a name: $line"
        }
    }
    # The winget manifests zip (on releases up to 1.2.0) is not a package
    # and is not hashed; everything else downloaded must be in SHA256SUMS,
    # and everything in it must be here. (Ordinal sorting, as `sort` in
    # the C locale.)
    $listed = [string[]]@($sums | ForEach-Object Name)
    [Array]::Sort($listed, [StringComparer]::Ordinal)
    $present = [string[]]@(Get-ChildItem -File -Name | Where-Object { $_ -cnotin 'SHA256SUMS', 'winget-manifest.zip' })
    [Array]::Sort($present, [StringComparer]::Ordinal)
    if ($listed.Count -eq 0) { Stop-Script 'SHA256SUMS is empty' }
    if (($listed -join "`n") -cne ($present -join "`n")) {
        [Console]::Error.WriteLine("SHA256SUMS lists:`n$($listed -join "`n")`nthe release has:`n$($present -join "`n")")
        Stop-Script "SHA256SUMS and the release's packages do not match"
    }
    "Checking $($listed.Count) packages against SHA256SUMS"
    $bad = 0
    foreach ($s in $sums) {
        $actual = (Get-FileHash -Algorithm SHA256 -LiteralPath $s.Name).Hash.ToLowerInvariant()
        if ($actual -ceq $s.Hash) { "$($s.Name): OK" } else { "$($s.Name): FAILED"; $bad++ }
    }
    if ($bad) { Stop-Script 'a package does not match its hash; do not sign' }

    ''
    "SHA256SUMS for Nimbus ${version}:"
    Get-Content -Raw SHA256SUMS
    'Signing (minisign asks for the key''s password)'
    minisign -S -s $seckey -m SHA256SUMS -t "Nimbus $version SHA256SUMS"

    "Checking the signature against $pubkey"
    $verified = Invoke-Checked minisign -V -p $pubkey -m SHA256SUMS
    if ($null -eq $verified) {
        Stop-Script 'the signature does not check against minisign.pub; is that the right secret key?'
    }
    $verified
    if ([IO.File]::ReadAllLines("$work/SHA256SUMS.minisig") -cnotcontains "trusted comment: Nimbus $version SHA256SUMS") {
        Stop-Script "the trusted comment is not 'Nimbus $version SHA256SUMS'"
    }

    gh release upload $tag SHA256SUMS.minisig --repo $repo
    ''
    "Signed. $tag now has SHA256SUMS.minisig; publish the draft if it is one."
} finally {
    Pop-Location
    Remove-Item -Recurse -Force -ErrorAction Ignore $work
}
