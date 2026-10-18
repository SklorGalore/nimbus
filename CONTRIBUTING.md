# Contributing

## Branches

Nimbus uses git flow. Two branches live forever; the rest are short-lived
and are deleted once merged.

| Branch | Holds | Starts from | Merges into |
|---|---|---|---|
| `main` | Released code only. Every commit on it is a tagged release. | | |
| `development` | The next release as it comes together. | `main` (once) | |
| `feature/<name>` | A new feature, a milestone, docs or tooling. | `development` | `development` |
| `bugfix/<name>` | A fix for something not yet released. | `development` | `development` |
| `release/X.Y.Z` | The release being prepared: version, changelog, last fixes. | `development` | `main`, then `development` |
| `hotfix/X.Y.Z` | An urgent fix to the released version. | `main` | `main`, then `development` |

So a pull request targets `development`, unless it is a release or a
hotfix, which target `main`. Dependabot's action updates target
`development` too.

`<name>` is a few lowercase words joined by hyphens, such as
`feature/remember-workspace` or `bugfix/fold-gutter-click`.

### Before a pull request is ready

Merge the latest base branch (`development`, or `main` for a release or
hotfix) into the branch, resolve every conflict, and wait for CI to pass
on the result. Never mark a pull request ready while it conflicts with
its base.

### Releases and hotfixes

[docs/releasing.md](docs/releasing.md) has the steps. In short: cut
`release/X.Y.Z` from `development`, set the version and changelog there,
merge it into `main`, tag that merge `vX.Y.Z`, and merge `main` back into
`development` so the version bump and any last fixes are not lost. A
hotfix is the same, except that it starts from `main`.

### Versions

`development` carries the version of the next planned release (a new
milestone bumps the minor version, as `1.1.0`). A hotfix bumps the patch
version of the release it fixes (`1.0.1`). When a hotfix is merged back
into `development`, keep `development`'s higher version.

## Code

The code rules (lints, tests, no panics, documenting public items) are in
[CLAUDE.md](CLAUDE.md#code-quality-and-linting), and the commands to
build, test and lint are at the top of that file.
