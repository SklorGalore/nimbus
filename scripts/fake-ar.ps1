#!/usr/bin/env pwsh
# Stand-in archiver for scripts/clippy-cross.ps1: writes an empty static
# library where the output points (`ar crs PATH ...`, or MSVC-style
# `-out:PATH`). Never use it for a real build.
#
# PowerShell splits an argument shaped like `-name:value` in two when it
# starts a script, so `-out:PATH` arrives as `-out` then `PATH`; the
# `/out:PATH` spellings arrive whole.
$out = ''
for ($i = 0; $i -lt $args.Count; $i++) {
    $arg = [string]$args[$i]
    if ($arg -ceq '-out' -and $i + 1 -lt $args.Count) {
        $out = [string]$args[++$i]
    } elseif ($arg -cmatch '^(-out|/out|/OUT):(.*)$') {
        $out = $Matches[2]
    }
}
# ar-style: the first argument after the flags word is the archive.
if (-not $out -and $args.Count -ge 2) { $out = [string]$args[1] }
# An empty archive is just the magic line "!<arch>" and a newline.
if ($out) { [IO.File]::WriteAllText($out, "!<arch>`n") }
exit 0
