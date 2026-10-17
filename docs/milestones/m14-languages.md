# M14 — Languages, second pass

**Status: planned, after M13.** Written on 2026-10-17 from the items
deferred by M5 and M8: Markdown inline highlighting and language
injections, and the features a syntax tree enables (M8's bracket
matching misled by strings, M11's expand selection, M13's fold regions).
Approved the same day, with the grammar list and the Go to Symbol key
decided (`DESIGN.md` §9).

## Goal

Markdown looks right, code inside other code is coloured, and the syntax
tree Nimbus already builds starts doing more than colouring: symbols,
selection by structure, folding and bracket matching that know what a
string is.

## Done when

- [ ] **Injections**: an injection layer in `SyntaxState` parses nested
      languages inside a host: Markdown inline (emphasis, code spans,
      links) inside each paragraph; fenced code blocks in Markdown in the
      fence's language when that grammar is built in; and any further
      `injections.scm` a grammar ships (JavaScript template strings,
      Python docstrings are candidates, not requirements). Nested spans
      colour with the inner language's captures over the host's.
- [ ] Injections stay within the M5 budgets: the main-thread reparse of
      host plus visible injections stays under the 2 ms budget on the
      bench Markdown file; injected trees exist only for the visible
      range plus a margin and are dropped when scrolled far away, so a
      15 MB Markdown file (the largest that gets a tree at all; past
      16 MB files are lexical) does not hold a tree per paragraph.
- [ ] **Go to Symbol** (`Primary+Shift+R`): a picker of the
      functions, types, constants and Markdown headings in the current
      file from a `tags.scm`-style query, fuzzy matched, jumping on
      Enter; built lazily on first use from the existing tree.
- [ ] **Structural selection**: M11's Expand Selection grows through
      syntax nodes (identifier, expression, statement, block, function)
      between its "bracket pair" and "line" steps when a tree exists.
- [ ] **Syntax-aware brackets**: the M8 matcher skips brackets inside
      strings and comments when a tree exists, fixing the M8 known
      limitation.
- [ ] **Syntax folds**: a `folds.scm` per built-in language gives M13's
      fold regions for functions, blocks, imports and Markdown sections,
      over the visible range plus a margin, like highlighting.
- [ ] **Markdown editing**: Enter continues a list item or quote,
      Enter on an empty item ends the list; Tab and Shift+Tab indent and
      outdent an item; soft wrap defaults on for Markdown through the new
      per-language settings (`[languages.markdown] wrap = true`, extending
      M11's `[indent.LANGUAGE]` tables into a general per-language
      table).
- [ ] **More grammars**: C, Go, HTML, CSS and TSX, each measured for
      binary size against the 35 MB budget before it stays (TSX was left
      out in M5 at about 1.5 MB; the budget has room now). Each gets the
      lexical rules table for files over 16 MB as the nine starters have.
- [ ] User queries (`queries/NAME.scm`, M5) also accept `injections.scm`,
      `folds.scm` and `tags.scm` files in the config directory, documented
      in `docs/customizing.md`.

## Scope

**In**: the injection layer, the four tree-backed features, Markdown
editing helpers, the per-language settings table, the chosen grammars.

**Deferred**: LSP (a non-goal in DESIGN.md §2; symbols here are per file
from the tree, not a language server), semantic rename, indentation
queries (`indents.scm`; M11's bracket heuristic is kept unless it proves
wrong in practice), spell checking in prose, Markdown preview.

## Design

**Injection layer** (`crates/syntax/src/inject.rs`). The host tree is
queried with the grammar's injections query over the visible range plus
a margin (two screens either side); each match gives a language name and
one or more node ranges. For each, a child `Parser` with
`set_included_ranges` parses only those byte ranges, reading the text
through the same `Text::chunk_at` callback as the host, so nothing is
copied. Children are kept in a small map keyed by the host node's
`id()`, with a version; an edit (`SyntaxState::edit`, which already sees
every edit) is applied to the children whose ranges it touches and
drops the ones whose host node vanished; children outside the kept range
are dropped on scroll. The highlight loop runs the host query, then each
visible child's query, and merges spans by depth so the inner wins. The
reparse budget (2 ms, then a worker) covers host and children together;
a child over budget is skipped this frame and coloured by the host until
it returns, the M5 rule applied one level down. Markdown's inline grammar
is `tree_sitter_md::INLINE_LANGUAGE`, already in the binary as part of
the Markdown crate, so the first injection costs no size.

**Symbols** (`crates/syntax/src/symbols.rs`). A `tags`-style query per
language (vendored into `crates/syntax/queries/` where the grammar crate
ships none) captures definitions with a kind; the picker (M4's widget
with `nucleo`) lists them with an icon from the Nerd Font set (M1 chose
the font for this). Built on a worker over a snapshot for files over the
background-search size, else in place; cached by the tree's revision.

**Structural selection, brackets and folds** each ask the tree for the
node at a byte or the nodes in a range: `named_descendant_for_byte_range`
and its parent chain for expansion, the node kind at a bracket's byte
for "is this a string or comment", and a `folds.scm` query for regions.
All three are read-only walks of the existing tree and cost microseconds
per call; none runs over the whole file.

**Per-language settings** (`config.rs`): `[languages.NAME]` with `indent`
(M11's fields), `wrap`, `auto_pairs`, `trim_trailing_whitespace`,
`final_newline`; the M11 `[indent.NAME]` form stays as a shorthand. The
same table in a user language file, so a `languages/lua.toml` can set its
own indentation.

**Grammar additions** follow M5's recipe: a cargo feature per grammar, a
lexical rules table, a highlights query from the crate or vendored with a
note on what was fixed, and a size measurement in the Results table. If
the total passes 30 MB on Linux, Bash goes first as DESIGN.md §4.2 says.

## Tasks

1. Injection layer: child trees, edits, eviction, merged highlighting;
   Markdown inline as the first injection with a screenshot test.
2. Fenced code blocks in Markdown; further injections the grammars ship.
3. Budget measurements on the bench Markdown file; the over-budget path.
4. Symbols query per language and the picker.
5. Structural expand selection (extending M11's stack).
6. Syntax-aware bracket matching.
7. `folds.scm` regions feeding M13's fold model.
8. Markdown list and quote continuation; per-language settings table;
   `customizing.md`.
9. The five grammars (C, Go, HTML, CSS, TSX), one commit each with its
   measured size; any that breaks the budget is dropped and noted.
10. Results.

## Concepts

- **Language injection**: a tree inside a tree, with `included_ranges`
  so the inner parser sees only its own bytes.
- **What a syntax tree gives you beyond colour**: a node at a byte, its
  parents, and queries as a pattern language over the tree.
- **Why not LSP here**: a tree knows structure; a server knows meaning;
  the first is free, the second is a process model and a non-goal.

## Risks

| Risk | Fallback |
|---|---|
| Child trees double the reparse cost on Markdown-heavy edits | Children parse lazily: only the paragraph being edited is reparsed on the main thread; the rest wait for a worker |
| A grammar's `injections.scm` names languages we lack | Unknown names are ignored, the host colours the range |
| Symbols queries vary wildly in quality across grammars | Ship them for Rust, Python, TypeScript, JavaScript and Markdown headings first; others as queries are vetted |
| New grammars push the Linux binary toward the 35 MB budget | Measure before merging each; TSX and Bash are the first to drop |
