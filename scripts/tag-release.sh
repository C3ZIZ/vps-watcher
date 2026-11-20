#!/bin/sh
# Tags main's newest commit vYY.MM.DD.HH.MM (Riyadh time) and publishes the GitHub Release.
# The release workflow then builds and attaches the binaries.
set -eu
cd "$(dirname "$0")/.."

die() { printf 'error: %s\n' "$*" >&2; exit 1; }

[ "$(git rev-parse --abbrev-ref HEAD)" = main ] || die "check out main first"
[ -z "$(git status --porcelain)" ] || die "working tree is not clean"
git fetch --quiet origin main --tags
[ "$(git rev-parse HEAD)" = "$(git rev-parse origin/main)" ] || die "main is not up to date with origin/main"
if git describe --exact-match --tags HEAD >/dev/null 2>&1; then
  die "this commit is already released as $(git describe --exact-match --tags HEAD)"
fi

TAG="v$(TZ=Asia/Riyadh date +%y.%m.%d.%H.%M)"
git rev-parse -q --verify "refs/tags/$TAG" >/dev/null && die "tag $TAG already exists; wait a minute"

NOTES="$(git log -1 --format=%B)"
git tag -a "$TAG" -m "$TAG"
git push origin "$TAG"
gh release create "$TAG" --title "$TAG" --notes "$NOTES" --latest
echo "Released $TAG"
