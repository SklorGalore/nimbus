# Nimbus — Design

A fast, lightweight, modeless text editor with its own GPU rendering pipeline.
Written in Rust. Personal tool first; clean enough to share later.

Decisions below come from the kickoff interview on 2026-10-15. Items marked
**open** need a decision before the milestone that touches them.

## 1. Goals

| Goal | Target | How we measure |
|---|---|---|
| Instant startup | Editable window in < 100 ms (cold, mid-size file) | `hyperfine` on launch-to-first-frame, logged at startup |
| Typing latency | Keystroke to pixels in < 1 frame (≤ 8 ms at 120 Hz) | Frame timer in debug overlay |
| Huge files | Open a 4 GB log, scroll and search it, under 1 s to interactive | Bench file in `bench/` |
| Low footprint | < 30 MB resident with one file open; single binary < 15 MB | `ps`, `ls -l target/release` |
| Modeless editing | Type to insert; Cmd/Ctrl shortcuts; mouse works | — |

Content we care about, in order: source code, prose and notes (Markdown),
config and data files (JSON, YAML, TOML, CSV, logs).

## 2. Non-goals (for now)

- Vim keybindings or any modal layer.
- Plugins or a scripting language.
- LSP, debugging, terminals, git integration.
- Collaborative editing.
- Rich text, proportional fonts in the editor area (UI chrome may use them later).

Several of these are easy to add later if the core is clean. None are needed
for the editor to be useful.

## 3. Platform

Targets macOS, Linux, Windows from day one, all through `winit` + `wgpu`.
No platform-specific windowing code. Native menu bar via `muda` (works on all
three). Primary development and testing happens on macOS; other platforms are
checked by CI builds and occasional manual runs.

Consequence: the modifier key is "Cmd on macOS, Ctrl elsewhere". We call it
`Primary` in keybindings and resolve it per platform.

## 4. Architecture

Cargo workspace, four crates. The split keeps the editor logic testable with
no GPU, and keeps compile times down when iterating on one layer.

```
nimbus/
  Cargo.toml            workspace
  crates/
    core/               text buffer, edits, undo, cursors, search   (no I/O, no GPU)
    syntax/             tree-sitter integration, highlight spans
    render/             wgpu pipeline, glyph atlas, text layout, rect/quad batching
    app/                winit loop, editor state, tabs, palette, file tree, keymap, config
  bench/                large test files, generated on demand
  DESIGN.md
```

Dependency direction is strictly downward: `app → render, syntax, core`;
`syntax → core`; `render → core` (for text slices only). `core` depends on
nothing in the workspace.

### 4.1 core: the text buffer

**Decision needed (open).** Two interview answers conflict:

- *Buffer:* rope via `ropey`. Ropey holds the entire text in memory.
- *Huge files:* memory-map and index lazily, never loading the whole file.

Recommended resolution: define a small `Buffer` trait in `core` and ship two
backends, in this order.

1. **`RopeBuffer`** (milestone 2). Wraps `ropey::Rope`. Used for every file
   under a size threshold (initially 256 MB). Gets us a correct, fast editor
   quickly.
2. **`MappedBuffer`** (milestone 7). A piece table whose "original" piece is
   an `mmap` of the file and whose "add" piece is an append-only `Vec<u8>`.
   Line starts are indexed in the background in 64 MB chunks; the UI shows
   what is indexed so far and the scrollbar grows as indexing proceeds.
   Edits are allowed; saving writes a new file and renames over the old one.

The trait is intentionally narrow so both backends stay honest:

```rust
pub trait Buffer {
    fn len_bytes(&self) -> usize;
    fn len_lines(&self) -> usize;            // may be a lower bound while indexing
    fn line(&self, idx: usize) -> Cow<str>;  // without trailing newline
    fn byte_to_line(&self, b: usize) -> usize;
    fn line_to_byte(&self, l: usize) -> usize;
    fn slice(&self, range: Range<usize>) -> Cow<str>;
    fn replace(&mut self, range: Range<usize>, text: &str) -> Edit;
    fn is_fully_indexed(&self) -> bool;
}
```

Everything above the buffer (cursors, undo, search, rendering) works in byte
offsets and asks the buffer for line conversions. Nothing else may assume the
whole text is resident.

Other `core` responsibilities:

- **Positions** are byte offsets into UTF-8. Char and grapheme boundaries are
  resolved via `unicode-segmentation` when moving the cursor. Column display
  is computed at render time, not stored.
- **Cursors and selections**: a `Selection` is `{anchor, head}`. The editor
  holds a `Vec<Selection>`, so multi-cursor is possible later without a
  redesign, but v1 uses exactly one.
