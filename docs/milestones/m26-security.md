# M26 — Security

**Status: draft (2026-10-18).** Written on 2026-10-18 at the owner's
request: a security milestone for the program and the repository,
supply chain attacks included. Its open questions are in `DESIGN.md`
§9. The owner chose to run it before the git client and updates plans
drafted the same evening, which move to M27–M30 (git client) and M31
(updates).

## Goal

Nobody can ship code to Nimbus users except the owner, and anyone can
check that a download is the owner's. Opening an untrusted file or
folder cannot run code, read files the user did not open, or crash the
editor and lose unsaved work in other tabs.

## Where things stand

What is already in place, so the plan does not redo it: strict lints
with `unsafe` denied outside four modules, each block with a `SAFETY`
comment; `cargo deny` (advisories, licences, crates.io only) on every
pull request; Dependabot alerts on and weekly action updates; six fuzz
targets weekly; atomic saves; a `crash.log` with no user text; HTTPS
with the system's certificate store for the update check; git run as a
subprocess with a timeout, an output cap and no terminal prompts;
Markdown preview links open in the browser only for `http` and
`https`.

What is missing, found while writing this plan:

- **No release has ever been signed.** `minisign.pub` is not in the
  repository and the `MINISIGN_SECRET_KEY` secret was never set, so
  1.0.0 and 1.0.1 shipped an unsigned `SHA256SUMS`, while the README
  tells people to check a signature with a key that does not exist.
  M31 (updates) will install only what that signature vouches for, so
  the key comes first.
- Every action is pinned by a movable tag (`actions/checkout@v7`,
  `dtolnay/rust-toolchain@stable`), so whoever controls one of those
  repositories controls our release job and its write token.
- `release.yml` downloads `appimagetool` from a release called
  `continuous` with no checksum and runs it with that token in scope.
- `ci.yml` declares no `permissions`, so its token gets whatever the
  repository default is; release builds run without `--locked`.
- The repository is public, `main` is still the default branch, and
  nothing is protected yet: anyone with push access (the owner, a
  leaked token, an installed app such as the Claude GitHub app) can push
  straight to `main` or create a `v*` tag, and a tag is a release.
- No `SECURITY.md`, so nobody knows how to report a problem privately.

## Done when

- [ ] **Releases are signed, and the key is not on GitHub**:
      `minisign.pub` is committed and also printed in the README and the
      manual; 1.0.x's `SHA256SUMS` are checked against the packages and
      signed after the fact; each new release is signed by
      `scripts/sign-release.sh` on the owner's Mac (the key file is
      passworded and never leaves it, with an offline backup), whose
      trusted comment is `Nimbus X.Y.Z SHA256SUMS`, the form M31 reads.
      A written plan covers a lost or leaked key (`docs/security.md`).
- [ ] **Only the owner can release**: `main` and `development` are
      protected (pull request required, CI required, no force push, no
      deletion); a tag ruleset lets only the owner create `v*` tags;
      the release workflow runs in a `release` environment that needs
      the owner's approval.
- [ ] **Workflows cannot be hijacked**: every action is pinned to a
      full commit SHA with its version in a comment (Dependabot keeps
      both current); every workflow starts from `permissions:
      contents: read` and grants write per job; downloaded tools are
      pinned by version and SHA-256; release jobs use no shared cache;
      `zizmor` checks the workflows on every pull request and reports
      nothing above "low".
- [ ] **Dependencies are reviewed, not just scanned**: new and updated
      crates pass `cargo vet` in CI (audits imported from Mozilla,
      Google, the Bytecode Alliance and others; the rest exempted at
      today's versions, so only what changes after this milestone needs
      reading); `cargo build --locked` everywhere; the release binary
      carries its dependency list (`cargo auditable`), so `cargo audit
      bin nimbus` works on a shipped copy.
- [ ] **Downloads are traceable**: each package has a GitHub build
      provenance attestation (`gh attestation verify` names the commit
      and workflow that built it), and the README says how to check it.
- [ ] **Untrusted files stay data**: the threat model in
      `docs/security.md` lists every input Nimbus reads (files, folders,
      `.nimbus/workspace.toml`, `.gitattributes` and git output, config,
      themes, queries, the update answer, dropped files, the clipboard)
      and what each may do; the audit in task 6 is written up in
      Results, with each gap fixed or listed.
- [ ] **Hidden text is visible**: bidirectional controls (U+202A–202E,
      U+2066–2069) and zero-width characters are drawn as visible marks
      even with whitespace rendering off, so "Trojan Source" code
      cannot hide in plain sight.
- [ ] **More fuzzing**: new targets for highlighting every bundled
      grammar (tree-sitter parsers are C), the Markdown preview scanner,
      encoding detection and decoding, workspace files and git's output
      parsing; all run weekly with the six existing ones.
