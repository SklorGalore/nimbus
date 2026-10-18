# Backlog and the milestones after M9

Written on 2026-10-17, after M8 landed and M9 was planned; extended on
2026-10-18 with M16–M21. Every plan from M1 on deferred something; this
file collects those items in one place, says which milestone picks each
one up, and lists what stays unscheduled. The owner approved M10–M15,
their order and the answers to their open questions on 2026-10-17, and
M16–M21 the same way on 2026-10-18; the answers are in `DESIGN.md` §9
and its decision log (§10).

## Order

| # | Draft | One line | Why here |
|---|---|---|---|
| 10 | [Find in files](m10-find-in-files.md) | Search and replace across the project, streaming, bounded memory | The largest gap for daily work on a codebase; reuses M3's search and M6's walker. **Done** |
| 11 | [Everyday editing](m11-everyday-editing.md) | Comment toggling, line operations, expand selection, auto-pairs, per-language indentation, chords | Many small wins; all built on M8's one-pass multi-cursor edits |
| 12 | [Workspace](m12-workspace.md) | Session restore, recent files, tree file operations, follow a growing log | Makes Nimbus the editor you leave open; the log tail finishes M7's story |
| 13 | [Split views and folding](m13-splits-and-folding.md) | A second editor column; fold regions at any file size | Needs the document-and-pane refactor; worth doing once the feature set above is stable |
| 14 | [Languages, second pass](m14-languages.md) | Injections (Markdown inline, fenced code), symbols, structural selection, syntax folds, more grammars | Builds on M13's fold model and M11's selection stack |
| 15 | [Distribution](m15-distribution.md) | Signing, notarisation, installers, update check, Linux QA | Owner decisions on accounts and cost; last so it packages a finished editor |

The owner chose this order over the alternative of M11 before M10. M13
stays before M14 (the fold model comes first, the fold queries second),
and M15 is last.

## Order, M16–M21

Drafted on 2026-10-18 from what M9–M15 deferred and the unscheduled list
below as it stood then; approved the same day in this order, with the
alternative of encodings first declined.

| # | Plan | One line | Why here |
|---|---|---|---|
| 16 | [Rendering, second pass](m16-rendering-second-pass.md) | Colour emoji, ligatures, zoom, whitespace and indent guides, scrollbar fade, the linear-blending experiment | Every rendering item in one place, all inside `render`, so it can run while M12–M15 settle |
| 17 | [Encodings and line endings](m17-encodings.md) | UTF-16 and Windows-1252 open and save, encoding and line ending in the status bar, conversion | Config and data files are a goal (§1) and non-UTF-8 ones open as `�` today; small and self-contained |
| 18 | [Compare, second pass](m18-compare-second-pass.md) | Diff options, folder compare, long-line splitting, Compare with HEAD | The items M9 deferred, on the interner, M10's walker and the compare view |
| 19 | [Workspace and search, second pass](m19-workspace-and-search-second-pass.md) | Multiple roots, named workspaces, drag and drop, search scope, history, multi-line patterns | What M10 and M12 deferred, once M12 has been used a while |
| 20 | [Reading aids](m20-reading-aids.md) | Sticky scroll, a windowed minimap, a proportional UI font, Markdown preview | What M13 and M14 deferred; needs M13's splits and M14's fold regions |
| 21 | [Foundations and 1.0](m21-foundations-and-1-0.md) | Ropey 2, winit and wgpu upgrades, fuzzing and soak in CI, a crash record, 1.0.0 | Last, so 1.0 packages everything above |

## Where each deferred item went

