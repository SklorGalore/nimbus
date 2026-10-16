# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

Nimbus is a fast, lightweight, modeless text editor in Rust with its own GPU text pipeline (winit + wgpu, glyphs rasterized with swash into our own atlas). `DESIGN.md` is the source of truth for architecture, performance budgets, and the milestone plan; check its "Open questions" list before starting a milestone. Each milestone has a plan in `docs/milestones/` with a done-when checklist and ordered tasks; work through the tasks in order, tick the checklist, and fill in the plan's Results section (startup time, binary size, surprises) when the milestone lands. Standing requirements for every milestone are in `docs/milestones/README.md`.

The owner is new to Rust, wgpu, and text rendering. When landing a milestone, explain each new concept in plain language and point at the code that embodies it. Prefer clear over clever.

## Commands

```sh
cargo build                              # debug build (deps are built at opt-level 2, see Cargo.toml)
cargo run -- --smoke                     # open window, exit after first frame, print first-frame-ms=
cargo build --release && ./target/release/nimbus --smoke   # the startup number that matters
RUST_LOG=nimbus=debug ./target/release/nimbus --smoke       # same, with per-phase startup times
cargo run -- --screenshot /tmp/frame.bmp                    # save the first frame; convert with `sips -s format png`
NIMBUS_SCALE=1 cargo run -- --screenshot /tmp/1x.bmp         # debug builds: render at another scale factor
./target/release/nimbus FILE --bench-typing 300            # type 300 chars through the edit path; prints latency percentiles
bench/gen.sh                                                # 10 MB / 100 MB test files in bench/out (also: logs, longline)
cargo run -- FILE --headless 1600x1000 --screenshot /tmp/f.bmp   # render with no window (works with the screen locked)
cargo run -- FILE --wrap --find PATTERN                     # start with soft wrap on and the find bar searching
cargo run -- FILE --theme light --headless 1600x1000 --screenshot /tmp/l.bmp   # override the config's theme
cargo run -- FOLDER a.rs b.rs                              # several tabs; a folder sets the file opener's root
cargo run -- . --headless 1600x900 --picker files:main --screenshot /tmp/p.bmp   # render a picker (palette|files[:query])
cargo test --release -p nimbus-core -- --ignored --nocapture # opt-in benchmarks (e.g. wrapping a 1 MB line)
cargo test --release -p nimbus-syntax -- --ignored --nocapture --test-threads 1   # keystroke and first-parse cost, tree memory (needs bench/gen.sh)
cargo test --workspace
cargo test -p nimbus-core <test_name>    # single test in one crate
cargo fmt --all --check
cargo clippy --workspace --all-targets   # strict lints from Cargo.toml; CI fails on any warning
```

On-screen `--smoke` and `--screenshot` need an unlocked screen with the window visible; they give up after 10 s otherwise. Prefer `--headless` for unattended rendering checks. Don't launch windowed test runs while the owner may be using the machine without saying so: windows appear on their screen and their input skews measurements.

Logging uses `env_logger`, defaulting to `nimbus=info`; override with `RUST_LOG`. wgpu reads `WGPU_BACKEND` and related env vars because the instance is built with `new_without_display_handle_from_env`.

## Code quality and linting

The strict lints live in `[workspace.lints]` in the root `Cargo.toml`, and every crate opts in with `[lints] workspace = true`. A new crate must add those two lines too. Plain `cargo clippy --workspace --all-targets` applies them, and CI runs it with `-D warnings`, so every lint warning fails CI. Reproduce CI locally with:

```sh
RUSTFLAGS="-D warnings" cargo clippy --workspace --all-targets
# Platform-specific code (menus, AltGr, dead code on one OS) only shows up
# per target; check the other two before pushing (targets installed via rustup).
# The script stands in for the C compiler the tree-sitter build scripts need,
# which macOS lacks for these targets; clippy never links, so that is enough:
scripts/clippy-cross.sh x86_64-unknown-linux-gnu
scripts/clippy-cross.sh x86_64-pc-windows-msvc
```

The set is `clippy::pedantic` plus `unwrap_used`, `expect_used`, `panic`, `todo`, `unimplemented`, `dbg_macro`, `print_stdout`, `print_stderr`, `undocumented_unsafe_blocks`, `missing_docs`, and `unsafe_code = "deny"`. Don't weaken the table to get a change through.

