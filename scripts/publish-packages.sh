#!/bin/sh
# Publish a signed, published release to Homebrew and winget.
#
# usage: scripts/publish-packages.sh vX.Y.Z [--dry-run] [--only homebrew|winget]
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
#      SHA256SUMS, so the cask and manifest name exactly the signed files,
#      and fill in the templates in packaging/;
#   3. check the release is published (Homebrew and winget download from
#      the public URLs, which a draft does not have) and that those URLs
#      answer;
#   4. show the cask and manifest and ask before going further;
#   5. Homebrew: write Casks/nimbus.rb in the tap repository (default
#      SklorGalore/homebrew-nimbus, so `brew install --cask
#      sklorgalore/nimbus/nimbus` works), offering to create the tap the
#      first time;
#   6. winget: fork microsoft/winget-pkgs if needed, bring the fork up to
#      date, put the three manifest files on a new branch of the fork and
#      open a pull request to microsoft/winget-pkgs.
#
# Everything goes through the GitHub API with gh, so nothing is cloned
# (winget-pkgs is several gigabytes). --dry-run does steps 1 and 2 only
# and prints the files; it works on a draft too. --only does one of the
# two package managers. Running it again for a version already published
# changes nothing.
#
# Needs gh (signed in as an account that can push to the tap), minisign
# and curl. NIMBUS_REPO overrides the repository (default
# SklorGalore/nimbus).
#
# Written for macOS's /bin/sh, which is bash 3.2: no bash 4 features,
# and no case statements inside $(...).
set -eu

repo=${NIMBUS_REPO:-SklorGalore/nimbus}
here=$(cd "$(dirname "$0")/.." && pwd)
pubkey="$here/minisign.pub"

die() { echo "publish-packages: $*" >&2; exit 1; }
usage="usage: scripts/publish-packages.sh vX.Y.Z [--dry-run] [--only homebrew|winget] [--tap OWNER/homebrew-NAME]"

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
'' | homebrew | winget) ;;
*) die "--only takes homebrew or winget, not '$only'" ;;
esac
case $tap in
*/homebrew-*) ;;
*) die "a tap repository is named OWNER/homebrew-NAME, not '$tap'" ;;
esac
version=${tag#v}

for tool in gh minisign curl; do
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
mkdir "$work/winget"
for t in "$here"/packaging/winget/*.yaml.in; do
	sed -e "s/@VERSION@/$version/g" -e "s/@SHA256@/$setup_hash/g" \
		"$t" >"$work/winget/$(basename "$t" .in)"
done

show() {
	if [ "$only" != winget ]; then
		echo "==== Homebrew cask: $tap, Casks/nimbus.rb"
		cat "$cask"
	fi
	if [ "$only" != homebrew ]; then
		for f in "$work"/winget/*.yaml; do
			echo "==== winget: manifests/s/SklorGalore/Nimbus/$version/$(basename "$f")"
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

# 3. Homebrew and winget users download from the release's public URLs,
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

publish_winget() {
	echo
	echo "winget: microsoft/winget-pkgs"
	upstream=microsoft/winget-pkgs
	dir=manifests/s/SklorGalore/Nimbus
	if gh api "repos/$upstream/contents/$dir/$version" >/dev/null 2>&1; then
		echo "$upstream already has $dir/$version; nothing to do."
		return 0
	fi
	if gh api "repos/$upstream/contents/$dir" >/dev/null 2>&1; then
		kind="New version"
	else
		kind="New package"
	fi
	me=$(gh api user --jq '.login')
	fork=$me/winget-pkgs
	if ! gh repo view "$fork" >/dev/null 2>&1; then
		echo "Forking $upstream to $fork"
		gh repo fork "$upstream" --clone=false >/dev/null
		# A new fork takes a moment to appear.
		tries=0
		until gh repo view "$fork" >/dev/null 2>&1; do
			tries=$((tries + 1))
			[ $tries -le 30 ] || die "the fork $fork did not appear"
			sleep 2
		done
	fi
	base=$(gh api "repos/$upstream" --jq '.default_branch')
	echo "Bringing $fork up to date with $upstream"
	gh api -X POST "repos/$fork/merge-upstream" -f "branch=$base" --silent ||
		die "could not update $fork from $upstream"
	head=nimbus-$version
	if gh api "repos/$fork/git/ref/heads/$head" >/dev/null 2>&1; then
		die "$fork already has a branch $head (an earlier run?); check for its pull request, or delete the branch on GitHub and run again"
	fi
	sha=$(gh api "repos/$fork/git/ref/heads/$base" --jq '.object.sha')
	gh api -X POST "repos/$fork/git/refs" -f "ref=refs/heads/$head" -f "sha=$sha" --silent
	for f in "$work"/winget/*.yaml; do
		put_file "$fork" "$head" "$dir/$version/$(basename "$f")" "$f" \
			"$kind: SklorGalore.Nimbus version $version"
	done
	cat >"$work/pr.md" <<EOF
$kind: SklorGalore.Nimbus version $version.

The manifest was generated from the release's signed SHA256SUMS
(https://github.com/$repo/releases/tag/$tag) by
scripts/publish-packages.sh in the Nimbus repository.
EOF
	gh pr create --repo "$upstream" --base "$base" --head "$me:$head" \
		--title "$kind: SklorGalore.Nimbus version $version" --body-file "$work/pr.md"
	echo "Opened. The winget bots validate it; the first pull request also asks you to sign Microsoft's CLA in a comment."
}

# (In if bodies, not after ||, so set -e still stops on a failed step.)
if [ "$only" != winget ]; then
	publish_homebrew
fi
if [ "$only" != homebrew ]; then
	publish_winget
fi
echo
echo "Done for Nimbus $version."