- **Undo**: a linear history of `Edit` records (range, removed text, inserted
  text, cursor before/after). Adjacent single-char insertions coalesce into
  one record. Branching undo trees are a non-goal.
- **Search**: literal and regex (`regex` crate) over `Buffer::slice` in
  chunks, so it works on mapped files. Results are byte ranges.
- **Line endings**: detected on open (LF / CRLF / mixed → LF), preserved on
  save. Tabs are kept as tabs; display width is a setting.

### 4.2 syntax: highlighting

`tree-sitter` with grammars compiled into the binary as crates
(`tree-sitter-rust`, `-markdown`, `-json`, `-toml`, `-yaml`, `-python`,
`-javascript`, `-typescript`, `-bash`). Highlighting through
`tree-sitter-highlight` queries, producing spans of `(byte_range, scope)`.

Rules that protect the performance goals:

- Grammars are initialized lazily on first use of that language, on a
  background thread. Startup never pays for a parser.
- Parsing is incremental: every `Edit` is fed to `Parser::parse` with the old
  tree, so a keystroke costs a few microseconds of reparse, not a full parse.
- Highlighting is computed only for the visible line range plus a margin.
- Above a per-file size threshold (initially 16 MB) or for `MappedBuffer`,
  highlighting is off. Big logs are plain text; that is fine.

The `app` crate maps scopes to colors via the theme.

### 4.3 render: GPU text

Our own pipeline on `wgpu`. Three pieces:

1. **Font loading and shaping**: `swash` for both. We discover fonts with
   `fontdb` (system fonts) and ship one bundled monospace fallback so the
   editor renders identically everywhere. Shaping is per line; results are
   cached keyed on `(line content hash, font id, size)`. Monospace is assumed
   for the editor area, which makes column math trivial and lets us skip
   bidi for now (**open**: bidi support is a non-goal until someone needs it).
2. **Glyph atlas**: a single `R8Unorm` texture (2048², grows by allocating a
   second page). Shelf packing. Glyphs are rasterized by `swash::scale` with
   grayscale antialiasing at the exact pixel size and device scale factor;
   subpixel positioning is quantized to 1/4 pixel horizontally. The atlas is
   invalidated and rebuilt on DPI or font-size change.
3. **Draw**: two instanced pipelines, one for solid rectangles (backgrounds,
   cursor, selection, gutter, UI chrome) and one for textured glyph quads.
   One vertex buffer each, rebuilt per frame from a `Vec<Instance>`. A whole
   frame of a 4K window is two draw calls.

Frames are drawn **on demand**, not in a loop. The app requests a redraw on
input, scroll, resize, cursor blink tick, and when a background task
(indexing, highlighting) posts an update. Idle CPU is zero.

Layout is simple because everything is a monospace grid: a `View` has a
scroll offset in lines plus a pixel fraction, a visible line range, and
produces `Vec<Instance>` from `Buffer::line` + highlight spans. Soft wrap for
prose is done in layout: a logical line maps to N visual rows, computed for
visible lines only and cached.

### 4.4 app: the editor

- **Event loop**: `winit`. All editor state lives in one `Editor` struct on
  the main thread. Background threads (file loading, indexing, parsing, file
  walking) send messages through a channel; the loop drains it and requests
  a redraw.
- **Actions**: every user-visible operation is an `Action` enum variant with
  a name and a description. Keymap maps chords to actions. The command
  palette (`Primary+Shift+P`) lists them with fuzzy search. This is the only
  way to add a feature: define the action, bind it, register it. Nothing is
  reachable only by a hard-coded key.
- **Tabs**: a `Vec<Document>` plus the active index. `Primary+1..9` switch,
  `Primary+W` closes, `Primary+Tab` cycles. Dirty state in the tab title.
- **Fuzzy file opener** (`Primary+P`): the project root is the folder passed
  on the command line, else the parent of the first opened file, else cwd.
  Walked with `ignore` (respects `.gitignore`) on a background thread;
  matched with `nucleo`.
- **File tree sidebar** (`Primary+B` toggles): lazy directory listing,
  expanded on click, same walker as above. Rendered with the same rect and
  glyph pipelines; it is just rows of text.
- **Find / replace** (`Primary+F`, `Primary+H`): inline bar at the top of
  the view, incremental, match count, regex toggle.
- **Clipboard**: `arboard`. **File dialogs**: `rfd`.
- **Config**: one TOML file at the platform config dir
  (`~/Library/Application Support/nimbus/config.toml` on macOS). Font, size,
  tab width, theme, soft-wrap default, keybinding overrides. Missing file
  means defaults; a broken file is reported in the status bar, never fatal.
