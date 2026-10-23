#!/bin/sh
# Publish a signed, published release to Homebrew and Chocolatey.
#
# usage: scripts/publish-packages.sh vX.Y.Z [--dry-run] [--only homebrew|chocolatey]
#                                    [--tap OWNER/homebrew-NAME]
#
# This is the last release step, after scripts/sign-release.sh and
# publishing the draft on GitHub (docs/releasing.md):
#
#   1. download the release's SHA256SUMS and SHA256SUMS.minisig and check
#      the signature against minisign.pub in this repository, with the
#      trusted comment "Nimbus X.Y.Z SHA256SUMS". Nothing is published
#      from a release that is unsigned or signed by another key;
#   2. take the hashes of the .dmg and the Windows installer from that
#      SHA256SUMS, so the cask and the Chocolatey install script name
#      exactly the signed files, and fill in the templates in packaging/;
#   3. check the release is published (Homebrew and Chocolatey download
#      from the public URLs, which a draft does not have) and that those
#      URLs answer;
#   4. show the cask and the Chocolatey scripts and ask before going
#      further;
#   5. Homebrew: write Casks/nimbus.rb in the tap repository (default
#      SklorGalore/homebrew-nimbus, so `brew install --cask
#      sklorgalore/nimbus/nimbus` works), offering to create the tap the
#      first time;
#   6. Chocolatey: download the release's nimbus.X.Y.Z.nupkg (packed on
#      Windows by .github/workflows/chocolatey.yml), check it holds only
#      the package files and that its scripts are byte for byte the ones
#      filled in at step 2, and push it to the community repository
#      (https://community.chocolatey.org/packages/nimbus), where it waits
#      for Chocolatey's moderators.
#
# The tap is written through the GitHub API with gh, so nothing is
# cloned. --dry-run does steps 1 and 2 only and prints the files; it
# works on a draft too. --only does one of the two package managers.
# Running it again for a version already published changes nothing.
#
# Needs gh (signed in as an account that can push to the tap), minisign,
# curl and unzip. The Chocolatey push needs your API key from
# https://community.chocolatey.org/account: set CHOCOLATEY_API_KEY, or
# paste it when asked (it is not echoed or saved). NIMBUS_REPO overrides
# the repository (default SklorGalore/nimbus).
#
# Written for macOS's /bin/sh, which is bash 3.2: no bash 4 features,
# and no case statements inside $(...).
set -eu

repo=${NIMBUS_REPO:-SklorGalore/nimbus}
here=$(cd "$(dirname "$0")/.." && pwd)
pubkey="$here/minisign.pub"

die() { echo "publish-packages: $*" >&2; exit 1; }
usage="usage: scripts/publish-packages.sh vX.Y.Z [--dry-run] [--only homebrew|chocolatey] [--tap OWNER/homebrew-NAME]"

tag=
dry_run=false
only=
tap=SklorGalore/homebrew-nimbus
while [ $# -gt 0 ]; do
	case $1 in
	--dry-run) dry_run=true ;;
	--only)
		[ $# -ge 2 ] || die "$usage"
		only=$2
		shift
		;;
	--tap)
		[ $# -ge 2 ] || die "$usage"
		tap=$2
		shift
		;;
	-h | --help)
		echo "$usage"
		exit 0
		;;
	v*)
		[ -z "$tag" ] || die "$usage"
		tag=$1
		;;
	*) die "$usage" ;;
	esac
	shift