| Item | Deferred by | Lands in |
|---|---|---|
| Find across files, find in files | M3, M8 | M10 |
| Configurable indentation per language | M3 | M11 |
| Multi-key chord sequences (`Ctrl+K Ctrl+C`) | M4 | M11 |
| Recently opened files | M4 | M12 |
| Markdown inline highlighting | M5, M8 stretch | M14 |
| Language injections (code blocks, strings) | M5 | M14 |
| Code folding | M5 | M13 (indentation, brackets), M14 (syntax) |
| Create, rename, delete from the tree | M6 | M12 |
| Following a growing log (`tail -f`) | M7 | M12 |
| Session restore | M8, DESIGN.md §2 | M12 |
| Split views | M8 (M6 prepared the layout) | M13 |
| Code signing and notarisation, installers | M8 | M15 |
| Auto-update | M8 | M15, as an on-demand check; in-place updates stay deferred |
| Linux run by hand (Wayland, X11) | M8 | M15 |
| Bracket matching misled by strings | M8 known limitation | M14 |
| Diff options (ignore whitespace), folder compare, git revisions | M9 | Unscheduled, below |
| Long-line splitting for the diff (minified files) | M9 | M18 |
| Colour emoji, ligatures, scrollbar fade, linear blending | M8, DESIGN.md §4.3 | M16 |
| Non-UTF-8 files shown as `�` | M7 (SUB substitution) | M17 |
| Searching outside the root, saved searches, history, multi-line patterns | M10 | M19 |
| Drag and drop in the tree, multiple roots, named workspaces | M12 | M19 |
| Sticky scroll, minimap | M13 | M20 |
| Markdown preview | M14 | M20 |
| Proportional fonts in UI chrome | DESIGN.md §2 | M20 |
| Ropey 2.0 | DESIGN.md §4.1 | M21, then after 1.0 (still beta), below |

## Unscheduled

Small or speculative items, kept so they are not forgotten. Each could
join a milestone above as a stretch task or make a short milestone of
its own later.

Items scheduled on 2026-10-18 moved to the M16–M21 table above; what is
left is small or still speculative.

- **Screen-reader support** (AccessKit through winit): the owner chose
  to ship 1.0 without it (2026-10-18). The first milestone after 1.0,
  since a GPU-drawn editor is invisible to VoiceOver, NVDA and Orca
  without it; the adapter can be created lazily so startup is unchanged.
- **Ropey 2 and winit 0.31** (M21): both were still betas when 1.0
  shipped (ropey 2.0.0-beta.1, winit 0.31.0-beta.3), so the owner chose
  to release on ropey 1.6 and winit 0.30. Each moves to a 1.x point
  release once stable, measured as M21's plan describes (one commit
  each, `--smoke`, typing benchmark and binary size before and after).
- **Compact typing in the undo history** (M21 soak): each typed
  character is its own `Edit` inside an undo group, about 100 bytes, so
  100,000 keystrokes keep about 10 MB. Merging a contiguous typing run
  into one edit would cut that to the text itself; `undo_oracle.rs`
  covers coalescing.
- **Finder "Open With"** (M21 deferred): the bundle can declare document
  types, but receiving the files needs the macOS app delegate's
  open-files message, which winit 0.30 does not pass on; new `unsafe`
  code, so it waits for winit 0.31 or a later point release.
- **macOS emoji picker input** (Ctrl+Cmd+Space): winit 0.30 drops text
  inserted without a preceding compose; the fix is in winit, so it
  waits for 0.31 rather than a patched copy of winit.
- **Autosave of unsaved work** for crash recovery: the owner decided
  against it when approving M12 (it would write the user's text
  somewhere they did not choose); it stays here in case that changes.
- **East Asian encodings** (Shift_JIS, GBK, EUC-KR, Big5): behind a cargo
  feature that is off, from M17; about 1 MB of tables.
- **Images in the Markdown preview**, exporting it to HTML (M20
  deferred).
- **Subpixel (LCD) antialiasing** and variable fonts (M16 deferred).
- **Hot keys the desktop grabs on Linux** (`Alt+Up` and friends):
  everything is rebindable; a note in `customizing.md` is the minimum.

## Still non-goals

Unchanged from DESIGN.md §2, and no draft reaches for them: Vim
keybindings or any modal layer; plugins or a scripting language; LSP,
debugging, terminals and git integration beyond M9's `--diff` and
`--merge` and M18's Compare with HEAD, the one exception the owner
chose on 2026-10-18 (M14's per-file symbols come from the syntax tree,
not a server; M6's git status colours stay out); collaborative editing; rich
text and proportional fonts in the editor area; bidirectional text;
multiple windows (M13's groups are columns in one window); a native menu
bar on Linux. If one of these is reconsidered, it is a DESIGN.md decision
first and a milestone second.