- **Themes**: a built-in dark and light theme as TOML; user themes are the
  same format in the config dir.

## 5. Performance budget enforcement

- A debug overlay (`F12`) shows frame time, last-keystroke latency, atlas
  occupancy, buffer backend, and background task status.
- `bench/gen.sh` generates 100 MB, 1 GB, and 4 GB files. `cargo bench` has
  criterion benches for buffer ops and line layout.
- CI fails if the release binary exceeds 15 MB.
- Release profile: `opt-level = 3`, `lto = "fat"`, `codegen-units = 1`,
  `panic = "abort"`, `strip = true`.

## 6. Testing

- `core` and `syntax` are pure and get ordinary unit tests plus property
  tests (`proptest`) that random edit sequences on `RopeBuffer` and
  `MappedBuffer` produce identical text to a `String` oracle.
- `render` layout code is tested headlessly: given a buffer and view, assert
  the produced instances. Shader and atlas code is tested by a single
  screenshot smoke test on macOS.
- `app` gets integration tests that drive `Action`s directly without a
  window.

## 7. Dependencies (initial)

| Crate | Role |
|---|---|
| `winit`, `wgpu`, `pollster` | window, GPU, blocking on adapter setup |
| `swash`, `fontdb` | shaping, rasterization, font discovery |
| `ropey`, `memmap2` | buffer backends |
| `unicode-segmentation`, `unicode-width` | grapheme and width rules |
| `regex` | search |
| `tree-sitter`, `tree-sitter-highlight`, grammar crates | syntax |
| `muda` | native menu bar |
| `arboard`, `rfd` | clipboard, dialogs |
| `ignore`, `nucleo` | project walking, fuzzy matching |
| `serde`, `toml`, `dirs` | config |
| `crossbeam-channel` | background → main thread messages |
| `log`, `env_logger` | logging |

Each dependency is pinned in `Cargo.lock`; we review the tree for size with
`cargo bloat` at milestone 4 and 8.

## 8. Milestones

Each milestone ends with something you can run. Since most of the stack is
new to you, each milestone's PR description explains the concepts it
introduces (what a rope is, what shaping is, how instanced drawing works)
and points at the specific code.

| # | Name | You can… | Key concepts explained |
|---|---|---|---|
| 0 | Scaffold | Run `cargo run` and see an empty window that resizes and closes cleanly on all three OSes. CI builds all three. | workspace layout, winit event loop, wgpu surface |
| 1 | Text on screen | See a hard-coded paragraph in a monospace font at correct DPI. Resize keeps it crisp. | swash shaping, glyph atlas, instanced quads, scale factor |
| 2 | Editable | `nimbus file.txt` opens a file; move with arrows and mouse, type, delete, scroll, see line numbers, `Primary+S` saves. | ropey, byte offsets vs graphemes, `Buffer` trait, on-demand redraw |
| 3 | Editing essentials | Shift-select, cut/copy/paste, undo/redo, find/replace bar, soft wrap toggle, auto-indent on Enter. | undo records and coalescing, regex search in chunks, layout rows vs lines |
| 4 | Many files | Tabs, `Primary+P` fuzzy opener, command palette, keymap and config file, native menus. | action registry, background walker and channels, nucleo |
| 5 | Highlighting | Syntax colors for the nine starter languages, updating as you type. Dark and light themes. | tree-sitter incremental parsing, highlight queries, scopes → theme |
| 6 | File tree | Sidebar with lazy folders, open on click, follows the active file. | — |
| 7 | Huge files | Open a 4 GB log in under a second, scroll it, search it, edit a line and save. | mmap, piece table, background indexing, atomic save |
| 8 | Polish | Bracket matching, multi-cursor (`Primary+D`), status bar, debug overlay, `cargo bloat` pass, Linux/Windows manual QA, a release build. | — |

Milestones 0–2 are the risky ones: they are where wgpu and swash either work
or fight us. If milestone 1 takes much longer than expected, the fallback is
to swap in `glyphon` for the atlas and shader while keeping our own layout,
which is a contained change inside `render`.

## 9. Open questions

1. Confirm the two-backend buffer resolution in §4.1, or choose one.
2. Bidi / RTL text: non-goal for v1? (§4.3)
3. Starter language list for highlighting: the nine in §4.2, or a different
   set?
4. Project name is `nimbus`. Binary name `nimbus`? Any conflict with a tool
   you already have on PATH?
