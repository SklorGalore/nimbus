# M31 — Updates

**Status: done (2026-10-19).** Written on 2026-10-18 at the owner's
request, from what M15 deferred: "automatic in-place updates
(downloading and replacing the binary)"; the owner answered its three
questions on 2026-10-19 (`DESIGN.md` §9). Numbered after security (M26)
and the git client plans (M27–M30). Wider hardening of the program, the
repository and the supply chain is M26's; this plan covers only getting
a release onto the machine and checking it before it runs.

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

- [x] **Install Update**: after a check finds a newer release, the
      message line offers "Install Update" (palette action and the Help
      menu on macOS and Windows). Running it downloads the release's
      package for this platform on a worker, verifies it, and when it is
      ready says "Nimbus X.Y.Z is ready; restart to finish". The
      download shows its progress in the status bar and can be
      cancelled; the editor stays usable throughout.
- [x] **On request only** (owner, 2026-10-19): nothing downloads until
      the person runs Install Update; the startup check (when on) only
      tells them. Downloading in the background and installing on quit
      were declined for now; a config key could add them later.
- [x] **The startup check is on by default** (owner, 2026-10-19):
      `check_updates` defaults to `true`, so each start asks GitHub once,
      after the first frame, and speaks only when a newer release
      exists. `check_updates = false` turns it off, and `--headless`,
      `--smoke` and the other unattended runs never check. The README,
      the manual and the config template stop saying Nimbus never
      contacts the network unless asked, and say what the check sends
      (one HTTPS request to GitHub's API with a `nimbus/X.Y.Z` user
      agent, nothing else) and how to turn it off; the CHANGELOG calls
      out the changed default.
- [x] **Verification before anything is written in place**:
      `SHA256SUMS.minisig` is checked against the public key compiled
      into the binary; its trusted comment must read exactly
      `Nimbus X.Y.Z SHA256SUMS` for the version being installed (so an
      older signed release cannot be replayed as a newer one), and that
      version must be newer than the running one (no downgrades); the
      package's SHA-256 must match its line in `SHA256SUMS`. Any failure
      stops the update, deletes the download and says why. A release
      with no signature is never installed; Nimbus falls back to Open
      Latest Release Page.
- [x] **Restart without losing work**: "Restart to Update" asks about
      unsaved tabs the same way quitting does, swaps the new version in,
      starts it with the session restored (M12) and quits. If the person
      quits instead, the swap happens on quit and the next launch is
      the new version.
- [x] **Per platform**:
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
- [x] **Package managers keep their installs** (owner, 2026-10-19). When Nimbus
      was installed by Homebrew (the app lives under Homebrew's
      `Caskroom`), as a `.deb` (`/usr/bin/nimbus`), or anywhere Nimbus
      cannot write, Install Update is replaced by the command to run
      (`brew upgrade --cask nimbus`, `sudo apt install ./nimbus_….deb`)
      and Open Latest Release Page. winget installs use the same Inno
      Setup installer and update in place; winget sees the new version
      on its next listing.
- [x] **Rollback**: the previous version is kept beside the new one
      (`Nimbus.app.previous`, `nimbus.previous`) until the new one has
      drawn its first frame, then removed. If the new version fails to
      start, the next launch of the old one, or "Roll Back Update", puts
      it back.
- [x] **Tests without the network**: a local test server serves a fake
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

**Prerequisite: the signing key.** M26 (security) makes the key, keeps
it on the owner's Mac with `scripts/sign-release.ps1`, and commits
`minisign.pub`; that, and one release signed with it, has to be done
before task 3 can be tested end to end. Where the
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

_Written 2026-10-19 from a cloud container (Linux, no screen, no Mac or
Windows). The download, verification and the Linux swap are tested
here. The owner ran check 1 below on the M5 Pro the same day (a build
numbered 1.0.0 updated itself to the signed 1.0.1) and reported it
done; no timings were recorded. Windows and the AppImage are still
untried on real machines._

**What landed.**

