# M5 — Highlighting

**Status: done**

## Goal

Syntax colours for the nine starter languages that update as you type
without ever costing a frame, with a dark and a light theme.

## Done when

Ticked items were verified here by tests, benchmarks or headless renders.
**Owner** items need a person at the keyboard; each says what to try.

- [x] Files in Rust, Markdown, JSON, TOML, YAML, Python, JavaScript,
      TypeScript and Bash are highlighted, chosen by extension or by a
      shebang on the first line. Every query compiles against its grammar
      (test); Rust, TypeScript, Python, Markdown and YAML checked in
      headless renders. Markdown is block-level only, as planned.
- [x] Typing in a 1 MB Rust file keeps keystroke-to-present under 8 ms.
      Highlighting's share, measured without a window (edit, incremental
      reparse, spans for 60 lines): p50 0.6 ms, p99 0.8 ms, max 1.7 ms;
      no keystroke ran over the 2 ms main-thread budget in 300.
- [x] **Owner, confirmed:** confirm the whole keystroke in a window:
      `./target/release/nimbus bench/out/src-1mb.rs --bench-typing 300`
      (make the file with `head -c 1048576 bench/out/src-10mb.rs`), and
      type in it by hand; nothing should feel slower than M4.
- [x] **Owner, confirmed:** open `bench/out/src-10mb.rs`: the text shows at once,
      uncoloured, and colours appear about half a second later (the
      first parse of 10 MB takes 450 ms on a worker; 5 MB takes 220 ms).
- [x] Files over 16 MB open uncoloured with a message-line note. Changed
      after the milestone: they now get simple line-by-line colours
      (comments, strings, keywords, numbers), and the note says so.
- [x] **Owner, confirmed:** startup with a Rust file is unchanged from M4
      (`--smoke`). Grammars load on the parse worker, never before the
      first frame, and only the opened language's.
- [x] Built-in dark and light themes; "Toggle Light/Dark Theme" action
      (palette and View menu); `theme` in config; user themes from
      `themes/NAME.toml` in the config directory (tests; headless renders
      with `--theme light`).
- [x] **Owner, confirmed:** try a user theme: copy `crates/app/themes/dark.toml` to
      `themes/mine.toml` next to your config, change a colour or two
      (delete the rest; they come from `base`), set `theme = "mine"`, run
      Reload Config.
