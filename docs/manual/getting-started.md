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
nimbus                       # an empty, untitled tab
nimbus notes.txt             # one file
nimbus src/main.rs README.md # several files, one tab each
nimbus .                     # a folder: the sidebar shows it, Go to File searches it
nimbus ~/project a.rs b.rs   # a folder plus some files in tabs
```

- A **file that does not exist yet** opens as an empty tab; saving creates
  it at that path.
- A **folder** becomes the *project folder*: the sidebar's file tree
  shows it, and Go to File and Find in Files search it. Without a folder,
  the project folder is the folder of the first file you named, or the
  folder the terminal was in. The project folder is fixed for the
  window's life.
- If a file cannot be opened (no permission, a device rather than a
  file), Nimbus says why in the terminal and in a dialog, and does not
  start.

Inside the editor you can open more files with Open File… (`Cmd+O` /
`Ctrl+O`, a system file dialog), with Go to File… (`Cmd+P` / `Ctrl+P`,
see [Navigating](navigation.md)), or by clicking a file in the sidebar.
New File (`Cmd+N` or `Cmd+T` / `Ctrl+N` or `Ctrl+T`) opens an untitled
tab; saving it asks for a name.

On Windows, "Open with Nimbus" on a file opens that file. On macOS,
opening files by dropping them on the app or with Finder's "Open With" is
not supported yet: use the terminal, Open File…, or the sidebar.

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