- [ ] **People know how to report**: `SECURITY.md` (private reporting
      through GitHub, which versions get fixes, what to expect), and
      private vulnerability reporting turned on.
- [ ] Budgets: `--smoke` unchanged; the binary grows under 50 KB
      (`cargo auditable`); CI time grows under two minutes.

## Scope

**In**: release signing and key custody; branch, tag and environment
protection (as settings the owner changes, listed below); workflow
hardening; dependency review; provenance; the threat model and an audit
of untrusted input; visible bidi and zero-width characters; fuzz
targets; `SECURITY.md`; rules handed to the plans in flight.

**Not here**: the update mechanism itself (M31 owns fetching, checking
the signature with the key built into the binary, refusing downgrades
and installing; this plan only supplies the key and the signed file).
Code signing and notarisation stay declined (`DESIGN.md` §9, M15).
Sandboxing the app (the macOS App Sandbox, AppArmor profiles) would
break opening arbitrary files from the tree and the command line, and is
not planned. Reproducible builds are a stretch goal (Results says how
far two builds of one tag differ).

## Design

**The signing key lives on the owner's Mac, not in GitHub.** Today's
design puts the minisign secret in an Actions secret. That makes
anyone who can run a workflow on a tag able to sign, and once M31
lands, a signature is what makes every Nimbus install itself. Signing
locally keeps a GitHub account or workflow compromise from producing an
update Nimbus will accept. The flow becomes: tag on the Mac (as now),
the workflow builds the draft and `SHA256SUMS` (as now, minus the
signing step), then `scripts/sign-release.sh vX.Y.Z` downloads the
draft's packages, recomputes every hash, compares them with
`SHA256SUMS`, signs it with the trusted comment M31 expects, uploads
`SHA256SUMS.minisig`, and only then does the owner publish. The key is
made with a password (`minisign -G`, no `-W`), so a copied key file
alone is useless; a second copy goes on a USB stick or in a password
manager. §9 asks whether to keep CI signing instead.

**Rotation is planned before it is needed.** If the key leaks, the next
release must be trusted by installs that know only the old key. The
binary carries a list of trusted keys rather than one; a rotation ships
a release signed by the old key that adds the new one, and the one
after drops the old key. M31 reads that list; this plan writes the
procedure in `docs/security.md`.

**A tag is a release, so tags are guarded like branches.** A repository
ruleset restricts creating, moving and deleting `v*` tags to the owner,
and the release workflow's jobs run in a `release` environment whose
only reviewer is the owner, so even a pushed tag waits for a click.
Branch rules on `main` and `development` require a pull request and
green CI; the owner can still merge their own pull requests, and the
Claude GitHub app can open them but not land them.

**A pinned SHA is the only reference that cannot change.** A tag such
as `v7` can be moved to any commit by whoever controls that repository,
and the March 2025 `tj-actions/changed-files` attack did exactly that to
steal secrets from thousands of workflows. A full 40-character commit
SHA cannot be moved. Dependabot understands `uses: owner/action@SHA #
v7.0.1` and updates both, so pinning costs no upkeep. `zizmor` is a
linter for workflows that catches this and the other common mistakes
(template injection from pull request titles, `pull_request_target`,
over-broad tokens, cache poisoning).

**Review what changes, not everything.** Nimbus has a few hundred
crates; nobody can read them all. `cargo vet` records who has reviewed
which crate version. Importing the published audits of Mozilla, Google
and others covers much of the tree; everything else is "exempted" at
today's version, which says "trusted as of 1.0" without pretending it
was read. From then on, a new crate or a version bump fails CI until it
is covered by an import or by a short review of the diff (`cargo vet
diff`), which is where a malicious update would show up. Build scripts
and proc macros run code on the build machine, so they get the closest
look. §9 asks whether this is worth the upkeep, or `cargo deny` alone
should stay.

**Untrusted files stay data.** Nimbus never executes what it opens:
no plugins, no modelines, no tasks. The audit checks the few places
where a file or folder steers behaviour:

- `.nimbus/workspace.toml` inside a cloned folder names roots and tabs;
  paths outside the folder it lives in are dropped with a message, so a
  repository cannot quietly open `~/.ssh` into a tab.
- git runs inside folders the user did not create. Every read-only git
  call gets `-c core.fsmonitor=false` and `-c core.hooksPath=` (an
  archive that ships its own `.git/config` can otherwise make git run a
  program), and `cat-file --filters` is used only when the repository
  passes git's own `safe.directory` ownership check. M27–M30 (the git
  client) inherit this rule for their read-only viewer.
