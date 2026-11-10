# The git window

`nimbus --git` opens a window for looking at a git repository: what you
have changed and not committed yet, the whole history with its branches
drawn as a graph, and any commit's files and differences. It uses your
own `git` program, so it agrees with your terminal, your config and
your credentials. From the Changes view you can stage, unstage and
discard whole files, single changes or just the lines you pick, and
write and make commits. Branches, remotes and merging come in later
versions.

## Opening it

| How | Opens |
|---|---|
| `nimbus --git` | The repository holding the current folder. |
| `nimbus --git DIR` | The repository holding `DIR` (any folder inside it). |
| **Open Git Window** in the editor's palette or View menu | The repository holding the workspace's first folder. |

The git window is a process of its own, separate from the editor: it
never takes over the editor's window, and closing one leaves the other
open. The title reads *repository* — Git — Nimbus.

Outside a repository, the window says so instead of showing anything. It
says so too when `git` is not installed or not on `PATH`, or is older than
2.25.

## The layout

The window has four panes, and the dividers between them can be dragged.

- **The sidebar** on the left lists **Changes** (with how many files
  have changed), then your **Branches**, **Remotes** (remote-tracking
  branches such as `origin/main`), **Tags** and **Stashes**. Click a
  section's title to fold it away; Remotes starts folded. The branch you
  are on has a dot. Clicking a branch, tag or remote jumps to its commit
  in the history; clicking a stash shows what it saved.
- **The history** in the middle lists every commit reachable from your
  branches, remote-tracking branches, tags and HEAD, newest first. Each
  row shows the graph, the branch and tag labels at that commit, the
  subject, the author (when the pane is wide enough) and how long ago it
  was made. The graph draws one line per branch in its own colour,
  curving where a branch forks off or is merged back. The commit you have
  checked out has a ring with a dot in it; a merge commit has a ring.
- **The details** on the right show the selected commit: its message,
  hash, author and date, the committer when someone else made the commit
  (after a rebase or a cherry-pick), its parent, and the files it changed
  (`M` modified, `A` added, `D` deleted, `R` renamed, shown as
  `old → new`).
- **The compare** below the details shows the selected file's
  differences against the commit's parent, side by side and highlighted,
  exactly as the editor's [compare view](compare-and-merge.md) shows them.

A merge commit has more than one parent. Its details show **Compare
with** and a button per parent: the files and differences are against
the parent you pick (the first one at the start, which is the branch the
merge was made on).

## Changes

Selecting **Changes** in the sidebar shows your working tree instead of
a commit:

- **Staged** files are what the next commit will hold: their differences
  are between the last commit and the index (the staging area).
- **Not staged** files have changes you have not added: their
  differences are between the index and the file on disk. New files git
  does not track yet (`?`) and files with a merge conflict (`!`) are
  listed here too. A new folder is listed as one entry, as `git status`
  does.

A file can be in both lists when part of it is staged: the staged row
says *part staged* and the other *rest not staged*. Select either row to
see that part of the change.

## Staging and unstaging

Every file in **Not staged** has a **Stage** button, and every file in
**Staged** has **Unstage**. **Stage All** and **Unstage All** sit next to
the two headings. Staging a new folder stages everything in it.

To stage part of a file, select its row and use the second row of the
bar above the compare:

- **Stage Hunk** stages one change: the one you last stepped to with
  Next Change (`F7`), or picked a row of, or else the first.
- **Stage Lines** stages only the rows you picked. Click a row of the
  compare to pick it, drag or `Shift`-click to pick several, or, with the
  compare focused, use `Up` and `Down` (`Shift` to extend). Picked rows
  have a tint and a bar on the left; `Escape` clears them. Picking works
  in the side-by-side view, not in Unified.

From a **Staged** row the same buttons read **Unstage Hunk** and
**Unstage Lines**, and take that part back out of the index; the file on
disk is not touched. The lists and the compare update as soon as git is
done.

A part of a file is put into the index as a small patch that git applies
(`git apply --cached`). If the file changed meanwhile so the patch no
longer fits, git refuses it whole, nothing changes, and the status bar
says so: look at the compare again and retry. Line endings are kept as
the file has them, and a missing newline at the end of the file stays
missing. Files with conflicts, deleted files and submodules are staged
whole.

## Discarding changes

**Discard** (on a Not staged row), **Discard Hunk** and **Discard Lines**
throw away changes in your working copy: the file goes back to what is
staged. The window always asks first. A new file you discard goes to
the system trash, as deleting from the editor's sidebar does.

**Undo Discard** (in the palette, or `Cmd+Z` / `Ctrl+Z` outside the
message box) puts back what you discarded last, then the one before,
while the window stays open. The window keeps the discarded text as a
git object, which changes no branch and is cleaned up by git in time;
closing the window ends Undo Discard, and the question before each
discard says so.

## Committing

Under the Changes lists is the commit message box. It edits like a
Nimbus document: undo, several cursors, cut, copy and paste, soft wrap
(Toggle Soft Wrap) and a ruler at 72 columns. `Tab` moves on to the
next pane instead of typing a tab; `Escape` leaves the box. What you
type is kept when you click away or close the window, one draft per
repository, until a commit uses it.

