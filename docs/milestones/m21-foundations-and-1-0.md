# M21 — Foundations and 1.0

**Status: in review (2026-10-18).** Started the day M20 landed. The
owner chose to ship 1.0 on ropey 1.6 and winit 0.30, since both upgrades
were still betas; they move to a point release (`backlog.md`). Written on 2026-10-18. The last milestone
of the set: bring the pinned dependencies forward, add the testing that
a 1.0 promise needs, and ship 1.0. Approved the same day: screen-reader
support (AccessKit) waits until after 1.0, and a panic writes a one-line
`crash.log` with no user text (`DESIGN.md` §9).

## Goal

Call Nimbus 1.0: current dependencies, a fuzzed and soak-tested editing
path, a record of why it crashed if it ever does, and a release the
owner would hand to someone else.

## Done when

- [ ] ~~**Ropey 2**~~ *(deferred: ropey 2 is at 2.0.0-beta.1; after 1.0)*: `RopeBuffer` moves to ropey 2 and the char-to-byte
      layer in `rope.rs` is deleted; the buffer oracle suite and every
      benchmark pass with numbers recorded before and after.
- [ ] ~~**winit and wgpu current**~~ *(deferred: winit 0.31 is at
      0.31.0-beta.3, and wgpu 30.0.1 is already the newest release)*: winit 0.31 (if stable by then) and the
      current wgpu, with the API drift noted in `CLAUDE.md` updated;
      `--smoke` within 10 % of the previous release.
- [x] **Fuzzing**: `cargo fuzz` targets for the buffer (random edit
      scripts against the `String` oracle), the diff, the search chunker
      and the config and theme parsers, run for an hour each in a weekly
      CI job; any crash found is a regression test.
- [ ] **Soak** *(built: `bench/soak.sh`; the owner's Mac run is pending)*: `--bench-typing` extended to a 100,000-keystroke session
      with saves, undo and find on the 100 MB file, memory sampled every
      1,000 keystrokes; growth under 10 MB over the run.
- [x] **Crash record**: a panic (release builds abort) writes a one-line
      record with the version and the panic message to `crash.log` in the
      state directory before aborting, and the next start says it is
      there. No text from the user's buffers is written (the M12 autosave
      decision stands).
- [ ] **1.0** *(version, CHANGELOG, §8 and Results done; the owner tags v1.0.0 after merge)*: `Cargo.toml` at 1.0.0; `CHANGELOG.md` from M0 to here;
      `DESIGN.md` §8 and this file's Results; the release workflow from
      M15 publishes it.

## Scope

**In**: the two upgrades, fuzzing and soak in CI, the crash record, the
1.0 release.

**Deferred**: screen-reader support through AccessKit (the owner chose
to ship 1.0 without it; it is the first item after 1.0 in
`backlog.md`); crash upload; a plugin or scripting layer (still
a non-goal, §2).

## Design

**Ropey 2 is a `core` change only.** The `Buffer` trait is in byte
offsets already; the wrapper's conversions go away and the rest of the
workspace does not notice. Done first, on its own commit, because the
oracle suite makes it safe and every later task benefits from the
simpler code.

**Upgrades are measured, not assumed.** Each upgrade is one commit with
`--smoke`, the typing benchmark and binary size before and after in its
message; a regression over 10 % is reverted and the upgrade waits.

