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
attached. Sign `SHA256SUMS` on the Mac (below), then publish the draft
on GitHub when it looks right. Never publish a release without its
`SHA256SUMS.minisig`.

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

The release key is a minisign key made on the owner's Mac with a
password (`minisign -G`). Its public half is `minisign.pub` at the
repository root (key ID `A5DFCD31F80FDD0F`); the secret half never goes
into GitHub, so a compromised account or workflow cannot sign a release
(`docs/milestones/m26-security.md`). Keep an offline copy of the secret
key file. Until M26 adds `scripts/sign-release.sh`, sign each draft by
hand, after the workflow has attached everything:

```sh
v=X.Y.Z
mkdir -p ~/nimbus-release/$v && cd ~/nimbus-release/$v
gh release download v$v --repo SklorGalore/nimbus --clobber \
  --pattern '*.zip' --pattern '*.tar.gz' --pattern '*.dmg' \
  --pattern '*.deb' --pattern '*.AppImage' --pattern '*-setup.exe' \
  --pattern SHA256SUMS
shasum -a 256 -c SHA256SUMS                 # every line must say OK
minisign -S -m SHA256SUMS -t "Nimbus $v SHA256SUMS"   # asks for the password
minisign -V -p /path/to/nimbus/minisign.pub -m SHA256SUMS
gh release upload v$v SHA256SUMS.minisig --repo SklorGalore/nimbus
```

The trusted comment must be exactly `Nimbus X.Y.Z SHA256SUMS`: the
updater (M31) reads the version from it. The same steps sign an already
published release (1.0.0 and 1.0.1 shipped without signatures).

The workflow's own signing step stays off: leave the
`MINISIGN_SECRET_KEY` secret unset. M26 removes that step.

The cask and winget manifest are attached for copying on: the cask goes
in a tap (`Casks/nimbus.rb` in a `homebrew-TAP` repository) and the
manifest in a pull request to `microsoft/winget-pkgs`.
