# Fuzzing Nimbus

This folder holds [cargo-fuzz](https://rust-fuzz.github.io/book/cargo-fuzz.html)
targets: small programs that feed generated inputs to the editor's core
and check that the answers are right.

## Fuzzing versus property testing

They are the same idea. A property test in `crates/core/tests/` builds an
input (a text and a list of edits), runs it through the real code and
through an *oracle*, the slow, obvious version (a `String` edited with
`replace_range`), and checks that the two agree. The fuzz targets here use
the same oracles and the same checks. What differs is where the inputs
come from and how long they run:

- **proptest** builds inputs from strategies written by hand, a few
  hundred per test, in seconds, on every `cargo test`. It finds the bugs
  the strategies were written to find.
- **libFuzzer** (what cargo-fuzz runs) starts from nothing and mutates
  bytes, keeping any input that makes the program take a branch it has not
  taken before. Over an hour it tries millions of inputs and wanders into
  cases nobody wrote a strategy for. The `arbitrary` crate turns its bytes
  into typed values, so a target receives an edit script rather than raw
  bytes (`#[derive(Arbitrary)]` on the input types).

So proptest guards what we thought of, quickly, and fuzzing looks for what
we did not, slowly. CI runs each target for an hour every week
(`.github/workflows/fuzz.yml`).

## The targets

| Target | What it checks | Oracle |
|---|---|---|
| `buffer` | Edit scripts (inserts, deletes, replaces, edits at raw offsets that must be refused, batches, undo and redo, snapshots) on the rope and the piece table | `buffer_oracle.rs`, `undo_oracle.rs` |
| `diff` | The line diff, `compare` at every layer (including tiny windows and split long lines), the normalising options, the live diff | `diff_oracle.rs` |
| `search` | Chunked search, literal search across its window edge, the line and multi-line block matchers | the regex crate searching the whole text at once |
| `merge` | Three-way merge chunks, and the merge view's regions through takes and edits | `merge_oracle.rs` |
| `config` | `Config::parse` never panics, and its range-checked settings are in range | none: no panic is the property |
| `theme` | `Theme::parse` never panics, and every colour it gives is a real one | none: no panic is the property |

`config` and `theme` compile the app's own `config.rs` and `theme.rs` in
with `#[path]`, since the app crate is a binary with no library to link.
They need those files' dependencies, so they sit behind features
(`app-config`, `app-theme`); the other targets build without wgpu or the
tree-sitter grammars.

This crate is not part of the root workspace (its `Cargo.toml` has its own
`[workspace]` table), so `cargo build` and `cargo test` at the root never
build it. It has its own `Cargo.lock`.

## Known limits the targets allow for

Some differences from the oracle are documented limits, not bugs, and the
`search` target skips its comparison for them rather than report them
every week:

- `find_all` misses or cuts short a match longer than `OVERLAP` (4 KB)
  that crosses a chunk edge, so matches that long are not compared.
- The block matchers (`LineMatcher` for find in files) search each run
  of whole lines as a text of its own, so `\A` and `\z` (and `^` and `$`
  with multi-line mode switched off) match at the ends of every run.
  Block independence is not checked for such patterns.
- `MultiLineMatcher` treats the end of what it holds as the end of the
  text, so `\z` there can produce a match that is not in the text; it is
  always longer than the window and reported as truncated, and inputs
  with truncated hits are not compared.
- Only literal queries are put across the 1 MB chunk edge: a megabyte
  through an instrumented regex costs about half a second per input.

## Running one

cargo-fuzz needs a nightly compiler, for the sanitizer flags it passes:

```sh
rustup toolchain install nightly --profile minimal
cargo install cargo-fuzz --locked
```

Then, from the repository root or from this folder:

```sh
cargo +nightly fuzz run buffer                          # until it finds something, or Ctrl+C
cargo +nightly fuzz run buffer -- -max_total_time=300   # five minutes
cargo +nightly fuzz run config --features app-config -- -max_total_time=300
cargo +nightly fuzz run theme --features app-theme -- -max_total_time=300
cargo +nightly fuzz list                                # the target names
```

Add `-jobs=4 -workers=4` after `--` to use four cores. The parser targets
start faster from real files: copy `seeds/config/*.toml` into
`corpus/config/`, and `crates/app/themes/*.toml` into `corpus/theme/`,
before the first run.

## When it finds something

The run stops, prints the panic message (which says what disagreed with
the oracle, and on which text), and saves the input under
`artifacts/TARGET/crash-…`. Then:

```sh
cargo +nightly fuzz run buffer artifacts/buffer/crash-…   # run just that input again
cargo +nightly fuzz tmin buffer artifacts/buffer/crash-…  # shrink it to a smaller one that still fails
cargo +nightly fuzz fmt buffer artifacts/buffer/crash-…   # print it as the typed input
```

Every crash becomes a regression test: a small `#[test]` with the shrunk
input in the module's `#[cfg(test)] mod tests`, or a case in the matching
property suite in `crates/core/tests/`, committed with its fix. The
`search` tests `word_boundaries_before_a_cut_character_are_found` and
`a_match_as_long_as_the_overlap_sees_the_whole_next_character` came from
here.

`corpus/` (inputs that reached new code, kept so the next run starts from
them), `artifacts/` and `target/` are ignored by git.
