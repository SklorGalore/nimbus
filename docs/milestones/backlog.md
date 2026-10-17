# Backlog and the draft milestones after M9

Written on 2026-10-17, after M8 landed and M9 was planned. Every plan
from M1 to M9 deferred something; this file collects those items in one
place, says which milestone picks each one up, and lists what stays
unscheduled. The owner approved M10–M15, their order and the answers to
their open questions on 2026-10-17; the answers are in `DESIGN.md` §9 and
its decision log (§10), and each plan carries them in place of the items
it had marked **Proposed**.

## Order

| # | Draft | One line | Why here |
|---|---|---|---|
| 10 | [Find in files](m10-find-in-files.md) | Search and replace across the project, streaming, bounded memory | The largest gap for daily work on a codebase; reuses M3's search and M6's walker. **Next** |
| 11 | [Everyday editing](m11-everyday-editing.md) | Comment toggling, line operations, expand selection, auto-pairs, per-language indentation, chords | Many small wins; all built on M8's one-pass multi-cursor edits |
| 12 | [Workspace](m12-workspace.md) | Session restore, recent files, tree file operations, follow a growing log | Makes Nimbus the editor you leave open; the log tail finishes M7's story |
| 13 | [Split views and folding](m13-splits-and-folding.md) | A second editor column; fold regions at any file size | Needs the document-and-pane refactor; worth doing once the feature set above is stable |
| 14 | [Languages, second pass](m14-languages.md) | Injections (Markdown inline, fenced code), symbols, structural selection, syntax folds, more grammars | Builds on M13's fold model and M11's selection stack |
| 15 | [Distribution](m15-distribution.md) | Signing, notarisation, installers, update check, Linux QA | Owner decisions on accounts and cost; last so it packages a finished editor |

The owner chose this order over the alternative of M11 before M10. M13
stays before M14 (the fold model comes first, the fold queries second),
and M15 is last.

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
| Long-line splitting for the diff (minified files) | M9 | Unscheduled, below |

## Unscheduled

Small or speculative items, kept so they are not forgotten. Each could
join a milestone above as a stretch task or make a short milestone of
its own later.

- **Colour emoji**: needs an RGBA atlas page beside the `R8Unorm` one
  (M8 known limitation). A rendering milestone item, with ligatures and
  the linear-blending experiment from DESIGN.md §4.3; none of the three
  affects correctness.
- **Ligatures** as a setting (M1 noted the one-glyph-per-cell grid
  assumption; ligatures break it, so this is layout work, not a toggle).
- **Scrollbar fade** when idle (needs a timed redraw; rendering is on
  demand and should stay so, but one `WaitUntil` after the last scroll
  is cheap).
- **Ropey 2.0** when it is stable: byte indexing natively, which removes
  the char-to-byte conversion layer in `rope.rs` (DESIGN.md §4.1).
- **Diff options and folder compare** (M9 deferred): ignore whitespace
  and case are flags on the interner; folder compare is M10's walker
  feeding M9's view.
- **Long-line splitting for the diff** (M9 deferred, the plan's layer 4):
  cut a line over a few kilobytes at content-chosen points (a rolling
  hash, so the cuts survive insertions) so minified JSON and similar
  one-line files get real diffs; needs the compare view to show a split
  line's pieces as rows of their own.
- **Autosave of unsaved work** for crash recovery: the owner decided
  against it when approving M12 (it would write the user's text
  somewhere they did not choose); it stays here in case that changes.
- **Drag and drop in the tree**, multiple roots, named workspaces (M12
  deferred).
- **Sticky scroll and minimap** (M13 deferred). The minimap in
  particular is at odds with the on-demand, per-visible-line rendering
  rules; it would need its own budget.
- **Markdown preview** (M14 deferred): rendering prose with proportional
  fonts in the editor area is a non-goal (§2); a preview would be UI
  chrome, which §2 allows later.
- **Proportional fonts in UI chrome** (§2, "may use them later").
- **Hot keys the desktop grabs on Linux** (`Alt+Up` and friends):
  everything is rebindable; a note in `customizing.md` is the minimum.

## Still non-goals

Unchanged from DESIGN.md §2, and no draft reaches for them: Vim
keybindings or any modal layer; plugins or a scripting language; LSP,
debugging, terminals and git integration beyond M9's `--diff` and
`--merge` (M14's per-file symbols come from the syntax tree, not a
server; M6's git status colours stay out); collaborative editing; rich
text and proportional fonts in the editor area; bidirectional text;
multiple windows (M13's groups are columns in one window); a native menu
bar on Linux. If one of these is reconsidered, it is a DESIGN.md decision
first and a milestone second.