**Commit** (or `Cmd+Enter` / `Ctrl+Enter`) commits what is staged with
the message. It runs `git commit`, so your hooks, commit signing,
templates and config work exactly as in a terminal. While it runs, the
button reads **Stop** and the status bar says so; a long hook can be
stopped with **Stop** or `Escape`. When git refuses the commit (a
failing hook, a signing problem), the window shows git's own words.

**Amend last commit** loads the last commit's message into the box, and
Commit then rewrites that commit with what is staged and the message as
you leave it. Turn it off again to get your own draft back.

Next to the buttons is who the commit will be by, from git's config
(`user.name` and `user.email`); when they are not set, the window says
so, and so would git.

## The palette

`Cmd+Shift+P` / `Ctrl+Shift+P` opens the git window's own palette: every
staging, discard and commit action, and the compare and view actions it
shares with the editor. Each has an id (such as `git.stageHunk`) you can
bind keys to in `config.toml`'s `[keys]`, as in the editor; only Commit
has a key by default.

## The bar above the compare

| Button | What it does |
|---|---|
| Spaces | Whether whitespace counts: always, not at the ends of lines, or not at all. |
| Case | Ignore differences of upper and lower case. |
| Blank lines | Ignore changes that only add or remove blank lines. |
| Unified | One column with the changes stacked, instead of two. |
| Open in Editor | Open the file's working copy in Nimbus. |

The first three are the same options as in the editor's Compare panel,
and start from the ones you last chose there.

**Open in Editor** (or double-clicking a file, or `Enter`) hands the file
to the Nimbus editor that is already running, the way `nimbus FILE` does
in a terminal; when none is running, it starts one.

## Keys

The git window follows your key bindings for the editor's commands:

| Command | Keys (defaults) | In the git window |
|---|---|---|
| Move up and down | `Up`, `Down`, `PageUp`, `PageDown`, `Home` and `End` of the document | Move through the focused list |
| Next Tab, Previous Tab | `Ctrl+Tab`, `Ctrl+Shift+Tab` | Move the focus between the sidebar, the history, the files, and under Changes the compare and the message box |
| Escape | `Escape` | Clear picked rows, leave the message box, stop a commit, or focus the history |
| Undo | `Cmd+Z` / `Ctrl+Z` | In the message box, undo typing; elsewhere, Undo Discard |
| Command Palette | `Cmd+Shift+P` / `Ctrl+Shift+P` | The git window's palette |
| Insert Newline | `Enter` | Open the selected file in the editor |
| Copy | `Cmd+C` / `Ctrl+C` | Copy the selected commit's hash, or with the files focused, the file's path |
| Next Change, Previous Change | `F7`, `Shift+F7` | Step through the compare's changes |
| Zoom In, Zoom Out, Reset Zoom | `Cmd+=`, `Cmd+-`, `Cmd+0` / `Ctrl+…` | Change the text size |
| Toggle Light/Dark Theme | | Switch theme for this window |
| Quit | `Cmd+Q` / `Ctrl+Q` | Close the window |

Clicking a pane focuses it too.

## Keeping up to date

The window watches the repository's git folder and the folders of the
changed files. When you commit, check out, stage or edit a file, from a
terminal or from the editor, the window reads the status again a moment
later, and when a branch or tag moved it reads the history again in the
background, showing the old one until the new one has caught up. Coming
back to the window (focusing it) also reads the status again, so a
change in a folder nobody watched still shows.

## Large repositories

The history is read as `git log` prints it, so the first screen appears
while the rest is still arriving ("Reading history…" in the top right
counts the commits so far), and only the rows on screen are drawn.
A million commits take under 150 bytes each in memory, plus their
subjects.

Two of git's own features make a large repository faster:

- **The commit-graph file** lets git list commits newest first without
  reading every commit beforehand. `git gc` writes it, and `git
  commit-graph write --reachable` writes it at once. Without it, the
  first screen of a very large history can take seconds.
- **`core.fsmonitor`** makes `git status` fast on a huge working tree.
  The git window turns it off for every call, because a repository's
  own config could name any program as the monitor; set `git config
  core.untrackedCache true` instead, which needs no program.

## Safety

A repository you cloned can carry config that names programs for git to
run. While it reads, the git window runs git with the same protections
as Compare with HEAD: no filesystem monitor, no hooks, no fetching of
missing objects, no signature checks (`log.showSignature` is forced
off), no password prompts, and no locks taken on the index, so it never
gets in the way of your own git commands.

What you ask it to change runs as the same command would in your
terminal, because that is what you expect of it: committing runs your
hooks and signs as your config says, and staging applies the
repository's clean filters (such as Git LFS). The filesystem monitor
stays off and git never prompts for a password, and file names are
always passed as names, never as patterns. Arguments always go to git as separate words,
never through a shell, and commit hashes are checked before they are
passed on.
