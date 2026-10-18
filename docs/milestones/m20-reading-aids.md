# M20 — Reading aids

**Status: done (2026-10-18).** Written on 2026-10-18 from what M13 and M14
deferred for the editor's second use, reading rather than typing: sticky
scroll, a minimap, Markdown preview, and proportional fonts in UI chrome
(which `DESIGN.md` §2 allows "later" and the preview needs). Approved
the same day with the minimap included, windowed and off by default
(`DESIGN.md` §9).

## Goal

Help a reader keep their place in a long file and read prose as prose,
without giving up on-demand rendering or the per-visible-line cost
model.

## Done when

- [x] **Sticky scroll**: the enclosing scopes of the first visible line
      (function, impl, class, Markdown heading) stay pinned at the top of
      the view, up to five rows, from M14's fold regions; clicking a row
      jumps to it. On by default where a syntax tree exists; off above
      the 16 MB limit, where there is none.
- [x] **Minimap**: a column at the right edge showing the file as
      coloured blocks per line, with the visible range marked; drag to
      scroll. It is drawn from the highlight spans of a window of lines
      around the view (at most 4,000 rows at two pixels each), never the
      whole file, so a 4 GB log shows its neighbourhood. Toggle in the
      palette, default off; the width is a setting.
- [x] **Proportional UI font**: `ui_font` in the config names a
      proportional system font for the sidebar, tabs, status bar,
      palette and pickers; the editor area stays monospace. Default is
      still the bundled font, so the first frame skips the font scan.
- [x] **Markdown preview**: "Open Preview" (`Primary+Shift+V`) renders
      the Markdown document in a second column (M13's split) with
      headings, emphasis, lists, code blocks (highlighted with the
      editor's grammars), links and tables, in the UI font, scrolled in
      step with the source; it updates on a 150 ms pause after typing.
      Images and HTML are shown as placeholders.
- [ ] Budgets: sticky scroll and the minimap add under 1 ms to a scroll
      frame on the 10 MB file; the preview re-renders a 1 MB document in
      under 50 ms off the main thread; `--smoke` unchanged.

## Scope

**In**: sticky scroll, the windowed minimap, the UI font setting, the
preview column with synchronised scrolling.

**Deferred**: images in the preview (an image decoder is a dependency
and a texture path); exporting the preview to HTML; previewing other
markup; proportional text in the editor area (still a non-goal).

## Design

**Sticky scroll is a query over the visible range**, the same
`folds.scm` regions M13 consumes, kept for the region containing the
first visible line and its ancestors. It draws as rows over the text
with a shadow, using the layered renderer from M3.

**The minimap is a tiny `View`** at a two-pixel row height, rendered with
rects (one per highlight span, colour from the theme) and no glyphs. It
uses the same anchored scroll position, so it needs no prefix sum over
the file; its window is the lines whose rows fit the column's height.

**Proportional text in chrome** uses swash shaping with real advances
instead of the cell grid; the sidebar and tabs already draw rows of
glyphs, and gain an `x` per glyph. The editor area keeps the grid.

**The preview is a layout pass over a Markdown tree.** M14's Markdown
grammar (block plus inline) already gives the tree; a `preview.rs`
module in `app` turns it into a list of blocks with wrapped runs in the
UI font and fenced code in the editor font, laid out on a worker from a
snapshot, and drawn with the same two pipelines. Scroll sync maps the
source's first visible line to the block containing it.

## Tasks

1. Sticky scroll from fold regions; click to jump; the 16 MB rule.
2. Minimap view and drawing; drag to scroll; width setting.
3. `ui_font` setting; proportional advances in chrome; font scan stays
   lazy.
4. Markdown block layout from the tree on a worker; `preview.rs`.
5. Preview column, scroll sync, the 150 ms update pause.
6. Code blocks in the preview through the grammars; links open in the
   browser.
7. Measurements; Results.

## Concepts

- **Scopes from the tree**: how sticky scroll is a by-product of fold
  regions.
- **A view with no glyphs**: why the minimap is cheap.
- **Shaping with real advances**: what changes when text is no longer
  on a grid.

## Risks

| Risk | Fallback |
|---|---|
| The minimap tempts a whole-file render | The window rule is a hard limit in the view; a file past it shows the neighbourhood only |
| Proportional chrome breaks the alignment of icons and columns in the sidebar | Icons keep a fixed advance; columns are measured, not counted |
| The preview grows into a browser | Scope is the block types listed; HTML and images are placeholders by decision |

## Results

Landed on 2026-10-18 (nimbus 0.10.0, nimbus-render 0.3.0, nimbus-syntax
0.3.0). Measured in the cloud container (4 cores, release build, Mesa's
software Vulkan for the GPU numbers); the owner's Mac numbers are to be
added. The budgets box stays open until the preview number is confirmed
on the Mac.

| What | Budget | Measured |
|---|---|---|
| Scroll frame on the 10 MB file, queueing time p50 (`m20_scroll_bench`): neither / sticky / minimap / both | under 1 ms added | 490 µs / 551 µs / 678 µs / 720 µs: both add 0.23 ms |
| Preview layout of a 1 MB document after an edit (`m20_preview_bench`) | under 50 ms, off the main thread | 76–82 ms here; the first layout, which parses every code block, 190 ms |
| `--smoke` | unchanged | not measurable here (no window); the first frame looks up no fonts unless `ui_font` is set, and the preview starts nothing until opened |
| Release binary (Linux) | under 35 MB | 26.4 MB; no new dependency |

How the preview number breaks down: about 45 ms reads the blocks from
the tree and about 30 ms wraps the text. A bare walk over every node of
that document's tree takes 32 ms in this container, about 110 ns a
node, which is slow for tree-sitter; the Mac is likely several times
faster (an estimate, not yet measured). If it still misses the budget,
the next step is to keep each top-level section's blocks between
layouts, as the code colours already are.

Surprises:

- **Asking a tree-sitter node for its next sibling is slow.** It
  searches again from the parent, so walking a long document's sections
  that way was quadratic. A tree cursor steps in constant time.
- **Parsing the document again on the worker cost 330–420 ms** for
  1 MB, and colouring every code block through the injection machinery
  2 s. The preview now copies the document's own tree, which the editor
  already keeps current, and parses each code block on its own, caching
  the colours by the block's text so an edit elsewhere reuses them.
- **Inline Markdown is read by a small scanner**, not the inline
  grammar: the grammar's tree gives colours, but the preview needs where
  bold or a link starts and ends, nested. The scanner opens emphasis
  only where a closer follows, so a lone `*` stays a `*`.
- **A space takes the look of the text before it.** Attached to the
  next word, the space before a link was underlined and the one before
  `code` sat on the code background.
- **The preview is a tab**, like compare and merge views: a tab whose
  document is the source's, with a flag. Edits, saving and closing need
  no new rules, and the session keeps it.
- Sticky scroll's own surprise, from the first part: a long `fn`
  signature pinned its `) {` line as a second scope; a body ending with
  its item is now pinned once when its first line only closes the
  signature.
