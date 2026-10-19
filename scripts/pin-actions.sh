#!/bin/bash
# Pin every GitHub Action in .github/workflows to a full commit SHA, and
# the release workflow's appimagetool download to its SHA-256 (M26).
#
# usage: scripts/pin-actions.sh
#
# Why: a tag such as `actions/checkout@v7` can be moved to any commit by
# whoever controls that repository, and our release job runs it with a
# token that can write releases. A 40-character commit SHA cannot be
# moved. Each `uses: owner/repo@REF` whose REF is not already a SHA
# becomes `uses: owner/repo@SHA # VERSION`, where VERSION is the most
# exact tag on that commit (v7 becomes `# v7.0.1`). Dependabot reads that
# comment and keeps both the SHA and the comment current from then on, so
# this script is only needed when a new action is added (zizmor in CI
# fails until it is pinned).
#
# appimagetool: the release workflow names an exact version in
# APPIMAGETOOL_VERSION; this downloads that file and writes its SHA-256
# into APPIMAGETOOL_SHA256, which the workflow checks before running it.
#
# Needs git and curl. Changes the files in place; look at `git diff`
# before committing. Kept bash 3.2 safe for macOS.
set -eu

cd "$(dirname "$0")/.."

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  else
    shasum -a 256 "$1" | cut -d' ' -f1
  fi
}

# Replace the literal text $2 with $3 on every line of file $1.
replace() {
  tmp="$1.pin.$$"
  awk -v old="$2" -v new="$3" '{
    out = ""
    line = $0
    while ((i = index(line, old)) > 0) {
      out = out substr(line, 1, i - 1) new
      line = substr(line, i + length(old))
    }
    print out line
  }' "$1" > "$tmp"
  mv "$tmp" "$1"
}

refs=$(grep -ho 'uses: *[A-Za-z0-9_.-]*/[A-Za-z0-9_./-]*@[A-Za-z0-9_./-]*' .github/workflows/*.yml \
  | sed 's/uses: *//' | sort -u)

for ref in $refs; do
  action=${ref%@*}
  rev=${ref##*@}
  if printf '%s' "$rev" | grep -Eq '^[0-9a-f]{40}$'; then
    continue
  fi
  # owner/repo, without any path to an action inside the repository.
  repo=$(printf '%s' "$action" | cut -d/ -f1-2)
  remote=$(git ls-remote --tags --heads "https://github.com/$repo")
  sha=$(printf '%s\n' "$remote" | awk -v r="refs/tags/$rev^{}" '$2 == r { print $1 }')
  if [ -z "$sha" ]; then
    sha=$(printf '%s\n' "$remote" | awk -v r="refs/tags/$rev" '$2 == r { print $1 }')
  fi
  comment=$rev
  if [ -n "$sha" ]; then
    # The longest tag on the same commit that starts with REF.: v7 -> v7.0.1.
    exact=$(printf '%s\n' "$remote" \
      | awk -v s="$sha" -v p="refs/tags/$rev." '$1 == s && index($2, p) == 1 { t = $2; sub(/^refs\/tags\//, "", t); sub(/\^\{\}$/, "", t); print length(t), t }' \
      | sort -n | tail -n 1 | cut -d' ' -f2)
    if [ -n "$exact" ]; then
      comment=$exact
    fi
  else
    # A branch, such as dtolnay/rust-toolchain@stable.
    sha=$(printf '%s\n' "$remote" | awk -v r="refs/heads/$rev" '$2 == r { print $1 }')
  fi
  if [ -z "$sha" ]; then
    echo "pin-actions: $repo has no tag or branch named $rev" >&2
    exit 1
  fi
  echo "$action@$rev -> $sha # $comment"
  for f in .github/workflows/*.yml; do
    replace "$f" "uses: $action@$rev" "uses: $action@$sha # $comment"
  done
done

release=.github/workflows/release.yml
version=$(sed -n 's/^  APPIMAGETOOL_VERSION: *"\(.*\)"$/\1/p' "$release")
if [ -n "$version" ]; then
  dir=$(mktemp -d)
  curl -fsSL -o "$dir/appimagetool" \
    "https://github.com/AppImage/appimagetool/releases/download/$version/appimagetool-x86_64.AppImage"
  hash=$(sha256 "$dir/appimagetool")
  rm -rf "$dir"
  echo "appimagetool $version -> $hash"
  tmp="$release.pin.$$"
  sed "s/^  APPIMAGETOOL_SHA256: .*/  APPIMAGETOOL_SHA256: \"$hash\"/" "$release" > "$tmp"
  mv "$tmp" "$release"
fi

echo "Done; review with: git diff .github/workflows"
