# M12 — Workspace

**Status: done (2026-10-18), after M11.** Written on 2026-10-17 from items
deferred by M4 (recently opened files), M6 (file operations in the tree),
M7 (following a growing log) and M8 and DESIGN.md §2 (session restore).
Approved the same day: `nimbus` alone reopens the last session, deletes
go to the trash through the `trash` crate, and autosave for crash
recovery is not planned (`DESIGN.md` §9).

## Goal

Nimbus remembers where you were and lets you manage files without
leaving it: the last session comes back, recent files are a keystroke
away, the tree can create, rename and delete, and a log that is still
being written can be followed.

## Done when

- [x] **Session restore**: closing Nimbus remembers, per project root,
      the open files, their cursor and scroll positions, the active tab,
      the expanded folders in the tree and the sidebar mode. `nimbus
      FOLDER` reopens that folder's session; `nimbus` with no arguments
      reopens the last one; `nimbus FILE` opens just that file, and
      `--no-session` skips restoring. Untitled buffers with text are not
      restored (there is no autosave; see Scope).
- [x] Restoring does not pay for every file before the first frame: the
      active file opens as now, the other tabs show their names and load
      when first shown or within the first second on a worker, whichever
      is first. `--smoke` with a ten-file session stays within the startup
      budget, recorded in Results.
- [x] A file that no longer exists is dropped from the session with a
      message-line note, not an error.
- [x] **Recent files**: "Open Recent" (`Primary+Shift+O`) lists the
      last 100 files opened in any session, fuzzy-matched like
      the file opener; the file opener itself lists recent files in the
      root first when the query is empty. "Reopen Closed Tab"
      (`Primary+Shift+T`) brings back the last closed tab with its cursor.
- [x] **Tree operations**: New File, New Folder, Rename (`F2`; `Enter` on
      macOS, where Finder uses it), Delete, and Reveal in Finder/Explorer/file manager,
      from the tree's keyboard, a context menu on macOS and Windows, and
      the palette on Linux. Rename and Delete of an open file move or
      close its tab (Delete asks when the tab has unsaved edits); a
      rename over an existing name asks. Delete moves to the system
      trash through the `trash` crate, measured for binary size.
- [x] **Follow a growing file**: "Follow Tail" (`view.followTail`,
      shown in the status bar for files whose size grew while open) keeps
      the view pinned to the end as new lines arrive, for both buffer
      backends, with no unsaved-edit dirtying and nothing in the undo
      history. On the 4 GB log with a writer appending 1 MB per second,
      the frame stays under budget and memory does not grow with the
      appended bytes for the mapped backend. Turning it off (any scroll
      up, or the toggle) leaves the file as it was loaded plus what has
      arrived. A truncated or rotated file reloads as M6 does.
- [x] The window's size and position are remembered too, and restored
      when that display is still present.

## Scope

**In**: session state in `state.toml`, recent files, reopen closed tab,
tree operations, follow tail, window geometry.

**Deferred**: drag and drop in the tree (moving files between folders;
rename-with-path covers it clumsily), multiple roots, named workspaces.

**Not planned**: autosave of unsaved work to the state folder for crash
recovery. It would protect against the one thing `panic = "abort"` makes
costly, but it means Nimbus writes copies of the user's text somewhere
they did not choose, and the owner decided against it (2026-10-17). It
stays in `backlog.md`'s unscheduled list in case that changes.

## Design

**State file** (`state.rs`). `State` gains `recent: Vec<RecentFile>`,
`window: Option<WindowGeometry>` and `sessions: Vec<Session>`, the last
capped at 20 roots in most-recent-first order. A `Session` is the root
path, the sidebar mode, the expanded folders (relative paths), and a
`Vec<SessionTab>` of path, cursor byte offset, scroll byte offset, and
whether it was active. Byte offsets rather than lines because that is
what the document and view already hold; on restore they are clamped to
the file's current length and widened to a character boundary. The file
stays small (a 20-root session file is a few kilobytes) and is written on
quit and whenever a tab opens or closes, debounced by a second, through
the same temp-and-rename write as documents.

**Lazy tabs**. A restored tab that has not been shown holds a
`Document::placeholder(path)`: no buffer, no syntax state. Switching to
it, or a worker finishing its background open, replaces the placeholder.
Everything that touches `tabs[i].doc` already goes through a few helpers
after M8 (saving all, the quit prompt, searching open tabs in M10); those
treat a placeholder as clean and skip it. The first frame opens only the
active file, so the startup budget is the same as opening one file.

**Recent files** are updated on open and on save-as, de-duplicated by
canonical path, and pruned of missing files when the picker opens (on a
worker: 100 `stat` calls can be slow on a network drive).

**Tree operations** (`tree.rs`, `sidebar.rs`). Each operation is an
`Action` acting on the tree's selected node; New and Rename use an inline
text field in the tree row, which is a `Document` like the find bar's
fields (M3), so editing keys work. The filesystem call runs on a worker
(a rename on a network share can block), and the tree waits for the
watcher's event (M6) rather than editing its own model, so the tree
always shows what the disk says. Open tabs compare the old path to their
own and move with it.

**Follow tail**. The watcher already reports a change to an open file's
folder (M6). When the file is longer than the document's recorded stamp
and the document has no edits, the document appends the new bytes:

- *Rope backend*: read the bytes from the old length to the new, check
  they are valid UTF-8 (a writer mid-character is retried at the next
  event), and insert them at the end through a path that neither marks
  the document dirty nor records undo (`Document::append_external`).
  The syntax state sees the edit as any other, so highlighting follows.
