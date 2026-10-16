# M6 — File tree

**Status: done**

## Goal

A collapsible project sidebar that stays in sync with the disk, and open
files that notice when something else changes them.

## Done when

Ticked items were verified here by tests or headless renders. **Owner**
items need a person at the keyboard; each says what to try.

- [x] `Primary+B` toggles a sidebar showing the project root (action and
      View menu; headless renders). `Primary+Shift+E` shows it and moves
      the keyboard into it; Escape gives the keyboard back.
- [x] Folders expand on click or Right/Enter and collapse on Left; their
      contents load on expand, never up front (tree and sidebar tests).
- [x] Folders sort before files; both sort case-insensitively;
      `.gitignore`d entries are hidden, with a "Show/Hide Ignored Files in
      Sidebar" action (listing test). `.git` is always hidden; dotfiles
      show.
- [x] Clicking a file opens it in a tab; the active tab's file is
      highlighted in the tree and revealed when you switch tabs (reveal
      test; headless render opens folders down to the file).
- [x] **Owner, confirmed:** drag the divider between the sidebar and the text; the
      pointer shows a resize cursor, and the width is kept after a
      restart. (Remembered in `state.toml`, not the config; see Results.)
- [x] **Owner, confirmed:** with the sidebar showing an open folder, create, delete
      and rename files in it from a terminal: the tree follows within a
      second.
- [x] **Owner, confirmed:** change an open file from a terminal (`echo x >> file`):
      with no unsaved edits it reloads, cursor on the same line; with
      unsaved edits the tab says "changed on disk" and the message line
      offers Reload from Disk and Keep My Version (palette). Document
      tests cover each case on real files.
- [x] **Owner, confirmed:** delete an open file from a terminal: its tab says
      "deleted"; closing it asks to save; saving recreates it.
- [x] **Owner, confirmed:** `--smoke` in a large repository (100k files) is no slower
      than M5. Nothing is walked or watched before the first frame: the
      root is listed on a worker, and watching starts after the frame.

## Scope

**In**: sidebar, lazy tree, file watching, external change handling.

**Deferred**: creating, renaming and deleting files from the tree (a
post-M8 idea; the OS file manager covers it), git status colours (non-goal).

## Design

**Model** (`app/tree.rs`): `Node { name, path, kind, children: Option<Vec<NodeId>>, expanded }`
in an arena `Vec<Node>`. `children: None` means not loaded yet. Expanding
sends a listing request to a worker, which uses `ignore::WalkBuilder` with
`max_depth(1)` so `.gitignore` applies, and replies with sorted entries.
The visible rows are a flattened list rebuilt when expansion changes.

**Watching** (`app/watch.rs`): one `notify` recommended watcher on the
project root, recursive. Events are debounced for 100 ms on the watcher
thread, then forwarded through the channel. The tree reloads only the
affected folders that are loaded. Open documents compare the event path to
their own path.

**Reload safety**: before reloading, compare the file's modification time and
length with what we last saw, so our own atomic saves (temp plus rename) do
not trigger a reload of the file we just wrote. Cursor position is kept by
line number.

**Layout**: the window splits into sidebar and editor columns. The editor
`View` takes an origin and width instead of assuming the whole window; this
also prepares for split views later.

## Tasks

1. View origin and width; sidebar column with a placeholder.
2. Tree model and lazy listing worker.
3. Sidebar rendering, keyboard and mouse interaction, reveal active file.
4. Resizable width persisted to config.
5. `notify` watcher with debouncing; tree refresh.
6. External change handling for open documents, ignoring our own saves.
7. Deleted-file tab state.

## Concepts

- **Arena trees**: indices instead of pointers, which keeps Rust's borrow
  checker happy.
- **File watching** and why events need debouncing.
- **Telling your own writes apart from someone else's.**

## Risks

| Risk | Fallback |
|---|---|
| Recursive watching of huge repositories hits OS watch limits on Linux | Watch only expanded folders and open files' parents; log when the limit is hit |
| `notify` event shapes differ per platform | Treat every event as "something under this path changed" and re-list |

## Results

| Metric | Value |
|---|---|
| Tests | 220 passing, plus 7 opt-in benchmarks or diagnostics |
| Release binary | 15.02 MB on macOS (M5: 14.80 MB); `notify` adds 0.22 MB |
| Startup work for the tree | none before the first frame |

**Design changes**

- **Watching is not recursive.** The plan said one recursive watch on the
  root. Instead each folder listed in the sidebar, and the folder of each
  open file, is watched on its own. On Linux a recursive watch registers
  every folder of the repository up front, which costs startup time and
  hits the system's watch limit; this way the cost follows what you open.
- **The sidebar width is remembered in `state.toml`**, beside the config,
  not in `config.toml`. Nimbus never writes the config file, which is the
  user's, hand-edited and commented.
- Watch events say whether a folder's listing may have changed (create,
  remove, rename) or a file was only written. Only the first kind re-lists
  folders, and a file written over and over reloads at most once a second.
- The editor column starts at `Layout::left`; the tab bar, find bar and
  text all sit right of the sidebar.
- Reloading a file changed on disk clears its undo history.
- Documents remember their resolved path (`Document::canonical`) at open
  and save, matching what file watching reports, instead of resolving it
  per frame.

**Concepts**

- *Arena trees* (`tree.rs`): nodes in one `Vec`, linked by index, so
  parents and children can point at each other without shared mutable
  pointers. Removed nodes' slots are reused.
- *File watching and debouncing* (`watch.rs`): the OS reports changes as
  bursts of events; collecting them for 100 ms turns a save into one
  update.
- *Telling your own writes apart* (`document.rs`, `Stamp`): each open and
  save records the file's size and modification time, taken from the
  written file itself before it replaces the old one. An event for a file
  that still matches is our own save.

**Surprises**

- Recording the file's time after reading it, or after saving, leaves a
  gap in which another program's write would be mistaken for ours; the
  stamp is now taken before reading, and from the file handle on save.
- On macOS, adding a watch restarts the event stream, so watching waits
  until after the first frame.
- **A freeze closing the app on Windows** (owner testing). Every call into
  `notify` that changes the watched folders could block the caller. On
  macOS it waits for its event loop to go idle, which never happens while
  a build writes under a watched folder (macOS watches are recursive
  underneath). On Windows each watch waits for an acknowledgement from
  `notify`'s thread, which can itself wait indefinitely when a watch is
  removed, as happens when a tab closes. The OS watcher now lives on its
  own thread; the main thread only sends it the folders to watch, in one
  batch, and never waits for it, at exit included.
- A file edit script truncated `app.rs` once during the work; it was
  restored from the last commit and the edits redone. Nothing committed
  was affected.

**End-of-milestone review** (code-review skill, high) found ten issues,
all fixed: toggling ignored files during a listing could leave a folder
empty for good; focus could stay in the sidebar after opening a file from
the picker, swallowing typing; closing a deleted file's tab did not ask to
save; the disk stamp was taken after reading, hiding a write in between;
Enter in an empty sidebar typed a newline; rows under the message line
counted as visible; a change during a listing was lost; merging a
re-listed folder was quadratic; the active file's path was resolved every
frame; and a file written continuously was reloaded every 100 ms.

**Known limitations**

- A clean file that changes on disk reloads on the main thread; for a very
  large file being appended to, that is a pause once a second.
- No creating, renaming or deleting from the tree (deferred, as planned).
- A change on disk within the same second as our save, of the same size,
  on a filesystem with one-second timestamps, is not noticed.
