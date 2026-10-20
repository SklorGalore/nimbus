# M27 — Git window: viewing

**Status: planned (2026-10-18).** The first of four plans (M27–M30) for a
git client built from Nimbus's own parts, written at the owner's request
after a feasibility study
showed that the hard half (diff, merge, text) already exists.
The owner chose *window mode*: the same `nimbus` binary opens a window
laid out for git with `nimbus --git [DIR]`, the way `--diff` and
`--merge` open their views today. This milestone is read-only: it shows
a repository and changes nothing in it. The owner answered the plans'
open questions the same day (`DESIGN.md` §9): the four run after M25 and M26 (security),
and the graph is drawn with smooth curves.

## Goal

`nimbus --git` in a repository opens a git window that shows the
uncommitted changes, the history with a branch graph, and any commit's
files and diffs, fast on very large repositories.

## Done when

- [ ] **Opening**: `nimbus --git` opens the repository holding the
      current folder, `nimbus --git DIR` the one holding `DIR`; "Open Git
      Window" in the editor's palette and View menu opens one for the
      workspace's first root. Outside a repository, or with no `git` on
      `PATH`, the window says so in plain words (the `GitError` messages
      from `app/git.rs`) instead of failing. The title is
      "*repo* — Git — Nimbus".
- [ ] **Layout**: a sidebar on the left (Changes, Branches, Remotes,
      Tags, Stashes), the history in the middle, and the selected
      commit's details on the right: hash, author, dates, full message,
      changed files, and the compare view for the selected file. Panes
      resize by dragging, as the editor's sidebar does.
- [ ] **Changes**: selecting Changes lists staged and unstaged files
      (added, modified, deleted, renamed, conflicted, untracked) and
      shows each file's diff against the index or HEAD in the compare
      view, highlighted. Read-only in M27; staging arrives in M28.
- [ ] **History**: every commit reachable from all branches and tags,
      newest first, with a graph of smooth, anti-aliased branch lines
      (curves where a branch forks or joins) in the left of each row,
      the subject, author, relative date and ref labels. Selecting a
      branch, tag or remote in the sidebar scrolls to its commit.
- [ ] **Commit details**: a commit's changed files are listed (renames
      shown as old → new), and selecting one shows its diff against the
      first parent in the compare view, with the M18 diff options
      (ignore whitespace, case, blank lines). Merge commits pick the
      parent to compare against.
- [ ] **Open in Nimbus**: double-clicking a file, or "Open in Editor",
      opens the working copy in the Nimbus editor that is already
      running, through M25's hand-off (which lands first).
- [ ] **Live**: committing, checking out or editing files from a
      terminal or the editor updates the window within a second, by
      watching `.git` and the working tree.
- [ ] **Budgets**: first frame under 500 ms in release (git is never
      run before the first frame); on a generated repository of 1
      million commits (`bench/gen.sh git`), the first screen of history
      shows within 1 s and scrolling the whole history never drops a
      frame; history memory under 150 bytes per commit plus its
      subject; no git call on the event loop.

## Scope

**In**: the `--git` flag and palette action; the window layout; status,
history with graph, commit details; refresh on change; `bench/gen.sh
git`; a manual chapter (`docs/manual/git.md`).

**Deferred**: everything that changes the repository (M28–M30); blame
and file history (M30); searching history (M30); submodule, worktree
and LFS screens (unscheduled; git itself still handles them).

## Design

**Window mode, one process per git window.** `--git` builds the same
`App` with a different root layout, so it shares the renderer, theme,
config, fonts, sidebar tree, pickers, compare view and palette. It is a
separate process from the editor and always starts its own, like the
measurement flags in M25; DESIGN.md §2's "one window" rule holds per
process. The layout is a new module, `app/git_window/`, with a view per
pane; the editor's `Document`, compare and picker code is used as it is.

**git as a subprocess, extended.** `app/git.rs` stays the only place
that runs git. It gains:

- `Repo`: the repository's top folder and git folder, found once with
  `rev-parse --show-toplevel --git-dir`.
