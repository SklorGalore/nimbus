# Compare and merge

Nimbus can show two texts side by side with their differences marked,
and merge two or three versions of a file into one.

## Comparing

A compare sits on an ordinary tab. The tab's own file stays on the
**right**, where you can keep editing it; the text it is compared with is
on the **left**, read-only. Start one from the palette:

| Command | Left side |
|---|---|
| Compare with Saved | The file as it is on disk, so you see your unsaved changes. |
| Compare with File… | A file you pick in a dialog. |
| Compare with Tab… | A copy of another tab's text as it is now. With only one other tab it is used straight away; otherwise a picker lists them. |

From the command line, `nimbus --diff OLD NEW` opens `NEW` in a tab and
compares it with `OLD`.

What you see:

- Rows line up across the two sides. Added, removed and changed lines
  have coloured backgrounds, and the words that changed within a line
  are marked more strongly. Both sides keep their syntax colours.
- Long stretches with no changes fold down to a row reading
  `··· N unchanged lines ···`, keeping three lines of context around each
  change. Click the row to open it.
- **Next Change** and **Previous Change** (`F7` and `Shift+F7`) move the
  cursor from change to change. The status bar reads, for example,
  `Compare with saved · change 3 of 12`.
- **Toggle Unified Compare** (palette) shows one column instead, with
  each change's old lines stacked above its new ones.
- Edits on the right update the comparison as you type.
- **Close Compare or Merge** (palette) returns the tab to a plain editor.

Very large texts are compared in the background; the status bar says
`comparing…` until the result is in. Beyond 256 MB the comparison is
coarser: it is redone after a pause in typing rather than on every
keystroke, and the status bar may warn that changes may not be
minimal.

## Merging

A merge tab has three columns: the left input, the **result** in the
middle, and the right input. The result is an ordinary document you can
edit; the inputs are read-only. For the change under the cursor, these
palette commands take a side, each as one undoable edit:

| Command | Puts in the result |
|---|---|
| Take Left | the left side's lines |
| Take Right | the right side's lines |
| Take Both | the left side's lines, then the right's |
| Take Both, Right First | the right side's lines, then the left's |

You can also just type in the result. `F7` and `Shift+F7` step through
the changes, and the status bar says which change you are on and what
was done with it: `(left taken)`, `(right taken)`, `(both taken)` or
`(edited)`.

### Two-way merge

**Merge Files…** asks for a left and a right file and opens a new tab
whose result starts as a copy of the left. Every difference is a choice
for you to make. The result is untitled; saving asks where to put it.

### Three-way merge

A three-way merge also knows the **base**, the version both sides
started from. A change made on only one side is applied for you; changes
that overlap are **conflicts**. Conflicts appear in the result as
git-style marker blocks (`<<<<<<<`, `=======`, `>>>>>>>`) and are counted
in the status bar (`2 conflicts left`). `F8` and `Shift+F8` jump between
conflicts. Resolve each by taking a side, both, or by editing by hand.
Saving with conflicts left asks first.

Ways to start one:

- **Merge with Base…** (palette): the active tab is yours (left); pick
  the base and then their version (right). The result opens in a new,
  untitled tab.
- **From the command line**, as git's merge tool:

  ```sh
  nimbus --merge BASE LOCAL REMOTE -o RESULT
  ```

  The result saves to `RESULT` (to `LOCAL` when `-o` is left out). The
  exit code is 0 only when the result was saved with no conflicts left,
  and 1 otherwise, so git knows whether the merge succeeded. To make
  Nimbus git's merge tool:

  ```sh
  git config --global merge.tool nimbus
  git config --global mergetool.nimbus.cmd 'nimbus --merge "$BASE" "$LOCAL" "$REMOTE" -o "$MERGED"'
  git config --global mergetool.nimbus.trustExitCode true
  ```

- **A file with conflict markers**: when you open a file that git left
  with markers in it, the message line says so. Run **Resolve
  Conflicts** (palette) and the tab becomes a three-way merge of what
  the markers separate (with the base too, if the markers are in git's
  `diff3` style). The tab's undo history starts again from there, and
  the result still saves to the same file.
- **Merge with Disk**: see below.

## When the file changes on disk under your edits

If a file you have unsaved changes in is changed by another program, the
tab is marked `(changed on disk)` and the message line offers four
palette commands:

- **Reload from Disk**: throw away your edits and load the new file.
- **Keep My Version**: ignore the change; saving will overwrite it.
- **Compare with Saved**: see how the file on disk differs from your
  text.
- **Merge with Disk**: a three-way merge in place of the tab, with the
  text as you last loaded it as the base, your edits on the left (`mine`)
  and the file as it is now on the right (`disk`). Save the result when
  you are happy with it.

Merge with Base, Resolve Conflicts and Merge with Disk refuse a tab
over 256 MB, or a huge file opened memory-mapped (see
[Large files](large-files.md)), with a message.

## Trying it out

The repository has small sample files for these views in
`examples/merge/`; `examples/README.md` there walks through them.
