# Navigating

Four *pickers* share one look: a box at the top of the window with a
query field and up to twelve rows below it. Typing narrows the list by
*fuzzy* matching: the letters you type must appear in order, not
necessarily together, so `gtl` finds "Go to Line…". Matching ignores case
unless you type a capital letter.

In every picker, Up and Down move the highlight (it wraps around), Page
Up and Page Down move twelve rows, Enter or a click chooses, and Escape
or a click outside the box closes it. In the command palette and Go to
File, pressing the same key again closes the picker, and pressing the
other one's key switches to it.

## Command palette

`Cmd+Shift+P` / `Ctrl+Shift+P`. Every command in Nimbus, with its
current shortcut beside it. Plain cursor movement and typing keys are
left out, since nobody looks those up by name. Use it to find a command
you do not know the key for, or to run the many commands that have no
key at all. The full list is in
[Keyboard shortcuts and commands](keybindings.md).

## Go to File

`Cmd+P` / `Ctrl+P`. Lists the files in the project folder (see
[Getting started](getting-started.md#opening-files-and-folders)) and
opens the one you choose, in the focused group.

- Results appear while the folder is still being listed, so even a large
  repository is usable at once.
- Files excluded by `.gitignore` and hidden files and folders (names
  starting with a dot) are left out.
- At most 300,000 files are listed, so opening your home folder by
  mistake cannot use unbounded memory.
- Matching works on the path, so `src/main` finds `src/main.rs`.

## Go to Line

`Cmd+G` / `Ctrl+G`. Type a line number and press Enter. A leading `:`
is allowed. The row beside it says how many lines the file has, or
"past the last line" when the number is too big, in which case Enter
goes to the last line.

In a huge file whose lines are still being counted, going to a line
beyond the count so far waits: the message line says "Going to line N
once lines are counted that far", and the jump happens when counting
gets there.

## Go to Symbol

`Cmd+Shift+R` / `Ctrl+Shift+R`. Lists the definitions in the current
file, such as functions, types, classes, constants and macros, with
their kind and line, and moves the cursor to the one you choose. In
Markdown it lists the headings.

Symbols come from the file's syntax tree, so this works for Rust,
Markdown, Python, JavaScript, TypeScript, TSX, C and Go. JSON, TOML,
YAML, Bash, HTML and CSS have no symbol list, and neither do files with
no tree (plain text, your own simple languages, files over 16 MB). The
message line says why when the list is empty. You can add or change what
is listed with a tags query (see
[customizing](customizing.md#the-other-query-kinds)).

## Brackets

The bracket pair around the cursor is outlined. **Go to Matching
Bracket** (`Cmd+Shift+\` / `Ctrl+Shift+\`) jumps to the partner of the
bracket next to the cursor, and **Select to Matching Bracket** (palette)
selects between them. Where the file has a syntax tree, brackets inside
strings and comments do not count.

## Folding

Folding hides the inside of a block so you can see its surroundings. A
folded line shows its first line followed by a chip such as `… 12
lines`; the hidden lines are still in the file.

**Which lines can fold.** In a file with a syntax tree, the language's
fold rules decide: functions, blocks, imports, Markdown sections, and so
on. Otherwise a line folds when it ends with an opening bracket (the fold
runs to the line before the closing one), or when the lines after it are
indented more deeply (blank lines belong to the block above them).

**With the mouse.** Hover over the gutter beside a line that can fold
and a marker appears; click it to fold or unfold. Folded lines keep
their marker. Click a fold's chip to unfold it.

**With the keyboard.**

| Command | macOS | Windows and Linux |
|---|---|---|
| Fold | `Cmd+Option+[` | `Ctrl+Alt+[` |
| Unfold | `Cmd+Option+]` | `Ctrl+Alt+]` |
| Toggle Fold | `Cmd+K` then `Cmd+L` | `Ctrl+K` then `Ctrl+L` |
| Fold All | `Cmd+K` then `Cmd+0` | `Ctrl+K` then `Ctrl+0` |
| Unfold All | `Cmd+K` then `Cmd+J` | `Ctrl+K` then `Ctrl+J` |
| Fold Level 1, 2, 3 | `Cmd+K` then `Cmd+1`, `2`, `3` | `Ctrl+K` then `Ctrl+1`, `2`, `3` |

- **Fold** folds the block that starts on the cursor's line, or, if none
  starts there, the innermost block around the cursor. The message line
  says "Nothing to fold here" when there is none.
- **Fold All** folds every block in the file. **Fold Level N** folds the
  blocks N levels deep, so Level 1 leaves only the outermost lines
  showing. Both scan the file in the background and are refused for files
  over 256 MB.
- The `Cmd+K` / `Ctrl+K` keys are *two-key shortcuts*: press the first,
  let go, then press the second within two seconds.

**How folds behave.**

- Editing inside a fold, or a find match, Go to Line or Go to Symbol
  landing inside one, opens it.
- The cursor steps over folded lines as if they were one line.
- Selecting across a fold selects the hidden text too, and copying
  includes it.
- Folding works with soft wrap on.
- Folds belong to the tab: the same file in another group keeps its own.
- Folds are not remembered after the tab closes.

Compare and merge views do not fold this way; they hide long unchanged
stretches instead (see [Compare and merge](compare-and-merge.md)).
