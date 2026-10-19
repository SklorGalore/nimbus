# Changelog

One section per release, newest first. The release workflow puts the
tagged version's section into the GitHub release notes, so write the
section before tagging.

## Unreleased

### Added

- The release signing key, `minisign.pub`, is in the repository and
  printed in the README, so `SHA256SUMS.minisig` can be checked.
- `scripts/publish-packages.sh` puts a signed, published release in the
  Homebrew tap and opens its winget pull request, with the hashes taken
  from the signed `SHA256SUMS`.
- The README and manual say how to install with Homebrew
  (`brew install --cask sklorgalore/nimbus/nimbus`) and winget
  (`winget install SklorGalore.Nimbus`).

## 1.1.0

### Added

- `remember_workspace = false` in the config stops Nimbus reopening
  the last session at startup and saving one, while recent files, the
  window position and the zoom are still kept.

## 1.0.1

### Fixed

- macOS: opening a file in Nimbus from Finder (Open With, a
  double-click when Nimbus is the default app, or a drop on the Dock
  icon) opened nothing and showed "Nimbus cannot open files in the …
  format". The file now opens in a tab, and a folder joins the sidebar.
  Nimbus also appears in Finder's Open With menu for text files and
  folders.

## 1.0.0

The first stable release: everything from milestones 9 to 21 since
0.2.1. Compare and merge (M9), find in files, everyday editing,
sessions, split views and folding, fourteen languages, packages for
every platform, rendering and encodings work, workspaces and reading
aids all land here, along with the testing that a 1.0 promise needs.

### Added

- A crash record: if Nimbus ever hits an internal error, it writes one
  line (time, version, where, and the message, with no text from your
  files) to `crash.log` beside `config.toml` before quitting, and the
  next start says so.
- Fuzz testing (`fuzz/`): the buffers, the diff, the merge, search and
  the config and theme parsers against their oracles, an hour per
  target every week in CI.
- `--soak`, with `--bench-typing`: 100,000 keystrokes with undo, find
  and saves on the 100 MB file, failing if memory grows over 20 MB
  (`bench/soak.sh`).

- Reading aids: sticky scroll pins the function, impl, class or Markdown
  section you are in at the top of the text (click a pinned row to jump
  to it; `sticky_scroll`, on by default). Toggle Minimap shows the lines
  around the view at the right edge, with drag to scroll (`minimap`,
  off by default, and `minimap_width`). `ui_font` draws the sidebar,
  tabs, status bar, palette and pickers in a proportional font such as
  Inter. Open Preview (`Primary+Shift+V`) shows a Markdown file as a
  page beside it, following as you type and scroll, with code blocks
  coloured by their languages and links that open in the browser.
- Compare, second pass: Ignore whitespace (at line ends, or all), Ignore
  case and Ignore blank lines, as buttons in the sidebar's Compare tab
  and as palette commands, remembered between runs. Lines over 4 KB,
  such as minified files, are compared in pieces. Compare with HEAD and
  Compare with Revision… read the file from git. Compare Folders… (or
  `nimbus --diff DIR_A DIR_B`) lists the files that differ; Enter opens a
  pair, and F7 past the last change steps to the next file.
- Workspaces of several folders: Add Folder to Workspace… (or drop a
  folder on the window, or `nimbus DIR1 DIR2`); Go to File and Find in
  Files cover every folder. Save Workspace As… and Open Workspace…, with
  `--workspace NAME`; Save Workspace Here writes `.nimbus/workspace.toml`
  so a workspace travels with its repository.
- Drag and drop in the file tree (Option/Alt copies), Undo Move, Search
  in Folder, and files dropped on the window open.
- Find in Files: folders outside the project in the include field
  (`~/notes`), the last 50 patterns and replacements on Up and Down,
  Save Search…, and regex patterns with `\n` that match across lines.
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
- Sessions: `nimbus FOLDER`, or `nimbus` alone for the last folder, reopens
  the files that were open with their cursors and scroll positions, the
  open folders in the tree and the sidebar's mode. `nimbus FILE` and
  `--no-session` leave sessions alone. Only the front tab is read before
  the window appears. Editor groups, their widths and each tab's folds
  come back too.
