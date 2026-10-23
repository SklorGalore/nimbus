#!/usr/bin/env pwsh
# Remake the screenshots in assets/screenshots/ that the README and the
# user manual show. Run it after a change to how Nimbus looks, then look at
# the images before committing them.
#
#   scripts/screenshots.ps1          all of them
#   scripts/screenshots.ps1 hero     only the named ones
#
# Every shot is rendered with --headless, so no window opens and the screen
# may be locked. Shots are at a Retina scale (2x): a 2560x1600 image is a
# 1280x800 window. A throwaway config folder (NIMBUS_CONFIG_DIR) holds the
# sidebar's state, so your own config and state.toml are neither read nor
# changed.
#
# Cursor positions are found by searching the shown files rather than by
# fixed byte offsets, so the shots survive small edits to those files. If a
# pattern stops matching, the script stops and names it.
#
# Needs sips (macOS) or ImageMagick to turn the BMP frames into PNG. The
# languages grid needs ImageMagick, and the huge-file shot
# bench/out/log-4gb.log (bench/gen.ps1 logs); each is skipped, with a
# note, when missing.
#Requires -Version 7.4
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
Set-StrictMode -Version Latest

$wanted = [Collections.Generic.List[string]]::new()
foreach ($a in $args) { $wanted.Add([string]$a) }

Set-Location (Join-Path $PSScriptRoot '..')
$out = 'assets/screenshots'
New-Item -ItemType Directory -Force $out | Out-Null
$tmp = Join-Path ([IO.Path]::GetTempPath()) "nimbus-shots-$([IO.Path]::GetRandomFileName())"
New-Item -ItemType Directory $tmp | Out-Null