- Preview links: `http` and `https` go to the browser; a relative link
  opens a file only if it resolves to a regular file (no `file:`,
  `javascript:` or other schemes).
- Huge or hostile files must not crash the editor: the fuzz targets
  above, plus the mapped-file `SAFETY` note in `mapping.rs` re-read
  against a file truncated by another process while it is open.

**Rules for the plans in flight.** M25's single-instance socket lives in
a directory only the user can read (mode 0700; on Windows a pipe whose
ACL names only the user), and it accepts only file paths and `--wait`,
never commands. M31's updater verifies before writing anything to the
install location, and never installs on a failed check. M27–M30 follow
the git rule above.

## Tasks

1. Key: generate, commit `minisign.pub`, check and sign 1.0.0 and 1.0.1
   by hand; `scripts/sign-release.sh`; drop the signing step from
   `release.yml`; `docs/releasing.md` and README updated.
2. Workflows: SHA pins, `permissions` blocks, pinned `appimagetool`
   with its hash, `--locked` everywhere, a `release` environment;
   `zizmor` in CI.
3. Dependencies: `cargo vet init`, imports, exemptions, CI job;
   `cargo auditable` in `release.yml`; build provenance attestations.
4. `SECURITY.md`; `docs/security.md` (threat model, key rotation,
   what each GitHub setting protects); the settings list below done by
   the owner and checked off in Results.
5. Visible bidi and zero-width characters in `render`, with a
   screenshot test.
6. The untrusted-input audit: workspace paths, git flags, preview
   links, mapped-file truncation; fixes with tests.
7. Fuzz targets: grammars, preview, encodings, workspace, git output;
   added to `fuzz.yml`.
8. Measurements; OpenSSF Scorecard run once and its score recorded;
   Results.

## GitHub settings for the owner

These need the owner's hands in GitHub; Claude sessions cannot change
repository settings. Task 4 writes them into `docs/security.md` with
what each one protects.

1. Settings > General: default branch = `development`; automatically
   delete head branches.
2. Settings > Rules > Rulesets: a branch ruleset on `main` and
   `development` (pull request required, status checks `ubuntu-latest`,
   `macos-latest`, `windows-latest`, `cargo deny` and `release binary
   size` required, block force pushes and deletion), with the owner
   allowed to bypass only in an emergency.
3. Settings > Rules > Rulesets: a tag ruleset on `v*` (restrict
   creation, update and deletion; owner bypass only).
4. Settings > Environments: a `release` environment, required reviewer
   the owner, deployment tags `v*` only.
5. Settings > Actions > General: allow only actions from GitHub,
   verified creators and the SHA-pinned list; require approval for
   workflows from all outside contributors; workflow permissions "Read
   repository contents"; allow Actions to create pull requests (for
   `back-merge.yml`).
6. Settings > Code security: Dependabot security updates on (cargo
   advisories get a pull request even though routine cargo updates
   stay manual); secret scanning and push protection on; private
   vulnerability reporting on.
7. Account: two-factor authentication with a passkey or hardware key;
   review Settings > Applications and personal access tokens, removing
   any with write access that are no longer used.
8. Delete the `MINISIGN_SECRET_KEY` secret if it was ever created.

## Concepts

- **Supply chain attack**: getting malicious code into a program
  through something it trusts (a dependency, a build tool, a workflow
  action, a maintainer's account) instead of through the program itself.
- **Signatures versus hashes**: a hash says a file was not damaged; a
  signature says who vouched for it. Anyone who can replace the file can
  replace its hash, but not forge the signature without the key.
- **Pinning by SHA**: why a commit hash is the only name for code that
  cannot be pointed somewhere else.
- **Provenance**: a signed record, made by GitHub's builder, of which
  commit and workflow produced a file.
- **Trojan Source**: text whose display order differs from its byte
  order, so code reads differently from how it compiles.

## Risks

| Risk | Fallback |
|---|---|
| Local signing is one more release step and gets skipped | The README says publish only after the script ran; M31 refuses unsigned releases, so a skipped step shows at once |
| The key is lost (Mac fails, no backup) | The backup is a done-when item; if both are lost, a new key ships in a release users install by hand, as before M31 |
| `cargo vet` exemptions are a long list nobody reads | Only diffs after the baseline are reviewed; imports shrink the list over time; if the upkeep is too much the owner can drop back to `cargo deny` alone |
| SHA pins break when an action deletes old commits | Dependabot moves pins weekly; a broken pin fails CI loudly, never silently |
| Rulesets block the Claude sessions' usual flow | They open pull requests already; only direct pushes to `main` and `development` stop, which is the point |
| Provenance or Scorecard features change on GitHub's side | Both are additions; the signature is what users and M31 rely on |

## Results

_To be filled in when the milestone lands._
