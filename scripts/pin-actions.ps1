#!/usr/bin/env pwsh
# Pin every GitHub Action in .github/workflows to a full commit SHA, and
# the release workflow's appimagetool download to its SHA-256 (M26).
#
# usage: scripts/pin-actions.ps1
#
# Why: a tag such as `actions/checkout@v7` can be moved to any commit by
# whoever controls that repository, and our release job runs it with a
# token that can write releases. A 40-character commit SHA cannot be
# moved. Each `uses: owner/repo@REF` whose REF is not already a SHA
# becomes `uses: owner/repo@SHA # VERSION`, where VERSION is the most
# exact tag on that commit (v7 becomes `# v7.0.1`). Dependabot reads that
# comment and keeps both the SHA and the comment current from then on, so
# this script is only needed when a new action is added (zizmor in CI
# fails until it is pinned).
#
# appimagetool: the release workflow names an exact version in
# APPIMAGETOOL_VERSION; this downloads that file and writes its SHA-256
# into APPIMAGETOOL_SHA256, which the workflow checks before running it.
#
# Needs git. Changes the files in place; look at `git diff` before
# committing.
#Requires -Version 7.4
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
Set-StrictMode -Version Latest

Set-Location (Join-Path $PSScriptRoot '..')
$workflows = Get-ChildItem .github/workflows/*.yml

# The SHA that ls-remote output gives for one ref name, or nothing.
function Find-Sha($remote, [string] $name) {
    foreach ($line in $remote) {
        if ($line.Ref -ceq $name) { return $line.Sha }
    }
}

# Every `uses: owner/repo[/path]@REF`, once each.
$refs = $workflows | Select-String -Pattern 'uses: *([A-Za-z0-9_.-]+/[A-Za-z0-9_./-]+@[A-Za-z0-9_./-]+)' -AllMatches |
    ForEach-Object { $_.Matches } | ForEach-Object { $_.Groups[1].Value } | Sort-Object -Unique -CaseSensitive

foreach ($ref in $refs) {
    $at = $ref.LastIndexOf('@')
    $action = $ref.Substring(0, $at)
    $rev = $ref.Substring($at + 1)
    if ($rev -cmatch '^[0-9a-f]{40}$') { continue }
    # owner/repo, without any path to an action inside the repository.
    $repo = ($action -split '/')[0..1] -join '/'
    # Each line of ls-remote is "SHA<tab>refs/...": make them objects.
    $remote = git ls-remote --tags --heads "https://github.com/$repo" | ForEach-Object {
        $sha, $name = $_ -split "`t", 2
        [pscustomobject]@{ Sha = $sha; Ref = $name }
    }
    # An annotated tag's own object is not the commit; its "^{}" line is.
    $sha = Find-Sha $remote "refs/tags/$rev^{}"
    if (-not $sha) { $sha = Find-Sha $remote "refs/tags/$rev" }
    $comment = $rev
    if ($sha) {
        # The longest tag on the same commit that starts with REF.: v7 -> v7.0.1.
        $exact = $remote |
            Where-Object { $_.Sha -ceq $sha -and $_.Ref.StartsWith("refs/tags/$rev.", [StringComparison]::Ordinal) } |
            ForEach-Object { $_.Ref -creplace '^refs/tags/', '' -creplace '\^\{\}$', '' } |
            Sort-Object Length, { $_ } | Select-Object -Last 1
        if ($exact) { $comment = $exact }
    } else {
        # A branch, such as dtolnay/rust-toolchain@stable.
        $sha = Find-Sha $remote "refs/heads/$rev"
    }
    if (-not $sha) {
        Write-Error "pin-actions: $repo has no tag or branch named $rev"
    }
    "$action@$rev -> $sha # $comment"
    foreach ($f in $workflows) {
        $text = [IO.File]::ReadAllText($f.FullName)
        $new = $text.Replace("uses: $action@$rev", "uses: $action@$sha # $comment")
        if ($new -cne $text) { [IO.File]::WriteAllText($f.FullName, $new) }
    }
}

$release = '.github/workflows/release.yml'
$text = [IO.File]::ReadAllText((Resolve-Path $release))
if ($text -cmatch '(?m)^  APPIMAGETOOL_VERSION: *"(.*)"\r?$') {
    $version = $Matches[1]
    $file = New-TemporaryFile
    try {
        Invoke-WebRequest -OutFile $file "https://github.com/AppImage/appimagetool/releases/download/$version/appimagetool-x86_64.AppImage"
        $hash = (Get-FileHash -Algorithm SHA256 $file).Hash.ToLowerInvariant()
    } finally {
        Remove-Item -Force $file
    }
    "appimagetool $version -> $hash"
    $text = $text -creplace '(?m)^  APPIMAGETOOL_SHA256: .*$', "  APPIMAGETOOL_SHA256: `"$hash`""
    [IO.File]::WriteAllText((Resolve-Path $release), $text)
}

'Done; review with: git diff .github/workflows'
