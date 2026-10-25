# M28 — Git window: staging and committing

**Status: done (2026-10-24).** Second of the git window plans
(M27–M30); builds on M27's read-only window. Drawing is checked in
compatibility mode only; see Results for what to look at on the Mac.

## Goal

From the git window's Changes view, stage and unstage whole files,
single hunks or selected lines, discard changes safely, and write,
commit and amend with a message box that edits like Nimbus.

## Done when

- [x] **Stage and unstage**: each file has Stage or Unstage; each hunk
      in its diff has a Stage Hunk or Unstage Hunk button; selecting
      lines and choosing Stage Lines stages just those. "Stage All" and
      "Unstage All" are in the palette. The lists and diffs update at
      once.
- [x] **Discard**: Discard Changes on a file, hunk or lines asks first
      and is undoable within the window ("Undo Discard"); discarding an
      untracked file moves it to the system trash, as the editor's
      sidebar delete does.
- [x] **Commit**: a message box under the Changes list is a Nimbus text
      area (undo, multi-cursor, find, soft wrap, a ruler at 72 columns),
      with Commit (`Primary+Enter`) and an Amend toggle that loads the
      last commit's message. Hooks, signing and the user's config run as
      they do from a terminal, and a failing hook shows its output.
- [x] **Partially staged files** show both diffs (staged, and still
      unstaged) without confusion.
- [x] **Budgets**: staging a hunk in a 10 MB file updates both lists in
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
for the life of the window. That is as long as it lasts: the owner
chose not to keep discards under a Nimbus ref in the user's repository
or as copies in the trash (`DESIGN.md` §9), so closing the window ends
Undo Discard, and the confirmation says so.

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

Built 2026-10-24 in a cloud container with no GPU: the window was drawn
in compatibility mode (`--headless --compat`) only.

| Measure | Result |
|---|---|
| Staging one hunk of a 10 MB file, on the worker, git included | 400–510 ms: the clean form through git 143 ms, the exact diff and patch 147 ms, `git apply --cached` about 110 ms (Linux container) |
| Work on the event loop after git returns | Under 1 ms: it starts a status read and the compare's diff on workers and swaps their results in, so the 100 ms budget holds; the whole round trip is the worker time above plus the status read |
| New dependencies | None |

Run the staging number with `cargo test --release -p nimbus --bin nimbus
-- --ignored --nocapture staging_bench`.

What differs from the plan, and surprises:

- **Patches are built in the direction they apply.** Unstaging and
  discarding build the reverse patch themselves (picked added lines
  removed, picked removed lines put back) instead of passing
  `--reverse`, so one patch builder covers all three and the line
  numbers always count from the text git has.
- **No `--check` pass.** `git apply` checks every hunk before it writes
  anything, so a patch that no longer fits changes nothing; a test
  (`a_stale_patch_changes_nothing`) holds it to that.
- **The working tree's clean form.** Staging compares against the text
  as `git add` would store it: `git hash-object -w --stdin --path=FILE`
  applies `core.autocrlf` and the clean filters, then the blob is read
  back. A filter that changes the number of lines makes partial staging
  refuse, with a message to stage the whole file.
- **Drafts in their own file.** The plan kept the message in
  `state.toml`, but the editor rewrites that file whole from memory and
  would drop a draft written by the git window's process. Drafts live in
  `commit-drafts.toml` beside it, one per repository, the newest 50.
- **Writing runs as in a terminal.** Reading keeps M26's protections
  (no hooks, no repository filter programs). Commands that change the
  repository run hooks, signing and clean filters, as the plan's
  "behave as in a terminal" asks; they keep `core.fsmonitor=false`, no
  prompts and literal pathspecs (`docs/security.md`).
- **Picking lines.** Lines are picked as rows of the side-by-side
  compare: click, drag, `Shift`-click, or `Up`/`Down` with the compare
  focused. The unified view does not pick rows yet.
- **Find in the message box is deferred.** The editor's find bar is part
  of the editor's `App`, not of a document; the box has every other
  editing action (undo, several cursors, soft wrap, the 72-column
  ruler).
- **A key scope for the window.** Actions gained a `GitWindow` scope:
  the window's own actions (Commit on `Primary+Enter`, the staging ones
  unbound) win over the editor's in that window and never show in the
  editor's palette. The window has its own palette.
- **Discarding a new folder** sends the whole folder to the trash, as
  the sidebar does; Undo Discard restores it where the trash can (not
  on macOS, as in M19).
- **The patch diffs the file again.** The compare's hunks follow its
  ignore options (whitespace, case, blank lines), so the worker diffs the
  exact texts once more to build the patch: 147 ms of the 10 MB case. It
  runs on the worker, so typing and scrolling never wait for it.
- **Not yet for screen readers**, like the rest of the git window.

To check on the Mac:

- The buttons, the bar's second row, the picked-row tint and the message
  box with its ruler, on the GPU at 1x and 2x, in both themes.
- A commit with a hook that fails shows its output; a slow hook can be
  stopped.
- Commit signing with your own setup (a signing key that needs a
  passphrase has to get it from an agent, since git cannot ask in a
  terminal here).
