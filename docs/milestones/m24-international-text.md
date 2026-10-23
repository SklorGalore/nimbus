# M24 — International text

**Status: in progress, built (2026-10-19); the mixed-script typing
benchmark and the checks on the owner's Mac (Results) wait.** Written on 2026-10-18 from what M16 and
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

- [x] **Fallback by script**: a missing glyph's fallback font is chosen
      for its script and, for Han characters, for the language: the
      `fallback_fonts` table in the config (`han = ["Hiragino Sans"]`,
      `cyrillic = [...]`) is tried first, then a built-in list per
      platform (PingFang, Hiragino, Microsoft YaHei, Noto Sans CJK and
      friends), then M8's smallest-suitable-font rule. `cjk_language =
      "ja" | "zh-Hans" | "zh-Hant" | "ko"` picks the regional glyph
      forms; by default it follows the system language.
- [x] **Wide glyphs fit their cells**: a fallback glyph wider than its
      cells (two for East Asian wide characters, one otherwise) is
      scaled down to fit, so mixed lines stay on the grid; a screenshot
      test shows a mixed Latin, CJK and Hangul line at 1× and 2×.
      _As built: `mixed_scripts_stay_on_the_grid_at_1x_and_2x` checks
      every glyph quad's cell at both scales, since CI machines lack
      CJK fonts for an image test; checked by eye with the CPU painter._
- [x] **Variable fonts**: a configured variable font draws at the
      `font_weight` the config names (default its regular instance);
      `font_weight` also applies to static font families that have that
      weight.
- [x] **East Asian encodings**: built with `--features east-asian`,
      Shift_JIS, EUC-JP, GBK, GB18030, Big5 and EUC-KR round-trip
      through "Reopen with Encoding", "Save with Encoding", `--encoding`
      and the `[encodings]` table, with a fixture for each. The feature
      stays off by default (owner's choice) and they are never guessed
      from content (M17's rule stands).
- [x] **Huge files in other encodings**: a mapped file over 256 MB in a
      single-byte encoding (Windows-1252, ISO-8859-1) is editable, decoded
      piece by piece; multi-byte encodings stay read-only above the
      threshold, as M17 left them. _As built: decoded into a private
      UTF-8 copy instead, which makes every known encoding editable,
      multi-byte ones included (Results)._
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

Built on 2026-10-19 in a Linux container without a GPU, so everything
drawn was checked with the CPU painter (`--compat --headless`) at 1×
and 2×, using WenQuanYi Zen Hei, Noto Color Emoji and Loma. A line of
Latin, Han, kana, Hangul, emoji, Thai and Devanagari stayed on the grid.

- **Linux release binary**: 29.2 MB (budget 35 MB). One new
  dependency, `sys-locale` 0.3 (the system language; a few KB),
  certified in `supply-chain/audits.toml` after reading its source.
- **Startup**: unchanged. The fallback preferences are sent to the
  worker with the GPU state, and no fallback font loads until a glyph
  needs one. `bench/smoke.sh` on the owner's M5 Pro (2026-10-23): cold
  726 ms, warm 128.8 / 132.1 / 132.4 / 140.0 ms (median 132 ms, budget
  500 ms), in line with earlier milestones.
- **Where the code is**: `render/src/script.rs` (script of a
  character, runs, `CjkLanguage`), `render/src/fallback.rs`
  (`FallbackPrefs`, built-in lists, `find_editor_font` with a weight),
  `render/src/font.rs` (variation coordinates), `render/src/shape.rs`
  (`shape_cluster`), `render/src/text.rs` (`fallback_cluster`,
  `fit_in_cells`), `app/src/io.rs` (`transcode`) and
  `app/src/saver.rs` (`SaveJob::encoded`).

Surprises and changes from the plan:

- **No `unicode-script`.** swash already knows each character's script
  (`Codepoint::script()`), so no new table was needed.
- **Graphemes are shaped per cell, not per run.** Only graphemes the
  editor font lacks go to a fallback font, and each is shaped on its
  own with its script and language (so `locl` picks the regional Han
  shape) and cached by (font, language, text). The run is still worked
  out, so a neutral character such as `ー` or `、` takes the script
  around it. Devanagari conjuncts shape correctly but are drawn small,
  since each grapheme is squeezed into its cells.
- **The default Han language.** With `cjk_language` unset, a Japanese,
  Chinese or Korean system language decides; any other gives
  Simplified Chinese, the most common reader.
- **Huge files: a decoded copy, not a decode step in the piece table.**
  The plan's "one byte, one character" did not keep offsets: positions
  are UTF-8 bytes and `é` is one byte in Windows-1252 but two in UTF-8,
  so offsets would not map back one to one. Instead the file is decoded
  once into a private UTF-8 file in the temp folder (mode 0600, deleted
  at once on Unix and on close on Windows) and that copy is mapped;
  saving encodes it back into a temporary file beside the original and
  renames it over. The piece table and `core` are untouched, and it
  works for every encoding, so huge UTF-16 or Shift_JIS files became
  editable too (`DESIGN.md` §9). Costs: a copy's worth of disk, and the
  window waits while decoding (about a second per GB here). If the
  copy cannot be written the file opens read-only, as before. The
  `random_edits_to_a_decoded_copy_save_exactly` test stands in for
  the oracle run on a mapped Windows-1252 file.

Left for the owner's Mac (no GPU or macOS fonts here):

1. A file mixing Latin, Japanese, Chinese and Hangul at 1× and 2×:
   wide characters in two cells, nothing overlapping.
2. `cjk_language = "ja"` against `"zh-Hans"` on `骨`.
3. A variable font (for example JetBrains Mono's variable build) with `font_weight = 300` and `700`; the ignored
   test `NIMBUS_VARIABLE_FONT=/path/font.ttf cargo test -p nimbus-render
   -- --ignored a_variable_font`.
4. `m16_typing_bench` on a mixed-script file, against the M16 budget.
5. A Windows-1252 file over 256 MB: Reopen with Encoding, edit, save,
   and check the saved bytes.
6. ~~`bench/smoke.sh` first-frame times~~: done, see Startup above.
