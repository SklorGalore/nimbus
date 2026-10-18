# Large files

Nimbus is built to open files of any size quickly, including
multi-gigabyte logs and files with lines millions of characters long. You
do not have to do anything different; this chapter explains what changes
behind the scenes, and the few limits you may meet.

## How a file is held

| File | How Nimbus holds it |
|---|---|
| Under 256 MB | Read into memory. Everything works without limits worth mentioning. A file in another encoding, such as UTF-16 or Windows-1252, is converted to UTF-8 as it is read (see [Encodings](encodings.md)). |
| 256 MB or larger | *Memory-mapped*: the file stays on disk and Nimbus reads only the parts on screen, so a 4 GB log shows its first screen at once and uses little memory. Your edits are kept separately and written out when you save. |
| 256 MB or larger, in a known other encoding | Not converted, since that would mean holding it all in memory: opens **read-only**, its bytes shown as they are. "Known" means a UTF-16 byte order mark, `--encoding`, the `[encodings]` setting or Reopen with Encoding. |
| Not valid UTF-8 and not recognised as another encoding, any size | Held byte for byte (in memory under 256 MB, mapped above). Invalid bytes show as `�`, the status bar says `Not UTF-8`, and the bytes are written back unchanged unless you edit them. A huge file that only looks like Windows-1252 is held this way too, so it stays editable. |

## Counting lines

A memory-mapped file's lines are counted in the background after it
opens. Meanwhile:

- the status bar shows `Counting lines 42%`;
- line numbers not yet counted show as `~`, in the gutter and in the
  status bar (`Ln ~`);
- you can still type, scroll, search and jump to the end
  (`Cmd+Down` / `Ctrl+End`) or anywhere with the scrollbar;
- Go to Line for a line beyond the count waits and jumps when counting
  gets there.

## Highlighting

Files over 16 MB are not parsed with a full grammar. They get simpler,
line-by-line colours instead (comments, strings, keywords, numbers), and
the message line says "Over 16 MB: simpler syntax colours" the first
time. Features that need a syntax tree then fall back to simpler rules:
Expand Selection counts brackets, folding uses brackets and indentation,
and Go to Symbol has nothing to list. See
[Languages and highlighting](languages.md).

## Saving a huge file

A memory-mapped file is saved in the background, with progress in the
message line (`Saving log.txt… 40%`). While it saves you can move,
scroll and search, but edits wait until it is done. Closing the tab or
quitting during a save waits for the save to finish. The save is as safe
as any other: the old file is replaced only once the new one is complete.

The `trim_trailing_whitespace` and `final_newline` settings are skipped
for memory-mapped files, since either would mean reading the whole file.

## Limits you may meet

Each limit exists so that one command cannot read gigabytes on the main
thread and freeze the window. The message line says when you hit one.

| Operation | Limit |
|---|---|
| Copy and Cut | 64 MB of selected text |
| Line operations (sort, move, duplicate, comment and so on) | 64 MB of selected lines |
| Add Next Occurrence (`Cmd+D` / `Ctrl+D`) | looks 32 MB ahead per press; press again to go on |
| Find in the file | no limit; files of 1 MB or more are searched in the background |
| Replace All in the find bar | waits until the search has finished counting; refuses over 250,000 matches |
| Find in Files | skips files over 256 MB |
| Fold All, Fold Level N | files up to 256 MB |
| Compare | follows edits live up to 256 MB; larger texts are compared again after a pause |
| Merge commands | tabs up to 256 MB, and not memory-mapped |
| Column selection | 10,000 lines |

## Very long lines

A single line can be hundreds of megabytes (minified JSON, for example).
It opens and scrolls sideways without stalling, with the trackpad or
Home and End, and find jumps to matches deep inside it.

## When a huge file changes on disk

A file in memory reloads by itself when another program changes it (if
you have no unsaved edits). A memory-mapped file does not, since
reloading could mean gigabytes; the message line suggests Reload from
Disk instead. For a log that keeps growing, turn on **Follow Tail**
(click it in the status bar): new lines are then added at the end as
they arrive, at most five times a second, without reading the file
again. See [Following a growing file](window.md#following-a-growing-file).
If another program **cuts the file short**, Nimbus reloads
it at once when it can, or warns you to run Reload from Disk straight
away, because reading the part that disappeared can crash the editor.
See [Updates and troubleshooting](troubleshooting.md#when-a-file-changes-on-disk).
