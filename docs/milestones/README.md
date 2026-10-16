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

| # | Plan | Status |
|---|---|---|
| 0 | [Scaffold](m0-scaffold.md) | Done |
| 1 | [Text on screen](m1-text-on-screen.md) | Done |
| 2 | [Editable](m2-editable.md) | Done |
| 3 | [Editing essentials](m3-editing-essentials.md) | Done |
| 4 | [Many files](m4-many-files.md) | Done |
| 5 | [Highlighting](m5-highlighting.md) | Done |
| 6 | [File tree](m6-file-tree.md) | Done |
| 7 | [Huge files](m7-huge-files.md) | Next |
| 8 | [Polish](m8-polish.md) | Planned |
| 9 | [Compare and merge](m9-diff.md) | Planned |
