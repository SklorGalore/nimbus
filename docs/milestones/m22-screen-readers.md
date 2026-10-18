# M22 — Screen readers

**Status: next (2026-10-18).** Written on 2026-10-18, after 1.0, from
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

- [ ] **The editor reads**: with VoiceOver on (macOS), moving the cursor
      speaks the character, word or line it moved over; Shift-selecting
      speaks the selected text; typing echoes characters. Windows
      (NVDA) and Linux (Orca) get the same tree, covered by the tree
      tests; the owner chose not to check them by hand.
- [ ] **Chrome reads**: tabs are a tab list (name, modified state,
      position "2 of 5"); the sidebar is a tree with expand and collapse;
      the palette and pickers are a list whose highlighted item is
      spoken as you arrow through it; the find bar's fields have labels
      and its match count is spoken; the status bar is a status region.
- [ ] **Messages are announced**: the message line ("Saved", errors,
      "Reloaded from disk") is a polite live region, so it is spoken
      without moving focus.
- [ ] **Huge files stay cheap**: a 4 GB mapped log exposes only a window
      of lines around the view (the same rule as M20's minimap), never
      the whole file; moving past the window rebuilds it.
- [ ] **Keyboard reach**: every panel can be reached and left without
      the mouse (a written audit in Results, with any gaps fixed or
      listed); `docs/manual/` gets an accessibility page, including the
      Linux desktop shortcuts that can grab `Alt+Up` and friends.
- [ ] Budgets: `--smoke` unchanged with no screen reader running (the
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

_To be filled in when the milestone lands._
