# M31 — Updates

**Status: draft (2026-10-18), awaiting the owner's answers.** Written on
2026-10-18 at the owner's request, from what M15 deferred: "automatic
in-place updates (downloading and replacing the binary)". Numbered after
security (M26) and the git client plans (M27–M30). Items marked
**Proposed** are the drafter's defaults until the owner answers them.
Wider hardening of the program, the repository and the supply chain is
M26's; this plan covers only getting a release onto the machine and
checking it before it runs.

## What exists today

Nimbus 1.x never updates itself (`crates/app/src/update.rs`, M15):

- **Check for Updates** in the palette asks GitHub's "latest release"
  endpoint for `tag_name` and says whether it is newer than the running
  version. With `check_updates = true` in `config.toml` the same check
  runs once after the first frame and speaks only when a newer release
  exists. The default is off: a personal tool should not phone home
  unless asked.
- **Open Latest Release Page** opens the releases list in the browser,
  and the person installs the new package the way they installed the
  first one (DMG, Homebrew cask, winget, Inno Setup installer, `.deb`,
  AppImage or tarball).
- Every release has a `SHA256SUMS` and, once the owner's key exists, a
  `minisign` signature over it. Nothing in Nimbus reads either today, and
  `minisign.pub` is not in the repository yet (M15 Results, owner's step
  1), so releases so far are unsigned.

## Goal

When a newer release exists, Nimbus can fetch it, prove it is the
owner's signed release for that version, put it in place and restart
into it with the session restored, in one step and without a browser,
a password prompt or a Gatekeeper or SmartScreen warning.

## Done when

- [ ] **Install Update**: after a check finds a newer release, the
      message line offers "Install Update" (palette action and the Help
      menu on macOS and Windows). Running it downloads the release's
      package for this platform on a worker, verifies it, and when it is
      ready says "Nimbus X.Y.Z is ready; restart to finish". The
      download shows its progress in the status bar and can be
      cancelled; the editor stays usable throughout.
- [ ] **On request only** (owner, 2026-10-19): nothing downloads until
      the person runs Install Update; the startup check (when on) only
      tells them. Downloading in the background and installing on quit
      were declined for now; a config key could add them later.
- [ ] **Proposed: the check stays off by default.** `check_updates`
      keeps its default of `false`. Install Update still works on
      demand after a manual Check for Updates.
- [ ] **Verification before anything is written in place**:
      `SHA256SUMS.minisig` is checked against the public key compiled
      into the binary; its trusted comment must read exactly
      `Nimbus X.Y.Z SHA256SUMS` for the version being installed (so an
      older signed release cannot be replayed as a newer one), and that
      version must be newer than the running one (no downgrades); the
      package's SHA-256 must match its line in `SHA256SUMS`. Any failure
      stops the update, deletes the download and says why. A release
      with no signature is never installed; Nimbus falls back to Open
      Latest Release Page.
- [ ] **Restart without losing work**: "Restart to Update" asks about
      unsaved tabs the same way quitting does, swaps the new version in,
      starts it with the session restored (M12) and quits. If the person
      quits instead, the swap happens on quit and the next launch is
      the new version.
- [ ] **Per platform**:
      - macOS: the `Nimbus-X.Y.Z-macos.zip` from the release is unpacked
        with `ditto` beside the running `Nimbus.app`, then swapped in
        with two renames. A file Nimbus downloads itself carries no
        quarantine attribute, so the new app opens without the
        Gatekeeper prompt.
      - Windows: the Inno Setup installer runs with
        `/VERYSILENT /SUPPRESSMSGBOXES /CLOSEAPPLICATIONS`; it installs
        per user (`PrivilegesRequired=lowest`), so there is no UAC
        prompt, and a file without the downloaded-from-the-internet mark
        gets no SmartScreen prompt.
      - Linux: an AppImage replaces its own file (`$APPIMAGE`); a
        tarball install replaces the binary when its folder is writable.
- [ ] **Proposed: package managers keep their installs.** When Nimbus
      was installed by Homebrew (the app lives under Homebrew's
      `Caskroom`), as a `.deb` (`/usr/bin/nimbus`), or anywhere Nimbus
      cannot write, Install Update is replaced by the command to run
      (`brew upgrade --cask nimbus`, `sudo apt install ./nimbus_….deb`)
      and Open Latest Release Page. winget installs use the same Inno
      Setup installer and update in place; winget sees the new version
      on its next listing.
- [ ] **Rollback**: the previous version is kept beside the new one
      (`Nimbus.app.previous`, `nimbus.previous`) until the new one has
      drawn its first frame, then removed. If the new version fails to
      start, the next launch of the old one, or "Roll Back Update", puts
      it back.
- [ ] **Tests without the network**: a local test server serves a fake
      release; tests cover a good update, a wrong hash, a bad signature,
      a signature for another version, a truncated download, no space
      left, and a folder Nimbus cannot write. `--smoke` is unchanged
      (nothing here runs before the first frame).

