# Changelog

One section per release, newest first. The release workflow puts the
tagged version's section into the GitHub release notes, so write the
section before tagging.

## Unreleased

### Added

- A user manual in `docs/manual/`, with a keyboard shortcut and command
  reference, every setting, and the command-line options.
- Compare and merge: Compare with Saved, Compare with File… and Compare
  with Tab…, and `--diff A B`, side by side with changed words marked
  (F7 / Shift+F7 step through changes; Toggle Unified Compare). Two-way
  Merge Files…, three-way Merge with Base… and `--merge BASE LOCAL
  REMOTE -o RESULT` for git's mergetool, with Take Left, Take Right and
  Take Both; F8 / Shift+F8 step through conflicts. Resolve Conflicts
  turns a file with git conflict markers into a merge, and Merge with
  Disk merges unsaved edits with a file changed on disk.
- Find in Files (`Primary+Shift+F`) in the sidebar's new Search panel:
  case, regex and whole-word toggles, include and exclude globs,
  results streaming in, unsaved tabs searched as shown, F4 / Shift+F4 to
  step through results. Replace in Files (`Primary+Shift+H`) with a
  preview and a checkbox per match and file, and Undo Replace in Files.
- Everyday editing: Toggle Line Comment (`Primary+/`) and Toggle Block
  Comment; Duplicate, Move Line Up and Down, Delete Line, Join Lines,
  Insert Line Above and Below; Select Line (`Primary+L`); Expand and
  Shrink Selection; Go to and Select to Matching Bracket; case changes,
  Sort Lines, Remove Duplicate Lines, Trim Trailing Whitespace.
- Auto-pairs for brackets and quotes (on for code, off for Markdown and
  plain text), and the `[editor]` settings `auto_pairs`,
  `trim_trailing_whitespace` and `final_newline`, per language too.
- Indentation detected from each file, shown in the status bar
  ("Spaces: 4"), with `[indent]` settings per language; Enter after an
  opening bracket indents, a closing bracket typed first on a line
  dedents.
- Two-key shortcuts in `[keys]`, such as `"Ctrl+K Ctrl+C"`.
- Split Right (`Primary+\`): up to three editor groups side by side, the
  same file editable in two of them; Focus Left/Right Group, Move Tab to
  Next Group, Close Group.
- Folding: markers in the gutter and a `… N lines` chip; Fold, Unfold,
  Toggle Fold, Fold All, Unfold All and Fold Level 1–3. Regions come from
  the language's grammar, else brackets or indentation.
- Languages: C, Go, HTML, CSS and TSX (fourteen in all); Markdown inline
  highlighting and fenced code blocks coloured in their own language;
  `<script>` and `<style>` in HTML. Go to Symbol (`Primary+Shift+R`).
  Expand Selection, bracket matching and folding use the syntax tree.
  User query files can also be `NAME.injections.scm`, `NAME.tags.scm`
  and `NAME.folds.scm`.
- Markdown editing: Enter continues lists and quotes, Tab and Shift+Tab
  indent list items, and Markdown files open with soft wrap on (`wrap`
  in `[editor.LANGUAGE]`).
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

### Changed

- Go to Line is `Primary+G` on every platform (it was `Primary+L` on
  macOS, which is now Select Line).
- A new file, or one whose indentation cannot be detected, indents with
  four spaces rather than a tab; `[indent]` can change that.

## 0.2.1

- `--version` prints the app's version, not the core crate's.

## 0.2.0

- Milestone 8, polish: several cursors, font fallback, bold and italic,
  the redrawn icon and themes, and the owner's on-screen checks.

## 0.1.0

- First tagged release, made to confirm the release workflow while
  milestone 8 was in progress.
