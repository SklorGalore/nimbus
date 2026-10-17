# M15 — Distribution

**Status: planned, last.** Written on 2026-10-17 from the items deferred
by M8 (code signing and notarisation, auto-update, installers) and the
Linux checks M8 recorded as not run. Shorter than the other plans because
most of its decisions were the owner's: accounts, money and how much to
promise to other people. Approved the same day: no Apple Developer
account and no Windows certificate, so builds ship unsigned; the update
check is off unless asked for; checksums are signed with `minisign`
(`DESIGN.md` §9).

## Goal

Someone who is not the owner can install Nimbus on each platform from a
package or installer, find out when a new version exists, and check that
what they downloaded is what was released. Builds are unsigned, so macOS
and Windows still show their first-run warnings; the README says what to
do about them.

## Done when

- [ ] **macOS**: a `.dmg` with the Applications shortcut, and a Homebrew
      cask in a tap. The `.app` stays unsigned and un-notarised (no Apple
      Developer account), so Gatekeeper blocks the first launch; the
      README's `xattr` step stays, and the cask carries the same note.
      The workflow's signing and notarisation steps, if added later, are
      conditional on secrets that do not exist yet.
- [ ] **Windows**: an installer (`.msi` via WiX, or `.exe` via Inno
      Setup; pick the one that is simplest in GitHub Actions) that adds a
      Start Menu entry and "Open with Nimbus". Unsigned (no Authenticode
      certificate): SmartScreen warns until downloads accumulate, and the
      README says how to get past it. A `winget` manifest.
- [ ] **Linux**: a `.deb`, an AppImage, and the existing tarball; the
      `.desktop` file and icon installed by the packages. A manual QA
      pass on Wayland and X11 (M8 left both unrun): HiDPI, IME, the
      clipboard, drag-resize, and keyboard shortcuts that the desktop
      grabs.
- [ ] **Check for Updates**: a palette action that fetches the latest
      release's tag from GitHub, compares it with the running version and
      offers the release page. On demand only; an automatic check at
      startup is off unless `check_updates = true` in the config (a
      personal tool should not phone home without being asked). Fetching
      uses the system's certificate store through a
      small HTTP client measured for binary size, or shells out to
      nothing: a dependency decision in task 4.
- [ ] **Release hygiene**: a `CHANGELOG.md` kept per release;
      `cargo-deny` in CI for licences and advisories; a `cargo bloat`
      review recorded in Results (the last was M8); `SHA256SUMS` signed
      with `minisign` (one small binary on every platform) and the public
      key published in the repository.
- [ ] The README's "Getting a build" section explains each platform's
      install path and its first-run warning: the `xattr` step on macOS,
      the SmartScreen prompt on Windows, and how to verify the signed
      checksums.

## Scope

**In**: installers and package manifests, update check, release
hygiene, Linux QA.

**Deferred**: code signing and notarisation (the owner decided against
the Apple Developer account and a Windows certificate; the workflow is
laid out so they can be added later behind secrets), automatic in-place
updates (downloading and replacing the binary; the update check plus
the package managers cover it with far less to go wrong), Flatpak and
Snap (added if someone asks), a Mac App Store build (sandboxing would
break opening arbitrary folders).

## Design

Everything here is in `.github/workflows/release.yml`, `scripts/` and
the packaging files it needs; the editor itself changes only for the
update check (`crates/app/src/update.rs`) and its config key. The
workflow builds unsigned artefacts; if signing is ever added, its steps
are conditional on GitHub Actions secrets, so forks and the owner's own
test tags keep working without them.

The one key that does exist is the `minisign` secret key that signs
`SHA256SUMS`. It lives in GitHub Actions secrets, never in the
repository; only its public key is checked in.

## Tasks

1. macOS `.dmg` and the Homebrew cask, with the first-launch note.
2. Windows installer and `winget` manifest.
3. Linux `.deb` and AppImage; the Wayland and X11 QA pass with its
   findings fixed or filed as known limitations.
4. Check for Updates, its dependency measured, its config key.
5. `CHANGELOG.md`, `cargo-deny`, signed checksums, README update,
   `cargo bloat` review; Results.

## Concepts

- **Code signing and notarisation**: what the OS checks, why the
  warning appears for unsigned builds, and what a signed checksum does
  and does not promise instead.
- **Reproducible release steps**: why packaging lives in the workflow
  and scripts, not in a person's memory.

## Risks

| Risk | Fallback |
|---|---|
| Gatekeeper or SmartScreen puts people off an unsigned build | The README's steps and the signed checksums are the answer for now; signing can be added behind secrets if an account is bought later |
| An HTTP client adds megabytes | The check can read the GitHub API with a minimal client, or be dropped in favour of the package managers' own update paths |
| Package manifests rot between releases | The workflow generates them from the tag |
