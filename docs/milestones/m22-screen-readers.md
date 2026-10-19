# M22 — Screen readers

**Status: done (2026-10-19).** Written on 2026-10-18, after 1.0, from
the first item the backlog kept for after the release: a GPU-drawn
editor is invisible to VoiceOver, NVDA and Orca, because nothing on
screen is a native control the operating system can describe. The
owner chose to ship 1.0 without it (`DESIGN.md` §9); this plan is what
"the first milestone after 1.0" means. Approved the same day: it goes
before the library upgrades (M23), on winit 0.30, and NVDA and Orca are
covered by the tree tests rather than checked by hand (`DESIGN.md` §9).

## Goal

Someone using a screen reader can open a file, move through it by
character, word and line, hear and change the selection, switch tabs,
walk the sidebar and use the palette, with no cost to anyone who is not
running one.

## Done when

- [x] **The editor reads**: with VoiceOver on (macOS), moving the cursor
      speaks the character, word or line it moved over; Shift-selecting
      speaks the selected text; typing echoes characters. Windows
      (NVDA) and Linux (Orca) get the same tree, covered by the tree
      tests; the owner chose not to check them by hand.
- [x] **Chrome reads**: tabs are a tab list (name, modified state,
      position "2 of 5"); the sidebar is a tree with expand and collapse;
      the palette and pickers are a list whose highlighted item is
      spoken as you arrow through it; the find bar's fields have labels
      and its match count is spoken; the status bar is a status region.
- [x] **Messages are announced**: the message line ("Saved", errors,
      "Reloaded from disk") is a polite live region, so it is spoken
      without moving focus.
