# Nimbus user manual

Nimbus is a fast, lightweight text editor with its own GPU text renderer.
It is *modeless*: you type and the text changes, like most editors, with
no separate command mode. It opens a multi-gigabyte log in a fraction of a
second, edits with several cursors, highlights fourteen languages, and
can compare and merge files.

This manual is for people who use Nimbus. It describes version 0.6.0. If
you want to work on Nimbus itself, start with `DESIGN.md` at the root of
the repository instead.

## Chapters

1. [Getting started](getting-started.md): installing, opening files and
   folders, sessions, the first few minutes.
2. [The window](window.md): tabs, the sidebar and its file operations,
   the status bar and message line, following a growing file, menus, and
   side-by-side groups.
3. [Editing](editing.md): cursors and selection, several cursors at once,
   the clipboard, undo, indentation, comments and line operations.
4. [Navigating](navigation.md): the command palette, Go to File, Open
   Recent, Go to Line, Go to Symbol, brackets and folding.
5. [Find and replace](find-and-replace.md): in the current file and
   across the project.
6. [Compare and merge](compare-and-merge.md): side-by-side differences,
   two- and three-way merges, conflict markers.
7. [Large files](large-files.md): what changes for huge files and very
   long lines.
8. [Encodings and line endings](encodings.md): files that are not
   UTF-8, how their encoding is found, converting encodings and line
   endings.
9. [Languages and highlighting](languages.md): the built-in languages,
   how a file's language is chosen, Markdown helpers.
10. [Settings](settings.md): the config folder, every setting in
    `config.toml`, changing key bindings, `state.toml`.
11. [Themes, your own languages and queries](customizing.md): colour
    themes, simple languages of your own, and tree-sitter query files.
12. [Keyboard shortcuts and commands](keybindings.md): every command with
    its id and default keys on each platform.
13. [Command line](command-line.md): every option `nimbus` accepts.
14. [Updates and troubleshooting](troubleshooting.md): checking for and
    installing new versions, files changing on disk, logs and environment variables.
15. [Accessibility](accessibility.md): screen readers, reaching every
    panel from the keyboard, and keys your system may take first.
16. [The git window](git.md): `nimbus --git`, your uncommitted changes,
    the history with its branch graph, any commit's files and
    differences, and staging, discarding and committing.

## A note on keys

Keys are written macOS first, then Windows and Linux:
`Cmd+S` / `Ctrl+S`. Where one key is written alone (`F7`, `Escape`), it
is the same everywhere. On macOS, `Option` is the key the other platforms
call `Alt`. Inside Nimbus's own palette, macOS shortcuts are shown with
symbols (`⇧⌘P`), and the other platforms spell them out (`Ctrl+Shift+P`).
