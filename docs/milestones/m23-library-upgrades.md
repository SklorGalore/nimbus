# M23 — Library upgrades

**Status: planned (2026-10-18), waiting on stable releases.** Written on
2026-10-18 from what M21 deferred: 1.0 shipped on ropey 1.6 and winit
0.30 because ropey 2 and winit 0.31 were still betas (ropey
2.0.0-beta.1, winit 0.31.0-beta.3), and the owner chose to move both
upgrades to the first point release after they are stable. The macOS
emoji picker bug waits on winit too. Approved the same day: it follows
M22 whenever the releases arrive (`DESIGN.md` §9). Finder "Open With"
was in this plan until the owner chose to fix it in 1.0.1 with a small
`objc2` open-files listener, since winit 0.31.0-beta.3 still does not
pass those files on.

## Goal

Bring the text and window libraries forward once they are stable, and
fix the macOS emoji picker that needs the newer winit, with every
upgrade measured against the numbers it replaces.

## Done when

- [ ] **Ropey 2**: `RopeBuffer` moves to ropey 2 and the char-to-byte
      layer in `rope.rs` is deleted; the buffer oracle suite, the fuzz
      targets from M21 and every benchmark pass, with numbers before and
      after in the commit.
- [ ] **winit 0.31**: the app moves to winit 0.31 and the matching
      `accesskit_winit`; the API drift is noted in `CLAUDE.md` beside
      the wgpu one; `--smoke` within 10 % of 1.x's number. If winit
      0.31 passes Finder's open-files message on by then, the 1.0.1
      `objc2` listener is replaced by winit's event and its `unsafe`
      removed.
- [ ] **macOS emoji picker**: text inserted from the Character Viewer
      (`Ctrl+Cmd+Space`) arrives in the buffer.
- [ ] **wgpu current**: whatever wgpu release is current when the
      milestone starts, with the same before-and-after numbers.

## Scope

**In**: the three upgrades, one commit each; the emoji picker fix they
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

**The emoji picker is checked, not patched.** The bug is winit 0.30
dropping text inserted without a preceding compose. If 0.31 fixes it,
the task is a manual check and a note; if not, a report upstream and the
item stays in the backlog. A patched copy of winit stays out.

## Tasks

1. Ropey 2: migrate, delete the conversion layer, oracle suite, fuzz
   and benchmarks before and after.
2. winit 0.31 and `accesskit_winit`: migrate, `CLAUDE.md` notes,
   `--smoke`.
3. Emoji picker check on the Mac (owner).
4. wgpu current, if newer than 30.
5. Point release; Results.

## Concepts

- **Byte-indexed ropes**: what ropey 2 changes and why our code gets
  shorter.

## Risks

| Risk | Fallback |
|---|---|
| A stable release does not arrive for months | The plan waits; later milestones go ahead of it. Nothing else depends on it except M22's adapter version, which follows winit |
| winit 0.31 changes the event loop enough to touch most of `app.rs` | One commit with no behaviour change, reviewed on its own; the screenshot test and `--smoke` show nothing moved |

## Results

_To be filled in when the milestone lands._