Rules behind those lints:
- **No panics in normal paths.** No `unwrap`, `expect`, `panic!`, `todo!`, or `unimplemented!` outside tests. Return errors, or log and degrade, as `app` does for GPU and window failures. Release builds use `panic = "abort"`, so a panic loses the user's unsaved work.
- **Silencing a lint needs a reason.** Use `#[expect(clippy::lint_name, reason = "...")]` on the narrowest item, never a crate-wide allow. `expect` fails once the lint stops firing, so stale suppressions get cleaned up. The `print_out` and `print_err` helpers in `crates/app/src/main.rs` are the model: all intended terminal output goes through them.
- **Document every public item.** Public functions that return `Result` need a `# Errors` section, and ones that can panic need `# Panics`. Doc comments should explain *why* and the concept involved, because the owner is learning the stack from them.
- **`unsafe` is a last resort.** It is expected only in `render` and the mmap buffer. Each block needs a `// SAFETY:` comment stating the invariant that makes it sound, and the module needs `#[expect(unsafe_code, reason = "...")]` to lift the workspace deny.
- **Errors are typed per crate.** Library crates (`core`, `syntax`, `render`) expose their own error enums that implement `Display` and `Error`, like `GpuError`. No `anyhow` or `Box<dyn Error>` in library APIs. `app` may flatten errors for logging.
- **No `dbg!` or stray `println!`.** Use `log::debug!` or `log::trace!` instead; they cost nothing when filtered out.
- **Hot paths do not allocate per frame or per keystroke.** Reuse buffers such as instance `Vec`s with `clear()` rather than building new ones. Prefer borrowing (`&str`, `Cow`) over cloning `String`s out of the buffer.
- **Tests live with the code.** Use `#[cfg(test)] mod tests` in the same file. The exception is property-test suites that exercise a crate's public interface across modules, such as `crates/core/tests/buffer_oracle.rs`; those live in the crate's `tests/` directory. Every buffer backend must pass that oracle suite.
- **Dependencies need a reason.** A new crate must fit the binary-size budget and be added with `cargo add` to the crate that uses it. Say why in the commit message.

## Subagents

Do most work inline. Spawn a subagent only for the cases in the table below; each one starts cold, so a subagent for a small or already-understood task costs more than it saves. Pass the `effort` listed. Raise it one level if the task touches `unsafe`, saving, or concurrency, even when it looks routine.

| Situation | Agent | Effort | Notes |
|---|---|---|---|
| Confirm a dependency's API before using it (wgpu, winit, swash, ropey, tree-sitter, muda, nucleo) | `Explore`, model `haiku` | `low` | Ask for exact signatures and enum variants from `~/.cargo/registry/src/*/<crate>-<version>/`, not prose. Keeps crate source out of the main context. This repo has already hit API drift once. |
| Locate code across several crates ("where is the cursor clamped", "every caller of `Buffer::line`") | `Explore` | `low`, or `medium` if the answer spans more than three files | Say "medium" or "very thorough" for breadth. |
| Turn a milestone task with real design uncertainty into an implementation plan: atlas packing and upload (M1), movement and IME (M2), anchored scrolling with soft wrap (M3), first parse plus replayed edits (M5), piece table and sparse index (M7), multi-cursor edit application (M8) | `Plan` | `high` | Give it the milestone doc and the `DESIGN.md` sections. Review the plan before writing code; don't forward it unread. |
| Implement two or more independent, well-specified tasks from the same milestone at once (for example the M1 atlas packer and font metrics, or the M2 property tests and `bench/gen.sh`) | `general-purpose` with `isolation: "worktree"` | `medium` | Only when the tasks touch disjoint files. Never for tasks that edit the same module. |
| Property tests and oracles for buffers, undo, search chunking or the line index | `general-purpose` | `high` | Tests that compare against a `String` oracle are where data-loss bugs get caught. |
| Anything in `unsafe` code, atomic save, the mmap backend, or cross-thread version checks | `general-purpose` | `xhigh` | Ask it to state the invariant each piece of code relies on. |
| End-of-milestone review | `/code-review` skill | `high`; `xhigh` for M2, M7 and M8 | M2 adds save, M7 adds `unsafe` and mmap, M8 adds multi-cursor edits. |
| A rendering or input bug that survived two focused attempts inline | `general-purpose` | `max` | Include what was tried and what was observed. |
| Questions about Claude Code itself | `claude-code-guide` | default | Continue an existing one with `SendMessage` rather than starting another. |

