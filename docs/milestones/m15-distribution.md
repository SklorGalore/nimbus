# M15 — Distribution

**Status: landed 2026-10-17, with the owner's steps listed in Results.**
Written on 2026-10-17 from the items deferred
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

- [x] **macOS**: a `.dmg` with the Applications shortcut, and a Homebrew
      cask in a tap. The `.app` stays unsigned and un-notarised (no Apple
      Developer account), so Gatekeeper blocks the first launch; the
      README's `xattr` step stays, and the cask carries the same note.
      The workflow's signing and notarisation steps, if added later, are
      conditional on secrets that do not exist yet.
- [x] **Windows**: an installer (`.msi` via WiX, or `.exe` via Inno
      Setup; pick the one that is simplest in GitHub Actions) that adds a
      Start Menu entry and "Open with Nimbus". Unsigned (no Authenticode
      certificate): SmartScreen warns until downloads accumulate, and the
      README says how to get past it. A `winget` manifest.
- [x] **Linux**: a `.deb`, an AppImage, and the existing tarball; the
      `.desktop` file and icon installed by the packages.
- [ ] A manual QA pass on Wayland and X11 (M8 left both unrun): HiDPI,
      IME, the clipboard, drag-resize, and keyboard shortcuts that the
      desktop grabs. Needs a Linux desktop; the container this landed
      from has none (owner's step).
- [x] **Check for Updates**: a palette action that fetches the latest
      release's tag from GitHub, compares it with the running version and
      offers the release page. On demand only; an automatic check at
      startup is off unless `check_updates = true` in the config (a
      personal tool should not phone home without being asked). Fetching
      uses the system's certificate store through a
      small HTTP client measured for binary size, or shells out to
      nothing: a dependency decision in task 4.
- [x] **Release hygiene**: a `CHANGELOG.md` kept per release;
      `cargo-deny` in CI for licences and advisories; a `cargo bloat`
      review recorded in Results (the last was M8); `SHA256SUMS` signed
      with `minisign` (one small binary on every platform) and the public
      key published in the repository.
- [x] The README's "Getting a build" section explains each platform's
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

## Results

Landed from a Linux container without a display, so the on-screen
numbers (`--smoke`, the installers run by hand) are the owner's to fill
in; everything else was run here.

| Metric | Value |
|---|---|
| Linux release binary | 21.7 MB (20.0 MB before the update check's HTTP client; budget 35 MB) |
| Update check: `ureq` 3 with rustls and the platform verifier | +1.7 MB, 10 s timeout, 1 MB body limit |
| Live check against GitHub (`cargo test -p nimbus live_check -- --ignored`) | 1.0 s through a proxy, with the system's certificate store |
| `.deb` | 6.7 MB, installs to `/usr/bin`, `/usr/share/applications`, `/usr/share/icons` |
| AppImage | 8.5 MB; runs with `APPIMAGE_EXTRACT_AND_RUN=1` where FUSE is missing |
| `--smoke` first frame | owner to measure (the check runs after the first frame, so it cannot move this) |
| `cargo deny check` | advisories, bans, licences and sources ok; 9 duplicate-version warnings (set by wgpu, winit, gtk) |
| `cargo bloat --release --crates` (Linux) | .text 10.2 MB: std 1.6 MB, naga 1.3 MB, wgpu_core 623 KB, nimbus 534 KB, winit 474 KB, wgpu_hal 436 KB, rustls 372 KB, regex_automata 309 KB, skrifa 288 KB, ring 272 KB; the update check is rustls + ring + the rest of ureq, about 1 MB of code and the balance in data |

**Owner's steps** (each needs an account or a machine this session
does not have):

1. Make the signing key: `minisign -G -W`, commit `minisign.pub` at the
   repository root, put the secret key file's contents in the
   `MINISIGN_SECRET_KEY` Actions secret. The workflow signs `SHA256SUMS`
   once the secret exists and says "skipped" until then.
2. Tag a release and try each package on its platform: the `.dmg`
   (Gatekeeper's warning, then the `xattr` step), the Windows installer
   (SmartScreen's "More info" > "Run anyway", the Start Menu entry, "Open
   with Nimbus"), the `.deb` and the AppImage.
3. Homebrew: create a `homebrew-nimbus` repository, copy the release's
   `nimbus.rb` to `Casks/nimbus.rb`; `brew install --cask
   sklorgalore/nimbus/nimbus`. winget: open a pull request to
   `microsoft/winget-pkgs` with the release's `winget-manifest.zip`
   contents under `manifests/s/SklorGalore/Nimbus/VERSION/`. Both can be
   automated with a token later; the files are generated from the tag so
   they cannot drift.
4. The Wayland and X11 QA pass above.

**Design changes**

- **Installers are scripts and one Inno Setup file**, not cargo plugins:
  `dpkg-deb`, `hdiutil` and `appimagetool` are each one command, so there
  is nothing to version besides the scripts. Inno Setup over WiX because
  it is on every `windows-latest` runner and its script is readable.
- **The update check reads one JSON field by hand** (`tag_name`) and
  builds the release URL from the tag, instead of adding `serde_json`.
- **Opening the release page** shells out to `open`, `rundll32` or
  `xdg-open` with an `https://` check, rather than adding a crate.
- **No in-app download**: the check says a version exists and opens the
  page; installing stays with the package or installer, as planned.

**Concepts**

- *Code signing and notarisation* (`.github/workflows/release.yml`,
  README): macOS and Windows check a signature made with a certificate
  that an identified developer paid for, and notarisation is Apple
  scanning the build and vouching for it. An unsigned build has neither,
  so the system warns once and lets the person decide. A minisign
  signature on `SHA256SUMS` promises something narrower: the file was
  made by whoever holds the key in the repository's secrets. The OS does
  not check it; a person does, with one command.
- *Reproducible release steps* (`scripts/`, `packaging/`): every package
  is built by a script that the workflow runs on a tag, and the cask and
  winget manifest are filled in from the files as uploaded, so a release
  is the same whoever pushes the tag and the hashes match what was
  shipped.
- *The system certificate store* (`crates/app/src/update.rs`): TLS
  trusts a server because a chain of certificates leads to a root the
  computer already trusts. Bundling Mozilla's root list works until a
  corporate proxy or test certificate is in play; asking the OS
  (`rustls-platform-verifier`) trusts what the computer trusts.
- *On-demand rendering and background work*: the check runs on its own
  thread and reports through the event-loop proxy like the file watcher,
  so a slow network never holds a frame, and the startup check starts
  after the first frame like the watches.

**Surprises**

- `ldd` lists no Wayland or xkbcommon library: winit opens both with
  `dlopen`, so the `.deb` names them as dependencies by hand.
- `appimagetool` is itself an AppImage and needs FUSE to run; the script
  sets `APPIMAGE_EXTRACT_AND_RUN=1` so a plain container works.
