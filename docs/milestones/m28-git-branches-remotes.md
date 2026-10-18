# M28 — Git window: branches and remotes

**Status: planned (2026-10-18).** Third of the git window plans
(M26–M29).

## Goal

Switch, create, rename and delete branches and tags, stash and
restore work, and fetch, pull and push, with the user's own
credentials and no way for a prompt to hang the window.

## Done when

- [ ] **Branches**: checkout from the sidebar or a fuzzy picker
      ("Switch Branch…", `Primary+Shift+B` in the git window); create a
      branch from any commit in the history; rename; delete (refusing an
      unmerged branch unless confirmed). Checking out with uncommitted
      changes that would be overwritten says so and offers to stash.
- [ ] **Tags**: create a lightweight or annotated tag on any commit;
      delete; push a tag.
- [ ] **Stashes**: stash (with an optional message and "include
      untracked"), apply, pop and drop from the sidebar; selecting a
      stash shows its files and diffs like a commit.
- [ ] **Remotes**: Fetch, Pull and Push in the toolbar and palette, with
      progress shown in the status bar and a Cancel button. Pull follows
      the user's `pull.rebase` setting. Push sets the upstream for a new
      branch. Force push is offered only as `--force-with-lease`, behind a
      confirmation.
- [ ] **Credentials**: HTTPS through the user's credential helper and
      SSH through their agent work with nothing to configure; when git
      needs a password or passphrase it cannot get, the window asks in a
      small dialog (through `GIT_ASKPASS` and `SSH_ASKPASS` pointing at
      `nimbus --askpass`), and never hangs.
- [ ] **Ahead and behind** counts for the current branch in the status
      bar, updated after every fetch.

## Scope

**In**: local branches, tags, stashes, fetch, pull, push, the askpass
helper, ahead/behind.

**Deferred**: adding and removing remotes (git's command line does it),
pull requests and any hosting service's API (unscheduled), automatic
background fetch (open question in M29 Results if wanted).

## Design

**Long operations are cancellable children.** Fetch, pull and push use
M26's streaming call with no timeout; `--progress` output on stderr is
parsed for the percentage, and Cancel kills the child (git leaves the
repository consistent when a fetch or push is interrupted).

**Askpass is the same binary.** git runs `GIT_ASKPASS` with a prompt
as its argument and reads the answer from stdout. `nimbus --askpass
PROMPT` connects back to the git window that started it over a socket
named in an environment variable, the window shows the prompt in a
dialog, and the answer goes back the same way. The password is never
written to disk or logged. It reuses the transport of M25's
single-instance socket, which lands first.

**Checkout safety comes from git.** The window does not predict
conflicts; it runs `git switch` and shows git's refusal in plain words,
with a Stash and Switch button when the refusal is about local changes.

## Tasks

1. Branch, tag and stash actions with the switch picker.
2. Streaming fetch, pull and push with progress and Cancel.
3. `--askpass` and its dialog.
4. Ahead and behind.
5. Manual, Results.

## Concepts

- **Refs and upstreams**: what a branch is (a name for a commit), and
  what "ahead 2, behind 1" means.
- **Askpass**: how git asks a GUI for a password without a terminal.
- **Force with lease**: overwriting a remote branch only if nobody else
  pushed in the meantime.

## Risks

| Risk | Fallback |
|---|---|
| An SSH host-key prompt cannot go through askpass | It is shown as git's error with the command to run once in a terminal |
| Credential helpers differ by platform | Nothing Nimbus-specific: git calls the user's own helper |
| A pull leaves conflicts | The window switches to M29's conflict list; until M29 lands, conflicted files open in the merge view and the status says to finish in a terminal |

## Results

_To be filled in when the milestone lands._
