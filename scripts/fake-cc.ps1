#!/usr/bin/env pwsh
# Stand-in C compiler for scripts/clippy-cross.ps1: writes an empty object
# file where the output flag points (`-o PATH`, or MSVC-style `-FoPATH`,
# which the `cc` crate falls back to when it cannot tell which compiler this
# is), and nothing else. Never use it for a real build.
#
# There is no param() block on purpose: without one, PowerShell hands every
# argument over in $args untouched, including ones that start with a dash.
$out = ''
for ($i = 0; $i -lt $args.Count; $i++) {
    $arg = [string]$args[$i]
    if ($arg -ceq '-o' -and $i + 1 -lt $args.Count) {
        $out = [string]$args[++$i]
    } elseif ($arg -cmatch '^[-/]Fo(.*)$') {
        $out = $Matches[1]
    }
}
if ($out) { [IO.File]::WriteAllBytes($out, [byte[]]@()) }
exit 0
