# M15 — Distribution

**Status: draft.** Written on 2026-10-17 from the items deferred by M8
(code signing and notarisation, auto-update, installers) and the Linux
checks M8 recorded as not run. Shorter than the other drafts because most
of its decisions are the owner's: accounts, money and how much to promise
to other people. Not yet approved.

## Goal

Someone who is not the owner can install Nimbus on each platform without
a terminal or a warning dialog, find out when a new version exists, and
trust what they downloaded.

## Done when

- [ ] **macOS**: the `.app` is signed with a Developer ID and notarised
      in the release workflow, so it opens with a double-click. Needs an
      Apple Developer account (owner decision; a yearly fee) and its
      secrets in the repository's GitHub Actions secrets. A `.dmg` with
      the Applications shortcut, and a Homebrew cask in a tap.
- [ ] **Windows**: an installer (`.msi` via WiX, or `.exe` via Inno
      Setup; pick the one that is simplest in GitHub Actions) that adds a
      Start Menu entry and "Open with Nimbus". Signing is **optional**:
      an Authenticode certificate costs more than the Apple account and
      SmartScreen warnings fade with downloads; the owner decides. A
      `winget` manifest.
- [ ] **Linux**: a `.deb`, an AppImage, and the existing tarball; the
      `.desktop` file and icon installed by the packages. A manual QA
      pass on Wayland and X11 (M8 left both unrun): HiDPI, IME, the
      clipboard, drag-resize, and keyboard shortcuts that the desktop
      grabs.
- [ ] **Check for Updates**: a palette action that fetches the latest
      release's tag from GitHub, compares it with the running version and
      offers the release page. On demand only; an automatic check at
      startup is **off** unless `check_updates = true` in the config
      (**Proposed**; a personal tool should not phone home without being
      asked). Fetching uses the system's certificate store through a
      small HTTP client measured for binary size, or shells out to
      nothing: a dependency decision in task 4.
- [ ] **Release hygiene**: a `CHANGELOG.md` kept per release;
      `cargo-deny` in CI for licences and advisories; a `cargo bloat`
      review recorded in Results (the last was M8); `SHA256SUMS` signed
      with a key published in the repository (**Proposed**: `minisign`,
      which is one small binary on every platform).
- [ ] The README's "Getting a build" section needs no `xattr` line.

## Scope

**In**: signing and notarisation, installers and package manifests,
update check, release hygiene, Linux QA.

**Deferred**: automatic in-place updates (downloading and replacing the
binary; the update check plus the package managers cover it with far
less to go wrong), Flatpak and Snap (added if someone asks), a Mac App
Store build (sandboxing would break opening arbitrary folders).

## Design

Everything here is in `.github/workflows/release.yml`, `scripts/` and
the packaging files it needs; the editor itself changes only for the
update check (`crates/app/src/update.rs`) and its config key. The
workflow keeps building unsigned artefacts when the signing secrets are
absent, so forks and the owner's own test tags keep working.

Secrets never appear in the repository: the Apple certificate and
notarisation credentials, and any Windows certificate, live in GitHub
Actions secrets, and the workflow's signing steps are conditional on
them.

## Tasks

1. macOS signing and notarisation in the workflow; the `.dmg`; the cask.
2. Windows installer; `winget` manifest; signing if the owner chooses.
3. Linux `.deb` and AppImage; the Wayland and X11 QA pass with its
   findings fixed or filed as known limitations.
4. Check for Updates, its dependency measured, its config key.
5. `CHANGELOG.md`, `cargo-deny`, signed checksums, README update,
   `cargo bloat` review; Results.

## Concepts

- **Code signing and notarisation**: what the OS checks and why the
  warning appears for unsigned builds.
- **Reproducible release steps**: why packaging lives in the workflow
  and scripts, not in a person's memory.

## Risks

| Risk | Fallback |
|---|---|
| Notarisation rejects the binary (hardened runtime entitlements) | The entitlements file is small; wgpu needs none beyond defaults; document what was needed |
| An HTTP client adds megabytes | The check can read the GitHub API with a minimal client, or be dropped in favour of the package managers' own update paths |
| Package manifests rot between releases | The workflow generates them from the tag |