try {
    cargo build --release --quiet
    $nimbus = if ($IsWindows) { './target/release/nimbus.exe' } else { './target/release/nimbus' }

    # Config folders: with the sidebar showing (two widths), without it,
    # and with whitespace and indent guides drawn.
    foreach ($d in 'sidebar', 'bare', 'wide', 'guides') { New-Item -ItemType Directory "$tmp/$d" | Out-Null }
    [IO.File]::WriteAllText("$tmp/sidebar/state.toml", "sidebar_visible = true`nsidebar_width = 260.0`nsessions = []`n")
    [IO.File]::WriteAllText("$tmp/wide/state.toml", "sidebar_visible = true`nsidebar_width = 340.0`nsessions = []`n")
    [IO.File]::WriteAllText("$tmp/bare/state.toml", "sidebar_visible = false`nsessions = []`n")
    Copy-Item "$tmp/bare/state.toml" "$tmp/guides/state.toml"
    [IO.File]::WriteAllText("$tmp/guides/config.toml", "render_whitespace = `"all`"`nindent_guides = true`n")

    # Byte offset just after NEEDLE in FILE, searching from the first line
    # that contains AFTER (or from the start). Nimbus's --cursors takes byte
    # offsets. Latin-1 turns each byte into one character, so a character
    # index in the text is a byte index in the file.
    function Get-Offset([string] $path, [string] $needle, [string] $after = '') {
        $text = [IO.File]::ReadAllText((Resolve-Path $path), [Text.Encoding]::Latin1)
        $start = if ($after) { $text.IndexOf($after, [StringComparison]::Ordinal) } else { 0 }
        $at = if ($start -ge 0) { $text.IndexOf($needle, $start, [StringComparison]::Ordinal) } else { -1 }
        if ($at -lt 0) { throw "screenshots.ps1: '$needle' not found in $path" }
        $at + [Text.Encoding]::Latin1.GetByteCount($needle)
    }

    function ConvertTo-Png([string] $bmp, [string] $png) {
        if (Get-Command sips -ErrorAction Ignore) {
            sips -s format png $bmp --out $png | Out-Null
        } else {
            magick $bmp $png
        }
    }

    # Whether NAME is among the shots asked for (all when none are).
    function Test-Wanted([string] $name) {
        $wanted.Count -eq 0 -or $wanted.Contains($name)
    }

    # Shot NAME CONFIG ARGS...: render one frame with the given config folder.
    function Shot([string] $name, [string] $config) {
        if (-not (Test-Wanted $name)) { return }
        $env:NIMBUS_CONFIG_DIR = "$tmp/$config"
        $PSNativeCommandUseErrorActionPreference = $false
        & $nimbus @args --no-session --screenshot "$tmp/$name.bmp" > $null 2> "$tmp/$name.log"
        $code = $LASTEXITCODE
        $PSNativeCommandUseErrorActionPreference = $true
        if ($code -ne 0) {
            [Console]::Error.Write((Get-Content -Raw "$tmp/$name.log"))
            throw "screenshots.ps1: nimbus failed on the $name shot"
        }
        ConvertTo-Png "$tmp/$name.bmp" "$out/$name.png"
        "$out/$name.png"
    }

    $undo = 'crates/core/src/undo.rs'
    $tabs = $undo, 'crates/core/src/search.rs', 'crates/app/src/action.rs', 'README.md'
    # The cursor on `record_batch`, which scrolls History's methods into view.
    $at = Get-Offset $undo 'pub fn ' 'pub fn record_batch'
    # Five cursors after the field names of the Group pushed in History::record.
    # The last offset is the primary cursor, which the view scrolls to: the
    # bottom one, so the four above it stay in view.
    $group = 'self.undo.push(Group {'
    $many = (@('id', 'edits', 'batch_ends', 'before', 'after') |
        ForEach-Object { Get-Offset $undo "                $_" $group } | Sort-Object) -join ','
    # Split so this script is not among the matches.
    $pattern = 'COALESCE' + '_WINDOW'
    $big = '2560x1600'

    Shot hero sidebar . @tabs --cursors $at --headless $big
    Shot light sidebar . @tabs --cursors $at --theme light --headless $big
    Shot palette sidebar . @tabs --cursors $at --picker palette --headless $big
    Shot files sidebar . @tabs --cursors $at --picker files:buf --headless $big
    Shot find bare $undo --cursors $at --find edits --headless 2400x1000
    Shot multicursor bare $undo --cursors $many --headless 2000x1000
    Shot search wide . $undo --search $pattern --headless $big
    Shot diff bare --diff examples/merge/local.rs examples/merge/remote.rs --headless 2560x1400
    Shot merge bare --merge examples/merge/base.rs examples/merge/local.rs examples/merge/remote.rs `
        -o "$tmp/merged.rs" --headless 3200x1500

    # Reading and layout aids.
    Shot workspace sidebar crates/core crates/syntax crates/syntax/src/symbols.rs --headless 2400x1400
    Shot guides guides $undo --cursors $at --headless 2400x1000
    Shot wrap bare docs/manual/getting-started.md --wrap --headless 2000x1200
    Shot symbols sidebar . @tabs --cursors $at --picker symbols:record --headless $big
    Shot debug bare $undo --cursors $at --debug-overlay --headless 2400x1200

    # Folder compare: crates/core/src at the v0.2.0 tag against a fixed later
    # commit (nimbus 0.9.1), so the list does not change as main moves on.
    if (Test-Wanted folders) {
        foreach ($rev in @{ Rev = 'v0.2.0'; Dir = '0.2.0' }, @{ Rev = 'd265e7c'; Dir = '0.9.1' }) {
            $dir = "$tmp/folders/$($rev.Dir)"
            New-Item -ItemType Directory -Force $dir | Out-Null
            git archive -o "$tmp/folders.tar" $rev.Rev crates/core/src
            tar -x -f "$tmp/folders.tar" -C $dir --strip-components 3
        }
    }
    Shot folders wide --diff "$tmp/folders/0.2.0" "$tmp/folders/0.9.1" --headless 2000x1200
    # Only the sidebar: a folder compare leaves the editor an empty tab. The
    # wide sidebar is 340 points, 680 pixels plus its divider at 2x.
    if (Test-Wanted folders) {
        magick "$out/folders.png" -crop 682x1200+0+0 +repage "$out/folders.png"
    }

    # A 4 GB log, every line counted, with the cursor on the last line.
    $log = 'bench/out/log-4gb.log'
    if (Test-Path $log) {
        Shot hugefile bare $log --cursors (Get-Item $log).Length --wait-index --headless 1800x1000
    } elseif (Test-Wanted hugefile) {
        [Console]::Error.WriteLine('skipping hugefile: run bench/gen.ps1 logs first')
    }

    # Four languages in a grid: each rendered alone, then joined. (The grid
    # showed bench/gen.sh until the scripts moved to PowerShell, which Nimbus
    # does not highlight; a Markdown file took its place.)
    if (Test-Wanted languages) {
        if (Get-Command magick -ErrorAction Ignore) {
            $parts = foreach ($file in 'scripts/make-icons.py', 'docs/manual/getting-started.md', 'Cargo.toml', '.github/workflows/ci.yml') {
                $name = 'lang-' + ((Split-Path -Leaf $file) -replace '\.', '-')
                $wanted.Add($name)
                Shot $name bare $file --headless 1400x900 | Out-Null
                "$out/$name.png"
            }
            $gap = '-bordercolor', '#101114', '-border', '8'
            magick '(' $parts[0] $parts[1] @gap +append ')' `
                '(' $parts[2] $parts[3] @gap +append ')' -append "$out/languages.png"
            Remove-Item $parts
            "$out/languages.png"
        } else {
            [Console]::Error.WriteLine('skipping languages: needs ImageMagick (magick)')
        }
    }
} finally {
    Remove-Item -Recurse -Force -ErrorAction Ignore $tmp
}
