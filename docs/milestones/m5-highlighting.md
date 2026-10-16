# M5 — Highlighting

**Status: next**

## Goal

Syntax colours for the nine starter languages that update as you type
without ever costing a frame, with a dark and a light theme.

## Done when

- [ ] Files in Rust, Markdown, JSON, TOML, YAML, Python, JavaScript,
      TypeScript and Bash are highlighted, chosen by extension or by a
      shebang on the first line.
- [ ] Typing in a 1 MB Rust file keeps keystroke-to-present under 8 ms.
- [ ] Opening a 5 MB file shows plain text immediately and colours appear
      when the background parse finishes.
- [ ] Files over 16 MB open uncoloured with a message-line note.
- [ ] Opening a file in one language does not load the other eight grammars;
      startup time is unchanged from M4.
- [ ] Built-in dark and light themes; "Toggle theme" action; theme name in
      config; user themes from the config directory.
- [ ] `cargo bloat` report recorded; release binary under 15 MB.

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

_Fill in when done._