- [x] **Huge files stay cheap**: a 4 GB mapped log exposes only a window
      of lines around the view (the same rule as M20's minimap), never
      the whole file; moving past the window rebuilds it.
- [x] **Keyboard reach**: every panel can be reached and left without
      the mouse (a written audit in Results, with any gaps fixed or
      listed); `docs/manual/` gets an accessibility page, including the
      Linux desktop shortcuts that can grab `Alt+Up` and friends.
- [x] Budgets: `--smoke` unchanged with no screen reader running (the
      tree is never built); with VoiceOver running, a keystroke on the
      10 MB file costs under 1 ms more than without; Linux binary under
      35 MB with the adapter's D-Bus dependencies counted.

## Scope

**In**: an AccessKit tree for the window, the editor, tabs, sidebar,
palette and pickers, find bar, status bar and message line; actions a
screen reader sends (focus, click, set selection, scroll into view)
mapped onto existing `Action`s; the accessibility manual page.

**Deferred**: the compare and merge views beyond reading each side as a
plain text field (their row structure needs its own design); the
Markdown preview as a document with headings (read as plain text
first); a high-contrast theme (a theme file, could ride along as a
stretch task); speech for minimap and sticky scroll (decorative, marked
hidden).

## Design

**AccessKit is a tree we describe, not controls we create.** The app
keeps its own drawing; beside it, it builds a tree of plain-data nodes
(`accesskit::Node`) with roles (`MultilineTextInput`, `TabList`,
`Tree`, `ListBox`), names, bounds and states, and hands updates to
`accesskit_winit::Adapter`, which translates them for each platform's
accessibility API. The crate's own types are plain data, so the tree
builders are unit-tested headlessly like everything else in `app`.

**Built only when asked.** The adapter calls our activation handler
when a screen reader first asks for the tree; until then
`update_if_active` skips the closure, so building costs nothing. The
one change on the startup path is that the adapter must exist before the
window is first shown: the window is created with `with_visible(false)`,
the adapter is created, then the window is shown. The adapter panics if
the window is already visible, and release builds abort on panic, so
this ordering gets a test and a comment at the call site.

**The editor is one text field with a window of lines.** Each line in
the window is a `TextRun` child with its text, the byte length of each
character (AccessKit counts in UTF-8, as our buffer does), word
boundaries from `unicode-segmentation`, and its bounds from the `View`.
The selection maps from byte offsets to (run, character index). The
window is the visible lines plus a margin (200 lines each side, a
constant), read through the `Buffer` trait, so mapped files behave like
any other.

**Updates are incremental.** An edit replaces the runs for the lines it
touched; a cursor move only changes the text field's selection; a tab
switch replaces the editor subtree. Node ids are stable per line within
a window (derived from the line number and a window generation), so a
reader keeps its place.

**Actions go through the registry.** A screen reader's `SetTextSelection`
becomes a cursor change; `Focus` and `Click` on a tab, tree item or list
item run the same `Action` the mouse would; nothing new is hard-coded in
`app.rs`.

**New dependencies**: `accesskit` and `accesskit_winit`. Version 0.34 of
the adapter targets winit 0.30, so this milestone does not wait for the
winit upgrade (M23). On Linux it brings `accesskit_unix` with `zbus`
and `async-io`; measured for binary size before it stays.

## Tasks

1. Invisible window, adapter, activation handler; `--smoke` before and
   after; a test that the window is shown only after the adapter.
2. Window root and chrome nodes: tabs, status bar, message line as a
   live region.
3. Editor text field: windowed runs, character and word lengths,
   selection mapping; tests against a `String`.
4. Incremental updates on edit, move, scroll and tab switch.
5. Sidebar tree, palette and pickers, find bar.
6. Actions from the reader onto the registry.
7. Keyboard reach audit and fixes; manual page; Linux shortcut note.
8. VoiceOver pass on the Mac (owner); measurements; Results.

## Concepts

- **The accessibility tree**: what a screen reader actually reads, and
  why a GPU editor has to describe itself.
- **Lazy activation**: why the tree costs nothing until someone asks.
- **Text runs and character lengths**: how a reader moves by character
  and word through text it never sees drawn.
- **Live regions**: speaking a message without stealing focus.

## Risks

| Risk | Fallback |
|---|---|
| Creating the window invisible delays or flickers the first frame on some platform | Measure with `--smoke`; if it costs over 10 ms, create the adapter on the first frame's window event instead and document the limitation per platform |
| `accesskit_unix` adds several MB to the Linux binary | Measure; if the 35 MB budget is at risk, put the Linux adapter behind a default-on cargo feature so packagers can drop it |
| Screen readers behave differently with a custom text field | VoiceOver is tested by hand; when a reader misbehaves, prefer AccessKit's documented patterns over workarounds, and list what remains in Results |
| NVDA and Orca are never checked by ear (owner's choice) | The tree tests pin what we describe; a problem a reader has with it is fixed when a user reports it, and the manual page says how to report one |

## Results

_Written 2026-10-19 from a cloud container with no screen and no screen
reader. Everything here is checked by tests against AccessKit's own
consumer (the model NVDA and Orca read through). The owner ran the
VoiceOver pass and the Mac checks in task 8 on the M5 Pro the same day
and reported them done with no problems; no timings were recorded._

**What landed.**

- `crates/app/src/access.rs`: the scene-to-tree builder. Plain data in
  (`Scene`), AccessKit nodes out, plus `Sent`, which diffs the chrome
  so an update carries only the nodes that changed. Node ids are
  stable: fixed ids for the window's parts, and the top byte names the
  kind for repeated ones (tabs, tree rows, options, text runs).
- `crates/app/src/access/text.rs`: `TextWindow`, the editor's text as
  runs. One run per visual row (wrapped rows read as lines), at most
  250 characters a run, the line break counted as the last character
  of its line's last run. Character lengths are bytes per grapheme;
  word starts follow the editor's own word classes, so a reader's
  "next word" lands where `Alt+Right` / `Ctrl+Right` does.
- `crates/app/src/app/access.rs`: the glue. Creates the adapter on a
  hidden window and shows it after (`open_window`, with a test of the
  order), builds the scene from `App` only inside `update_if_active`,
  and maps reader actions: focus or click on a tab, tree row or option
  runs the same code the mouse does; setting the selection moves the
  cursor; replacing the selected text is a normal insert, so undo and
  syntax see it.

**Deviation from the plan.** The text window is centred on the cursor,
not on the view: a reader's place is the caret, and a window that
followed scrolling would drop the caret's run while a sighted helper
scrolls. Scrolling only moves the field's transform (one node). The
window is 200 lines or 256 KB either side, whichever is smaller, and is
rebuilt within 50 lines of an edge; a line over 64 KB is read as the
4 KB blocks around the cursor (`long_line::around`, as M20's minimap).
Folded lines are left out.

**Keyboard reach audit.** Every panel has a key in and `Escape` out:
the tree (`Primary+Shift+E`, `Primary+B`), the palette and pickers
(`Primary+Shift+P`, `Primary+P`, and the other picker actions), find
(`Primary+F`), the Search panel (`Primary+Shift+F`, `Tab` between its
fields and results), tabs (`Ctrl+Tab`, `Primary+1`..`8`) and groups
(`Primary+Alt+Left/Right`). Two gaps, both with a keyboard route and
listed in the manual rather than fixed: the tree's right-click menu
(its five commands are palette actions on the selected row), and
Compare Folders, which has no default key. The manual page is
`docs/manual/accessibility.md`, with the Linux desktop shortcuts that
take `Ctrl+Alt+Up/Down/Left/Right`, `Alt+Up/Down` and friends.

**Not described yet** (deferred, listed in the manual): the Search
panel's results list, the sidebar's Compare panel, rows of the compare
and merge views, the preview's headings, and split groups other than
the focused one. The plan's deferral of compare and merge rows and
preview headings stands; the results list and Compare panel join it.

**Measurements.**

| | Before | After |
|---|---|---|
| Linux release binary (x86_64) | 26,441,824 bytes | 28,818,304 bytes (+2.3 MB, budget 35 MB) |
| Editor description per keystroke, 10 MB file (`access_typing_bench`, container) | n/a | p50 0.26 ms, p99 0.88 ms (budget 1 ms) |
| `--smoke` warm, no screen reader (M5 Pro) | checked by the owner, no change reported | |
| VoiceOver pass and keystroke feel (M5 Pro) | checked by the owner, no problems reported | |

The Linux size includes `accesskit_unix` with `zbus` and `async-io`,
so no cargo feature was needed to keep the budget.

**Surprises.**

- AccessKit panics, and a release build aborts, if the adapter is
  created after the window is visible, or if the first update after a
  reader appears is not a whole tree. Both are guarded: the window is
  created hidden, and a `need_full` flag forces a whole tree after
  every activation.
- A reader's "end of word" from inside `let x` is the space after
  `let`; AccessKit counts the trailing space as part of the word, as
  platform text fields do.
- The first version cut every line of the window again on each
  keystroke: 2.2 ms at p50 on the 10 MB file, over budget. Unchanged
  lines now keep their runs (checked by text and rows, not by offsets,
  so a same-length replace is still seen), and a line in one rope chunk
  is read without a copy: 0.26 ms. The p99 is a new line, which moves
  every row below it and so resends those runs.
- Character lengths are `u8`, so a grapheme over 255 bytes (long
  combining runs) is split into several characters.

**Concepts, and where they live.**

- *The accessibility tree*: `access::build` turns a `Scene` into nodes
  with roles, names, bounds and states; the adapter translates them
  for each platform.
- *Lazy activation*: `App::update_access` hands its builder to
  `update_if_active`, which never calls it until a reader asks.
- *Text runs and character lengths*: `TextWindow::update` and
  `Walk::row_runs` in `access/text.rs`.
- *Live regions*: the message line is a label with `Live::Polite`; the
  status bar is `Role::Status` with `Live::Off`, so cursor moves are
  not spoken twice.