done
[ -n "$tag" ] || die "$usage"
case $tag in
v[0-9]*.[0-9]*.[0-9]*) ;;
*) die "the tag must look like v1.2.3, not '$tag'" ;;
esac
case $only in
'' | homebrew | chocolatey) ;;
*) die "--only takes homebrew or chocolatey, not '$only'" ;;
esac
case $tap in
*/homebrew-*) ;;
*) die "a tap repository is named OWNER/homebrew-NAME, not '$tap'" ;;
esac
version=${tag#v}

for tool in gh minisign curl unzip; do
	command -v "$tool" >/dev/null 2>&1 || die "$tool is not installed (brew install $tool)"
done
[ -f "$pubkey" ] || die "no minisign.pub at $pubkey"

work=$(mktemp -d "${TMPDIR:-/tmp}/nimbus-publish-$version.XXXXXX")
trap 'rm -rf "$work"' EXIT

# 1. The signature. sign-release.sh made it with the key whose public
# half is minisign.pub; a SHA256SUMS that does not check against it did
# not come from that key, so nothing below may trust its hashes.
is_draft=$(gh release view "$tag" --repo "$repo" --json isDraft --jq '.isDraft') ||
	die "no release $tag in $repo (is gh signed in?)"
echo "Downloading SHA256SUMS and its signature for $tag"
gh release download "$tag" --repo "$repo" --dir "$work" \
	--pattern 'SHA256SUMS' --pattern 'SHA256SUMS.minisig' ||
	die "could not download SHA256SUMS from $tag"
[ -f "$work/SHA256SUMS.minisig" ] ||
	die "$tag has no SHA256SUMS.minisig; run scripts/sign-release.sh $tag first"
minisign -V -p "$pubkey" -m "$work/SHA256SUMS" ||
	die "SHA256SUMS.minisig does not check against minisign.pub; do not publish"
grep -qx "trusted comment: Nimbus $version SHA256SUMS" "$work/SHA256SUMS.minisig" ||
	die "the signature's trusted comment is not 'Nimbus $version SHA256SUMS'"

# 2. The hashes, from the signed file. (A file name in SHA256SUMS may
# start with "*", meaning binary mode.)
hash_of() {
	h=$(awk -v f="$1" '{ n = $2; sub(/^\*/, "", n); if (n == f) print $1 }' "$work/SHA256SUMS")
	[ -n "$h" ] || die "SHA256SUMS has no line for $1"
	printf '%s\n' "$h"
}
dmg=Nimbus-$version-macos.dmg
setup=nimbus-$version-windows-x86_64-setup.exe
dmg_hash=$(hash_of "$dmg")
setup_hash=$(hash_of "$setup")

cask="$work/nimbus.rb"
sed -e "s/@VERSION@/$version/g" -e "s/@SHA256@/$dmg_hash/g" \
	"$here/packaging/homebrew/nimbus.rb.in" >"$cask"
# The Chocolatey scripts as they must appear in the .nupkg. (The nuspec
# is rewritten by `choco pack`, so only its id and version are checked.)
choco="$work/choco"
mkdir -p "$choco/tools"
for t in tools/chocolateyInstall.ps1.in tools/chocolateyUninstall.ps1; do
	sed -e "s/@VERSION@/$version/g" -e "s/@SHA256@/$setup_hash/g" \
		"$here/packaging/chocolatey/$t" >"$choco/${t%.in}"
done

show() {
	if [ "$only" != chocolatey ]; then
		echo "==== Homebrew cask: $tap, Casks/nimbus.rb"
		cat "$cask"
	fi
	if [ "$only" != homebrew ]; then
		for f in "$choco"/tools/*.ps1; do
			echo "==== Chocolatey: nimbus $version, tools/$(basename "$f")"
			cat "$f"
		done
	fi
}

if $dry_run; then
	show
	echo
	if [ "$is_draft" = true ]; then
		echo "Dry run: nothing published. $tag is still a draft; publish it on GitHub before a real run."
	else
		echo "Dry run: nothing published."
	fi
	exit 0
fi

# 3. Homebrew and Chocolatey users download from the release's public URLs,
# which only exist once the draft is published.
[ "$is_draft" = false ] || die "$tag is still a draft; publish it on GitHub first (--dry-run works on a draft)"
for f in "$dmg" "$setup"; do
	url="https://github.com/$repo/releases/download/$tag/$f"
	curl -fsSIL -o /dev/null "$url" || die "$url does not download"
done

# 4. Look before anything leaves this machine.
show
echo
printf 'Publish these for Nimbus %s? [y/N] ' "$version"
read -r answer || answer=
[ "$answer" = y ] || [ "$answer" = Y ] || die "stopped; nothing published"

# Puts a local file at a path in a repository through the contents API.
# Updating needs the current file's blob sha; a new file does not.
put_file() { # REPO BRANCH PATH LOCAL_FILE MESSAGE [OLD_SHA]
	if [ -n "${6:-}" ]; then
		base64 <"$4" | tr -d '\n' |
			gh api -X PUT "repos/$1/contents/$3" -f "message=$5" -f "branch=$2" \
				-f "sha=$6" -F 'content=@-' --silent
	else
		base64 <"$4" | tr -d '\n' |
			gh api -X PUT "repos/$1/contents/$3" -f "message=$5" -f "branch=$2" \
				-F 'content=@-' --silent
	fi
}

# "a.b.c" sorts after "x.y.z" as numbers, not text.
newer_than() { # A B: is A newer than B?
	[ "$1" != "$2" ] &&
		[ "$(printf '%s\n%s\n' "$1" "$2" | sort -t. -k1,1n -k2,2n -k3,3n | tail -n 1)" = "$1" ]
}

publish_homebrew() {
	echo
	echo "Homebrew: $tap"
	if ! gh repo view "$tap" >/dev/null 2>&1; then
		printf '%s does not exist. Create it as a public repository? [y/N] ' "$tap"
		read -r answer || answer=
		[ "$answer" = y ] || [ "$answer" = Y ] || die "no tap; Homebrew not published"
		gh repo create "$tap" --public --add-readme \
			--description "Homebrew tap for Nimbus, a fast GPU text editor"
	fi
	branch=$(gh api "repos/$tap" --jq '.default_branch')
	old_sha=
	if old_sha=$(gh api "repos/$tap/contents/Casks/nimbus.rb?ref=$branch" --jq '.sha' 2>/dev/null); then
		gh api "repos/$tap/contents/Casks/nimbus.rb?ref=$branch" \
			-H 'Accept: application/vnd.github.raw' >"$work/old.rb"
		if cmp -s "$work/old.rb" "$cask"; then
			echo "Casks/nimbus.rb is already this cask; nothing to do."
			return 0
		fi
		old_version=$(sed -n 's/^ *version "\(.*\)".*/\1/p' "$work/old.rb" | head -n 1)
		if [ -n "$old_version" ] && newer_than "$old_version" "$version"; then
			die "the tap already has Nimbus $old_version, newer than $version; Homebrew not published"
		fi
	else
		old_sha=
	fi
	put_file "$tap" "$branch" Casks/nimbus.rb "$cask" "nimbus $version" "$old_sha"
	echo "Published. Users run: brew install --cask $(echo "${tap%%/*}" | tr '[:upper:]' '[:lower:]')/${tap#*/homebrew-}/nimbus"
}

publish_chocolatey() {
	echo
	echo "Chocolatey: nimbus $version"
	nupkg=nimbus.$version.nupkg
	gh release download "$tag" --repo "$repo" --dir "$work" --pattern "$nupkg" 2>/dev/null ||
		die "$tag has no $nupkg; run the Chocolatey package workflow for $tag (Actions > Chocolatey package > Run workflow) and try again"
	[ -f "$work/$nupkg" ] ||
		die "$tag has no $nupkg; run the Chocolatey package workflow for $tag (Actions > Chocolatey package > Run workflow) and try again"

	# Only the package files: the nuspec, the two scripts, and the
	# bookkeeping files every .nupkg has (`unzip -Z1` lists one name per
	# line).
	unzip -Z1 "$work/$nupkg" >"$work/nupkg.list" || die "$nupkg is not a zip file"
	while IFS= read -r entry; do
		case $entry in
		nimbus.nuspec | tools/chocolateyInstall.ps1 | tools/chocolateyUninstall.ps1) ;;
		'[Content_Types].xml' | _rels/.rels | package/services/metadata/core-properties/*.psmdcp) ;;
		*/) ;; # a folder entry holds nothing
		*) die "$nupkg holds an unexpected file, $entry; do not publish" ;;
		esac
	done <"$work/nupkg.list"

	# Its scripts must be exactly the ones filled in from the signed
	# SHA256SUMS: those carry the installer's URL and hash.
	mkdir "$work/packed"
	unzip -q "$work/$nupkg" nimbus.nuspec 'tools/*' -d "$work/packed" ||
		die "could not unpack $nupkg"
	for f in tools/chocolateyInstall.ps1 tools/chocolateyUninstall.ps1; do
		[ -f "$work/packed/$f" ] || die "$nupkg has no $f"
		cmp -s "$work/packed/$f" "$choco/$f" ||
			die "$nupkg's $f differs from packaging/chocolatey filled in from the signed SHA256SUMS; do not publish"
	done
	grep -q '<id>nimbus</id>' "$work/packed/nimbus.nuspec" ||
		die "$nupkg's nuspec is not for the package id nimbus"
	grep -q "<version>$version</version>" "$work/packed/nimbus.nuspec" ||
		die "$nupkg's nuspec is not version $version"
	echo "$nupkg matches the signed SHA256SUMS."

	key=${CHOCOLATEY_API_KEY:-}
	if [ -z "$key" ]; then
		printf 'Chocolatey API key (https://community.chocolatey.org/account): '
		stty -echo 2>/dev/null || true
		read -r key || key=
		stty echo 2>/dev/null || true
		echo
	fi
	[ -n "$key" ] || die "no API key; Chocolatey not published"
	# The key goes in a header file, not on curl's command line, where
	# other users of this machine could see it in ps.
	(umask 077 && printf 'X-NuGet-ApiKey: %s\n' "$key" >"$work/key.header")
	# NuGet v2 push: a PUT with the .nupkg as the first form part.
	status=$(curl -sS -o "$work/push.out" -w '%{http_code}' -X PUT \
		-H @"$work/key.header" -F "package=@$work/$nupkg" \
		https://push.chocolatey.org/api/v2/package/) ||
		die "could not reach push.chocolatey.org"
	rm -f "$work/key.header"
	case $status in
	200 | 201 | 202)
		echo "Pushed. Chocolatey's automatic checks run first, then a moderator approves it; until then it is listed at https://community.chocolatey.org/packages/nimbus/$version as waiting for review."
		;;
	409)
		echo "Chocolatey already has nimbus $version; nothing to do."
		;;
	*)
		cat "$work/push.out" >&2
		echo >&2
		die "Chocolatey refused the push (HTTP $status)"
		;;
	esac
}

# (In if bodies, not after ||, so set -e still stops on a failed step.)
if [ "$only" != chocolatey ]; then
	publish_homebrew
fi
if [ "$only" != homebrew ]; then
	publish_chocolatey
fi
echo
echo "Done for Nimbus $version."
