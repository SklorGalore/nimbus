# Nimbus

A fast, lightweight, modeless text editor, with its own GPU text renderer.
It opens a 4 GB log in a fraction of a second, edits with several cursors,
and highlights nine languages. See `DESIGN.md` for how it works and
`docs/customizing.md` for settings, themes and languages.

## Getting a build

Builds for macOS, Linux and Windows are attached to each release on
GitHub.

- **macOS**: unzip and move `Nimbus.app` to Applications. It is not
  signed, so the first time, open it with right-click > Open, or run
  `xattr -d com.apple.quarantine /Applications/Nimbus.app`.
- **Linux**: unpack the tarball and put `nimbus` on your `PATH`; copy
  `nimbus.desktop` to `~/.local/share/applications` and `nimbus.png` to
  `~/.local/share/icons` for a menu entry.
- **Windows**: unzip and run `nimbus.exe`.

## Building from source

```sh
cargo build --release
./target/release/nimbus FILE_OR_FOLDER
```

Linux needs `libwayland-dev` and `libxkbcommon-dev`.

## Making a release

Set the version in `Cargo.toml` (`[workspace.package]`), commit, then push
a tag with that version, such as `v0.8.0`. The release workflow builds every
platform and makes a draft release with the packages attached; publish it
on GitHub when it looks right. The app icon is drawn by
`scripts/make-icons.py`.