**Fuzzing reuses the oracles.** The property tests from M2, M7 and M9
already compare against a `String` oracle; fuzz targets feed them
arbitrary input instead of `proptest` strategies. The targets live in
`fuzz/` at the workspace root (cargo-fuzz's layout) and are excluded
from the normal build.

**The crash record uses a panic hook** set in `main` that writes the
message and version with `std::fs` (no allocation beyond the message)
and then lets the abort proceed.

## Tasks

1. Ropey 2 migration; delete the conversion layer; oracle suite and
   benchmarks before and after.
2. winit upgrade; wgpu upgrade; `CLAUDE.md` gotchas updated; `--smoke`.
3. Fuzz targets and the weekly CI job; first findings as tests.
4. Soak run in `--bench-typing`; memory sampling; the growth check.
5. Panic hook and `crash.log`; the next-start message.
6. `CHANGELOG.md`, version 1.0.0, release; Results.

## Concepts

- **An oracle makes an upgrade safe**: why the buffer rewrite is low
  risk despite touching the lowest layer.
- **Fuzzing versus property testing**: same idea, different source of
  inputs, different time budget.
- **A panic hook**: the one piece of code that runs between a panic and
  the abort, and why it must not allocate much or touch the buffers.

## Risks

| Risk | Fallback |
|---|---|
| winit 0.31 is still beta | Stay on 0.30; record it and move the task to a later point release |
| The panic hook itself fails (disk full, no state directory) | It ignores its own errors; the abort proceeds either way |
| The fuzzers find more than one milestone can fix | Each finding is a test and a fix on its own commit; 1.0 waits for the buffer and save targets only |

## Results

Written on 2026-10-18 in a Linux container with no GPU; the numbers that
need a window (`--smoke`, the soak) are the owner's to record on their
Mac before tagging.

**Upgrades.** None. ropey 2 was at 2.0.0-beta.1 and winit 0.31 at
0.31.0-beta.3, and the owner chose to release 1.0 on ropey 1.6 and
winit 0.30 rather than on betas (decision log, 2026-10-18). wgpu 30.0.1
was already the newest release. Both upgrades are first in
`backlog.md` for a point release.

**Fuzzing** (`fuzz/`, `.github/workflows/fuzz.yml`). Six targets:
`buffer` (edit scripts with undo, redo and snapshots on the rope and the
piece table with tiny limits, against a `String`), `diff`, `merge`,
`search`, and `config` and `theme`, which compile the app's own parser
files with `#[path]` because `app` has no library. Each runs an hour a
week on nightly, one job per target, with the corpus cached between
runs; `ci.yml` type-checks the core and config targets on every pull
request. Local runs of 10–40 minutes per target found two bugs, both in
`search.rs` and both fixed with regression tests (nimbus-core 0.7.2):

- `find_all` skipped a match that started where a match from the
  previous chunk ended (`vvv` in a run of `v`s across the 1 MB edge):
  the next chunk searched from its own start, found and dropped an
  overlapping match, and stepped past the real one. It now resumes at
  the end of the last match.
- `\b` next to a character cut in two by a window end was judged on half
  a character. Windows now read four bytes (one character) further.

Known limits the targets allow for rather than fix: Find in Files
treats each run of lines as its own text, so `\A` and `\z` match at run
edges; the multi-line matcher's `\z` at the end of what it holds only
ever produces a match it already marks truncated. Not fuzzed yet: the
non-UTF-8 and partially indexed mapped buffers, and saving (which lives
in `app`; its atomic-write path keeps its existing tests).

**Soak.** `--bench-typing N --soak` mixes a line break every 60
keystrokes, an undo every 50, a find every 2,500 and a save every 10,000
into the typing, samples resident memory with `ps` every 1,000
keystrokes (no new dependency and no `unsafe`), and exits 1 if it grew
more than 10 MB from the first sample to the last. `bench/soak.sh` runs
100,000 keystrokes on a scratch copy of the 100 MB file. Owner's run:
_pending_.

**Crash record.** `crates/app/src/crash.rs`. The hook removes anything
between backticks from the message, because the standard library's
string-slicing panics quote the string, which may be the user's text.

**Also fixed for 1.0**, from the manual pass: the config template's
language list, `--picker` in `--help`, a silent exit 0 when the GPU,
window or font fails (now a message, a dialog without a terminal, and
exit 1), `--no-session` writing `state.toml`, and Go to File hiding
dotfiles. Finder "Open With" and the emoji-picker input bug wait for
winit 0.31 (`backlog.md`).

**Startup and size.** Nothing was added to the startup path except
reading one possibly missing file (`crash.log`). Owner's
`bench/smoke.sh` on the M5 Pro, 2026-10-18: cold 764.9 ms (first run
after a fresh build); warm 140.1, 147.9, 133.8 and 133.5 ms, median
136.9 ms, within the 500 ms budget. M20 recorded 119 ms warm; the
difference is within the 115–150 ms spread seen on this machine across
milestones, and M21 adds nothing to startup but one file check. Binary size: the `release-size` CI job.

**Surprises.** M14 and M19 had landed but their status was never
updated in the milestone tables; corrected here.
