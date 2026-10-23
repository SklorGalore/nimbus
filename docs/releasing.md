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
the packages, the Homebrew cask and `SHA256SUMS` attached, and then
the Chocolatey package (`nimbus.X.Y.Z.nupkg`, packed on Windows by
[`chocolatey.yml`](../.github/workflows/chocolatey.yml)). Sign `SHA256SUMS` on the Mac (below), then publish the draft
on GitHub when it looks right. Never publish a release without its
`SHA256SUMS.minisig`.

## Hotfixes

An urgent fix to the released version starts from `main` instead:
branch `hotfix/X.Y.Z` (the patch version, such as `1.0.1`) from `main`,
make the fix, bump the patch version and add a changelog heading for it,
then follow steps 3 to 5 above. When merging back into `development`,
keep `development`'s own version if it is higher.

## Packaging scripts and signing setup

The packaging steps are `scripts/bundle-macos.ps1`,
`scripts/dmg-macos.ps1`, `scripts/deb-linux.ps1`,
`scripts/appimage-linux.ps1`, `packaging/windows/nimbus.iss` and
`scripts/manifests.sh`. The app icon is drawn by `scripts/make-icons.py`.

The release key is a minisign key made on the owner's Mac with a
password (`minisign -G`). Its public half is `minisign.pub` at the
repository root (key ID `A5DFCD31F80FDD0F`); the secret half never goes
into GitHub, so a compromised account or workflow cannot sign a release
(`docs/milestones/m26-security.md`). Keep an offline copy of the secret
key file.

Once the workflow has attached everything to the draft, sign it from
a checkout on the Mac:

```sh
scripts/sign-release.sh vX.Y.Z            # key in ~/.minisign/minisign.key
scripts/sign-release.sh vX.Y.Z PATH/TO/minisign.key
```

It downloads the packages and `SHA256SUMS`, refuses to sign unless
every package matches its hash and `SHA256SUMS` lists exactly the
packages on the release, signs (minisign asks for the password), checks
the signature against `minisign.pub`, and uploads
`SHA256SUMS.minisig`. A release that already has a signature is left
alone. Needs `gh` (signed in) and `minisign` (`brew install minisign`).

The trusted comment must be exactly `Nimbus X.Y.Z SHA256SUMS`: the
updater (M31) reads the version from it. The script also signs an already
published release: run it for `v1.0.0` and `v1.0.1`, which shipped
without signatures.

The workflow has no signing step and reads no key: leave the
`MINISIGN_SECRET_KEY` secret unset (delete it if it exists). Why, and
what to do if the key is lost or leaks: [security.md](security.md).

The first job waits in the `release` environment for your approval
(Actions tab, the run, **Review deployments**); nothing builds before
that.

## Homebrew and Chocolatey

Once the release is signed and published, put it in the package
managers from the same checkout:

```sh
scripts/publish-packages.sh vX.Y.Z --dry-run   # print the cask and Chocolatey scripts only
scripts/publish-packages.sh vX.Y.Z             # publish both, after asking
scripts/publish-packages.sh vX.Y.Z --only homebrew   # or --only chocolatey
```

It downloads `SHA256SUMS` and `SHA256SUMS.minisig`, refuses to go on
unless the signature checks against `minisign.pub` with the trusted
comment `Nimbus X.Y.Z SHA256SUMS`, and takes the `.dmg` and Windows
installer hashes from that signed file. It fills in the templates in
`packaging/`, checks the release is published and its download links
work, shows the files, and asks before sending anything. Then:

- Homebrew: it writes `Casks/nimbus.rb` in the tap
  `SklorGalore/homebrew-nimbus` (offering to create the tap the first
  time; `--tap OWNER/homebrew-NAME` picks another), so people install
  with `brew install --cask sklorgalore/nimbus/nimbus`. It never moves
  the tap back to an older version.
- Chocolatey: it downloads the release's `nimbus.X.Y.Z.nupkg`, checks it
  holds only the package files and that its install and uninstall
  scripts are byte for byte the templates in `packaging/chocolatey`
  filled in from the signed `SHA256SUMS` (they carry the installer's URL
  and hash), and pushes it to the community repository. It needs
  your API key from <https://community.chocolatey.org/account>: from
  `CHOCOLATEY_API_KEY` if set, else from the Keychain item
  `chocolatey-api-key`, else it asks. It never echoes or saves the key.
  Save it in the Keychain once with
  `security add-generic-password -a "$USER" -s chocolatey-api-key -w`,
  which prompts for it, so it stays out of your shell history and
  dotfiles. Chocolatey's
  automatic checks run first, then a moderator approves the package
  (the first version is reviewed most closely); people install it with
  `choco install nimbus`. For a release made before the Chocolatey
  workflow existed, run it first: Actions > Chocolatey package > Run
  workflow, with the tag.

Running it again for a version already published changes nothing.
Needs `gh` (signed in), `minisign`, `curl` and `unzip`. `--dry-run` also works on
a draft, to look at the files before publishing.