- `crates/app/src/update/verify.rs`: the signature check.
  `minisign.pub` is compiled in with `include_str!` (`TRUSTED_KEYS`,
  a list so a rotated key can sit beside the old one). `verify_sums`
  refuses a version that is not newer, a signature no trusted key made,
  and a trusted comment other than exactly `Nimbus X.Y.Z SHA256SUMS`;
  only prehashed (`ED`) signatures, which is what `sign-release.ps1`
  makes. Its tests use the owner's real v1.0.0 and v1.0.1 signatures
  (copied into `update/testdata`) and a throwaway test key
  (`make-fixtures.py` makes it and the fake release).
- `crates/app/src/update/download.rs`: fetches `SHA256SUMS.minisig`
  and `SHA256SUMS` first (a 404 on the signature means "unsigned, not
  installed"), checks them before the large download, then streams the
  package to the cache folder, hashing as it goes with `ring`'s SHA-256
  (already linked by rustls). HTTPS only, redirects included; 200 MB
  cap; Cancel Update is a flag checked between 64 KB reads; progress
  every 256 KB.
- `crates/app/src/update/install.rs`: which install this is
  (`detect_from`, tested for each kind), unpacking beside the running
  copy (`ditto` on macOS, `tar` for the tarball, a copy for the
  AppImage), the two-rename swap with undo, Roll Back Update, the
  restart, and the `update-pending` note that rolls back a version that
  never draws its first frame.
- `crates/app/src/app/updates.rs`: the four actions (Install Update,
  Restart to Update, Cancel Update, Roll Back Update; the first two in
  the Help menu), the worker, progress on the message line, and
  `take_update`, which `main` calls after the event loop ends to make
  the swap and restart. Quitting with an update ready installs it too.
- `check_updates` defaults to `true`; README, manual
  (`troubleshooting.md`, `settings.md`, `keybindings.md`), config
  template and CHANGELOG say so and say what the request sends.
- Release workflow: fails if a package the updater downloads is
  missing. `nimbus.iss` gains a `/RELAUNCH=1` parameter that starts
  Nimbus after a silent install.
- `minisign-verify` 0.3.0 is the one new crate (no dependencies, no
  `unsafe`, no build script); it is exempted in `supply-chain/` with a
  note, not certified as reviewed.

**Deviations from the plan.**

- The previous version is kept until the *next* update, not deleted at
  the new version's first frame: deleting it then would leave Roll Back
  Update nothing to roll back to. The first frame only clears the
  "not started yet" note.
- On Windows the restart reopens the last session (Nimbus with no
  arguments) rather than passing the window's arguments through: the
  installer starts Nimbus, and it has no way to carry them. Windows
  keeps no previous version, so Roll Back Update says to use an older
  installer.
- Homebrew's cask copies `Nimbus.app` into `/Applications` rather than
  leaving it under `Caskroom`, so a Homebrew install is recognised by
  its `Caskroom/nimbus` record, not by the app's path.
- Builds from source (`target/…`, `~/.cargo/bin`) are never replaced;
  Install Update says to use git and cargo.
- Progress is shown on the message line ("Downloading Nimbus 1.2.0:
  3.1 of 22.4 MB"), which is part of the status bar, rather than as a
  separate status bar item.

**Measurements.** None from the cloud: binary size is checked by the
`release-size` CI job (the new code is small; `minisign-verify` is
about 2,900 lines with no dependencies). `--smoke` is unchanged:
nothing new runs before the first frame except reading
`update-pending` when it exists.

**Owner's checks on the Mac** (the cloud cannot run these):

1. Set `version = "1.0.0"` in the root `Cargo.toml` (locally, not
   committed), `cargo build --release`, bundle it with
   `scripts/bundle-macos.ps1 target/release/nimbus 1.0.0 /tmp/nb`, and
   copy the app to a folder you can write, outside `/Applications` if
   Homebrew owns that one. Open it, run Check for Updates (it finds the
   signed 1.0.1), Install Update, then Restart to Update. 1.0.1 should
   open with the session and no Gatekeeper prompt, and
   `Nimbus.app.previous` should sit beside it.
2. Roll Back Update can only be tried from a release that has it, so
   after 1.1.0 is released and signed: the same steps from a 1.1.0
   build numbered lower, then Roll Back Update and Restart to Update.
3. On a Homebrew install, Install Update should print the
   `brew upgrade --cask nimbus` line and download nothing.
4. Windows and the AppImage are untested on real machines; the Windows
   `/RELAUNCH=1` change to `nimbus.iss` is first compiled by the
   release workflow.
