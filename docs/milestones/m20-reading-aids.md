# M20 — Reading aids

**Status: proposed.** Written on 2026-10-18 from what M13 and M14
deferred for the editor's second use, reading rather than typing: sticky
scroll, a minimap, Markdown preview, and proportional fonts in UI chrome
(which `DESIGN.md` §2 allows "later" and the preview needs).

## Goal

Help a reader keep their place in a long file and read prose as prose,
without giving up on-demand rendering or the per-visible-line cost
model.

## Done when

- [ ] **Sticky scroll**: the enclosing scopes of the first visible line
      (function, impl, class, Markdown heading) stay pinned at the top of
      the view, up to five rows, from M14's fold regions; clicking a row
      jumps to it. On by default where a syntax tree exists; off above
      the 16 MB limit, where there is none.
- [ ] **Minimap**: a column at the right edge showing the file as
      coloured blocks per line, with the visible range marked; drag to
      scroll. It is drawn from the highlight spans of a window of lines
      around the view (at most 4,000 rows at two pixels each), never the
      whole file, so a 4 GB log shows its neighbourhood. Toggle in the
      palette, default off; the width is a setting.
- [ ] **Proportional UI font**: `ui_font` in the config names a
      proportional system font for the sidebar, tabs, status bar,
      palette and pickers; the editor area stays monospace. Default is
      still the bundled font, so the first frame skips the font scan.
- [ ] **Markdown preview**: "Open Preview" (`Primary+Shift+V`) renders
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

_To be filled in when the milestone lands._