- A streaming call beside `run`: a child whose stdout is read in chunks
  on a worker and parsed as it arrives, cancelled by dropping it. `log`
  needs it (a large history is hundreds of MB of output); `run` keeps
  its 10 s timeout and cap for short calls.
- Parsers for machine output only, never for text meant for people:
  `status --porcelain=v2 -z --branch`, `log -z` with a fixed
  `--format` of hashes, parents, author, dates, refs and subject,
  `for-each-ref --format`, `diff-tree -z -r -M --raw`, and
  `cat-file --batch` for reading many blobs through one process.

Every call keeps `GIT_TERMINAL_PROMPT=0` and `GIT_OPTIONAL_LOCKS=0` (so
a status refresh never takes the index lock from the user's terminal),
and arguments are always passed as separate words, never through a
shell.

**History as compact rows.** Each commit is a fixed-size record (object
id, parent indices, author index, time, offset of its subject into one
string arena), parents resolved to row indices as the stream arrives.
Authors are interned. Rows are drawn with the same "only what is on
screen" rule as the compare view's `Rows`.

**The graph.** Lanes are assigned in one pass as rows arrive, the way
`git log --graph` does: each row knows which lanes pass through it and
which lines join or leave. The owner chose smooth curves over straight
lines and box-drawing glyphs, so `render` gains a third instanced
pipeline beside rects and glyph quads: one instance per segment (a
straight line or a cubic Bézier curve, with width and colour), drawn as
a quad around the segment whose fragment shader works out the distance
to the curve and turns it into anti-aliased coverage. Only the rows on
screen produce segments, so the instance count stays in the hundreds.
Compatibility mode (M23) draws the same segment list on the CPU by
flattening each curve into short lines and filling them with coverage,
added in the same change as the GPU pipeline, as the standing
requirement in `docs/milestones/README.md` asks.

**Commit details reuse folder compare and the compare view.** The file
list is `diff-tree` output; a selected file's two sides are read with
`cat-file --batch` on a worker and handed to the compare view as two
read-only texts, so large-file handling, long-line splitting and the
diff options come for free.

**Refresh.** `watch.rs` watches `.git` (HEAD, refs, index, packed-refs)
and the folders the status lists; a change schedules one status and
ref refresh after a short pause, and a new commit appends to the
history without reloading it.

## Tasks

1. `Repo`, the streaming call and the four parsers in `app/git.rs`,
   with tests against repositories built in a temporary folder.
2. `bench/gen.sh git`: a repository of 1 million commits with merges,
   built with `git fast-import`.
3. `--git` and "Open Git Window"; the window layout with empty panes and
   the error states.
4. History rows and the streaming load; scrolling; ref labels.
5. Graph lanes; the curve pipeline in `render` and its CPU twin for
   compatibility mode.
6. Commit details: file list, compare view on a worker, parent choice
   for merges.
7. Changes view (read-only) and the sidebar sections.
8. Refresh on change; Open in Editor.
9. Manual chapter, measurements, Results.

## Concepts

- **Plumbing and porcelain**: git's machine-readable commands versus the
  ones meant for people, and why a client only parses the former.
- **The index**: the staging area between the working tree and a commit,
  and why status has two columns.
- **A commit graph**: commits as nodes with parent links, and how lanes
  turn that into lines on screen.
- **Drawing curves on the GPU**: a Bézier curve, signed distance, and
  why a quad with a distance shader gives smooth edges at any zoom.
- **Streaming a child process**: reading output as it arrives instead of
  waiting for the end.

## Risks

| Risk | Fallback |
|---|---|
| `git status` takes seconds on a huge working tree | It never blocks a frame; the Changes view shows "reading…", and the manual points at git's own `core.fsmonitor` |
| Starting a process per call is slow on Windows | Batch reads through one `cat-file --batch` process; refresh is debounced |
| The curve shader looks wrong on some GPUs or costs too much | Measure it in the scroll benchmark; the fallback is straight segments in the same pipeline |
| Graph lanes explode on histories with many parallel branches | Cap visible lanes and draw an overflow marker, as other clients do |
| Different git versions print different output | Only stable plumbing formats are parsed; the minimum git version is checked once and named in the error |

## Results

_To be filled in when the milestone lands._
