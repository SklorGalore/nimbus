# Example files

Small, hand-sized inputs for trying features by eye. The bench files from
`bench/gen.sh` measure; these show. The user manual's
[Compare and merge](../docs/manual/compare-and-merge.md) chapter explains
the views they exercise.

## `merge/`: compare and merge

Three versions of one small Rust file, and the conflicted result git makes
of them:

| File | What it is |
|---|---|
| `base.rs` | The common ancestor. |
| `local.rs` | Adds a `shelf` field, an `empty()` method, and rewrites the failure arm of `take` to log. |
| `remote.rs` | Derives `Default`, rewrites the same failure arm of `take` differently, pins `sum::<u64>()`, adds a test. |
| `conflicted.rs` | `git merge-file -p local.rs base.rs remote.rs`: the merge with one conflict block. |

What a three-way merge of them should show: one conflict (the failure arm
of `take`, which both sides changed), three changes only on the left and
three only on the right, among them one that adds lines in the middle
without changing anything near them (`#[derive(Default)]` on the right).

Try, from the repository root:

```sh
cargo build --release
R=examples/merge

# Compare two files side by side; F7 and Shift+F7 step through changes.
./target/release/nimbus --diff $R/base.rs $R/local.rs

# A three-way merge as git's mergetool would call it; the result saves
# to OUT and the exit code is 0 only when it is saved with no conflicts.
./target/release/nimbus --merge $R/base.rs $R/local.rs $R/remote.rs -o /tmp/merged.rs; echo "exit $?"

# A file with conflict markers: the message line offers "Resolve Conflicts".
cp $R/conflicted.rs /tmp/conflicted.rs && ./target/release/nimbus /tmp/conflicted.rs

# A file that changes on disk under unsaved edits.
cp $R/base.rs /tmp/changing.rs && ./target/release/nimbus /tmp/changing.rs
# ... type something in the window, then from another terminal:
cp $R/remote.rs /tmp/changing.rs
# ... the message line lists the choices; run "Merge with Disk" from the palette.
```

Inside the editor, "Merge Files…" and "Merge with Base…" in the palette
open the same views from file dialogs. F8 and Shift+F8 step through the
conflicts; the palette has Take Left, Take Right, Take Both and Take Both
Reversed for the change under the cursor.

Copy files to `/tmp` before editing them so the examples stay as they are.
