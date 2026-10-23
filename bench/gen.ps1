#!/usr/bin/env pwsh
# Generate large test files for performance checks (DESIGN.md §5).
#
#   bench/gen.ps1            10 MB and 100 MB source-like files
#   bench/gen.ps1 logs       also 1 GB and 4 GB log files (for M7)
#   bench/gen.ps1 longline   a single 100 MB line of minified JSON (for M7)
#   bench/gen.ps1 sizes      64, 128, 256 and 512 MB source files (for M7's
#                            rope-versus-mapped threshold; see threshold.ps1)
#   bench/gen.ps1 diff       edited copies of the 10 MB and 100 MB files, one
#                            change per thousand lines (for M9's compare)
#   bench/gen.ps1 difflogs   the 1 GB log with 164 scattered edits and the
#                            4 GB log with 100,000 lines appended (M9; makes
#                            the logs first)
#   bench/gen.ps1 git        a repository of 1,000,000 commits with a merge
#                            every 50 and a tag every 10,000, for the git
#                            window (M27); GIT_COMMITS=N for another size
#
# Output goes to bench/out/, which git ignores. Files are reused if present.
#
# The generators are a few lines of C# compiled when the script starts
# (Add-Type). A PowerShell loop runs a few hundred thousand lines a second,
# so the 4 GB log would take many minutes; compiled code writes it about
# as fast as the disk takes it, on macOS, Linux and Windows alike.
#Requires -Version 7.4
param([string] $Kind = '')
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
Set-StrictMode -Version Latest

$kinds = '', 'logs', 'longline', 'sizes', 'diff', 'difflogs', 'git'
if ($Kind -cnotin $kinds) {
    Write-Error "usage: bench/gen.ps1 [logs|longline|sizes|diff|difflogs|git]" -ErrorAction Continue
    exit 2
}

Set-Location $PSScriptRoot
New-Item -ItemType Directory -Force out | Out-Null
$out = (Resolve-Path out).Path

Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.Text;

public static class NimbusBench
{
    const int Buffer = 1 << 20;

    static Stream Create(string path) =>
        new FileStream(path, FileMode.Create, FileAccess.Write, FileShare.None, Buffer);

    static void Write(Stream s, string text)
    {
        byte[] b = Encoding.UTF8.GetBytes(text);
        s.Write(b, 0, b.Length);
    }

    // The seed repeated until the file has `size` bytes, cut to size, then
    // without its last line: a byte cut could split a multi-byte character
    // and make the file invalid UTF-8 (the last line goes even when the cut
    // falls on a line end, as `sed '$d'` did).
    public static void SourceLike(string path, byte[] seed, long size)
    {
        using (var s = Create(path))
        {
            for (long left = size; left > 0; left -= seed.Length)
                s.Write(seed, 0, (int)Math.Min(left, seed.Length));
        }
        long keep = 0;
        using (var s = new FileStream(path, FileMode.Open, FileAccess.Read))
        {
            // Look back from the second-to-last byte for the last line end.
            long from = size - 2;
            var block = new byte[65536];
            while (from >= 0 && keep == 0)
            {
                long start = Math.Max(0, from - block.Length + 1);
                int len = (int)(from - start + 1);
                s.Position = start;
                s.ReadExactly(block, 0, len);
                for (int i = len - 1; i >= 0; i--)
                    if (block[i] == (byte)'\n') { keep = start + i + 1; break; }
                from = start - 1;
            }
        }
        using (var s = new FileStream(path, FileMode.Open, FileAccess.Write))
            s.SetLength(keep);
    }

    // Log lines: timestamp, level, component, message with a varying number.
    public static void LogFile(string path, long size)
    {
        string[] lv = { "INFO", "WARN", "DEBUG", "ERROR", "TRACE" };
        string[] cp = { "http", "db", "cache", "auth", "queue" };
        using (var w = new StreamWriter(Create(path), new UTF8Encoding(false), Buffer))
        {
            w.NewLine = "\n";
            long bytes = 0;
            for (long n = 0; bytes < size; n++)
            {
                string line = string.Format(
                    "2026-10-15T{0:D2}:{1:D2}:{2:D2}.{3:D3}Z {4,-5} {5,-5} request {6} took {7} ms",
                    (n / 3600000) % 24, (n / 60000) % 60, (n / 1000) % 60, n % 1000,
                    lv[n % 5], cp[n % 5], n, (n * 7919) % 1000);
                w.WriteLine(line);
                bytes += line.Length + 1;
            }
        }
    }

