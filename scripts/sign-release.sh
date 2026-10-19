#!/bin/sh
# Sign a release's SHA256SUMS with the release key, on the owner's Mac.
#
# usage: scripts/sign-release.sh vX.Y.Z [SECRET_KEY_FILE]
#
# The release workflow builds a draft release with every package and a
# SHA256SUMS file listing their hashes, but it cannot sign: the secret
# key never goes into GitHub (docs/milestones/m26-security.md), so a
# hijacked account or workflow cannot make a release Nimbus will trust.
# This script is the step between the draft and publishing it:
#
#   1. download the release's packages and SHA256SUMS from GitHub;
#   2. check every package against SHA256SUMS, and that SHA256SUMS lists
#      every package and nothing else;
#   3. sign SHA256SUMS with minisign (it asks for the key's password),
#      with the trusted comment "Nimbus X.Y.Z SHA256SUMS" that the
#      updater reads the version from;
#   4. check the signature against minisign.pub in this repository;
#   5. upload SHA256SUMS.minisig to the release.
#
# It works on a draft or an already published release (1.0.0 and 1.0.1
# shipped unsigned). A release that already has a signature is refused,
# so a good signature is never replaced by accident; delete the
# .minisig asset on GitHub first to re-sign on purpose.
#
# SECRET_KEY_FILE defaults to minisign's own default,
# ~/.minisign/minisign.key. Needs gh (signed in), minisign and shasum.
# NIMBUS_REPO overrides the repository (default SklorGalore/nimbus).
set -eu

repo=${NIMBUS_REPO:-SklorGalore/nimbus}
here=$(cd "$(dirname "$0")/.." && pwd)
pubkey="$here/minisign.pub"

die() { echo "sign-release: $*" >&2; exit 1; }

if [ $# -lt 1 ] || [ $# -gt 2 ]; then
	die "usage: scripts/sign-release.sh vX.Y.Z [SECRET_KEY_FILE]"
fi
tag=$1
case $tag in
v[0-9]*.[0-9]*.[0-9]*) ;;
*) die "the tag must look like v1.2.3, not '$tag'" ;;
esac
version=${tag#v}
seckey=${2:-$HOME/.minisign/minisign.key}

for tool in gh minisign shasum; do
	command -v "$tool" >/dev/null 2>&1 || die "$tool is not installed (brew install $tool)"
done
[ -f "$pubkey" ] || die "no minisign.pub at $pubkey"
[ -f "$seckey" ] || die "no secret key at $seckey (pass its path as the second argument)"

# What the release has. A signature already there is left alone.
assets=$(gh release view "$tag" --repo "$repo" --json assets --jq '.assets[].name') ||
	die "no release $tag in $repo (is gh signed in?)"
if printf '%s\n' "$assets" | grep -qx 'SHA256SUMS.minisig'; then
	die "$tag already has SHA256SUMS.minisig; delete that asset on GitHub first to re-sign"
fi
printf '%s\n' "$assets" | grep -qx 'SHA256SUMS' || die "$tag has no SHA256SUMS yet; wait for the workflow to finish"

work=$(mktemp -d "${TMPDIR:-/tmp}/nimbus-sign-$version.XXXXXX")
trap 'rm -rf "$work"' EXIT
cd "$work"

echo "Downloading $tag from $repo into $work"
gh release download "$tag" --repo "$repo" \
	--pattern '*.zip' --pattern '*.tar.gz' --pattern '*.dmg' \
	--pattern '*.deb' --pattern '*.AppImage' --pattern '*-setup.exe' \
	--pattern 'SHA256SUMS'

# The manifests zip is not a package and is not hashed; everything else
# downloaded must be in SHA256SUMS, and everything in it must be here.
listed=$(awk '{ sub(/^\*/, "", $2); print $2 }' SHA256SUMS | sort)
# (A function, not a case inside $(...): macOS's /bin/sh is bash 3.2,
# which cannot parse a case pattern's ")" inside a command substitution.)
packages() {
	for f in *; do
		case $f in
		SHA256SUMS | winget-manifest.zip) ;;
		*) printf '%s\n' "$f" ;;
		esac
	done
}
present=$(packages | sort)
[ -n "$listed" ] || die "SHA256SUMS is empty"
if [ "$listed" != "$present" ]; then
	printf 'SHA256SUMS lists:\n%s\nthe release has:\n%s\n' "$listed" "$present" >&2
	die "SHA256SUMS and the release's packages do not match"
fi
echo "Checking $(printf '%s\n' "$listed" | wc -l | tr -d ' ') packages against SHA256SUMS"
shasum -a 256 -c SHA256SUMS || die "a package does not match its hash; do not sign"

echo
echo "SHA256SUMS for Nimbus $version:"
cat SHA256SUMS
echo
echo "Signing (minisign asks for the key's password)"
minisign -S -s "$seckey" -m SHA256SUMS -t "Nimbus $version SHA256SUMS"

echo "Checking the signature against $pubkey"
minisign -V -p "$pubkey" -m SHA256SUMS ||
	die "the signature does not check against minisign.pub; is that the right secret key?"
grep -qx "trusted comment: Nimbus $version SHA256SUMS" SHA256SUMS.minisig ||
	die "the trusted comment is not 'Nimbus $version SHA256SUMS'"

gh release upload "$tag" SHA256SUMS.minisig --repo "$repo"
echo
echo "Signed. $tag now has SHA256SUMS.minisig; publish the draft if it is one."
