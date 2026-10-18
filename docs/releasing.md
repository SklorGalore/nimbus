# Making a release

Maintainer notes for cutting a Nimbus release. Users looking for downloads
want the [README](../README.md#installation) instead.

Set the version in `Cargo.toml` (`[workspace.package]`), move the
`CHANGELOG.md` "Unreleased" notes under a heading for that version,
commit, then push a tag with that version, such as `v0.8.0`. The release
workflow ([`release.yml`](../.github/workflows/release.yml)) builds every
platform, makes a draft release with the packages, the Homebrew cask,
the winget manifest and `SHA256SUMS` attached, and signs the checksums;
publish it on GitHub when it looks right.

## Packaging scripts and signing setup

The packaging steps are `scripts/bundle-macos.sh`,
`scripts/dmg-macos.sh`, `scripts/deb-linux.sh`,
`scripts/appimage-linux.sh`, `packaging/windows/nimbus.iss` and
`scripts/manifests.sh`. The app icon is drawn by `scripts/make-icons.py`.

Signing `SHA256SUMS` needs a one-time setup: run `minisign -G -W` (no
password, since the key lives in a secret), commit the public key as
`minisign.pub`, and put the secret key file's contents in the
repository's `MINISIGN_SECRET_KEY` Actions secret. Until then the
workflow says the signing step was skipped and the release has no
`.minisig`.

The cask and winget manifest are attached for copying on: the cask goes
in a tap (`Casks/nimbus.rb` in a `homebrew-TAP` repository) and the
manifest in a pull request to `microsoft/winget-pkgs`.