- [x] Release binary under the budget: 35 MB from M5 on, raised by the
      owner from 15 MB. Linux, the largest build, is 18.97 MB (13.34 MB at
      M4: wgpu's Vulkan and GL backends, Wayland and X11); macOS 14.80 MB.
      `cargo bloat` is not installed; sizes were measured by building
      without each grammar instead.

## Scope

**In**: the `syntax` crate, language detection, lazy grammar loading,
background first parse, incremental reparse, visible-range highlighting,
themes.

**Deferred**: Markdown inline highlighting (M8 stretch), language injections
such as code blocks in Markdown or SQL in strings, highlighting-based
features like code folding.

## Design

**Crate layout** (`crates/syntax/src`):

| Module | Owns |
|---|---|
| `language.rs` | `LanguageId` enum, extension and shebang detection, and per-language data: the grammar function and the highlights query source (`include_str!`). Each grammar behind a cargo feature, all on by default. |
| `registry.rs` | Lazy `OnceLock` per language for the parsed `tree_sitter::Language` and compiled `Query`. Query compilation is the expensive part and happens on first use, on a worker thread. |
| `document.rs` | `SyntaxState { tree, language, pending_edits }`. `apply_edit` converts a `core::Edit` to `tree_sitter::InputEdit` (byte offsets and row/column points) and calls `tree.edit`. `reparse` runs the parser with the old tree. |
| `highlight.rs` | `highlight(&SyntaxState, &dyn BufferSnapshot, range) -> Vec<Span { range, capture }>`, using a `QueryCursor` limited to `range` and a text callback that reads chunks from the snapshot. |

**First parse** runs on a worker with a snapshot at version `v`. Edits made
while it runs are queued in `pending_edits`. When the tree comes back, the
queued edits are applied to it and one incremental reparse brings it to the
current version.

**Main-thread reparse budget**: tree-sitter supports a timeout; reparses on
the main thread get 2 ms. If one times out, the reparse moves to the worker
and the previous colours stay on screen until it returns.

**Themes** (`app/theme.rs`): TOML mapping capture names to colours plus UI
colours (background, gutter, selection, cursor). Lookup is longest-prefix:
`function.method.builtin` tries itself, then `function.method`, then
`function`. Resolved per capture index once when a query loads, so the
per-frame cost is an array lookup.

## Tasks

1. Check each grammar crate is compatible with tree-sitter 0.27 (they depend
   on `tree-sitter-language`); pick versions; note any that lag.
2. `language.rs` and detection tests.
3. `registry.rs` with lazy loading; confirm with startup timing.
4. `SyntaxState` with edit conversion; property test that incremental and
   full parses produce the same tree s-expression after random edits.
5. `highlight.rs` and rendering of spans.
6. Background first parse with replayed edits.
7. Reparse budget and worker fallback.
8. Themes, theme action, config key.
9. `cargo bloat --release --crates`; drop or defer a grammar if one is
   disproportionate, and record the decision.

## Concepts

- **Parse trees versus regex highlighting**, and why incremental parsing
  makes highlighting cheap per keystroke.
- **Queries and captures**: what `highlights.scm` files are.
- **Points versus byte offsets**: tree-sitter wants both.
- **Lazy statics** with `OnceLock`.

## Risks

| Risk | Fallback |
|---|---|
| A grammar crate pins an older tree-sitter | Use the grammar's git source at a compatible commit, or drop it from v1 |
| TypeScript and Markdown grammars are large | Measure; keep behind features; drop from default if over 2 MB each |
| The chunk text callback is slow on ropes | Copy the visible range plus margin into a reused `String` per frame; it is small |

## Results

| Metric | Value |
|---|---|
| Tests | 193 passing, plus 7 opt-in benchmarks or diagnostics |
| Keystroke, 1 MB Rust (highlighting part) | p50 0.6 ms, p99 0.8 ms, max 1.7 ms |
| Spans for 60 visible lines | p50 35 µs, p99 79 µs |
| First parse, Rust | 1 MB 60 ms, 5 MB 220 ms, 10 MB 450 ms (worker) |
| Query compile (first use of a language) | Rust 16 ms, TypeScript 13 ms, others under 6 ms |
| Syntax tree memory | about 28× the file: 10 MB of Rust keeps 276 MB |
| Release binary | macOS 14.80 MB (M4: 8.7 MB); Linux 18.97 MB (M4: 13.34 MB); budget raised to 35 MB |

Binary cost per grammar, from builds without each one: TypeScript 1.42 MB,
Bash 1.37 MB, Python 0.46 MB, Markdown 0.38 MB, YAML 0.18 MB; tree-sitter
itself plus Rust about 1.8 MB. JavaScript, JSON and TOML were not built
without; their unlinked archives are 430 KB, 9 KB and 31 KB.

**Binary budget raised to 35 MB** by the owner: the grammars put the Linux
build at 18.97 MB, over the old 15 MB, and dropping TypeScript, Bash and
Python would only have reached about 15.7 MB.

**Syntax tree memory: not counted.** A tree-sitter tree holds about 108
bytes per node and Rust has a node every 3.9 bytes, so a tree is about 28×
its file: 200 KB of source costs about 6 MB, a file at the 16 MB limit
about 450 MB. The owner decided highlighting memory is outside the per-file
budget, and that files over 16 MB get simple line-by-line highlighting
instead of no colours (added after the milestone; DESIGN.md §4.2).

**Design changes**

- Each grammar is a cargo feature. TSX was dropped (about 1.5 MB).
- The Rust highlights query is vendored and fixed (`crates/syntax/queries/`).
- `Text::chunk_at` lets tree-sitter read rope chunks, so parsing and queries
  never copy the document.
- `History::undo`/`redo` report each edit through a callback, so undo keeps
  the tree in step.
- `nimbus-syntax` starts no threads; `SyntaxState` hands out `Job`s that the
  app runs on short-lived threads, and headless rendering parses in place.
- Main-thread reparses get 2 ms; over that, the parse restarts on a worker
  and the edited old tree keeps colouring the view.
- Long lines highlight only their visible bytes, not the whole line.
- Highlighting comes back if a file shrinks back under 16 MB (an undone
  paste).
- Colours are themed: every interface colour moved from constants in
  `view.rs` into the theme. The dark theme matches the old constants to
  8 bits.
- `--theme NAME` overrides the config, for headless checks.
- `scripts/clippy-cross.ps1` runs the Linux and Windows clippy checks from
  macOS: grammars are C, and macOS has no C toolchain for those targets.

**Concepts**

- *Parse tree versus regex highlighting*: tree-sitter builds a real syntax
  tree, so a string or comment spanning lines is coloured correctly, and an
  edit reparses only what changed (`state.rs`, `refresh`).
- *Queries and captures*: `highlights.scm` files are patterns over the tree
  that name parts of it (`@keyword`); the theme maps names to colours
  (`highlight.rs`, `theme.rs`).
- *Points versus byte offsets*: tree-sitter wants both for each edit;
  `edit.rs` converts ours.
- *Lazy statics*: each grammar is a `OnceLock`, filled on first use
  (`registry.rs`).

**Surprises**

- Upstream's Rust query never coloured constants (a stray quote).
- An unterminated `/*` is lexed in one step, so the parse-time budget
  cannot interrupt it; a big paste can.
- For text with syntax errors, an incremental parse may recover differently
  from a fresh parse. The oracle compares against a reference incremental
  parse instead, and against a fresh parse only for error-free text.
- Comparing reparsed trees cannot catch a wrong edit position (reparsing
  heals it), so the oracle also checks the edited tree's positions before
  reparsing.
- Tree-sitter allocates with C `malloc`, so a Rust counting allocator sees
  none of its memory; the measurement reads the process size instead.

**End-of-milestone review** (code-review skill, high) found nine issues,
all fixed: highlight ranges growing with line length; copying a whole
first line to check for a shebang; the over-16 MB note drawn before the
layout had room for it; highlighting never returning after a file shrank;
the theme toggle ignoring a user theme's base; the syntax crate failing
lints with grammar features off; a failed thread spawn leaving a document
waiting forever (it now parses in place); an unused grammar accessor; an
`allow` where the rules require `expect`.

**Known limitations**

- Many long lines on screen at once still highlight the bytes between
  their visible parts (one range per frame).
- No language injections (code blocks in Markdown), and no Markdown inline
  highlighting (M8 stretch).
- Composed IME text on the cursor's row is drawn uncoloured.
