#!/bin/sh
# Signs a published release on this machine, then marks it latest. Before signing it checks that:
#   - each binary was built by this repository's release workflow from the tagged commit (attestation),
#   - every other file is byte-for-byte the tagged source in this checkout,
# then writes SHA256SUMS, signs it with the offline key and uploads SHA256SUMS and SHA256SUMS.sig.
#
#   scripts/sign-release.sh vYY.MM.DD.HH.MM
#
# The key comes from VPSW_SIGNING_KEY (default ~/.config/vps-watcher/release-signing-key.pem).
set -eu

die() { printf 'error: %s\n' "$*" >&2; exit 1; }

TAG="${1:?usage: scripts/sign-release.sh vYY.MM.DD.HH.MM}"
KEY="${VPSW_SIGNING_KEY:-$HOME/.config/vps-watcher/release-signing-key.pem}"
REPO="C3ZIZ/vps-watcher"
BINS="vps-watcher-x86_64-linux vps-watcher-aarch64-linux"
# Release file name and its path in the repository.
SOURCES="vps-watcher.service:packaging/vps-watcher.service vps-watcher.env:packaging/vps-watcher.env
vps-watcher-update.service:packaging/vps-watcher-update.service vps-watcher-update.timer:packaging/vps-watcher-update.timer
install.sh:install.sh"

cd "$(dirname "$0")/.."
[ -f "$KEY" ] || die "signing key not found at $KEY (create it once with scripts/make-signing-key.sh)"
openssl pkey -in "$KEY" -noout 2>/dev/null || die "cannot read $KEY with $(command -v openssl); OpenSSL 3 or newer is needed"
command -v gh >/dev/null || die "the GitHub CLI (gh) is required"
if command -v sha256sum >/dev/null; then SHA="sha256sum"; else SHA="shasum -a 256"; fi

git fetch --quiet origin --tags
COMMIT="$(git rev-parse "$TAG^{commit}")" || die "tag $TAG is not in this checkout"

DIR="$(mktemp -d)"
trap 'rm -rf "$DIR"' EXIT
FILES="$BINS"
for pair in $SOURCES; do FILES="$FILES ${pair%%:*}"; done
for f in $FILES; do
  gh release download "$TAG" --repo "$REPO" --pattern "$f" --dir "$DIR" --clobber >/dev/null 2>&1 || true
  [ -f "$DIR/$f" ] || die "$TAG has no $f yet; wait for the release workflow to finish"
done

# Binaries: built by this repo's release workflow, from exactly the tagged commit, on GitHub's runners.
for f in $BINS; do
  built_from="$(gh attestation verify "$DIR/$f" --repo "$REPO" \
    --signer-workflow "$REPO/.github/workflows/release.yml" --source-ref "refs/tags/$TAG" \
    --deny-self-hosted-runners --format json \
    --jq '.[0].verificationResult.signature.certificate.sourceRepositoryDigest')" \
    || die "$f has no valid build attestation; not signing"
  [ "$built_from" = "$COMMIT" ] || die "$f was built from $built_from, not $TAG ($COMMIT); not signing"
done
# Everything else must match the tagged source exactly.
for pair in $SOURCES; do
  git show "$TAG:${pair#*:}" | cmp -s - "$DIR/${pair%%:*}" || die "${pair%%:*} differs from ${pair#*:} at $TAG; not signing"
done

cd "$DIR"
# First line names the release, so a signed list cannot be reused for another version.
{
  printf 'vps-watcher %s\n' "$TAG"
  for f in $FILES; do $SHA "$f"; done
} > SHA256SUMS
openssl pkeyutl -sign -inkey "$KEY" -rawin -in SHA256SUMS -out SHA256SUMS.bin
openssl pkey -in "$KEY" -pubout -out pub.pem
openssl pkeyutl -verify -pubin -inkey pub.pem -rawin -in SHA256SUMS -sigfile SHA256SUMS.bin >/dev/null \
  || die "the new signature does not verify"
openssl base64 -A -in SHA256SUMS.bin > SHA256SUMS.sig

gh release upload "$TAG" SHA256SUMS SHA256SUMS.sig --repo "$REPO" --clobber
# Only now do servers see it: the release was published as "not latest" until signed.
gh release edit "$TAG" --repo "$REPO" --latest >/dev/null
echo "Signed $TAG and marked it latest"
