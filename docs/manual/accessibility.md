# Accessibility

Nimbus draws its own text on the GPU, so nothing in its window is a
control the operating system already knows how to describe. To make it
readable, Nimbus describes itself to the system's accessibility
interface: VoiceOver on macOS, Narrator and NVDA on Windows, and Orca
on Linux. This chapter says what a screen reader can read, how to reach
every part of the window from the keyboard, and what to do when
something does not work.

None of this costs anything when no screen reader is running: Nimbus
builds its description only once a screen reader asks for it.

## What a screen reader reads

| Part of the window | What it is described as |
|---|---|
| The text | A multi-line text field. Moving by character, word or line speaks what you moved over; selecting with Shift speaks the selection; typing echoes characters. Wrapped rows are read as separate lines. |
| Tabs | A tab list. Each tab says its name, whether it has unsaved changes, and its place ("2 of 5"). |
| The sidebar's file tree | A tree. Each row says its name, its level, and whether a folder is open. |
| The command palette, Go to File, Go to Line and the other pickers | A dialog with a text field and a list. The highlighted item is spoken as you arrow through the list, with its place in the list. |
| The find bar | Labelled find and replace fields. The match count ("3 of 12") is spoken when it changes. |
| The Search panel | Labelled query, replacement, include and exclude fields; the result count is spoken when it changes. |
| The status bar | A status region. It is read when you ask for it, not on every cursor move. |
| The message line | A polite live region: messages such as "Saved" or "Reloaded from disk" are spoken without moving focus. |

Not described yet: the Search panel's list of results, the sidebar's
Compare panel, the rows of the compare and merge views (each side reads
as plain text), the Markdown preview's headings (it reads as plain
text), and any group but the focused one when the window is split.
The minimap and sticky scroll are decorative and are not read.

**Huge files.** In a very large file, Nimbus describes only the 200
lines either side of the cursor (less when lines are long). Moving
toward the edge of that window moves the window with you, so you can
read the whole file; a screen reader's "read all" stops at the window's
end, and continues once you move on.

## Reaching everything from the keyboard

| Go to | macOS | Windows and Linux |
|---|---|---|
| The text | `Escape` from any panel | `Escape` from any panel |
| The file tree | `Cmd+Shift+E` | `Ctrl+Shift+E` |
| Show or hide the sidebar | `Cmd+B` | `Ctrl+B` |
| The command palette | `Cmd+Shift+P` | `Ctrl+Shift+P` |
| Go to File | `Cmd+P` | `Ctrl+P` |
| Find in the file | `Cmd+F` | `Ctrl+F` |
| The Search panel | `Cmd+Shift+F` | `Ctrl+Shift+F` |
| Next or previous tab | `Ctrl+Tab`, `Ctrl+Shift+Tab` | `Ctrl+Tab`, `Ctrl+Shift+Tab` |
| Tab 1 to 8 | `Cmd+1` … `Cmd+8` | `Ctrl+1` … `Ctrl+8` |
| The left or right group in a split | `Cmd+Option+Left`, `Cmd+Option+Right` | `Ctrl+Alt+Left`, `Ctrl+Alt+Right` |

Inside the tree, the arrow keys move and open folders and `Escape`
returns to the text (see [The window](window.md#the-sidebar)). In the
Search panel, `Tab` and `Shift+Tab` move between the fields and the
results (see [Find and replace](find-and-replace.md)).

Every command, including ones with no key, is in the command palette,
and you can give any command keys of your own in `config.toml` (see
[Settings](settings.md#key-bindings)). Two things are mouse-only, and
both have a keyboard route:

- The file tree's right-click menu. Its five commands (New File, New
  Folder, Rename, Move to Trash, Reveal) are in the palette and act on
  the selected row; Rename is also `F2` and Move to Trash `Delete`.
- Compare Folders has no default key. Run it from the palette, or give
  it one (`"diff.compareFolders"`).

## Keys your system may take first

A screen reader and the desktop see keys before Nimbus does. When a
Nimbus shortcut does nothing, something else probably took it.

- **macOS, VoiceOver**: VoiceOver commands use `Control+Option` (the VO
  keys). Nimbus uses `Cmd+Option`, not `Control+Option`, so the two do
  not overlap by default. If you set VoiceOver's modifier to Caps Lock,
  nothing changes for Nimbus.
- **Windows, NVDA and Narrator**: their commands start with their own
  modifier key, `Insert` or Caps Lock, which Nimbus does not use.
- **Linux desktops**: many desktops bind `Ctrl+Alt+Up`, `Ctrl+Alt+Down`,
  `Ctrl+Alt+Left` and `Ctrl+Alt+Right` to switching workspaces, and
  some bind `Alt+Up` and `Alt+Down` too, or `Alt+F7` and `Alt+F8` to
  moving and resizing windows. In Nimbus those are Add Cursor Above and
  Below (`Ctrl+Alt+Up`, `Ctrl+Alt+Down`), Move Line Up and Down
  (`Alt+Up`, `Alt+Down`) and Focus Left and Right Group
  (`Ctrl+Alt+Left`, `Ctrl+Alt+Right`). Either change the desktop's
  shortcut (in GNOME, Settings, then Keyboard, then View and Customize
  Shortcuts; in KDE Plasma, System Settings, then Shortcuts), or give
  the Nimbus command other keys:

  ```toml
  [keys]
  "edit.moveLineUp" = ["Alt+Shift+Up"]
  "edit.moveLineDown" = ["Alt+Shift+Down"]
  "selection.addCursorAbove" = ["Ctrl+Shift+Alt+Up"]
  "selection.addCursorBelow" = ["Ctrl+Shift+Alt+Down"]
  ```

  Orca's own commands use the Orca key (`Insert`, or Caps Lock in the
  laptop layout), which Nimbus does not use.

## Reporting a problem

VoiceOver is checked by hand before each release. NVDA, Narrator and
Orca get the same description and are covered by automated tests, but
nobody listens to them before a release, so a problem with one of them
may go unnoticed until someone reports it. If a screen reader reads
something wrong, skips something, or loses its place, please open an
issue on the project's GitHub page and include:

- your system and its version, the screen reader and its version, and
  the Nimbus version (`nimbus --version`);
- what you did, key by key, what you heard, and what you expected to
  hear;
- if you can, a log from starting Nimbus with `RUST_LOG=nimbus=debug`
  (see [Troubleshooting](troubleshooting.md)).
