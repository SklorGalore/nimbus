# M24 — Writing prose

**Status: draft (2026-10-18).** Written on 2026-10-18 from what M14 and
M20 deferred for the second kind of content in `DESIGN.md` §1, prose
and notes in Markdown: spell checking (M14), images in the preview and
exporting it to HTML (M20), plus the word count every prose editor
shows. Open questions are in `DESIGN.md` §9.

## Goal

Make Nimbus a good place to write Markdown: misspellings marked as you
type, a word count, and a preview that shows the document's images and
can be saved as a web page.

## Done when

- [ ] **Spell checking**: misspelled words in Markdown and plain text,
      and in comments and strings in code, get a wavy underline in the
      theme's `spelling` colour, checked for the visible lines on a
      worker after a 150 ms pause in typing. "Spelling Suggestions" at
      the cursor lists up to eight corrections in a picker; "Add to
      Dictionary" and "Ignore Word" work at the cursor. Off for files
      over 16 MB, which have no syntax tree to say where prose is;
      `spell_check = false` turns it off, and per-language settings
      override it.
- [ ] **Dictionaries**: an English (US) Hunspell dictionary is bundled;
      other languages load from `.aff` and `.dic` files dropped in the
      config folder's `dictionaries/` and chosen with `spell_language`.
      Added words go to `words.txt` in the config folder, a plain list
      the user can edit.
- [ ] **Word count**: the status bar shows words (and words selected,
      when there is a selection) for Markdown and plain text, counted
      on a worker for files over 1 MB.
- [ ] **Preview images**: `![alt](path)` with a local PNG or JPEG path
      (relative to the document) shows the image in the preview,
      scaled to the column width; anything else, including web
      addresses, stays the M20 placeholder with its alt text.
- [ ] **Export to HTML**: "Export Preview as HTML…" writes one
      self-contained file (styles inline, local images embedded) to a
      path chosen in the save dialog.
- [ ] Budgets: checking the visible lines takes under 2 ms on a worker;
      a keystroke's cost unchanged on the main thread; `--smoke`
      unchanged (the dictionary loads on first use); Linux binary under
      35 MB with the dictionary and image decoders counted.

## Scope

**In**: spell checking on visible lines, bundled English, user
dictionaries and a word list, the word count, local images in the
preview, HTML export.

**Deferred**: grammar checking; auto-correct while typing (it changes
text the user did not type); spell checking whole files at once (a
"next misspelling" command would need it); images from the web (the
editor makes no network requests beyond the opt-in update check); GIF,
SVG and WebP images; exporting to PDF.

## Design

**Spell checking is a highlight layer, like search matches.** A worker
gets the visible lines and the prose ranges in them (Markdown text,
plain text, and the `comment` and `string` captures from the syntax
tree), splits words with `unicode-segmentation`, and checks each with
`spellbook`, a pure-Rust reader of Hunspell dictionaries. It returns
byte ranges with the buffer version; stale results are dropped, as
search results are. Words in code-shaped spans (`snake_case`,
`camelCase`, URLs, anything inside backticks) are skipped.

**The dictionary loads lazily.** The bundled English files are
`include_bytes!` (about 0.5 MB); they are parsed on the worker the first
time a prose file is shown, so neither startup nor a code-only session
pays for them.

**`words.txt` is the one file Nimbus writes in the config folder**, and
only on "Add to Dictionary". `config.toml` is still never written
(`CLAUDE.md`); the word list is data the user asked to keep, beside
their themes and languages.

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

1. `spellbook` and the bundled dictionary, loaded on a worker; tests
   with known words and a misspelling list.
2. Prose ranges from the tree; word splitting; code-shaped words
   skipped.
3. The underline layer, versioned results, the 150 ms pause.
4. Suggestions picker, Add to Dictionary, Ignore Word; `words.txt`;
   `spell_check` and `spell_language` settings; user dictionaries.
5. Word count in the status bar.
6. Image decoding and textures in the preview.
7. HTML export.
8. Measurements; manual pages; Results.

## Concepts

- **Hunspell dictionaries**: why a dictionary is two files, a word list
  and a file of affix rules, and how "walking" becomes "walk" plus "-ing".
- **A highlight layer from a worker**: the same versioned-result pattern
  as search, now for spelling.
- **Textures that are not glyphs**: drawing an image with the glyph
  pipeline.

## Risks

| Risk | Fallback |
|---|---|
| The English dictionary or the decoders push the Linux binary near 35 MB | Load the dictionary from the config folder only and ship it beside the binary in packages; drop JPEG |
| Spell checking flags too much code (identifiers in comments) | Skip more code shapes; per-language off by default for code if it stays noisy |
| `spellbook` is slow on some dictionaries' affix rules | Cache results per word for the session; the visible-lines limit bounds the work |

## Results

_To be filled in when the milestone lands._