Every subagent prompt must stand on its own. Include:
- the milestone doc path and the `DESIGN.md` section numbers that apply;
- the invariants from Architecture below that the task could break;
- the lint rules, or the instruction to make `RUSTFLAGS="-D warnings" cargo clippy --workspace --all-targets` pass;
- the wgpu 30 warning below when GPU code is involved;
- for implementation agents, which files they may change.

Subagents do not commit, push, or edit `DESIGN.md` or milestone docs. After a subagent returns, verify its work in the main session before relying on it: read the diff, then run fmt, strict clippy and the tests. Merge worktree results one at a time and rerun the checks after each.

## Architecture

Cargo workspace; crates live in `crates/`, each package named `nimbus-<dir>` except the binary, which is `nimbus` in `crates/app`.

- `core`: text buffers, edits, undo, cursors, search. No I/O, no GPU, no workspace deps. Must stay testable headlessly.
- `syntax`: tree-sitter highlighting. Depends on `core`. `SyntaxState` (one per `Document`) must see every edit, undo and redo included, through `SyntaxState::edit`; a missed edit leaves the tree describing other text. It starts no threads: it returns `Job`s that the app runs (`spawn_syntax` in `app.rs`). Each grammar is a cargo feature; the Rust highlights query is vendored in `crates/syntax/queries/`.
- `render`: owns the wgpu surface and, later, the glyph atlas, layout, and the two instanced pipelines (rects and glyph quads). Depends on `core`.
- `app`: winit `ApplicationHandler`, editor state, UI chrome, keymap, config. Depends on everything.

Dependencies only point downward. Don't add a dependency from `core` onto any other workspace crate, and don't put GPU or windowing types in `core`.

Invariants from DESIGN.md that code above the buffer must respect:
- Positions are byte offsets into UTF-8. Never assume the whole file is resident; go through the `Buffer` trait (milestone 2), because the mmap-backed piece table (milestone 7) only indexes lazily.
- Rendering is on demand. The event loop uses `ControlFlow::Wait`; call `request_redraw` on input or background updates. Never introduce a continuous render loop.
- Every user-visible operation is an `Action` in `crates/app/src/action.rs` with a stable id, title and default keys. Add new operations there, never as a hard-coded key in `app.rs`; the palette, menus and config overrides all derive from the registry.

## Performance budgets (enforced or measured)

- First frame under 500 ms in release, measured from the top of `main` by `--smoke`. M1 measured 116–132 ms warm on an M5 Pro, of which about 3 ms is ours and the rest is winit and AppKit. The headroom is for real features (opening files, config, highlighting), not for unnecessary work: keep startup work lazy or off the main thread. Check per-phase times with `RUST_LOG=nimbus=debug`.
- Release binary under 35 MB (raised from 15 MB at M5 for the grammars; Linux is the largest build); the `release-size` CI job fails above that.
- The release profile (`lto = "fat"`, `panic = "abort"`, `strip`) is deliberate; don't loosen it.

## wgpu version gotcha

The project uses wgpu 30, whose API differs from older examples and from model training data: `get_current_texture()` returns a `CurrentSurfaceTexture` enum rather than a `Result`, frames are presented with `queue.present(frame)`, `InstanceDescriptor` has no `Default`, and `RenderPassDescriptor` requires `multiview_mask`. Check the crate source under `~/.cargo/registry/src/*/wgpu-30*` rather than guessing. Expect similar drift in swash and winit (pinned to 0.30; 0.31 is still beta).

## CI

`.github/workflows/ci.yml` runs fmt, clippy, and tests on macOS, Ubuntu, and Windows, plus the release-size check. Linux needs `libwayland-dev` and `libxkbcommon-dev`. Remote: `github.com/SklorGalore/nimbus` (private).
