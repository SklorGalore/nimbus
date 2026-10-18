# M19 — Workspace and search, second pass

**Status: proposed.** Written on 2026-10-18 from what M10 and M12
deferred: drag and drop in the tree, multiple roots, named workspaces;
searching outside the root, saved searches and history, multi-line
patterns. Scheduled after M12 has been used for a while, so the tree
and session code it extends are settled.

## Goal

Let Nimbus hold a project made of several folders, move files around in
it, and search it with the patterns and history a long-running session
needs.

## Done when

- [ ] **Multiple roots**: "Add Folder to Workspace…" adds a second root
      to the sidebar, with its own walker, watcher and `.gitignore`; the
      fuzzy opener and find in files cover every root, prefixing paths
      with the root's name when there is more than one. `nimbus DIR1
      DIR2` opens both.
- [ ] **Named workspaces**: "Save Workspace As…" writes a `NAME.toml`
      under `workspaces/` in the config directory listing roots and the
      session; "Open Workspace…" lists them; `nimbus --workspace NAME`
      opens one. The automatic per-root session from M12 is unchanged
      for folders opened directly.
- [ ] **Drag and drop**: dragging a file or folder in the tree moves it
      (copies with `Alt`), with the same conflict prompt as Rename; open
      tabs follow the moved file; a drop from the desktop onto the
      window opens the file. Undo for a move is "Undo Move" in the
      message line for the session, using the trash crate's path for
      deletes and a reverse rename for moves.
- [ ] **Search scope**: the search panel's include field accepts an
      absolute path or `~/…`, so a search can run outside the roots;
      "Search in Folder" on a tree folder fills it in.
- [ ] **History**: the last 50 patterns and replacements are kept per
      workspace in `state.toml`; `Up` and `Down` in the field walk them;
      "Save Search" pins one with a name, listed at the top of the panel.
- [ ] **Multi-line patterns**: with regex on, a pattern containing `\n`
      matches across lines inside one file, bounded by a 1 MB window per
      match so memory stays per block; the panel marks such hits with
      the span of lines.
- [ ] Budgets: a second root adds no startup cost until expanded; a
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

**A workspace file is small**: roots, the saved-session fields from M12,
and nothing else. It lives in the config directory so Nimbus never
writes into the user's project folders.

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
3. Workspace files: save, open, `--workspace`; `customizing.md`.
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

_To be filled in when the milestone lands._
