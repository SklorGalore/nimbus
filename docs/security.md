# Security

How Nimbus keeps two promises (M26, `docs/milestones/m26-security.md`):

1. **Nobody can ship code to Nimbus users except the owner, and anyone
   can check that a download is the owner's.**
2. **Opening an untrusted file or folder cannot run code, read files the
   user did not open, or crash the editor and lose unsaved work in other
   tabs.**

Reporting a problem: [SECURITY.md](../SECURITY.md). Making a release:
[releasing.md](releasing.md).

## Contents

- [The threat model: what Nimbus reads](#the-threat-model-what-nimbus-reads)
- [Release signing and the key](#release-signing-and-the-key)
- [If the key is lost or leaks](#if-the-key-is-lost-or-leaks)
- [The build pipeline](#the-build-pipeline)
- [Dependencies](#dependencies)
- [Provenance](#provenance)
- [GitHub settings](#github-settings)
- [Rules for later milestones](#rules-for-later-milestones)

## The threat model: what Nimbus reads

Nimbus never executes what it opens: no plugins, no modelines, no tasks,
no macros. Everything it reads is one of the inputs below, and each may
do only what its row says. "Untrusted" means someone other than the user
may have written it (a cloned repository, an unpacked archive, a
download).

| Input | Trusted? | What it may do | What keeps it there |
|---|---|---|---|
| A file opened in a tab | Untrusted | Be shown, highlighted, searched, edited and saved back where it came from | No code runs from file contents. Highlighting is tree-sitter grammars (data tables in C, fuzzed weekly: `highlight` target) or line rules; encodings are decoded by `encoding_rs` (fuzzed: `encoding`). Devices, pipes and sockets are refused (`io.rs`). Huge files are mapped, not loaded (see "Mapped files" below). |
| A folder in the sidebar | Untrusted | Be listed when a folder is expanded; files open only when clicked | Listing reads names only. File watching is non-recursive, on listed folders (`watch.rs`). |
| `.nimbus/workspace.toml` in a folder | Untrusted | Name folders, tabs and expanded folders **inside that folder**, and only when the user runs Open Project Workspace | `workspace_file::load_local` drops every path that resolves outside the project (absolute, `../`, or a symlink pointing out), and refuses a file whose first folder is elsewhere. Never loaded by itself, only offered. Fuzzed: `workspace`. |
| git's output and a repository's `.git/config` | Untrusted | Supply a file as it was at a revision, for Compare with HEAD | git runs as a subprocess with a 10 s timeout and an output cap, no terminal prompts, `core.fsmonitor=false`, hooks pointed at a folder that does not exist, and no lazy fetching from remotes. Checkout filters (`.gitattributes` `filter=`) apply only when no filter program comes from the repository's own config; otherwise the stored blob is read unfiltered (`git.rs`, `repo_filters`). Revisions starting with `-` are refused. Fuzzed: `git_output`. |
| Links in the Markdown preview | Untrusted | Open `http`/`https` in the browser on a click, or open a relative path that is a regular file in a tab | `update::open_url` accepts only `http://` and `https://`; relative links with a scheme (`file:`, `javascript:`) or an absolute path are refused (`preview_tab.rs`). |
| `config.toml`, `themes/`, `languages/`, `queries/` in the config folder | Trusted (the user's) | Change settings, colours, file-type rules and highlight queries | Bad values fall back with a warning; never fatal. Fuzzed: `config`, `theme`. Nimbus never writes `config.toml`. Tree-sitter queries are patterns, not code. |
| `state.toml`, named workspaces | Trusted (Nimbus's own) | Restore windows and sessions | Written atomically; a broken file is ignored with a warning. |
| The update check's answer | Untrusted (the network) | Report a newer version | Only when asked, or when `check_updates` is on; HTTPS with the system's certificates; the answer is capped and only `tag_name` is read, which must be letters, digits, `.` and `-`; the release page URL is built from it, not taken from the answer (`update.rs`). Nothing is downloaded until M31, which checks the signature first. |
| Files dropped on the window, or passed by Finder's Open With | Untrusted | Open in tabs, like files named on the command line | The same path as opening a file. |
| The clipboard | Untrusted | Be pasted as text | Pasted as text; never interpreted. |

**Hidden text.** Bidirectional controls (U+202A–202E, U+2066–2069,
U+200E, U+200F, U+061C) and zero-width characters (U+200B, U+2060–2064,
U+FEFF, U+00AD, U+180E) draw as a red box in their cell, whatever the
`render_whitespace` setting says, so "Trojan Source" code, whose display
order differs from its byte order, cannot hide in plain sight
(`column::is_hidden_char`, drawn in `view.rs`). The joiners U+200C and
U+200D are left out: emoji and scripts such as Persian need them.

**Mapped files.** Files over the rope threshold are memory-mapped. On
macOS and Linux, if another program truncates such a file while it is
open, reading the lost part through the mapping kills Nimbus (`SIGBUS`),
which Rust cannot catch. The `SAFETY` note in `mapping.rs` spells this
out; what keeps it rare is that bulk work (indexing, search, save) reads
with ordinary reads that fail cleanly, only the text on screen goes
through the mapping, and the watcher re-maps a shrunk clean file at once
and warns loudly about one with unsaved edits. Windows refuses to
truncate a mapped file. This is an accepted, documented gap: closing it
fully would mean not mapping files at all.

## Release signing and the key

A hash says a file was not damaged on the way; a **signature** says who
vouched for it. Anyone who can replace a file on the release page can
replace its hash too, but cannot make a signature without the key.

- The key is a minisign key (Ed25519), made on the owner's Mac with a
  password (`minisign -G`). Key ID `A5DFCD31F80FDD0F`; the public half is
  [`minisign.pub`](../minisign.pub) and is printed in the README.
- The secret half lives in `~/.minisign/minisign.key` on the owner's Mac,
  with an offline backup. It is **never** an Actions secret: a key in
  GitHub would let anyone who can run a workflow on a tag sign a
  release, and once M31 lands a signature is what makes every Nimbus
  install itself.
- The release workflow builds the packages and `SHA256SUMS` into a draft;
  `scripts/sign-release.sh vX.Y.Z` on the Mac downloads them, checks
  every hash and that the list is exact, signs with the trusted comment
  `Nimbus X.Y.Z SHA256SUMS` (the form M31 reads), verifies against
  `minisign.pub` and uploads `SHA256SUMS.minisig`. Only then is the
  draft published.

## If the key is lost or leaks

The binary will carry a **list** of trusted keys (M31), not one, so a
key can be replaced without stranding installs that know only the old
one.

**Planned rotation, or a leak:**

1. Make a new key on the Mac (`minisign -G -p minisign-2.pub -s
   ~/.minisign/minisign-2.key`), with a password, and back it up offline.
2. Release N: add the new public key to the binary's trusted list and to
   the repository (`minisign.pub` keeps the current key; the new one
   goes beside it until step 3), and sign release N with the **old** key.
   Installs that know only the old key accept it and learn the new one.
3. Release N+1: sign with the new key, make it `minisign.pub`, drop the
   old key from the list, and update the README.
4. After a leak, also: say so in the release notes and in a GitHub
   security advisory, with the old key ID, so nobody trusts a file
   signed by it after the date of the leak.

**Lost key, no backup:** an old install cannot be told about a new key.
Make a new key, ship it in a release that users install by hand (as
before M31), and say so prominently in the README and release notes.

## The build pipeline

Whoever controls a workflow on a tag controls a release, so workflows
are guarded like code.

- **Actions are pinned to full commit SHAs** (`uses: owner/action@SHA #
  vX.Y.Z`). A tag such as `v7` can be moved to any commit by whoever
  controls that repository; in March 2025 the `tj-actions/changed-files`
  tag was moved to steal secrets from thousands of workflows. A SHA
  cannot be moved. Dependabot updates the SHA and the comment together.
  `scripts/pin-actions.sh` pins any action added later.
- **Tokens start read-only.** Every workflow declares `permissions:
  contents: read`, and each job that writes asks for exactly what it
  needs (the release jobs: `contents: write`; the build job also
  `id-token: write` and `attestations: write` for provenance). Checkouts
  use `persist-credentials: false`, so the token is not left in
  `.git/config` for later steps.
- **The `release` environment.** The release workflow's first job runs
  in it and waits for the owner's approval; every other job depends on
  it, so nothing builds or uploads before that click.
- **No caches in release jobs.** A pull request can write to the Actions
  cache; restoring that cache in a release build would let it slip code
  in.
- **Downloaded tools are pinned by version and SHA-256**:
  `appimagetool` (`APPIMAGETOOL_VERSION` and `APPIMAGETOOL_SHA256` in
  `release.yml`), never its moving `continuous` release.
- **`--locked` everywhere**: builds use exactly what `Cargo.lock` names
  and fail rather than resolve a newer crate by themselves.
- **`zizmor`** lints the workflows on every pull request and fails on
  anything above "low": unpinned actions, template injection from pull
  request titles, over-broad tokens, cache poisoning.

## Dependencies

Nimbus has a few hundred crates; nobody can read them all.

- **`cargo vet`** (CI job `cargo vet`, files in `supply-chain/`) records
  who has reviewed which crate version. Audits are imported from
  Mozilla, Google, the Bytecode Alliance, Embark, ISRG, Zcash and
  Ariel OS; everything else is **exempted** at its version as of 1.0,
  which says "trusted as of 1.0" without pretending it was read. A new
  crate or a version bump fails CI until an imported audit covers it or
  someone reviews the change:

  ```sh
  cargo install cargo-vet --locked
  cargo vet                         # what needs review
  cargo vet diff CRATE OLD NEW      # read what changed
  cargo vet certify CRATE OLD NEW   # record the review in supply-chain/audits.toml
  ```

  Build scripts (`build.rs`) and proc macros run on the build machine, so
  they get the closest look. `cargo vet regenerate imports` refreshes the
  imported audits; commit `supply-chain/imports.lock` with it.
- **`cargo deny`** (CI job `cargo deny`, `deny.toml`) still checks
  licences, known advisories, and that every crate comes from crates.io.
- **`cargo auditable`** builds the release binaries, embedding the
  dependency list (a few KB), so `cargo audit bin nimbus` checks a
  shipped copy against the advisory database.

## Provenance

Every package on a release has a **build provenance attestation**: a
statement signed by GitHub's builder (through Sigstore) naming the
commit and workflow run that produced it. Check one with the GitHub CLI:

```sh
gh attestation verify Nimbus-1.1.0-macos.dmg --repo SklorGalore/nimbus
```

It passes only for a file built by this repository's release workflow.
The minisign signature is still what users and the updater rely on; the
attestation adds "and it was built from this commit".

## GitHub settings

These are repository settings only the owner can change. Each protects
one of the promises above. Results in the milestone doc records when
each was done.

1. **Settings > General**: default branch `development`; automatically
   delete head branches. *Pull requests target the integration branch
   by default.*
2. **Settings > Rules > Rulesets > New branch ruleset** on `main` and
   `development`: require a pull request; require status checks
   `ubuntu-latest`, `macos-latest`, `windows-latest`, `cargo deny`,
   `cargo vet`, `zizmor` and `release binary size`; block force pushes;
   restrict deletions; bypass list: the owner only. *Nobody, including a
   leaked token or an installed app, can push straight to a release
   branch.*
3. **Settings > Rules > Rulesets > New tag ruleset** on `v*`: restrict
   creations, updates and deletions; bypass list: the owner only. *A tag
   is a release, so only the owner can make one.*
4. **Settings > Environments > New environment** `release`: required
   reviewer the owner; deployment branches and tags: tags `v*` only.
   *Even a pushed tag waits for the owner's click before building.*
5. **Settings > Actions > General**: allow GitHub's actions, verified
   creators' and the pinned list; require approval for workflows from
   all outside contributors; workflow permissions "Read repository
   contents and packages permissions"; allow GitHub Actions to create
   pull requests (for `back-merge.yml`). *A fork's pull request cannot
   run code with a write token.*
6. **Settings > Code security**: Dependabot alerts and security updates
   on; secret scanning and push protection on; private vulnerability
   reporting on. *Advisories get a pull request; a committed secret is
   blocked; reports arrive privately (SECURITY.md).*
7. **Your account**: two-factor authentication with a passkey or
   hardware key; review Settings > Applications and personal access
   tokens and remove any with write access that are no longer used.
   *The owner's account is the last line.*
8. **Settings > Secrets and variables > Actions**: delete
   `MINISIGN_SECRET_KEY` if it was ever created. *The workflow no longer
   reads it.*

## Rules for later milestones

- **M25 (command line)**: the single-instance socket lives in a
  directory only the user can read (mode 0700; on Windows a pipe whose
  ACL names only the user), and accepts only file paths and `--wait`,
  never commands.
- **M27–M30 (git client)**: the read-only viewer uses the same git
  settings as `git.rs` (`git()`): no fsmonitor, no hooks, no lazy
  fetch, no repository-defined filter programs. Commands that write
  (commit, merge, checkout) run only on the user's action, in a
  repository the user opened.
- **M31 (updates)**: verify the signature with the built-in key list and
  the exact version before writing anything to the install location;
  never install on a failed check; refuse downgrades.
