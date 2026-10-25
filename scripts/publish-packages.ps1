#!/usr/bin/env pwsh
# Publish a signed, published release to Homebrew and Chocolatey.
#
# usage: scripts/publish-packages.ps1 vX.Y.Z [--dry-run] [--only homebrew|chocolatey]
#                                     [--tap OWNER/homebrew-NAME]
#
# This is the last release step, after scripts/sign-release.ps1 and
# publishing the draft on GitHub (docs/releasing.md):
#
#   1. download the release's SHA256SUMS and SHA256SUMS.minisig and check
#      the signature against minisign.pub in this repository, with the
#      trusted comment "Nimbus X.Y.Z SHA256SUMS". Nothing is published
#      from a release that is unsigned or signed by another key;
#   2. take the hashes of the .dmg and the Windows installer from that
#      SHA256SUMS, so the cask and the Chocolatey install script name
#      exactly the signed files, and fill in the templates in packaging/;
#   3. check the release is published (Homebrew and Chocolatey download
#      from the public URLs, which a draft does not have) and that those
#      URLs answer;
#   4. show the cask and the Chocolatey scripts and ask before going
#      further;
#   5. Homebrew: write Casks/nimbus.rb in the tap repository (default
#      SklorGalore/homebrew-nimbus, so `brew install --cask
#      sklorgalore/nimbus/nimbus` works), offering to create the tap the
#      first time;
#   6. Chocolatey: download the release's nimbus.X.Y.Z.nupkg (packed on
#      Windows by .github/workflows/chocolatey.yml), check it holds only
#      the package files and that its scripts are byte for byte the ones
#      filled in at step 2, and push it to the community repository
#      (https://community.chocolatey.org/packages/nimbus), where it waits
#      for Chocolatey's moderators.
#
# The tap is written through the GitHub API with gh, so nothing is
# cloned. --dry-run does steps 1 and 2 only and prints the files; it
# works on a draft too. --only does one of the two package managers.
# Running it again for a version already published changes nothing.
#
# Needs gh (signed in as an account that can push to the tap) and
# minisign. The Chocolatey push needs your API key from
# https://community.chocolatey.org/account. It comes from
# CHOCOLATEY_API_KEY if set, else from the macOS Keychain item
# "chocolatey-api-key" if there is one (save it once with
# `security add-generic-password -a "$USER" -s chocolatey-api-key -w`,
# which prompts for the key), else you paste it when asked. It is never
# echoed, saved, or put on a command line. NIMBUS_REPO overrides the
# repository (default SklorGalore/nimbus).
#Requires -Version 7.4
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
Set-StrictMode -Version Latest
Add-Type -AssemblyName System.IO.Compression.FileSystem

$repo = if ($env:NIMBUS_REPO) { $env:NIMBUS_REPO } else { 'SklorGalore/nimbus' }
$here = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$pubkey = Join-Path $here 'minisign.pub'
$usage = 'usage: scripts/publish-packages.ps1 vX.Y.Z [--dry-run] [--only homebrew|chocolatey] [--tap OWNER/homebrew-NAME]'

# Print a message and stop with exit code 1.
function Stop-Script([string] $message) {
    [Console]::Error.WriteLine("publish-packages: $message")
    exit 1
}

# Run a program, returning its output, or $null if it failed: for the
# programs whose failure has a message of our own. -Quiet hides its stderr.
function Invoke-Checked {
    $quiet = $args[0] -ceq '-Quiet'
    $cmd = @($args | Select-Object -Skip ([int]$quiet))
    $PSNativeCommandUseErrorActionPreference = $false
    $out = if ($quiet) {
        & $cmd[0] @($cmd | Select-Object -Skip 1) 2> $null
    } else {
        & $cmd[0] @($cmd | Select-Object -Skip 1)
    }
    if ($LASTEXITCODE -ne 0) { return $null }
    , @($out)
}

# Ask a yes/no question; only y or Y is yes.
function Confirm-Step([string] $question) {
    $answer = Read-Host "$question [y/N]"
    $answer -ceq 'y' -or $answer -ceq 'Y'
}

