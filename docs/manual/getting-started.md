# Getting started

## Installing

Packages for macOS, Linux and Windows are attached to each
[release on GitHub](https://github.com/SklorGalore/nimbus/releases).
None is signed, so macOS and Windows warn the first time you open
Nimbus; the steps below get past the warning once.

| System | What to download | Notes |
|---|---|---|
| macOS 11 or later | the `.dmg` (or the `.zip`) | Drag `Nimbus.app` to Applications. The first time, right-click it and choose Open, or run `xattr -d com.apple.quarantine /Applications/Nimbus.app`. With Homebrew, the release's `nimbus.rb` cask installs it: `brew install --cask --no-quarantine ./nimbus.rb`. |
| Windows | `nimbus-VERSION-windows-x86_64-setup.exe` | SmartScreen says "Windows protected your PC": click "More info", then "Run anyway". The installer adds a Start Menu entry and, if you tick it, "Open with Nimbus" to the right-click menu of files and folder backgrounds. It does not put `nimbus.exe` on your `PATH`. The `.zip` holds the bare `nimbus.exe` if you prefer no installer. |
| Debian, Ubuntu | `nimbus_VERSION_amd64.deb` | `sudo apt install ./nimbus_VERSION_amd64.deb` puts `nimbus` in `/usr/bin` and adds a menu entry. |
| Other Linux | the `.AppImage`, or the tarball | Make the AppImage executable and run it (it needs FUSE). The tarball is the bare binary: put `nimbus` on your `PATH`, and copy `nimbus.desktop` to `~/.local/share/applications` and `nimbus.png` to `~/.local/share/icons` for a menu entry. |

The project's [README](../../README.md) explains how to check a download
against the release's `SHA256SUMS` and its signature, and how to build
Nimbus from source.

### Using Nimbus from a terminal

Everything in this manual that starts with `nimbus` is typed in a
terminal. On Linux with the `.deb` the command is already there. On macOS
the program inside the app is
`/Applications/Nimbus.app/Contents/MacOS/nimbus`; to type just `nimbus`,
link it into a folder on your `PATH`, for example:

```sh
ln -s /Applications/Nimbus.app/Contents/MacOS/nimbus /usr/local/bin/nimbus
```

On Windows, run `nimbus.exe` from the folder it was installed to, or add
that folder to your `PATH`.

## Opening files and folders

```sh
nimbus                       # the last project you worked in, as you left it
nimbus ~/project             # a folder: its session, or a fresh start there
nimbus notes.txt             # just this file
nimbus src/main.rs README.md # several files, one tab each
nimbus ~/project a.rs b.rs   # a folder for the sidebar, plus these files
```

- A **folder** becomes the *project folder*: the sidebar's file tree
  shows it, and Go to File and Find in Files search it. When you name
  files and no folder, the project folder is the folder of the first
  file.
- A **file that does not exist yet** opens as an empty tab; saving
  creates it at that path.
- If a file cannot be opened (no permission, a device rather than a
  file), Nimbus says why in the terminal and in a dialog, and does not
  start.

Inside the editor you can open more files with Open File… (`Cmd+O` /
`Ctrl+O`, a system file dialog), Go to File… (`Cmd+P` / `Ctrl+P`), Open
Recent… (`Cmd+Shift+O` / `Ctrl+Shift+O`), or by clicking a file in the
sidebar (see [Navigating](navigation.md)). New File (`Cmd+N` or `Cmd+T`
/ `Ctrl+N` or `Ctrl+T`) opens an untitled tab; saving it asks for a
name.

On Windows, "Open with Nimbus" on a file opens that file. On macOS,
opening files by dropping them on the app or with Finder's "Open With"
is not supported yet: use the terminal, Open File…, Open Recent or the
sidebar. Opening the app itself (from the Dock or Launchpad) reopens
your last project.

## Sessions: picking up where you left off

Nimbus remembers each project folder as you left it: its *session*.
When you open the folder again, the session comes back:

- the files that were open, in the same order, with the same tab in
  front;
- each file's cursor and scroll position;
- the folders that were open in the file tree, and whether the sidebar
  showed Files or Search.

Which session a start uses:

| You type | What opens |
|---|---|
| `nimbus FOLDER` | That folder's session, or the folder with one empty untitled tab if it has none yet. |
| `nimbus` | The most recent session. The first time, the folder the terminal is in. |
| `nimbus FILE…` (with or without a folder) | Just those files. Sessions are left alone, so a quick edit, or git asking for a commit message, does not disturb your project. |
| `nimbus --no-session` (optionally with a folder) | One empty untitled tab, in that folder or the one the terminal is in. No session is restored, and this run's is not saved. |

Notes:

- Nimbus saves the session as you open and close tabs, and when it
  quits. Sessions for the last 20 project folders are kept.
- **Untitled tabs are not saved**, and neither is unsaved text: Nimbus
  has no autosave, and quitting still asks about unsaved changes.
- Only the front tab is read before the window appears; the other tabs
  show their names at once and their text loads a moment later, or as
  soon as you click one.
- A file that has disappeared since is left out, with a note in the
  message line. If a file changed, its cursor lands as near as it can to
  where it was.
- Tabs come back in one group: side-by-side groups and folds are not
  part of the session.
- **Reopen Last Session** (palette, and the File menu) switches the
  window to the most recent session of a *different* project folder:
  the current one is saved first, its tabs close (tabs with unsaved
  changes stay open, so nothing is lost), and the other project's tabs,
  tree and sidebar mode come back.

The window's size and position, and whether it was maximized, are
remembered too. The position is used again only when the same display
is still connected; otherwise your system places the window.

Sessions, recent files and the window's position are kept in
`state.toml` in the config folder (see [Settings](settings.md#statetoml)).

## Your first few minutes

- **Find any command**: press `Cmd+Shift+P` / `Ctrl+Shift+P` for the
  command palette and type part of a command's name. Every command in
  Nimbus is there, with its shortcut beside it, so this is the one key to
  remember.
- **Save**: `Cmd+S` / `Ctrl+S`. Saves are *atomic*: Nimbus writes the new
  text to a temporary file beside yours and then swaps it in, so a crash
  or power cut leaves either the old file or the new one, never half of
  each. The window title shows the file name, with a `●` while there are
  unsaved changes.
- **Close**: `Cmd+W` / `Ctrl+W` closes a tab, asking first if it has
  unsaved changes (Save, Don't Save, Cancel). Closing the last tab quits.
  `Cmd+Q` / `Ctrl+Q` quits, asking about each unsaved tab.
- **Settings**: run "Open Config File" from the palette. See
  [Settings](settings.md).

Nimbus draws only when something changes, so an idle window uses no CPU.
The cursor stops blinking after 10 seconds without input for the same
reason.
