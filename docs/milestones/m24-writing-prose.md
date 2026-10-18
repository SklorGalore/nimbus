# M24 — Writing prose

**Status: planned (2026-10-18).** Written on 2026-10-18 from what M20
deferred for the second kind of content in `DESIGN.md` §1, prose and
notes in Markdown: images in the preview and exporting it to HTML, plus
the word count every prose editor shows. The draft also had spell
checking (deferred by M14); the owner chose to leave it out
(`DESIGN.md` §9).

## Goal

Make Nimbus a better place to write Markdown: a word count, and a
preview that shows the document's images and can be saved as a web
page.

## Done when

- [ ] **Word count**: the status bar shows words (and words selected,
      when there is a selection) for Markdown and plain text, counted
      on a worker for files over 1 MB and not at all above 16 MB.
- [ ] **Preview images**: `![alt](path)` with a local PNG or JPEG path
      (relative to the document) shows the image in the preview,
      scaled to the column width; anything else, including web
      addresses, stays the M20 placeholder with its alt text.
- [ ] **Export to HTML**: "Export Preview as HTML…" writes one
      self-contained file (styles inline, local images embedded) to a
      path chosen in the save dialog.
- [ ] Budgets: a keystroke's cost unchanged on the main thread;
      `--smoke` unchanged (the decoders run only when a preview shows
      an image); Linux binary under 35 MB with the image decoders
      counted.

## Scope

**In**: the word count, local images in the preview, HTML export.

**Deferred**: spell checking (the owner's choice, 2026-10-18); images
from the web (the editor makes no network requests beyond the opt-in
update check); GIF, SVG and WebP images; exporting to PDF.

## Design

**Words are counted the way the preview reads them.** Word boundaries
come from `unicode-segmentation`, already a dependency, over the
document's text; Markdown syntax characters (`#`, `*`, backticks, link
targets) are not counted. Small files count on the main thread after an
edit's pause; files over 1 MB count on a worker against a buffer
version, and a stale count is dropped, as search results are.

**Images reuse the colour glyph path.** M16 added an `Rgba8Unorm` atlas
page for colour emoji; preview images get their own textures (one per
image, at most 2048 pixels on a side after scaling) drawn as textured
quads by the same glyph pipeline with the colour flag. They are decoded
on a worker with the `png` crate and `zune-jpeg`, and dropped when the
preview tab closes.

**The export walks the preview's blocks**, the same list M20 lays out,
and writes HTML with a small built-in stylesheet that follows the
current theme's colours. Code blocks keep their highlight colours as
inline spans.

## Tasks

1. Word count in the status bar; the worker path for large files.
2. Image decoding on a worker; `png` and `zune-jpeg` added with size
   measurements.
3. Image textures and quads in the preview; scaling to the column.
4. HTML export.
5. Measurements; manual pages; Results.

## Concepts

- **Textures that are not glyphs**: drawing an image with the glyph
  pipeline.
- **Self-contained HTML**: embedding images as `data:` addresses so one
  file carries everything.

## Risks

| Risk | Fallback |
|---|---|
| The decoders push the Linux binary near 35 MB | Drop JPEG, or put both decoders behind a default-on cargo feature |
| A huge image fills GPU memory | Scale on the worker before upload; the 2048-pixel cap is a hard limit |

## Results

_To be filled in when the milestone lands._