- Open Recent (`Primary+Shift+O`), recent files first in the file opener,
  and Reopen Closed Tab (`Primary+Shift+T`).
- New File, New Folder, Rename (`F2`; `Enter` on macOS), Move to Trash and
  Reveal in Finder/Explorer from the file tree, its right-click menu on
  macOS and Windows, and the palette. Open tabs follow a rename.
- Follow Tail: a file that grows while open (a log) gets just the new
  lines, without becoming unsaved, and Follow Tail in the status bar keeps
  the view at the end. Works on the 4 GB mapped files too.
- The window comes back at its size and place.
- Colour emoji from the system emoji font, on a second atlas page.
- Ligatures with `ligatures = true` and a font that has them; the new
  `font` setting names an installed family or a font file.
- Zoom In (`Primary+=`), Zoom Out (`Primary+-`) and Reset Zoom
  (`Primary+0`), remembered between runs unless `remember_zoom = false`.
- Visible whitespace (`render_whitespace`, default `selection`) and indent
  guides (`indent_guides`), both toggles in the palette.
- The scrollbar fades out a second after the last scroll.
- Encodings: files in UTF-16 (by their byte order mark) and Windows-1252
  (guessed from the first 64 KB) open, edit and save back in their own
  encoding, with ISO-8859-1, the other Windows and ISO-8859 tables, KOI8
  and Mac Roman by choice. The encoding and line ending show in the status
  bar; click them, or run Reopen with Encoding…, Save with Encoding… and
  Change Line Endings to LF/CRLF (one undo step). `--encoding NAME` and
  the `[encodings]` table in `config.toml` skip the guess. A character the
  encoding cannot hold stops the save and offers UTF-8 instead. Files of
  256 MB or more in a known other encoding open read-only. East Asian
  encodings are in builds with `--features east-asian`.

### Changed

- Go to File lists dotfiles such as `.github/` and `.env.example`; only
  the `.git` folder and ignored files are left out.
- `--no-session` remembers nothing from the run: no recent files or
  window position either.
- Go to Line is `Primary+G` on every platform (it was `Primary+L` on
  macOS, which is now Select Line).
- A new file, or one whose indentation cannot be detected, indents with
  four spaces rather than a tab; `[indent]` can change that.

### Fixed

- Find and Replace All could miss an occurrence in a run of repeated
  text (such as `vvv` in a row of `v`s) at a 1 MB boundary of a large
  file, and a `\b` next to a character split across a search window
  could be judged wrong. Both found by the new fuzz tests.
- A window, GPU or font failure at startup now says why (on the
  terminal, and in a dialog when there is none) and exits with code 1;
  it used to quit silently with 0.
- The config template lists all fourteen built-in languages, and
  `--help` lists `--picker`.
- Find in Files results, and anything else drawn in the sidebar after a
  text field, were no longer clipped to the sidebar: long result lines
  ran on across the editor.

## 0.2.1

- `--version` prints the app's version, not the core crate's.

## 0.2.0

- Milestone 8, polish: several cursors, font fallback, bold and italic,
  the redrawn icon and themes, and the owner's on-screen checks.

## 0.1.0

- First tagged release, made to confirm the release workflow while
  milestone 8 was in progress. It had what milestones 0 to 7 built: a
  window drawn with our own GPU text pipeline (M0, M1); opening,
  editing, scrolling and saving files safely (M2); selection, undo,
  clipboard, find and replace, soft wrap and auto-indent (M3); tabs, the
  file opener, the command palette, `config.toml` and menus (M4); syntax
  highlighting for nine languages and two themes (M5); the file tree and
  reloading files changed on disk (M6); and files of several gigabytes,
  memory-mapped and indexed in the background (M7).
