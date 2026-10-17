<img src="assets/icon/nimbus-1024.png" alt="" width="128" align="right">

# Nimbus

A fast, lightweight, modeless text editor, with its own GPU text renderer.
It opens a 4 GB log in a fraction of a second, edits with several cursors,
and highlights fourteen languages. See `DESIGN.md` for how it works and
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

Each release also has a `SHA256SUMS` file. To check a download, put it in
the same folder and run `shasum -a 256 -c SHA256SUMS --ignore-missing`
(macOS) or `sha256sum -c SHA256SUMS --ignore-missing` (Linux); on Windows,
compare the output of `Get-FileHash FILE` with its line.

## Building from source

```sh
cargo build --release
./target/release/nimbus FILE_OR_FOLDER
```

Linux needs `libwayland-dev` and `libxkbcommon-dev`.

## Licence

Nimbus is under the MIT licence (`LICENSE`). The bundled font, Hack Nerd
Font Mono, has its own licence (`crates/render/assets/HackNerdFont-LICENSE.md`);
both come with every release.

## Making a release

Set the version in `Cargo.toml` (`[workspace.package]`), commit, then push
a tag with that version, such as `v0.8.0`. The release workflow builds every
platform and makes a draft release with the packages attached; publish it
on GitHub when it looks right. The app icon is drawn by
`scripts/make-icons.py`.
