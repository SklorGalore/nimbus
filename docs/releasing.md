# Making a release

Maintainer notes for cutting a Nimbus release. Users looking for downloads
want the [README](../README.md#installation) instead.

Releases follow the git flow branches in
[CONTRIBUTING.md](../CONTRIBUTING.md#branches): `main` only ever holds
released code.

1. Cut `release/X.Y.Z` from the latest `development`.
2. On that branch, set the version in `Cargo.toml`
   (`[workspace.package]`), run `cargo build` so `Cargo.lock` follows,
   and move the `CHANGELOG.md` "Unreleased" notes under a heading for
   that version. Only release fixes go on this branch from here on.
3. Open a pull request from `release/X.Y.Z` into `main`. Merge it once
   CI passes.
4. Tag the merge commit on `main` with that version, such as `v1.1.0`,
   and push the tag. The tag must match the version in `Cargo.toml`.
5. Merge `main` back into `development` (the back-merge workflow opens
   that pull request for you), so the version, changelog and
   any release fixes are there too.

The release workflow ([`release.yml`](../.github/workflows/release.yml))
runs on the tag. It builds every platform, makes a draft release with
the packages, the Homebrew cask, the winget manifest and `SHA256SUMS`
attached, and signs the checksums; publish it on GitHub when it looks
right.

## Hotfixes

An urgent fix to the released version starts from `main` instead:
branch `hotfix/X.Y.Z` (the patch version, such as `1.0.1`) from `main`,
make the fix, bump the patch version and add a changelog heading for it,
then follow steps 3 to 5 above. When merging back into `development`,
keep `development`'s own version if it is higher.

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
