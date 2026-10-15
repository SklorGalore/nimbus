# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

Nimbus is a fast, lightweight, modeless text editor in Rust with its own GPU text pipeline (winit + wgpu, glyphs rasterized with swash into our own atlas). `DESIGN.md` is the source of truth for architecture, performance budgets, and the milestone plan; read the relevant section before starting a milestone, and check its "Open questions" list. Work proceeds one milestone at a time, each ending in something runnable.

The owner is new to Rust, wgpu, and text rendering. When landing a milestone, explain each new concept in plain language and point at the code that embodies it. Prefer clear over clever.

## Commands

```sh
cargo build                              # debug build (deps are built at opt-level 2, see Cargo.toml)
cargo run -- --smoke                     # open window, exit after first frame, print first-frame-ms=
cargo build --release && ./target/release/nimbus --smoke   # the startup number that matters
cargo test --workspace
cargo test -p nimbus-core <test_name>    # single test in one crate
cargo fmt --all --check
cargo clippy --workspace --all-targets   # strict lints from Cargo.toml; CI fails on any warning
```

Logging uses `env_logger`, defaulting to `nimbus=info`; override with `RUST_LOG`. wgpu reads `WGPU_BACKEND` and related env vars because the instance is built with `new_without_display_handle_from_env`.

## Code quality and linting

The strict lints live in `[workspace.lints]` in the root `Cargo.toml`, and every crate opts in with `[lints] workspace = true`. A new crate must add those two lines too. Plain `cargo clippy --workspace --all-targets` applies them, and CI runs it with `-D warnings`, so every lint warning fails CI. Reproduce CI locally with:

```sh
RUSTFLAGS="-D warnings" cargo clippy --workspace --all-targets
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
- **Tests live with the code.** Use `#[cfg(test)] mod tests` in the same file. Buffer logic needs property tests against a `String` oracle once `proptest` is added in milestone 2.
- **Dependencies need a reason.** A new crate must fit the binary-size budget and be added with `cargo add` to the crate that uses it. Say why in the commit message.

## Architecture

Cargo workspace; crates live in `crates/`, each package named `nimbus-<dir>` except the binary, which is `nimbus` in `crates/app`.

- `core`: text buffers, edits, undo, cursors, search. No I/O, no GPU, no workspace deps. Must stay testable headlessly.
- `syntax`: tree-sitter highlighting (milestone 5). Depends on `core`.
- `render`: owns the wgpu surface and, later, the glyph atlas, layout, and the two instanced pipelines (rects and glyph quads). Depends on `core`.
- `app`: winit `ApplicationHandler`, editor state, UI chrome, keymap, config. Depends on everything.

Dependencies only point downward. Don't add a dependency from `core` onto any other workspace crate, and don't put GPU or windowing types in `core`.

Invariants from DESIGN.md that code above the buffer must respect:
- Positions are byte offsets into UTF-8. Never assume the whole file is resident; go through the `Buffer` trait (milestone 2), because the mmap-backed piece table (milestone 7) only indexes lazily.
- Rendering is on demand. The event loop uses `ControlFlow::Wait`; call `request_redraw` on input or background updates. Never introduce a continuous render loop.
- Every user-visible operation is an `Action` registered with a name so the command palette can reach it (milestone 4).

## Performance budgets (enforced or measured)

- Time to first frame under 100 ms in release. Milestone 0 measured about 95 ms warm on an M5 Pro while drawing nothing, so startup work added later must be lazy or off the main thread.
- Release binary under 15 MB; the `release-size` CI job fails above that.
- The release profile (`lto = "fat"`, `panic = "abort"`, `strip`) is deliberate; don't loosen it.

## wgpu version gotcha

The project uses wgpu 30, whose API differs from older examples and from model training data: `get_current_texture()` returns a `CurrentSurfaceTexture` enum rather than a `Result`, frames are presented with `queue.present(frame)`, `InstanceDescriptor` has no `Default`, and `RenderPassDescriptor` requires `multiview_mask`. Check the crate source under `~/.cargo/registry/src/*/wgpu-30*` rather than guessing. Expect similar drift in swash and winit (pinned to 0.30; 0.31 is still beta).

## CI

`.github/workflows/ci.yml` runs fmt, clippy, and tests on macOS, Ubuntu, and Windows, plus the release-size check. Linux needs `libwayland-dev` and `libxkbcommon-dev`. Remote: `github.com/SklorGalore/nimbus` (private).