# Arguments, read by hand: PowerShell's own parameters take a single
# dash, and this keeps the --long spellings the sh script had.
$tag = ''
$dryRun = $false
$only = ''
$tap = 'SklorGalore/homebrew-nimbus'
for ($i = 0; $i -lt $args.Count; $i++) {
    $arg = [string]$args[$i]
    switch -CaseSensitive -Regex ($arg) {
        '^--dry-run$' { $dryRun = $true; break }
        '^--only$' {
            if ($i + 1 -ge $args.Count) { Stop-Script $usage }
            $only = [string]$args[++$i]; break
        }
        '^--tap$' {
            if ($i + 1 -ge $args.Count) { Stop-Script $usage }
            $tap = [string]$args[++$i]; break
        }
        '^(-h|--help)$' { $usage; exit 0 }
        '^v' {
            if ($tag) { Stop-Script $usage }
            $tag = $arg; break
        }
        default { Stop-Script $usage }
    }
}
if (-not $tag) { Stop-Script $usage }
if ($tag -cnotmatch '^v[0-9]+\.[0-9]+\.[0-9]+') { Stop-Script "the tag must look like v1.2.3, not '$tag'" }
if ($only -cnotin '', 'homebrew', 'chocolatey') { Stop-Script "--only takes homebrew or chocolatey, not '$only'" }
if ($tap -cnotmatch '^[^/]+/homebrew-.+$') { Stop-Script "a tap repository is named OWNER/homebrew-NAME, not '$tap'" }
$version = $tag.Substring(1)

foreach ($tool in 'gh', 'minisign') {
    if (-not (Get-Command $tool -ErrorAction Ignore)) { Stop-Script "$tool is not installed (brew install $tool)" }
}
if (-not (Test-Path $pubkey)) { Stop-Script "no minisign.pub at $pubkey" }