    // One line of JSON objects, `size` bytes or a little over.
    public static void LongLine(string path, long size)
    {
        using (var w = new StreamWriter(Create(path), new UTF8Encoding(false), Buffer))
        {
            w.Write("[");
            long bytes = 1;
            for (long n = 0; bytes < size; n++)
            {
                string s = "{\"id\":" + n + ",\"name\":\"item" + n + "\",\"tags\":[\"a\",\"b\"]},";
                w.Write(s);
                bytes += s.Length;
            }
            w.Write("{}]\n");
        }
    }

    // The lines of a file as raw bytes, without their "\n", so text that is
    // not valid UTF-8 passes through unchanged.
    static IEnumerable<byte[]> Lines(string path)
    {
        using (var s = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.Read, Buffer))
        {
            var line = new MemoryStream();
            var block = new byte[Buffer];
            int len;
            while ((len = s.Read(block, 0, block.Length)) > 0)
            {
                int at = 0;
                for (int i = 0; i < len; i++)
                {
                    if (block[i] != (byte)'\n') continue;
                    line.Write(block, at, i - at);
                    yield return line.ToArray();
                    line.SetLength(0);
                    at = i + 1;
                }
                line.Write(block, at, len - at);
            }
            if (line.Length > 0) yield return line.ToArray();
        }
    }

    // A copy of a file with a deterministic edit every `every` lines,
    // cycling through a changed line, a deleted line and an inserted line,
    // so a diff of the two has changes of every shape spread through the
    // whole file.
    public static void EditedCopy(string src, string path, long every)
    {
        using (var s = Create(path))
        {
            long nr = 0;
            foreach (byte[] line in Lines(src))
            {
                nr++;
                if (nr % every == 0)
                {
                    long k = (nr / every) % 3;
                    if (k == 0) { Write(s, "    let changed_" + nr + " = other();\n"); continue; }
                    if (k == 1) continue;
                    Write(s, "    // inserted line " + nr + "\n");
                }
                s.Write(line, 0, line.Length);
                s.WriteByte((byte)'\n');
            }
        }
    }

    // A copy of a file with `lines` log lines appended: a log that grew.
    public static void GrownCopy(string src, string path, long lines)
    {
        File.Copy(src, path, true);
        using (var w = new StreamWriter(new FileStream(path, FileMode.Append, FileAccess.Write, FileShare.None, Buffer), new UTF8Encoding(false), Buffer))
        {
            for (long i = 0; i < lines; i++)
                w.Write("2026-10-16T00:00:00.000Z INFO  tail  appended " + i + "\n");
        }
    }

    // A `git fast-import` stream (see gen.ps1's git section), written
    // straight into git's standard input.
    public static void GitHistory(string repo, long n)
    {
        var psi = new ProcessStartInfo("git") { RedirectStandardInput = true, UseShellExecute = false };
        foreach (var a in new[] { "-C", repo, "fast-import", "--quiet" }) psi.ArgumentList.Add(a);
        using (var git = Process.Start(psi))
        {
            var w = new StreamWriter(git.StandardInput.BaseStream, new UTF8Encoding(false), Buffer);
            w.NewLine = "\n";
            long t = 1500000000, mark = 0, total = 0, main = 0, tags = 0;
            void Commit(string reference, long from, long merge, string msg)
            {
                mark++; total++; t += 60;
                w.Write("commit " + reference + "\nmark :" + mark + "\n");
                w.Write("author Author " + total % 37 + " <a" + total % 37 + "@example.com> " + t + " +0000\n");
                w.Write("committer Bench <bench@example.com> " + t + " +0000\n");
                w.Write("data " + msg.Length + "\n" + msg + "\n");
                if (from != 0) w.Write("from :" + from + "\n");
                if (merge != 0) w.Write("merge :" + merge + "\n");
                string body = "line " + total + "\n";
                w.Write("M 100644 inline src/f" + total % 100 + ".txt\ndata " + body.Length + "\n" + body + "\n");
            }
            while (total < n)
            {
                if (total % 50 == 49 && main > 0 && total < n - 4)
                {
                    long side = main;
                    for (int k = 0; k < 3; k++)
                    {
                        Commit("refs/heads/side", side, 0, "Side work " + total);
                        side = mark;
                    }
                    Commit("refs/heads/main", main, side, "Merge branch side at " + total);
                }
                else
                {
                    Commit("refs/heads/main", main, 0, "Change number " + total + " of the bench history");
                }
                main = mark;
                if (total / 10000 > tags)
                {
                    tags++;
                    w.Write("reset refs/tags/v" + tags + "\nfrom :" + main + "\n\n");
                }
            }
            w.Flush();
            git.StandardInput.Close();
            git.WaitForExit();
            if (git.ExitCode != 0) throw new Exception("git fast-import failed with exit code " + git.ExitCode);
        }
    }
}
'@

