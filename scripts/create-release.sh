#!/usr/bin/env bash
#
# Cut a release via a PR so CI validates the version bump before the
# tag is pushed. The tag push then triggers .github/workflows/publish.yml
# which builds binaries and publishes to crates.io.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" >/dev/null && pwd)"
cd "$SCRIPT_DIR/.."

REPO="nix-community/nix-user-chroot"
MAIN_BRANCH="master"

version="${1:-}"
if [[ -z $version ]]; then
  echo "USAGE: $0 <version>" >&2
  exit 1
fi

if [[ "$(git symbolic-ref --short HEAD)" != "$MAIN_BRANCH" ]]; then
  echo "must be on $MAIN_BRANCH branch" >&2
  exit 1
fi

# ensure we are clean and up-to-date
uncommitted_changes=$(git diff --compact-summary)
if [[ -n $uncommitted_changes ]]; then
  echo -e "There are uncommitted changes, exiting:\n${uncommitted_changes}" >&2
  exit 1
fi
git pull "git@github.com:${REPO}" "$MAIN_BRANCH"
unpushed_commits=$(git log --format=oneline "origin/${MAIN_BRANCH}..${MAIN_BRANCH}")
if [[ -n $unpushed_commits ]]; then
  echo -e "\nThere are unpushed changes, exiting:\n$unpushed_commits" >&2
  exit 1
fi

if git tag -l | grep -q "^${version}\$"; then
  echo "Tag ${version} already exists, exiting" >&2
  exit 1
fi

# bump version and regenerate lockfile
sed -i -e "0,/^version = \".*\"/s//version = \"${version}\"/" Cargo.toml
cargo update --workspace --offline
git add Cargo.toml Cargo.lock

# open a release PR so CI runs the full test matrix on the bump
release_branch="release-${version}"
git branch -D "$release_branch" 2>/dev/null || true
git checkout -b "$release_branch"
git commit -m "release ${version}"
git push --force origin "$release_branch"

pr_url=$(gh pr create \
  --base "$MAIN_BRANCH" \
  --head "$release_branch" \
  --title "Release ${version}" \
  --body "Release ${version} of nix-user-chroot")
pr_number="${pr_url##*/}"

gh pr merge "$pr_number" --auto --rebase --delete-branch
git checkout "$MAIN_BRANCH"

# wait for CI + auto-merge
echo "Waiting for PR #${pr_number} to be merged by CI..."
while [[ "$(gh pr view "$pr_number" --json state --jq .state)" != "MERGED" ]]; do
  sleep 10
done

# tag the merged commit; this triggers the publish workflow which
# creates the GitHub release and uploads binaries
git pull "git@github.com:${REPO}" "$MAIN_BRANCH"
git tag "${version}"
git push origin "${version}"

# Wait for the publish workflow to create the release, then add notes.
# upload-release-action creates a bare release with no title/body.
echo "Waiting for publish workflow to create the release..."
while ! gh release view "${version}" --repo "${REPO}" >/dev/null 2>&1; do
  sleep 5
done

prev_tag=$(git tag -l --sort=-version:refname | grep -v "^${version}\$" | head -1)
notes=$(gh api "repos/${REPO}/releases/generate-notes" \
  -f tag_name="${version}" \
  -f previous_tag_name="${prev_tag}" \
  --jq .body)
gh release edit "${version}" --repo "${REPO}" --title "${version}" --notes "${notes}"

echo "Release ${version} published: https://github.com/${REPO}/releases/tag/${version}"
