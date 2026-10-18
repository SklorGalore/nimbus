# Changelog

One section per release, newest first. The release workflow puts the
tagged version's section into the GitHub release notes, so write the
section before tagging.

## Unreleased

### Added

- Check for Updates: a palette action that asks GitHub for the latest
  release and says whether it is newer than the running version; Open
  Latest Release Page opens it in the browser. The check also runs at
  startup when `check_updates = true` is set in `config.toml`, and never
  otherwise.
- Packages for every platform: a macOS `.dmg` with an Applications
  shortcut and a Homebrew cask; a Windows installer (Start Menu entry and
  "Open with Nimbus") and a winget manifest; a Linux `.deb` and an
  AppImage. All unsigned; the README says how to get past the first-run
  warnings.
- `SHA256SUMS` is signed with minisign when the release has a key, and
  `cargo-deny` checks dependency licences and advisories in CI.
- Sessions: `nimbus FOLDER`, or `nimbus` alone for the last folder, reopens
  the files that were open with their cursors and scroll positions, the
  open folders in the tree and the sidebar's mode. `nimbus FILE` and
  `--no-session` leave sessions alone. Only the front tab is read before
  the window appears.
- Open Recent (`Primary+Shift+O`), recent files first in the file opener,
  and Reopen Closed Tab (`Primary+Shift+T`).
- New File, New Folder, Rename (`F2`; `Enter` on macOS), Move to Trash and
  Reveal in Finder/Explorer from the file tree, its right-click menu on
  macOS and Windows, and the palette. Open tabs follow a rename.
- Follow Tail: a file that grows while open (a log) gets just the new
  lines, without becoming unsaved, and Follow Tail in the status bar keeps
  the view at the end. Works on the 4 GB mapped files too.
- The window comes back at its size and place.

## 0.2.1

- `--version` prints the app's version, not the core crate's.

## 0.2.0

- Milestone 8, polish: several cursors, font fallback, bold and italic,
  the redrawn icon and themes, and the owner's on-screen checks.

## 0.1.0

- First tagged release, made to confirm the release workflow while
  milestone 8 was in progress.
