# M24 — International text

**Status: planned (2026-10-18).** Written on 2026-10-18 from what M16 and
M17 deferred for text that is not Latin: font fallback per script
(M16), variable fonts (M16), East Asian encodings (M17, behind a cargo
feature that is off) and transcoding mapped files (M17). Bidirectional
and right-to-left text stay a non-goal (`DESIGN.md` §2). Approved the
same day with the East Asian encodings kept behind their cargo feature
(`DESIGN.md` §9).

## Goal

Chinese, Japanese, Korean, Cyrillic, Greek, Indic and other
left-to-right scripts look right in the editor and the chrome, files in
their common legacy encodings open and save, and a huge file in one of
them is no longer read-only.

## Done when

- [ ] **Fallback by script**: a missing glyph's fallback font is chosen
      for its script and, for Han characters, for the language: the
      `fallback_fonts` table in the config (`han = ["Hiragino Sans"]`,
      `cyrillic = [...]`) is tried first, then a built-in list per
      platform (PingFang, Hiragino, Microsoft YaHei, Noto Sans CJK and
      friends), then M8's smallest-suitable-font rule. `cjk_language =
      "ja" | "zh-Hans" | "zh-Hant" | "ko"` picks the regional glyph
      forms; by default it follows the system language.
- [ ] **Wide glyphs fit their cells**: a fallback glyph wider than its
      cells (two for East Asian wide characters, one otherwise) is
      scaled down to fit, so mixed lines stay on the grid; a screenshot
      test shows a mixed Latin, CJK and Hangul line at 1× and 2×.
- [ ] **Variable fonts**: a configured variable font draws at the
      `font_weight` the config names (default its regular instance);
      `font_weight` also applies to static font families that have that
      weight.
- [ ] **East Asian encodings**: built with `--features east-asian`,
      Shift_JIS, EUC-JP, GBK, GB18030, Big5 and EUC-KR round-trip
      through "Reopen with Encoding", "Save with Encoding", `--encoding`
      and the `[encodings]` table, with a fixture for each. The feature
      stays off by default (owner's choice) and they are never guessed
      from content (M17's rule stands).
- [ ] **Huge files in other encodings**: a mapped file over 256 MB in a
      single-byte encoding (Windows-1252, ISO-8859-1) is editable, decoded
      piece by piece; multi-byte encodings stay read-only above the
      threshold, as M17 left them.
- [ ] Budgets: the first frame still draws with Hack only (fallback
      fonts load on a worker on first need, as in M8); a line of mixed
      scripts shapes within the M16 typing budget; Linux binary under
      35 MB.

## Scope

**In**: per-script and per-language fallback, wide-glyph fitting,
variable font weights, testing the East Asian encodings behind their
feature, editable mapped files in single-byte encodings.

**Deferred**: bidirectional text and right-to-left scripts (non-goal);
vertical text; input method candidate windows beyond what winit and the
OS provide today; guessing any East Asian encoding from content;
editable mapped files in multi-byte encodings (a character can straddle
pieces); subpixel (LCD) antialiasing, which macOS no longer does and a
composited surface cannot use.

## Design

**The script comes from the character.** Each character's script is
read from Unicode data (`unicode-script`, small tables), and runs of one
script are shaped with one font each.
The fallback lookup becomes "first font in the script's list that has
the glyph", cached per (script, character), so a page of Japanese text
does one lookup per new character, on a worker, then a refill of the
atlas.

**Han unification is a language choice.** The same code point has
different preferred shapes in Japanese, Simplified and Traditional
Chinese and Korean; fonts carry those as separate faces or `locl`
features. `cjk_language` selects the face list and the shaping
language, so `骨` looks Japanese to a Japanese reader.

**The grid is kept by scaling, never by widening.** M8 and M16 keep
every glyph in its cells; a fallback glyph that is too wide is drawn
at a smaller size centred in its cells. Columns, cursor movement and
selections do not change (`core/src/column.rs` already counts wide
characters as two cells).

**Variable fonts are an axis value at rasterization.** `swash` takes
variation settings when it scales a glyph; the weight joins the atlas
key, so a weight change refills the atlas like a size change.

**Mapped single-byte files decode per piece.** In a single-byte
encoding every byte is one character, so a piece of the file decodes to
text without looking at its neighbours, and an edit's offsets map back
one to one. The piece table from M7 gains a decode step on read and an
encode step on save; multi-byte encodings lack that property, which is
why they stay read-only.

**The East Asian encodings stay behind `east-asian`.** M17 measured
that offering them costs about 1 KB, since `encoding_rs` links their
tables whatever is offered; the owner kept the feature off anyway
(2026-10-18), so this milestone only makes sure they work when it is
on, with fixtures and a CI job that builds with the feature.

## Tasks

1. Script detection and per-script runs; tests with mixed lines.
2. Fallback lists per script and platform; the `fallback_fonts` table;
   cache and worker lookup.
3. `cjk_language` and the system-language default; regional faces and
   `locl`.
4. Wide-glyph fitting; screenshot test.
5. Variable fonts and `font_weight`.
6. Fixtures for each East Asian encoding behind `east-asian`; a CI
   job that tests with the feature on.
7. Decode and encode steps for single-byte mapped files; the buffer
   oracle suite run on a mapped Windows-1252 file.
8. Measurements; manual pages; Results.

## Concepts

- **Scripts and runs**: why a line is shaped in pieces, one font per
  piece.
- **Han unification**: one code point, several correct shapes.
- **Font variation axes**: one font file, a continuous range of
  weights.
- **Why single-byte encodings are easy to map**: one byte, one
  character, so offsets survive decoding.

## Risks

| Risk | Fallback |
|---|---|
| System font lists differ across OS versions | The built-in list is a preference order with M8's rule as the last step; a missing font is skipped, never an error |
| Shaping per script run slows typing on mixed lines | Runs are cached by line hash like every shaped line; measure with `m16_typing_bench` on a mixed file |
| Editable mapped Windows-1252 files touch the piece table, the riskiest code in `core` | Behind the oracle suite and the M21 fuzz targets; if either finds a problem late, keep these files read-only and ship the rest |

## Results

_To be filled in when the milestone lands._
