# M23 — Library upgrades

**Status: draft (2026-10-18), waiting on stable releases.** Written on
2026-10-18 from what M21 deferred: 1.0 shipped on ropey 1.6 and winit
0.30 because ropey 2 and winit 0.31 were still betas (ropey
2.0.0-beta.1, winit 0.31.0-beta.3), and the owner chose to move both
upgrades to the first point release after they are stable. Two macOS
bugs waited on winit too. Open questions are in `DESIGN.md` §9.

## Goal

Bring the text and window libraries forward once they are stable, and
fix the two macOS problems that need the newer winit, with every
upgrade measured against the numbers it replaces.

## Done when

- [ ] **Ropey 2**: `RopeBuffer` moves to ropey 2 and the char-to-byte
      layer in `rope.rs` is deleted; the buffer oracle suite, the fuzz
      targets from M21 and every benchmark pass, with numbers before and
      after in the commit.
- [ ] **winit 0.31**: the app moves to winit 0.31 and the matching
      `accesskit_winit`; the API drift is noted in `CLAUDE.md` beside
      the wgpu one; `--smoke` within 10 % of 1.x's number.
- [ ] **Finder "Open With"**: Nimbus appears in Finder's "Open With"
      for text files, and opening a file that way (or dropping it on the
      Dock icon) opens it in a tab, whether Nimbus is running or not.
- [ ] **macOS emoji picker**: text inserted from the Character Viewer
      (`Ctrl+Cmd+Space`) arrives in the buffer.
- [ ] **wgpu current**: whatever wgpu release is current when the
      milestone starts, with the same before-and-after numbers.

## Scope

**In**: the three upgrades, one commit each; the two macOS fixes they
unlock.

**Deferred**: anything the new winit offers beyond the two fixes (for
example its new window or IME features) becomes a backlog line, not
part of the upgrade.

## Design

**Start condition.** Each upgrade starts when its crate has a stable
release, not before; ropey and winit are independent, so whichever is
released first goes first, and the milestone may land in two point
releases. Until then the plan sits at the top of the backlog as
"Waiting".

**Ropey 2 is a `core` change only.** The `Buffer` trait is in byte
offsets already; ropey 2 indexes by byte too, so the wrapper's
conversions go away and the rest of the workspace does not notice. The
oracle suite and the fuzz targets are what make that safe.

**Upgrades are measured, not assumed.** Each one is a commit with
`--smoke`, the typing benchmark and the binary size before and after in
its message; a regression over 10 % is reverted and the upgrade waits.
This is M21's rule, kept.

**Open With needs the app delegate's open-files message.** The
`Info.plist` that `scripts/bundle-macos.sh` writes declares no document
types yet; adding them is easy, but Finder would then send files the
app cannot receive. If winit 0.31 passes that message on, the
handler is plain Rust; if it does not, it is a small `objc2` hook in
`app`, which is new `unsafe` code and needs the owner's say first
(`CLAUDE.md` keeps `unsafe` to `render` and the mmap buffer).

**The emoji picker is checked, not patched.** The bug is winit 0.30
dropping text inserted without a preceding compose. If 0.31 fixes it,
the task is a manual check and a note; if not, a report upstream and the
item stays in the backlog. A patched copy of winit stays out.

## Tasks

1. Ropey 2: migrate, delete the conversion layer, oracle suite, fuzz
   and benchmarks before and after.
2. winit 0.31 and `accesskit_winit`: migrate, `CLAUDE.md` notes,
   `--smoke`.
3. Finder Open With: document types in `Info.plist`, the open-files
   handler, a cold-start and running-app check.
4. Emoji picker check on the Mac (owner).
5. wgpu current, if newer than 30.
6. Point release; Results.

## Concepts

- **Byte-indexed ropes**: what ropey 2 changes and why our code gets
  shorter.
- **The app delegate**: how macOS hands files to a running app, and why
  a cross-platform window library has to opt in to passing that on.

## Risks

| Risk | Fallback |
|---|---|
| A stable release does not arrive for months | The plan waits; later milestones go ahead of it. Nothing else depends on it except M22's adapter version, which follows winit |
| winit 0.31 changes the event loop enough to touch most of `app.rs` | One commit with no behaviour change, reviewed on its own; the screenshot test and `--smoke` show nothing moved |
| Open With needs `unsafe` | Ask the owner; if declined, it stays in the backlog and the bundle keeps no document types |

## Results

_To be filled in when the milestone lands._