- *Mapped backend*: the mapping is fixed-length (its `Source` promises
  the bytes never change), so growth is a new `Source`: a second mapping
  of the file at its new length, which shares the pages of the first.
  The piece table's original piece is extended to the new length, the
  line index scans the new bytes (the M7 indexer, continuing from where
  it stopped), and the old mapping is dropped when nothing holds it. The
  file's first length bytes are the same bytes, which is what makes the
  swap sound; the invariant is stated in a `// SAFETY:` comment at the
  one `unsafe` call that maps it, in `mapping.rs` where M7 put the other.
  Memory stays flat because mapped pages are the OS's, as in M7.

A shrink (rotation, truncation) is the existing M6 reload. The view pins
to the end only while `follow` is on; any upward scroll turns it off, so
reading an old part of the log is never interrupted.

**Window geometry** is saved as logical size and position plus the
monitor's name; winit gives the monitors on restore, and a position off
every current monitor is ignored.

**Actions**: `file.openRecent`, `file.reopenClosed`, `file.clearRecent`,
`session.reopenLast`, `tree.newFile`, `tree.newFolder`, `tree.rename`,
`tree.delete`, `tree.reveal`, `view.followTail`.

## Tasks

1. `State` with sessions, recent files and window geometry; load, save,
   cap and prune; unit tests on the TOML round trip.
2. Session capture on quit and tab changes; the command-line rules;
   `--no-session`.
3. Placeholder tabs and background opening; the startup measurement.
4. Open Recent, recent-first file opener, Reopen Closed Tab.
5. Tree inline text field; New File and New Folder; Rename with tab
   updates.
6. Delete to trash with the unsaved prompt; Reveal; context menu on macOS
   and Windows.
7. Follow tail for the rope backend, with the append path that skips
   dirty and undo; the status bar toggle.
8. Follow tail for the mapped backend: the second mapping, piece
   extension, continued indexing; `xhigh` review of the `unsafe` and
   its invariant, per `CLAUDE.md`.
9. Window geometry; measurements (startup with ten tabs, the 1 MB/s
   append test); Results.

## Concepts

- **Why state is not config**: the user's file is theirs; what the app
  remembers goes elsewhere (M6 started this).
- **Placeholders and lazy loading**: restoring ten files costs one file's
  work at startup.
- **Extending a memory mapping**: a mapping has a fixed length; growth
  is a new mapping of the same bytes, and why that is safe while
  truncation is not.
- **Letting the watcher drive the tree**: after a filesystem operation,
  the disk is the truth, so the tree waits for the event rather than
  guessing.

## Risks

| Risk | Fallback |
|---|---|
| A restored tab's byte offsets land in a changed file | Clamp and widen; worst case the cursor is at a nearby place, never a crash |
| `trash` pulls in platform libraries that bloat the binary | Measure; fall back to a confirmed permanent delete |
| A log writer appends faster than 1 MB/s | Debounce already batches; appends are O(new bytes); the view redraws at most once per frame |
| Two Nimbus windows (not supported) overwrite each other's `state.toml` | Last writer wins; the file is small and never holds text |

## Results

Built on 2026-10-18 in a Linux container with four shared cores, with
the window on a virtual display (Xvfb) drawn by llvmpipe, Mesa's software
Vulkan; the owner's M5 Pro is faster and steadier, so the first-frame
numbers below are for comparing with each other, not with M10's.

| Metric | Value |
|---|---|
| Tests | 504 passing with M14 merged in, plus 20 opt-in benchmarks or diagnostics |
| Release binary | 22.4 MB on Linux (M15 recorded 21.7 MB; budget 35 MB). `trash` brings little on Linux (it is plain file moves there) and more on macOS and Windows, where it talks to the system's file manager APIs; CI's `release-size` job checks all three |
| `--smoke` first frame, ten-file session | 84–105 ms warm (median 89 ms); 184 ms on the first, cold run |
| `--smoke` first frame, one file named | 80–98 ms warm (median 86 ms) |
| `--smoke` first frame, `--no-session` | 62–76 ms warm |
| Restoring ten tabs before the first frame | one file read (the front tab, 0.4 ms for 200 KB); the other nine are read on a worker right after it |
| 4 GB log, Follow Tail, writer appending 1 MB/s for 30 s | resident memory 132.7 MB before and 132.6 MB after 31 MB arrived; frame 3.1 ms, worst 6.5 ms (software rendering); line numbers counted to the new end (65,724,595 lines) |
| New dependency | `trash` 5.2.9 |

**Surprises.**

- A file rewritten in place changes what its old mapping shows too, so
  comparing the old mapping with the new one cannot tell a rewrite from
  an append. `MappedFile` keeps a copy of the first and last 64 bytes it
  mapped, and the new mapping is compared with that copy.
- Undo runs (long deletions kept as pieces) named their document by the
  source's address. A grown file has a new source, so runs recorded
  before the growth would have been refused; a lineage number now names
  the document, and is kept when the source grows.
- The line index's scanner could not be restarted part way: a resumed
  scanner (`Scanner::resume`) now continues from an index's last
  checkpoint and gives the same checkpoints as indexing the file whole
  (a test in `mapped.rs` checks this).
- `--smoke` is unattended, so it restores a session to measure it but
  never saves one; otherwise every measurement would overwrite the
  owner's session.
- On macOS the `trash` crate's default asks Finder through an Apple
  script, which makes macOS ask the user to let Nimbus control Finder.
  Asking `NSFileManager` directly (its other mode) needs no permission.

