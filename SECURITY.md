# Security policy

## Reporting a problem

Please report security problems privately, not in a public issue: open
the repository's **Security** tab and choose **Report a vulnerability**
(GitHub's private vulnerability reporting). Only the maintainer sees the
report, and you can talk it through there before anything is public.

Useful things to include: the Nimbus version (`nimbus --version`), the
operating system, what you did, what happened, and a file or folder that
shows it if one is needed. Please do not include anything you would not
want the maintainer to keep.

## What to expect

Nimbus is maintained by one person in their spare time, so there is no
guaranteed response time. The aim is to acknowledge a report within a
week, agree on what it is and how serious, and ship a fix in a patch
release (a `hotfix/` branch from `main`, see
[CONTRIBUTING.md](CONTRIBUTING.md)) before saying anything in public.
You are credited in the release notes unless you would rather not be.

## Which versions get fixes

Only the latest release. A fix ships as a new patch release of the
current version; older versions are not patched, so please update.

## What counts

Anything that lets a file, a folder, or something downloaded make Nimbus
do more than show it: run a program, read or write files the user did
not open, reach the network, or crash and lose unsaved work in other
tabs. Also anything that would let someone other than the maintainer
publish a release, or make a download pass the checks in the README
when it did not come from the maintainer. [docs/security.md](docs/security.md)
lists every input Nimbus reads and what each is allowed to do.

## Checking a download

Each release's `SHA256SUMS` is signed with the maintainer's minisign
key, which never leaves their computer, and each package has a GitHub
build provenance attestation. The README's
[Verifying a download](README.md#verifying-a-download) section shows how
to check both.