# A file's size for the "wrote" lines, the way `du -h` shows it.
function Format-Size([long] $bytes) {
    if ($bytes -ge 1GB) { '{0:0.#}G' -f ($bytes / 1GB) }
    elseif ($bytes -ge 1MB) { '{0:0.#}M' -f ($bytes / 1MB) }
    else { '{0:0.#}K' -f ($bytes / 1KB) }
}

# Run one generator unless its output is already there.
function Make([string] $name, [scriptblock] $make) {
    $path = Join-Path $out $name
    if (Test-Path $path) { "exists: out/$name"; return }
    # Write to a temporary name first, so a run stopped halfway leaves no
    # short file behind for the next run to reuse.
    $tmp = "$path.tmp"
    & $make $tmp
    Move-Item -Force $tmp $path
    "wrote out/$name ($(Format-Size (Get-Item $path).Length))"
}

# Concatenate the project's own Rust sources until the target size is reached,
# so the text looks like real code: indentation, comments, long and short lines.
function Source-Like([string] $name, [long] $size) {
    Make $name {
        param($path)
        $files = Get-ChildItem -Recurse -File -Filter *.rs ../crates | Sort-Object FullName
        $seed = [IO.MemoryStream]::new()
        foreach ($f in $files) {
            $bytes = [IO.File]::ReadAllBytes($f.FullName)
            $seed.Write($bytes, 0, $bytes.Length)
        }
        [NimbusBench]::SourceLike($path, $seed.ToArray(), $size)
    }
}

function Log-File([string] $name, [long] $size) {
    Make $name { param($path) [NimbusBench]::LogFile($path, $size) }
}

function Long-Line([string] $name, [long] $size) {
    Make $name { param($path) [NimbusBench]::LongLine($path, $size) }
}

function Edited-Copy([string] $src, [string] $name, [long] $every) {
    Make $name { param($path) [NimbusBench]::EditedCopy((Join-Path $out $src), $path, $every) }
}

function Grown-Copy([string] $src, [string] $name, [long] $lines) {
    Make $name { param($path) [NimbusBench]::GrownCopy((Join-Path $out $src), $path, $lines) }
}

# A git repository with a long history, built in one pass by `git
# fast-import` (a stream of commits on stdin, no working tree involved):
# a main line with a side branch of three commits forked and merged back
# every 50 commits, so the graph has curves, and a tag every 10,000. Then
# the commit-graph file, which lets `git log --date-order` stream at once
# instead of reading every commit first, as `git gc` would write it.
function Git-Repo([string] $name, [long] $n) {
    $repo = Join-Path $out $name
    if (Test-Path $repo) { "exists: out/$name"; return }
    git init -q $repo
    git -C $repo symbolic-ref HEAD refs/heads/main
    [NimbusBench]::GitHistory($repo, $n)
    git -C $repo reset -q --hard main
    git -C $repo commit-graph write --reachable
    $count = git -C $repo rev-list --count --all
    $bytes = (Get-ChildItem -Recurse -File -Force $repo | Measure-Object -Sum Length).Sum
    "wrote out/$name ($count commits, $(Format-Size $bytes))"
}

if ($Kind -ceq 'git') {
    $commits = if ($env:GIT_COMMITS) { [long]$env:GIT_COMMITS } else { 1000000 }
    Git-Repo git-1m $commits
    exit 0
}

Source-Like src-10mb.rs 10MB
Source-Like src-100mb.rs 100MB

switch -CaseSensitive ($Kind) {
    'logs' {
        Log-File log-1gb.log 1GB
        Log-File log-4gb.log 4GB
    }
    'longline' {
        Long-Line longline-100mb.json 100MB
    }
    'diff' {
        Edited-Copy src-10mb.rs src-10mb-edited.rs 1000
        Edited-Copy src-100mb.rs src-100mb-edited.rs 1000
    }
    'difflogs' {
        Log-File log-1gb.log 1GB
        Log-File log-4gb.log 4GB
        Edited-Copy log-1gb.log log-1gb-edited.log 100000
        Grown-Copy log-4gb.log log-4gb-grown.log 100000
    }
    'sizes' {
        foreach ($mb in 64, 128, 256, 512) {
            Source-Like "src-${mb}mb.rs" ($mb * 1MB)
        }
    }
}
