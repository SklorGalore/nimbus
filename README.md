<img src="assets/icon/nimbus-1024.png" alt="" width="128" align="right">

# Nimbus

A fast, lightweight, modeless text editor, with its own GPU text renderer.
It opens a 4 GB log in a fraction of a second, edits with several cursors,
and highlights nine languages. See `DESIGN.md` for how it works and
`docs/customizing.md` for settings, themes and languages.

## Getting a build

Packages for macOS, Linux and Windows are attached to each
[release](https://github.com/SklorGalore/nimbus/releases). None is
signed (that needs paid developer accounts), so macOS and Windows warn
the first time; the steps below get past the warning once.

- **macOS**: open the `.dmg` and drag `Nimbus.app` to Applications (or
  unzip the `.zip`). The first time, open it with right-click > Open, or
  run `xattr -d com.apple.quarantine /Applications/Nimbus.app`. With
  Homebrew, each release carries a cask file, `nimbus.rb`:
  `brew install --cask --no-quarantine ./nimbus.rb` skips the warning.
- **Windows**: run `nimbus-VERSION-windows-x86_64-setup.exe`. SmartScreen
  says "Windows protected your PC": click "More info", then "Run anyway".
  The installer adds a Start Menu entry and "Open with Nimbus" to the
  right-click menu, and uninstalls from Settings. The `.zip` is the bare
  `nimbus.exe` for no installer. Each release also carries a winget
  manifest (`winget-manifest.zip`) for `winget install --manifest DIR`.
- **Linux**: `sudo apt install ./nimbus_VERSION_amd64.deb` on Debian and
  Ubuntu, or make the `.AppImage` executable and run it (it needs FUSE,
  which most desktops have). The tarball is the bare binary: put `nimbus`
  on your `PATH`, and copy `nimbus.desktop` to
  `~/.local/share/applications` and `nimbus.png` to `~/.local/share/icons`
  for a menu entry.

"Check for Updates" in the command palette asks GitHub whether a newer
release exists and "Open Latest Release Page" opens it; nothing is
downloaded or installed by the editor. Set `check_updates = true` in
`config.toml` to run the check once at each start; it is off by default.

### Checking a download

Each release has a `SHA256SUMS` file. To check that a download is intact,
put both in one folder and run `shasum -a 256 -c SHA256SUMS
--ignore-missing` (macOS) or `sha256sum -c SHA256SUMS --ignore-missing`
(Linux); on Windows, compare the output of `Get-FileHash FILE` with its
line.

A hash proves the file is undamaged; it does not prove who made it. For
that, `SHA256SUMS.minisig` is a [minisign](https://jedisct1.github.io/minisign/)
signature by the release key, whose public half is `minisign.pub` at the
root of this repository (releases before the key existed have no
signature). With minisign installed (`brew install minisign`, `apt install
minisign`, or `scoop install minisign`):

```sh
minisign -V -p minisign.pub -m SHA256SUMS
```

It prints "Signature and comment signature verified" when the file is
the one that was signed.

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

Set the version in `Cargo.toml` (`[workspace.package]`), move the
`CHANGELOG.md` "Unreleased" notes under a heading for that version,
commit, then push a tag with that version, such as `v0.8.0`. The release
workflow (`.github/workflows/release.yml`) builds every platform, makes
a draft release with the packages, the Homebrew cask and winget manifest
and `SHA256SUMS` attached, and signs the checksums; publish it on GitHub
when it looks right. The packaging steps are `scripts/bundle-macos.sh`,
`scripts/dmg-macos.sh`, `scripts/deb-linux.sh`,
`scripts/appimage-linux.sh`, `packaging/windows/nimbus.iss` and
`scripts/manifests.sh`. The app icon is drawn by `scripts/make-icons.py`.

Signing `SHA256SUMS` needs a one-time setup: run `minisign -G -W` (no
password, since the key lives in a secret), commit the public key as
`minisign.pub`, and put the secret key file's contents in the repository's
`MINISIGN_SECRET_KEY` Actions secret. Until then the workflow says the
signing step was skipped and the release has no `.minisig`.

The cask and winget manifest are attached for copying on: the cask goes
in a tap (`Casks/nimbus.rb` in a `homebrew-TAP` repository) and the
manifest in a pull request to `microsoft/winget-pkgs`.
