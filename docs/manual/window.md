# The window

From top to bottom, a Nimbus window has a **tab strip**, an optional
**find bar**, the **text**, and a **status bar** along the bottom. The
**sidebar** sits on the left. You can also put up to three **groups** of
tabs side by side.

## Tabs

Every open file is a tab. The strip at the top shows each tab's file
name, a dot for unsaved changes, and a note when the file changed on disk
behind your back: `name (changed on disk)` or `name (deleted)` (see
[Updates and troubleshooting](troubleshooting.md#when-a-file-changes-on-disk)).
Each tab keeps its own cursor, scroll position and undo history.

| To | macOS | Windows and Linux |
|---|---|---|
| Switch to a tab | click it | click it |
| Next or previous tab | `Ctrl+Tab`, `Ctrl+Shift+Tab` | `Ctrl+Tab`, `Ctrl+Shift+Tab` |
| Go to tab 1 to 8 | `Cmd+1` … `Cmd+8` | `Ctrl+1` … `Ctrl+8` |
| Go to the last tab | `Cmd+9` | `Ctrl+9` |
| New untitled tab | `Cmd+N` or `Cmd+T` | `Ctrl+N` or `Ctrl+T` |
| Close the tab | `Cmd+W` | `Ctrl+W` |

Opening a file that is already open in this group switches to its tab
rather than opening it twice; if it is open only in another group, you
get a second tab on the same text (see [Groups](#groups-tabs-side-by-side)).
When the only tab is an untouched untitled one, the
first file you open replaces it. Closing the last tab quits Nimbus.

## The sidebar

The sidebar (`Cmd+B` / `Ctrl+B` to show or hide it) has two tabs in its
header, **Files** and **Search**, followed by the project folder's name.
Search is the Find in Files panel, described in
[Find and replace](find-and-replace.md#find-in-files). Files is the
project tree:

- Folders come first, then files, both sorted without regard to case.
  A folder's contents are read the first time you open it, so even a
  huge repository costs nothing until you look inside.
- Files your `.gitignore` excludes are hidden, and so is the `.git`
  folder. Dotfiles are shown. "Show/Hide Ignored Files in Sidebar" in the
  palette shows the ignored files too (Find in Files follows the same
  setting).
- Click a folder to open or close it, click a file to open it. The
  active tab's file is highlighted, and the tree opens folders down to it
  when you switch tabs.
- The tree follows changes on disk: create, delete or rename files from
  a terminal and it updates within about a second. Only folders you have
  opened are watched.
- Drag the sidebar's right edge to resize it.

To use the tree from the keyboard, press `Cmd+Shift+E` / `Ctrl+Shift+E`
(Focus Sidebar). Then Up and Down move, Page Up and Page Down move a
screen, Home and End go to the first and last row, Right opens a folder
(and steps into it when it is open), Left closes a folder (or goes to its
parent), and Enter opens a file or toggles a folder. Escape gives the
keyboard back to the text. Typing does nothing while the tree has focus.

Whether the sidebar is shown, and how wide it is, are remembered between
runs in `state.toml` (see [Settings](settings.md#statetoml)). The
sidebar cannot create, rename or delete files yet.

## The status bar and message line

The right side of the status bar describes the active tab. From left to
right, each item appears only when it applies:

| Item | Meaning |
|---|---|
| `3 cursors` | More than one cursor is active. |
| `12 selected` | Characters selected, across all cursors (a size such as `80 MB` for very large selections). |
| `Ln 120, Col 8` | The cursor's line and column. In a huge file, `Ln ~` means that line has not been counted yet. |
| `Spaces: 4` or `Tabs` | How Tab and Enter indent in this file. Click it to change it (see [Editing](editing.md#indentation)). |
| `LF` or `CRLF` | The file's line endings. Nimbus keeps whatever the file uses. |
| `UTF-8`, `UTF-8 BOM`, `Not UTF-8` | The encoding. `Not UTF-8` means the file has bytes that are not valid UTF-8; they show as `�` and are written back unchanged unless you edit them. |
| `Rust`, `Markdown`, … | The language used for highlighting. |
| `Counting lines 42%` | A huge file's lines are still being counted in the background. |

In a compare or merge view the status bar also says where you are, such
as `change 3 of 12` or how many conflicts are left.

The left side is the **message line**: short notes such as "Saved",
"Replaced 3 matches" or "Soft wrap on", and warnings such as a mistake in
your config file. A message disappears after four seconds. Errors are
shown in the theme's error colour. When you press the first half of a
two-key shortcut such as `Cmd+K Cmd+0`, the status bar shows the first
key while it waits (two seconds) for the second.

## Menus

On macOS and Windows, Nimbus has a menu bar: File, Edit, Selection, View
and Help (and the Nimbus application menu on macOS). The menus run the
same commands as the keys and the palette, and show your current
shortcuts, including ones you changed in the config. Linux has no menu
bar; the command palette lists every command instead.

## Groups: tabs side by side

Split Right (`Cmd+\` / `Ctrl+\`) opens a new **group** to the right of
the current one, showing the same file. You can have up to three groups
side by side. Each group has its own tab strip, scrollbar and find bar.

- **The same file in two groups** is one document shown twice. Typing in
  one shows in the other at once, while each keeps its own cursors,
  selection, scroll and folds. Undo in either group undoes the
  document's last edit. Saving saves it once, and the unsaved-changes
  dot applies to both. Closing one of the two tabs never asks to save,
  since the other still shows the text.
- **Focus**: click in a group, or use Focus Left Group and Focus Right
  Group (`Cmd+Option+Left` and `Cmd+Option+Right` /
  `Ctrl+Alt+Left` and `Ctrl+Alt+Right`). Files opened from the sidebar,
  Go to File or a dialog open in the focused group. Only the focused
  group's cursor blinks.
- **Move Tab to Next Group** (palette) moves the current tab one group
  to the right, making a new group if there is room, or wrapping round to
  the first.
- **Close Group** (palette) closes every tab in the focused group,
  asking about unsaved changes as Close Tab does. A group also closes
  when its last tab does.
- **Resize** groups by dragging the divider between them. The scroll
  wheel scrolls whichever group is under the pointer.

Compare and merge tabs work in a group like any other tab.

On some Linux desktops `Ctrl+Alt+Left` and `Ctrl+Alt+Right` switch
workspaces before Nimbus sees them; use the palette, or change the keys
(see [Settings](settings.md#key-bindings)).
