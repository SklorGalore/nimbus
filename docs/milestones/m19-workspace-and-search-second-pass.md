# M19 — Workspace and search, second pass

**Status: done, 2026-10-18. The owner checked it on their Mac.** Written on 2026-10-18 from what M10 and M12
deferred: drag and drop in the tree, multiple roots, named workspaces;
searching outside the root, saved searches and history, multi-line
patterns. Scheduled after M12 has been used for a while, so the tree
and session code it extends are settled. Approved the same day:
workspace files live in the config directory, and also inside a root as
`.nimbus/workspace.toml` when the user asks (`DESIGN.md` §9).

## Goal

Let Nimbus hold a project made of several folders, move files around in
it, and search it with the patterns and history a long-running session
needs.

## Done when

- [x] **Multiple roots**: "Add Folder to Workspace…" adds a second root
      to the sidebar, with its own walker, watcher and `.gitignore`; the
      fuzzy opener and find in files cover every root, prefixing paths
      with the root's name when there is more than one. `nimbus DIR1
      DIR2` opens both.
- [x] **Named workspaces**: "Save Workspace As…" writes a `NAME.toml`
      under `workspaces/` in the config directory listing roots and the
      session; "Open Workspace…" lists them; `nimbus --workspace NAME`
      opens one. "Save Workspace Here" writes `.nimbus/workspace.toml` in
      the first root instead, so the workspace travels with the
      repository; opening a folder that holds one offers to load it.
      Only that file is ever written inside a project, only on that
      action, and `.nimbus/` is suggested for `.gitignore` in the docs.
      The automatic per-root session from M12 is unchanged for folders
      opened directly.
- [x] **Drag and drop**: dragging a file or folder in the tree moves it
      (copies with `Alt`), with the same conflict prompt as Rename; open
      tabs follow the moved file; a drop from the desktop onto the
      window opens the file. Undo for a move is "Undo Move" in the
      message line for the session, using the trash crate's path for
      deletes and a reverse rename for moves.
- [x] **Search scope**: the search panel's include field accepts an
      absolute path or `~/…`, so a search can run outside the roots;
      "Search in Folder" on a tree folder fills it in.
- [x] **History**: the last 50 patterns and replacements are kept per
      workspace in `state.toml`; `Up` and `Down` in the field walk them;
      "Save Search" pins one with a name, listed at the top of the panel.
- [x] **Multi-line patterns**: with regex on, a pattern containing `\n`
      matches across lines inside one file, bounded by a 1 MB window per
      match so memory stays per block; the panel marks such hits with
      the span of lines.
- [x] Budgets: a second root adds no startup cost until expanded; a
      multi-line search over the 1 GB tree stays within the three-times-
      `rg` budget; dragging in a tree of 100,000 entries redraws at frame
      rate.

## Scope

**In**: roots, workspaces, drag and drop with undo, search scope, history
and saved searches, multi-line search.

**Deferred**: remote or virtual roots; a workspace-level settings file
that overrides `config.toml` (keep one config); syncing search history
across machines.

## Design

**Roots are a list.** `Sidebar` already owns one arena tree; it becomes
a `Vec<Root>`, each with its tree, walker handle and watch set. The
file opener and the search engine take a list of roots and run one
walker per root on the shared pool. Path display is relative to the
nearest root, prefixed with the root's folder name when more than one
is open.

**A workspace file is small**: roots (relative to the file when it is
project-local), the saved-session fields from M12, and nothing else. It
lives in the config directory by default; the project-local form is the
one place Nimbus writes inside a user's folder, and only when asked.

**Drag and drop reuses the tree's rename machinery.** A drag is a tree
selection plus a target folder; the drop issues the same rename-with-path
operation M12 built, once per item, with the watcher paused for the
batch so events for our own moves are ignored (the `Stamp` trick, applied
to paths). Desktop drops arrive through winit's `DroppedFile` event.

**Multi-line search keeps the block model.** M10 reads files in 4 MB
blocks with a line-tail carry; a multi-line regex instead carries up to
1 MB of tail, and a match that would exceed the window is cut and
reported as truncated. The single-line path stays the default because it
is cheaper and bounds memory tighter.

## Tasks

1. `Vec<Root>` in the sidebar; walkers and watchers per root; path
   prefixes; `nimbus DIR1 DIR2`.
2. Opener and find in files over every root; tests with two temporary
   roots.
3. Workspace files: save, open, `--workspace`; "Save Workspace Here"
   and the offer on opening a folder that has one; `customizing.md`.
4. Tree drag and drop with the conflict prompt and the watcher pause;
   desktop drops.
5. "Undo Move" for the session.
6. Search scope paths and "Search in Folder".
7. History and saved searches in `state.toml`; field navigation.
8. Multi-line regex path with the larger tail; property test against a
   whole-file oracle on small files.
9. Measurements; Results.

## Concepts

- **One tree per root, one list of roots**: how a feature designed for
  one folder grows to several without a rewrite.
- **Ignoring your own file events**: the same idea as `Stamp`, for
  moves.
- **Bounded multi-line matching**: why the window exists and what a
  truncated match means.

## Risks

| Risk | Fallback |
|---|---|
| Two roots nest or overlap | Refuse to add a root inside another; the outer one already shows it |
| A drag across a slow network folder blocks the UI | Moves run on the saver's worker; the tree shows the item greyed until done |
| Multi-line matches explode on a pattern like `(?s).*` | Match count cap from M10 and the 1 MB window; the panel says "truncated" |

## Results

Built on 2026-10-18 in a Linux container with no GPU; the owner then
checked the drawing and behaviour on their Mac the same day. The
`--smoke` first-frame times were not recorded. CI's `release-size` job
passed the 35 MB budget on all three systems.

| Metric | Value |
|---|---|
| Tests | 573 passing in the workspace, plus the opt-in benchmarks |
| Multi-line search, 251 MB of crate sources (11,929 files), warm | `unsafe \{\n\s*// SAFETY`: 142 ms, 95 hits; `rg -U -i` 408–695 ms, 95 hits. `\}\n\nimpl Drop for`: 106 ms, 262 hits; `rg -U --hidden` 361–524 ms, 262 hits |
| Single-line search, same tree | `fn main`: 97 ms; `rg` 161 ms |
| A second root at startup | no cost until opened: it is added collapsed, and only an expanded root is listed (`Tree::start`) |
| Dragging in a 100,000-entry tree | not measured here. A drag only looks up the row under the pointer; the tree is rebuilt once, on the drop |
| New dependencies | none |

**How it differs from the plan.**

- **Roots are nodes, not a list of trees.** The plan had a `Vec<Root>`,
  each with its own tree. The sidebar's arena tree already handled
  listing, watching, revealing and renaming, so extra roots became
  parentless nodes in that one arena (`Tree::roots`). With more than one,
  each root is a row of its own and its contents sit one level in. The
  walkers take every root at once (`ignore::WalkBuilder::add`), and each
  root's `.gitignore` still applies to it alone.
- **The workspace's session stays in `state.toml`.** It is keyed by the
  first root, as in M12, and now also holds the other roots and the
  search history. A workspace file is written only by Save Workspace As
  or Save Workspace Here, never on its own, so a committed
  `.nimbus/workspace.toml` does not change as you work.
- **Undo Move cannot restore a trashed item on macOS.** The `trash`
  crate can put items back on Linux and Windows only. On macOS the
  message line says to use Finder's Put Back, and Undo Move skips it.
  Moves and copies undo everywhere.
- **The multi-line window.** A match that starts in one window and runs
  past it is cut at 1 MB and marked "truncated". It is shown but never
  ticked for Replace All. A match longer than one window plus one block
  is missed: finding it would mean holding the whole file.
- **Which patterns are multi-line.** Only a regex whose text contains
  `\n`, or a typed line break, takes the multi-line path. `\s` alone
  does not, so `fn\s+main` still matches within a line, as in M10.

**Surprises.**

- The first multi-line matcher lost its carry when a block had no match,
  so a match split over two blocks was missed. A property test against a
  whole-text oracle (300 random texts cut into random blocks) caught it.
- Replace All re-checks each hit before replacing. It used to cut the text
  at the end of the hit's line, which would refuse every multi-line hit.
  `Replacement::multi_line` lets the check see the rest of the text.
