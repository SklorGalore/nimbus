# M6 — File tree

**Status: planned**

## Goal

A collapsible project sidebar that stays in sync with the disk, and open
files that notice when something else changes them.

## Done when

- [ ] `Primary+B` toggles a sidebar showing the project root.
- [ ] Folders expand on click or Right/Enter and collapse on Left; their
      contents load on expand, never up front.
- [ ] Folders sort before files; both sort case-insensitively;
      `.gitignore`d entries are hidden, with a "Show ignored files" action.
- [ ] Clicking a file opens it in a tab; the active tab's file is
      highlighted in the tree and revealed when you switch tabs.
- [ ] The sidebar width is draggable and remembered in config.
- [ ] Creating, deleting or renaming files outside the editor updates the
      tree within a second.
- [ ] An open file changed on disk reloads automatically when it has no
      unsaved edits, keeping the cursor near its old line; with unsaved
      edits, the message line warns and offers "Reload" and "Keep mine".
- [ ] A file deleted on disk marks its tab as deleted; saving recreates it.
- [ ] Opening a 100k-file repository does not slow startup.

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

_Fill in when done._