$work = Join-Path ([IO.Path]::GetTempPath()) "nimbus-publish-$version.$([IO.Path]::GetRandomFileName())"
New-Item -ItemType Directory $work | Out-Null
try {
    # 1. The signature. sign-release.ps1 made it with the key whose public
    # half is minisign.pub; a SHA256SUMS that does not check against it did
    # not come from that key, so nothing below may trust its hashes.
    $draft = Invoke-Checked gh release view $tag --repo $repo --json isDraft --jq '.isDraft'
    if ($null -eq $draft) { Stop-Script "no release $tag in $repo (is gh signed in?)" }
    $isDraft = "$draft".Trim() -ceq 'true'
    "Downloading SHA256SUMS and its signature for $tag"
    if ($null -eq (Invoke-Checked gh release download $tag --repo $repo --dir $work `
                --pattern 'SHA256SUMS' --pattern 'SHA256SUMS.minisig')) {
        Stop-Script "could not download SHA256SUMS from $tag"
    }
    if (-not (Test-Path "$work/SHA256SUMS.minisig")) {
        Stop-Script "$tag has no SHA256SUMS.minisig; run scripts/sign-release.ps1 $tag first"
    }
    $verified = Invoke-Checked minisign -V -p $pubkey -m "$work/SHA256SUMS"
    if ($null -eq $verified) { Stop-Script 'SHA256SUMS.minisig does not check against minisign.pub; do not publish' }
    $verified
    if ([IO.File]::ReadAllLines("$work/SHA256SUMS.minisig") -cnotcontains "trusted comment: Nimbus $version SHA256SUMS") {
        Stop-Script "the signature's trusted comment is not 'Nimbus $version SHA256SUMS'"
    }

    # 2. The hashes, from the signed file. (A file name in SHA256SUMS may
    # start with "*", meaning binary mode.)
    $sums = [IO.File]::ReadAllLines("$work/SHA256SUMS")
    function Get-SignedHash([string] $name) {
        foreach ($line in $sums) {
            $fields = $line -split '\s+', 2
            if ($fields.Count -eq 2 -and ($fields[1] -creplace '^\*', '') -ceq $name) { return $fields[0] }
        }
        Stop-Script "SHA256SUMS has no line for $name"
    }
    $dmg = "Nimbus-$version-macos.dmg"
    $setup = "nimbus-$version-windows-x86_64-setup.exe"
    $dmgHash = Get-SignedHash $dmg
    $setupHash = Get-SignedHash $setup

    # Fill in a template: the same text with @VERSION@ and @SHA256@
    # replaced, written as UTF-8 without a byte order mark (as
    # .github/workflows/chocolatey.yml does), so the result compares byte
    # for byte with the packed copy.
    function Write-Template([string] $from, [string] $to, [string] $hash) {
        $text = [IO.File]::ReadAllText($from).Replace('@VERSION@', $version).Replace('@SHA256@', $hash)
        [IO.File]::WriteAllText($to, $text, [Text.UTF8Encoding]::new($false))
    }
    $cask = Join-Path $work 'nimbus.rb'
    Write-Template "$here/packaging/homebrew/nimbus.rb.in" $cask $dmgHash
    # The Chocolatey scripts as they must appear in the .nupkg. (The nuspec
    # is rewritten by `choco pack`, so only its id, version and icon are
    # checked.)
    $choco = Join-Path $work 'choco'
    New-Item -ItemType Directory "$choco/tools" | Out-Null
    $scripts = 'chocolateyInstall.ps1', 'chocolateyUninstall.ps1'
    Write-Template "$here/packaging/chocolatey/tools/chocolateyInstall.ps1.in" "$choco/tools/chocolateyInstall.ps1" $setupHash
    Write-Template "$here/packaging/chocolatey/tools/chocolateyUninstall.ps1" "$choco/tools/chocolateyUninstall.ps1" $setupHash

    function Show-Files {
        if ($only -cne 'chocolatey') {
            "==== Homebrew cask: $tap, Casks/nimbus.rb"
            [IO.File]::ReadAllText($cask).TrimEnd("`n")
        }
        if ($only -cne 'homebrew') {
            foreach ($s in $scripts) {
                "==== Chocolatey: nimbus $version, tools/$s"
                [IO.File]::ReadAllText("$choco/tools/$s").TrimEnd("`n")
            }
        }
    }

    if ($dryRun) {
        Show-Files
        ''
        if ($isDraft) {
            "Dry run: nothing published. $tag is still a draft; publish it on GitHub before a real run."
        } else {
            'Dry run: nothing published.'
        }
        exit 0
    }

    # 3. Homebrew and Chocolatey users download from the release's public
    # URLs, which only exist once the draft is published.
    if ($isDraft) { Stop-Script "$tag is still a draft; publish it on GitHub first (--dry-run works on a draft)" }
    foreach ($f in $dmg, $setup) {
        $url = "https://github.com/$repo/releases/download/$tag/$f"
        try {
            Invoke-WebRequest -Method Head -Uri $url -MaximumRedirection 10 | Out-Null
        } catch {
            Stop-Script "$url does not download"
        }
    }

    # 4. Look before anything leaves this machine.
    Show-Files
    ''
    if (-not (Confirm-Step "Publish these for Nimbus ${version}?")) { Stop-Script 'stopped; nothing published' }

    # Puts a local file at a path in a repository through the contents
    # API. Updating needs the current file's blob sha; a new file does not.
    # The request body goes through a file, so the content is not limited
    # by the length of a command line.
    function Set-RepoFile([string] $repoName, [string] $branch, [string] $path, [string] $file, [string] $message, [string] $oldSha) {
        $body = [ordered]@{
            message = $message
            branch = $branch
            content = [Convert]::ToBase64String([IO.File]::ReadAllBytes($file))
        }
        if ($oldSha) { $body.sha = $oldSha }
        $json = Join-Path $work 'put.json'
        [IO.File]::WriteAllText($json, ($body | ConvertTo-Json -Compress))
        gh api -X PUT "repos/$repoName/contents/$path" --input $json --silent
    }

    function Publish-Homebrew {
        ''
        "Homebrew: $tap"
        if ($null -eq (Invoke-Checked -Quiet gh repo view $tap)) {
            if (-not (Confirm-Step "$tap does not exist. Create it as a public repository?")) {
                Stop-Script 'no tap; Homebrew not published'
            }
            gh repo create $tap --public --add-readme `
                --description 'Homebrew tap for Nimbus, a fast GPU text editor'
        }
        $branch = "$(gh api "repos/$tap" --jq '.default_branch')".Trim()
        $oldSha = ''
        $existing = Invoke-Checked -Quiet gh api "repos/$tap/contents/Casks/nimbus.rb?ref=$branch"
        if ($null -ne $existing) {
            $old = ($existing -join "`n") | ConvertFrom-Json
            $oldSha = $old.sha
            $oldBytes = [Convert]::FromBase64String(($old.content -replace '\s', ''))
            $newBytes = [IO.File]::ReadAllBytes($cask)
            if ([Linq.Enumerable]::SequenceEqual($oldBytes, $newBytes)) {
                'Casks/nimbus.rb is already this cask; nothing to do.'
                return
            }
            $oldText = [Text.Encoding]::UTF8.GetString($oldBytes)
            if ($oldText -cmatch '(?m)^ *version "([0-9]+\.[0-9]+\.[0-9]+)"') {
                $oldVersion = $Matches[1]
                if ([version]$oldVersion -gt [version]$version) {
                    Stop-Script "the tap already has Nimbus $oldVersion, newer than $version; Homebrew not published"
                }
            }
        }
        Set-RepoFile $tap $branch Casks/nimbus.rb $cask "nimbus $version" $oldSha
        $owner = ($tap -split '/')[0].ToLowerInvariant()
        $name = ($tap -split '/homebrew-', 2)[1]
        "Published. Users run: brew install --cask $owner/$name/nimbus"
    }

    function Publish-Chocolatey {
        ''
        "Chocolatey: nimbus $version"
        $nupkg = "nimbus.$version.nupkg"
        $missing = "$tag has no $nupkg; run the Chocolatey package workflow for $tag (Actions > Chocolatey package > Run workflow) and try again"
        if ($null -eq (Invoke-Checked -Quiet gh release download $tag --repo $repo --dir $work --pattern $nupkg)) {
            Stop-Script $missing
        }
        $package = Join-Path $work $nupkg
        if (-not (Test-Path $package)) { Stop-Script $missing }

        # Only the package files: the nuspec, the two scripts, and the
        # bookkeeping files every .nupkg has.
        try {
            $zip = [IO.Compression.ZipFile]::OpenRead($package)
        } catch {
            Stop-Script "$nupkg is not a zip file"
        }
        try {
            $allowed = 'nimbus.nuspec', 'tools/chocolateyInstall.ps1', 'tools/chocolateyUninstall.ps1',
                '[Content_Types].xml', '_rels/.rels'
            $packed = @{}
            foreach ($entry in $zip.Entries) {
                $name = $entry.FullName
                if ($name.EndsWith('/')) { continue } # a folder entry holds nothing
                if ($name -cnotin $allowed -and
                    $name -cnotmatch '^package/services/metadata/core-properties/[^/]*\.psmdcp$') {
                    Stop-Script "$nupkg holds an unexpected file, $name; do not publish"
                }
                if ($name -cin 'nimbus.nuspec', 'tools/chocolateyInstall.ps1', 'tools/chocolateyUninstall.ps1') {
                    $stream = $entry.Open()
                    $copy = [IO.MemoryStream]::new()
                    $stream.CopyTo($copy)
                    $stream.Dispose()
                    $packed[$name] = $copy.ToArray()
                }
            }
        } finally {
            $zip.Dispose()
        }

        # Its scripts must be exactly the ones filled in from the signed
        # SHA256SUMS: those carry the installer's URL and hash.
        foreach ($s in $scripts) {
            $f = "tools/$s"
            if (-not $packed.ContainsKey($f)) { Stop-Script "$nupkg has no $f" }
            if (-not [Linq.Enumerable]::SequenceEqual([byte[]]$packed[$f], [IO.File]::ReadAllBytes("$choco/$f"))) {
                Stop-Script "$nupkg's $f differs from packaging/chocolatey filled in from the signed SHA256SUMS; do not publish"
            }
        }
        if (-not $packed.ContainsKey('nimbus.nuspec')) { Stop-Script "$nupkg has no nimbus.nuspec" }
        $nuspec = [Text.Encoding]::UTF8.GetString($packed['nimbus.nuspec'])
        if (-not $nuspec.Contains('<id>nimbus</id>')) { Stop-Script "$nupkg's nuspec is not for the package id nimbus" }
        if (-not $nuspec.Contains("<version>$version</version>")) { Stop-Script "$nupkg's nuspec is not version $version" }
        # Its icon must be the template's jsDelivr URL pinned to the tag's
        # commit: Chocolatey's moderators refuse raw.githubusercontent.com,
        # and a .nupkg packed before that fix still has it.
        $commit = Invoke-Checked gh api "repos/$repo/commits/$tag" --jq .sha
        if ($null -eq $commit) { Stop-Script "could not find the commit $tag points at" }
        $commit = "$commit".Trim()
        $iconLine = [IO.File]::ReadAllLines("$here/packaging/chocolatey/nimbus.nuspec.in") |
            Where-Object { $_.Contains('<iconUrl>') } | Select-Object -First 1
        if (-not $iconLine) { Stop-Script 'packaging/chocolatey/nimbus.nuspec.in has no iconUrl' }
        $icon = $iconLine.Trim().Replace('@COMMIT@', $commit)
        if (-not $nuspec.Contains($icon)) {
            Stop-Script "$nupkg's iconUrl is not $icon; rerun the Chocolatey package workflow for $tag (Actions > Chocolatey package > Run workflow) and try again"
        }
        "$nupkg matches the signed SHA256SUMS."

        $key = $env:CHOCOLATEY_API_KEY
        if (-not $key -and $IsMacOS) {
            # macOS may ask once to let the terminal read the item.
            $user = if ($env:USER) { $env:USER } else { "$(id -un)".Trim() }
            $found = Invoke-Checked -Quiet security find-generic-password -a $user -s chocolatey-api-key -w
            if ($null -ne $found -and "$found".Trim()) {
                $key = "$found".Trim()
                'Using the API key from the Keychain item chocolatey-api-key.'
            }
        }
        if (-not $key) {
            $secure = Read-Host -AsSecureString 'Chocolatey API key (https://community.chocolatey.org/account)'
            $key = [Net.NetworkCredential]::new('', $secure).Password
        }
        if (-not $key) { Stop-Script 'no API key; Chocolatey not published' }
        # NuGet v2 push: a PUT with the .nupkg as a form part. The key
        # stays inside this process, in a request header.
        try {
            $response = Invoke-WebRequest -Method Put -Uri 'https://push.chocolatey.org/api/v2/package/' `
                -Headers @{ 'X-NuGet-ApiKey' = $key } -Form @{ package = Get-Item $package } `
                -SkipHttpErrorCheck
        } catch {
            Stop-Script "could not reach push.chocolatey.org ($($_.Exception.Message))"
        }
        switch ([int]$response.StatusCode) {
            { $_ -in 200, 201, 202 } {
                "Pushed. Chocolatey's automatic checks run first, then a moderator approves it; until then it is listed at https://community.chocolatey.org/packages/nimbus/$version as waiting for review."
            }
            409 { "Chocolatey already has nimbus $version; nothing to do." }
            default {
                [Console]::Error.WriteLine($response.Content)
                Stop-Script "Chocolatey refused the push (HTTP $($response.StatusCode))"
            }
        }
    }

    if ($only -cne 'chocolatey') { Publish-Homebrew }
    if ($only -cne 'homebrew') { Publish-Chocolatey }
    ''
    "Done for Nimbus $version."
} finally {
    Remove-Item -Recurse -Force -ErrorAction Ignore $work
}
