# M29 — Git window: history tools and merging

**Status: planned (2026-10-18).** Last of the git window plans
(M26–M29).

## Goal

Blame a file, follow one file's history, search the history, and run
merges, rebases, cherry-picks and reverts to the end, resolving
conflicts in Nimbus's three-way merge view.

## Done when

- [ ] **Blame**: "Blame" on a file shows its text with each line's
      commit, author and date in a gutter, coloured by age; clicking an
      entry selects that commit in the history; "Blame Previous" steps
      back past a commit. Works on files up to 256 MB without stalling a
      frame.
- [ ] **File history**: "File History" lists only the commits that
      touched a file, following renames, each opening its diff.
- [ ] **Search**: a search box over the history filters by subject and
      message, author, hash prefix, or changed text (`-S` and `-G`), with
      results streaming in like find in files.
- [ ] **Merge, rebase, cherry-pick, revert**: each is a context-menu
      action on a commit or branch. When git stops on conflicts, the
      window shows a conflict list; each file opens in the existing
      three-way merge view (base, ours, theirs read from the index
      stages) and saving it with no conflicts marks it resolved.
      Continue, Skip and Abort finish the operation. A banner says which
      operation is in progress, also when it was started from a
      terminal.
- [ ] **Reset and revert a file**: soft, mixed and hard reset to a
      commit, hard behind a confirmation that names what is lost;
      "Restore File From This Commit".

## Scope

**In**: blame, file history, search, the four operations with the
conflict flow, reset.

**Deferred**: interactive rebase (reordering, squashing and editing
commits), which the owner left for a later plan of its own and
`backlog.md` lists as unscheduled; bisect, submodules, worktrees,
patches by email.

## Design

**Blame streams.** `git blame --porcelain --incremental` reports line
ranges as it finds them; the view fills the gutter as they arrive, so a
large file shows its first screen early. The text itself is an ordinary
read-only `Document`, highlighted.

**Conflicts use the index stages.** For a conflicted path git keeps the
base, ours and theirs versions as stages 1, 2 and 3; reading them with
`cat-file --batch` gives the merge view its three inputs directly,
instead of parsing markers. Saving writes the working file and runs
`git add` on it. The merge view's "no conflicts left" rule (from
`--merge -o`) decides when Continue is enabled.

**The operation in progress is read from `.git`.** The presence of
`MERGE_HEAD`, `rebase-merge/`, `CHERRY_PICK_HEAD` or `REVERT_HEAD` says
what is under way, so the banner is right even when a terminal started
it.

## Tasks

1. Blame view with the incremental stream.
2. File history and history search.
3. Merge, cherry-pick and revert with the conflict list.
4. Rebase (non-interactive) with Continue, Skip and Abort.
5. Reset and Restore File.
6. Manual, measurements, Results.

## Concepts

- **Index stages**: how git holds three versions of a file during a
  conflict.
- **Merge versus rebase**: joining histories versus replaying commits.
- **Reset modes**: what soft, mixed and hard each move.

## Risks

| Risk | Fallback |
|---|---|
| Blame on a long history is slow | It streams and is cancellable; `--incremental` shows recent lines first |
| A rebase stops for something other than a conflict (an `edit` from a terminal, a failing hook) | The banner shows git's state and message, with Continue and Abort |
| Hard reset loses work | Confirmation names the files; the old HEAD stays in git's reflog, and the manual says how to get it back |

## Results

_To be filled in when the milestone lands._
