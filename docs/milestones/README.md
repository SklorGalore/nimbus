# Milestone plans

One file per milestone. `DESIGN.md` holds decisions that span milestones;
these files hold the work for one milestone. Every plan uses the same shape:

- **Goal**: one sentence.
- **Done when**: a checklist you can verify by running the app. A milestone
  is done when every box is ticked, CI is green, and the results section is
  filled in.
- **Scope**: what is in, and what is explicitly deferred.
- **Design**: types, modules and decisions specific to this milestone.
- **Tasks**: an ordered list, each small enough for one commit.
- **Concepts**: what the commit message or PR should explain, since the
  owner is learning the stack.
- **Risks**: what might go wrong and the fallback.
- **Results**: filled in when the milestone lands: measurements, surprises,
  changes to `DESIGN.md`.

Every milestone also carries these standing requirements, which are not
repeated in each file:

- CI green on macOS, Linux and Windows with the strict lint table.
- `nimbus --smoke` cold and warm `first-frame-ms` recorded in Results.
- Release binary size recorded in Results.
- `DESIGN.md` updated, with a decision-log line, for any decision that changed.
- No new `unsafe`, `unwrap` or crate-wide lint allows unless the plan says so.
- Once compatibility mode exists (M23), anything new the GPU renderer
  draws (a new primitive or pipeline) is drawn by the CPU painter too,
  in the same change, and the screenshot test passes in both modes.

| # | Plan | Status |
|---|---|---|
| 0 | [Scaffold](m0-scaffold.md) | Done |
| 1 | [Text on screen](m1-text-on-screen.md) | Done |
| 2 | [Editable](m2-editable.md) | Done |
| 3 | [Editing essentials](m3-editing-essentials.md) | Done |
| 4 | [Many files](m4-many-files.md) | Done |
| 5 | [Highlighting](m5-highlighting.md) | Done |
| 6 | [File tree](m6-file-tree.md) | Done |
| 7 | [Huge files](m7-huge-files.md) | Done |
| 8 | [Polish](m8-polish.md) | Done |
| 9 | [Compare and merge](m9-diff.md) | Done |
| 10 | [Find in files](m10-find-in-files.md) | Done |
| 11 | [Everyday editing](m11-everyday-editing.md) | Done |
| 12 | [Workspace](m12-workspace.md) | Done |
| 13 | [Split views and folding](m13-splits-and-folding.md) | Done |
| 14 | [Languages, second pass](m14-languages.md) | Done |
| 15 | [Distribution](m15-distribution.md) | Done |
| 16 | [Rendering, second pass](m16-rendering-second-pass.md) | Done |
| 17 | [Encodings and line endings](m17-encodings.md) | Done |
| 18 | [Compare, second pass](m18-compare-second-pass.md) | Done |
| 19 | [Workspace and search, second pass](m19-workspace-and-search-second-pass.md) | Done |
| 20 | [Reading aids](m20-reading-aids.md) | Done |
| 21 | [Foundations and 1.0](m21-foundations-and-1-0.md) | Done |
| 22 | [Screen readers](m22-screen-readers.md) | Done |
| 23 | [Library upgrades](m23-library-upgrades.md) | In progress: compatibility mode built; the upgrades wait on stable releases |
| 24 | [International text](m24-international-text.md) | In progress: built; the typing benchmark and the Mac checks wait |
| 25 | [Command line](m25-command-line.md) | In progress |
| 26 | [Security](m26-security.md) | In progress |
| 27 | [Git window: viewing](m27-git-viewer.md) | Planned |
| 28 | [Git window: staging and committing](m28-git-staging.md) | Planned |
| 29 | [Git window: branches and remotes](m29-git-branches-remotes.md) | Planned |
| 30 | [Git window: history tools and merging](m30-git-history-merging.md) | Planned |
| 31 | [Updates](m31-updates.md) | Done |

**Next** is the milestone to start; **Planned** plans are approved and
queued in table order. M10–M15 were drafted after M8 and approved, with
their order and the answers to their open questions, on 2026-10-17
(`DESIGN.md` §9 and §10). M16–M21 were drafted on 2026-10-18 from what
M9–M15 deferred and the backlog's unscheduled list, and approved the
same day with their order and eight answers (`DESIGN.md` §9 and §10).
M22–M24, the plans after 1.0, were drafted and approved on 2026-10-18
with their order and answers; a fourth draft, Writing prose, was
dropped (`DESIGN.md` §9 and §10).
M27–M30, a git window opened with `nimbus --git`, were drafted on
2026-10-18 at the owner's request and approved the same day with five
answers (`DESIGN.md` §9 and §10); they run after M25 and M26 (security).
[`backlog.md`](backlog.md) maps every deferred item to the plan that
picks it up, and lists what stays unscheduled. A plan becomes **In progress** when its first task lands and
**Done** when its checklist is ticked and its Results filled in.
