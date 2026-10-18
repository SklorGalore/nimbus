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

## Order, M22–M25 (after 1.0)

Drafted on 2026-10-18, after 1.0, from what M21 and the earlier plans
deferred and the unscheduled list below; approved the same day in this
order. A fourth draft, Writing prose (spell checking, a word count,
preview images, HTML export), was dropped whole (`DESIGN.md` §9).

| # | Plan | One line | Why here |
|---|---|---|---|
| 22 | [Screen readers](m22-screen-readers.md) | An AccessKit tree for the editor and chrome, built only when a screen reader asks | The owner named it the first milestone after 1.0; `accesskit_winit` 0.34 works on winit 0.30, so it need not wait |
| 23 | [Library upgrades](m23-library-upgrades.md) | Ropey 2, winit 0.31, current wgpu; the emoji picker; compatibility mode without a GPU | Waits on stable releases (compatibility mode does not); goes whenever they arrive, ahead of whatever is next |
| 24 | [International text](m24-international-text.md) | Fallback fonts per script and language, variable fonts, tested East Asian encodings behind their feature, editable mapped files in single-byte encodings | What M16 and M17 deferred; the riskiest piece (the piece table) last |
| 25 | [Command line](m25-command-line.md) | Install the `nimbus` command from the palette, open files in the running window, `--wait` | Added at the owner's request on 2026-10-18; groups the palette action with the two command-line items that were unscheduled |

## Order, M26–M29 (the git window)

Drafted on 2026-10-18 at the owner's request, after a feasibility study
and the owner's choice of *window mode* (`nimbus --git`) over git inside
the editor or a separate app; approved the same day in this order,
after M25, with five answers (`DESIGN.md` §9).

| # | Plan | One line | Why here |
|---|---|---|---|
| 26 | [Git window: viewing](m26-git-viewer.md) | `--git`, the window layout, changes, history with a graph, commit diffs; read-only | Proves the idea and the git layer before anything writes to a repository |
| 27 | [Git window: staging and committing](m27-git-staging.md) | Stage files, hunks and lines; discard with undo; commit and amend | The daily loop; needs M26's diffs |
| 28 | [Git window: branches and remotes](m28-git-branches-remotes.md) | Branches, tags, stashes; fetch, pull, push; askpass | Needs M26's streaming calls; can swap with M27 |
| 29 | [Git window: history tools and merging](m29-git-history-merging.md) | Blame, file history, search; merge, rebase, cherry-pick, revert with the merge view | Last: the conflict flow sits on everything above |

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
| Ropey 2.0 | DESIGN.md §4.1 | M21, then M23 (still beta at 1.0) |
| winit 0.31, macOS emoji picker | M21 | M23 |
| Running without a GPU (compatibility mode) | 1.0 exits when no GPU opens | M23 |
| Opening files in the running window, `--wait` | Unscheduled (owner, 2026-10-18) | M25 |
| Finder "Open With" | M21 | 1.0.1, with an `objc2` open-files listener (owner, 2026-10-18) |
| Screen-reader support (AccessKit) | M21 | M22 |
| Hot keys the desktop grabs on Linux (a manual note) | Unscheduled | M22 |
| Spell checking, images in the Markdown preview, export to HTML | M14, M20 | Declined (owner, 2026-10-18); below |
| Font fallback per script, variable fonts | M16 | M24 |
| East Asian encodings, transcoding mapped files | M17 | M24 (encodings tested but still behind `east-asian`; mapped files: single-byte encodings only) |

## Unscheduled

Small or speculative items, kept so they are not forgotten. Each could
join a milestone above as a stretch task or make a short milestone of
its own later.

- **Interactive rebase in the git window** (reorder, squash, reword,
  drop): left out of M29 by the owner on 2026-10-18 for a later plan of
  its own once the git window has been used.

Items scheduled on 2026-10-18 moved to the M16–M21 and M22–M24 tables above; what is
left is small or still speculative.

- **Compact typing in the undo history** (M21 soak): each typed
  character is its own `Edit` inside an undo group, about 100 bytes, so
  100,000 keystrokes keep about 10 MB. Merging a contiguous typing run
  into one edit would cut that to the text itself; `undo_oracle.rs`
  covers coalescing.
- **Autosave of unsaved work** for crash recovery: the owner decided
  against it when approving M12 (it would write the user's text
  somewhere they did not choose); it stays here in case that changes.
- **Subpixel (LCD) antialiasing** (M16 deferred): macOS no longer
  draws it and a composited surface cannot use it; kept here in case a
  Windows or Linux user asks.
- **Spell checking, a word count, images in the Markdown preview,
  exporting it to HTML or PDF**: drafted as M24 (Writing prose) and
  dropped by the owner on 2026-10-18 as not worth it.
- **Vertical text, editable mapped files in multi-byte encodings** (M24
  deferred).

## Still non-goals

Unchanged from DESIGN.md §2, and no draft reaches for them: Vim
keybindings or any modal layer; plugins or a scripting language; LSP,
debugging, terminals and git in the *editor* beyond M9's `--diff` and
`--merge` and M18's Compare with HEAD (M14's per-file symbols come from
the syntax tree, not a server; M6's git status colours stay out). A git
client is no longer a non-goal: on 2026-10-18 the owner chose a separate
git window, M26–M29; collaborative editing; rich
text and proportional fonts in the editor area; bidirectional text;
multiple windows (M13's groups are columns in one window); a native menu
bar on Linux. If one of these is reconsidered, it is a DESIGN.md decision
first and a milestone second.
