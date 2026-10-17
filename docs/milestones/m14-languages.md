# M14 — Languages, second pass

**Status: in progress.** Started on 2026-10-17 alongside M10, M11 and
M15, for the parts that do not depend on them: the injection layer, Go to
Symbol, the tree-side of structural selection and folds, syntax-aware
brackets, the five grammars and the user query kinds. Markdown editing
and the per-language settings table wait for M11's `[indent.NAME]`
tables; wiring `structure::expand` into Expand Selection and
`structure::fold_regions` into the fold model are one task each in M11
and M13. Written on 2026-10-17 from the items
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

- [x] **Injections**: an injection layer in `SyntaxState` parses nested
      languages inside a host: Markdown inline (emphasis, code spans,
      links) inside each paragraph; fenced code blocks in Markdown in the
      fence's language when that grammar is built in; and any further
      `injections.scm` a grammar ships (JavaScript template strings,
      Python docstrings are candidates, not requirements). Nested spans
      colour with the inner language's captures over the host's.
- [x] Injections stay within the M5 budgets: the main-thread reparse of
      host plus visible injections stays under the 2 ms budget on the
      bench Markdown file; injected trees exist only for the visible
      range plus a margin and are dropped when scrolled far away, so a
      15 MB Markdown file (the largest that gets a tree at all; past
      16 MB files are lexical) does not hold a tree per paragraph.
- [x] **Go to Symbol** (`Primary+Shift+R`): a picker of the
      functions, types, constants and Markdown headings in the current
      file from a `tags.scm`-style query, fuzzy matched, jumping on
      Enter; built lazily on first use from the existing tree.
- [ ] **Structural selection**: M11's Expand Selection grows through
      syntax nodes (identifier, expression, statement, block, function)
      between its "bracket pair" and "line" steps when a tree exists.
      The tree side (`structure::expand`) is done; the step in the
      selection stack lands with M11's Expand Selection.
- [x] **Syntax-aware brackets**: the M8 matcher skips brackets inside
      strings and comments when a tree exists, fixing the M8 known
      limitation.
- [ ] **Syntax folds**: a `folds.scm` per built-in language gives M13's
      fold regions for functions, blocks, imports and Markdown sections,
      over the visible range plus a margin, like highlighting. The
      queries and `structure::fold_regions` are done; M13's fold model
      consumes them.
- [ ] **Markdown editing** (waits for M11's per-language tables): Enter continues a list item or quote,
      Enter on an empty item ends the list; Tab and Shift+Tab indent and
      outdent an item; soft wrap defaults on for Markdown through the new
      per-language settings (`[languages.markdown] wrap = true`, extending
      M11's `[indent.LANGUAGE]` tables into a general per-language
      table).
- [x] **More grammars**: C, Go, HTML, CSS and TSX, each measured for
      binary size against the 35 MB budget before it stays (TSX was left
      out in M5 at about 1.5 MB; the budget has room now). Each gets the
      lexical rules table for files over 16 MB as the nine starters have.
- [x] User queries (`queries/NAME.scm`, M5) also accept `injections.scm`,
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

## Results

_In progress; Markdown editing and the per-language settings table wait
for M11, and the Expand Selection and fold-model hookups for M11 and M13._

Measured on 2026-10-17 in a Linux container with a shared, noisy CPU and
no GPU (Mesa's software Vulkan for the rendering checks), so the owner's
machine is faster and steadier. `--smoke` needs a window and could not be
run here; the startup path gained nothing eager (grammars, queries,
symbols and injections all load on first use), so the number to record
is the owner's next `--smoke` on main.

**Binary size** (Linux release, stripped; the same measurement for each
grammar: a build with only that feature added to the M13 base). All five
stayed.

| Grammar | Adds | Note |
|---|---|---|
| TSX | 1.39 MB | a second compiled grammar in the TypeScript crate; its queries are JavaScript's plus JSX plus TypeScript's |
| C | 0.60 MB | |
| Go | 0.22 MB | |
| CSS | 0.12 MB | |
| HTML | 0.03 MB | the grammar is tiny; its scanner does the work |
| all five | 2.33 MB | 19.1 MB → 21.4 MB, of the 35 MB budget |

**Injection cost** (`cargo test --release -p nimbus-syntax --test
injection_cost -- --ignored --nocapture`; a generated Markdown file of
headings, paragraphs with emphasis, code spans and links, lists and Rust
fences; 60 lines on screen):

| | 1 MB | 15 MB |
|---|---|---|
| first parse of the host (worker) | 0.7 s | 10.5 s |
| jump to the middle, until every child is parsed | 91 ms over 12 frames, 148 child trees | 236 ms over 76 frames, 151 child trees |
| scroll one screen, until settled (p50 / p99) | 5.4 / 10.8 ms | 13.7 / 23.6 ms |
| keystroke in a paragraph (edit, reparse, highlight; p50 / p99) | 2.0 / 3.4 ms | 1.7 / 2.1 ms |
| a frame with nothing changed | 0.4 µs | |

Child trees exist only for the view plus one screen each side (about 150
for this file at any size), so the 15 MB file holds no tree per
paragraph. A frame parses children within the 2 ms budget and at least
one per frame past it, which is why a jump settles over several frames
rather than in one 27 ms frame; the host's colours show meanwhile. One
host reparse in 200 keystrokes went over 2 ms and took the worker path.

**Surprises.**

- *Querying a wide tree costs the whole tree.* tree-sitter's cursor walks
  every child of each node it enters even with a byte range set, and a
  Markdown document is one node with thousands of blocks under it, so
  the first version spent milliseconds per frame on a 1 MB file just
  finding paragraphs. `split.rs` descends to the nodes that cover the
  range and queries a wide node (over 64 children) child by child. The
  same cost had been hiding in M5's highlighting of large Markdown files.
- *The inline grammar is slow to parse.* About 0.3 ms per paragraph,
  against about 5 µs for a paragraph's worth of Rust: its external
  scanner re-examines the text for every delimiter pair. That set the
  margin (one screen each side rather than two) and the rule that
  children parse lazily over frames rather than all at once.
- *Re-querying children every frame cost more than parsing them.* Each
  child was asked for its own nested regions on every frame (about 16 µs
  each, times 150). A child is now queried once per parse and again only
  while something below it is unfinished, and the highlighter returns
  its cached spans when the tree, ranges and injections are unchanged.
- *A cursor's included ranges stick.* A `Parser` keeps its
  `set_included_ranges` until reset, so an early measurement of "5 µs
  per inline parse" was parsing nothing. Every child parse sets its
  ranges explicitly.
- *Rust's injection query is a trap.* It marks every macro invocation's
  token tree for a second Rust parse; on real code that is most lines
  with a `println!` or `vec!`, for colours the host already gets right.
  It is not used.
- *Grammar queries disagree on names.* The TypeScript grammar's fold
  query cannot mention `jsx_element` (only TSX has it), so JSX folds are
  a separate file combined per language; Go's tags query lacks
  constants, so one pattern is added.

**Changes to DESIGN.md**: §4.2 gained the injection layer, capture
tables, wide-tree splitting and the grammar sizes; the drop order is now
TSX, Bash, TypeScript; three decision-log lines.
