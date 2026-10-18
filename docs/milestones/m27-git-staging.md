# M27 — Git window: staging and committing

**Status: draft (2026-10-18).** Second of the git window plans
(M26–M29); builds on M26's read-only window.

## Goal

From the git window's Changes view, stage and unstage whole files,
single hunks or selected lines, discard changes safely, and write,
commit and amend with a message box that edits like Nimbus.

## Done when

- [ ] **Stage and unstage**: each file has Stage or Unstage; each hunk
      in its diff has a Stage Hunk or Unstage Hunk button; selecting
      lines and choosing Stage Lines stages just those. "Stage All" and
      "Unstage All" are in the palette. The lists and diffs update at
      once.
- [ ] **Discard**: Discard Changes on a file, hunk or lines asks first
      and is undoable within the window ("Undo Discard"); discarding an
      untracked file moves it to the system trash, as the editor's
      sidebar delete does.
- [ ] **Commit**: a message box under the Changes list is a Nimbus text
      area (undo, multi-cursor, find, soft wrap, a ruler at 72 columns),
      with Commit (`Primary+Enter`) and an Amend toggle that loads the
      last commit's message. Hooks, signing and the user's config run as
      they do from a terminal, and a failing hook shows its output.
- [ ] **Partially staged files** show both diffs (staged, and still
      unstaged) without confusion.
- [ ] **Budgets**: staging a hunk in a 10 MB file updates both lists in
      under 100 ms after git returns; no git call on the event loop.

## Scope

**In**: stage, unstage and discard at file, hunk and line level;
commit; amend; the message box; author shown from git's config.

**Deferred**: committing with a different author, commit templates
beyond `commit.template` (which git applies itself), and partial
staging of binary files (whole file only).

## Design

**Hunks and lines become patches.** The compare view already knows the
hunks between the index and the working tree. Staging a hunk or lines
writes a minimal unified diff for just that part and feeds it to
`git apply --cached --unidiff-zero` (or `--reverse` to unstage),
through stdin, never a temporary file. Line staging builds the patch
by keeping the chosen added and removed lines and turning the rest
into context, the method other clients use.

**Discard keeps a way back.** Before discarding tracked changes the
window saves the file's current contents (`git hash-object -w`, which
only writes an object, never a ref), so Undo Discard can restore them
for the life of the window; how long a discard stays recoverable beyond
that is open question 3 in DESIGN.md §9.

**Commit through git.** `git commit -F -` with the message on stdin,
plus `--amend` when toggled, so `commit.gpgsign`, hooks and templates
behave as in a terminal. Hooks may run long, so commit uses the
streaming call with no timeout and a Cancel button.

**The message box is a `Document`.** It reuses the editor's document
and view with a fixed height and Markdown-free plain text, so every
editing action already works; its text survives closing the window in
`state.toml`, keyed by repository, until a commit uses it.

## Tasks

1. Patch building for hunks and lines, with tests applying them to real
   repositories, including no final newline, CRLF and renames.
2. Stage, unstage and discard actions; Undo Discard.
3. The message box; Commit and Amend; hook output.
4. Partially staged files.
5. Manual, measurements, Results.

## Concepts

- **Unified diffs and `git apply`**: how a hunk header says where a
  change goes, and why context lines matter.
- **Objects and refs**: why writing an object is harmless and moving a
  ref is not.
- **Hooks**: scripts git runs around a commit, and why a client must
  show their output.

## Risks

| Risk | Fallback |
|---|---|
| Line staging builds a patch git rejects | The patch is checked with `git apply --check` first; on failure the message names it and nothing changes |
| A hook waits for terminal input | `GIT_TERMINAL_PROMPT=0` and no stdin; Cancel kills it |
| Line endings and filters make the index text differ from the file | Read both sides through git (`cat-file --filters`), as M18 does |

## Results

_To be filled in when the milestone lands._