## Scope

**In**: download, verification, swap and restart on the three platforms;
detecting installs that belong to a package manager; rollback; the
release workflow changes the updater needs; the manual's "Checking for
updates" section.

**Deferred**: delta or patch updates (releases are under 25 MB);
beta or nightly channels; updating `.deb` installs through an apt
repository; code signing and notarisation, which stay declined (M15);
key rotation beyond the one documented below. Wider supply-chain work
(dependency review, pinned CI actions, reproducible builds) is M26's.

## Design

**No new trust, only the key the releases already use.** The
`minisign` public key from `minisign.pub` is compiled into the binary
with `include_str!`. Nimbus trusts a package only when that key signed
the `SHA256SUMS` that lists it. HTTPS (`ureq` with the system's
certificates, as today) protects the download in transit; the signature
protects it against a changed release page or a compromised GitHub
account without the key. Verification uses `minisign-verify` (a small
crate with no dependencies by minisign's author, measured for size) and
`ring`'s SHA-256, which rustls already brings in, so no hashing crate is
added.

**Prerequisite: the signing key.** The owner's M15 step 1 (make the key,
commit `minisign.pub`, set `MINISIGN_SECRET_KEY`) has to be done, and
one release signed, before task 3 can be tested end to end. Where the
secret key lives, its backup, rotation and revocation, and the signing
pipeline itself are M26's. The contract between the two plans: the app
trusts exactly the key or keys embedded at build time, the signature is
over `SHA256SUMS`, and its trusted comment carries the version. If M26
ships a second key for rotation, the updater accepts either.

**Asset names, not JSON.** The release packages already have fixed
names (`Nimbus-X.Y.Z-macos.zip`, `nimbus-X.Y.Z-windows-x86_64-setup.exe`,
`Nimbus-X.Y.Z-x86_64.AppImage`, …). The updater builds each URL from the
tag it already reads, so the hand-written `tag_name` scan stays and no
JSON crate is needed. A test in the release workflow checks the names
the updater expects are the names it uploads.

**A module, a state machine and a worker.** `update.rs` grows
`download`, `verify` and `install` steps behind an `Updater` state
(`Idle`, `Downloading { got, total }`, `Ready(PathBuf)`, `Failed`). Each
step runs on a worker and posts an `AppEvent`, as the check does today;
the event loop stays on `ControlFlow::Wait`, and the progress shown in
the status bar is redrawn only when a new chunk arrives. Downloads go to
a folder in the cache directory, never next to the user's files.

**Swapping a running program.** macOS and Linux let a running program's
files be renamed: the new copy is unpacked beside the old, then
`rename(old, old.previous)` and `rename(new, old)`, both on one volume
so each is atomic. Windows will not replace a running `.exe`, which is
why the installer does it after Nimbus exits (`/CLOSEAPPLICATIONS`).
The restart starts the new binary with the current session file, then
the old process exits.

**Every new operation is an `Action`**: Install Update, Restart to
Update, Cancel Update, Roll Back Update, in `action.rs` with stable ids.

## Tasks

1. Release workflow: confirm the zip, setup and AppImage names; a check
   that the expected names exist; the trusted comment format pinned in
   a test.
2. `verify`: `minisign-verify`, the compiled-in key, the version in the
   trusted comment, SHA-256 against `SHA256SUMS`; tests with a test key
   for every failure case.
3. `download` on a worker with progress, cancel, size limit and the
   test server; the status bar progress.
4. Install and restart on macOS (`ditto`, renames, relaunch) with
   rollback.
5. Windows (silent Inno Setup run) and Linux (AppImage, tarball).
6. Detecting package-manager installs and the messages for them.
7. Manual (`troubleshooting.md`, `getting-started.md`), README "Updates"
   section, CHANGELOG; measurements; Results.

## Concepts

- **Signatures versus checksums**: a checksum says the file is the one
  listed; a signature says the owner made the list. Why both are
  needed, and why the version goes inside the signed comment.
- **Quarantine and the mark of the web**: why a browser download is
  blocked on an unsigned app and Nimbus's own download is not, and why
  that makes the signature check the only gate.
- **Replacing a running program**: what renaming does to an open file
  on macOS, Linux and Windows.

## Risks

| Risk | Fallback |
|---|---|
| The signing key is lost or leaks | Updates through Nimbus stop until a build trusting a new key is installed; custody, backup and rotation are planned in M26 |
| A swap fails halfway (power cut, full disk) | The two renames are atomic one at a time; at worst the old app is at `.previous` and the next start of Nimbus or the release page restores it |
| `/Applications` is not writable for this user | Detected before downloading; Nimbus offers the release page instead |
| Homebrew or winget sees a version it did not install | Homebrew installs are left to `brew`; winget uses the same installer, so its record stays right |
| The download adds to the binary | `minisign-verify` is small and the rest is already linked; measured, budget 35 MB |

## Results

_To be filled in when the milestone lands._
