#!/bin/sh
# Signs a published release on this machine: downloads its files, hashes them, signs the list with the
# offline release key, and uploads SHA256SUMS and SHA256SUMS.sig. Servers install only signed releases.
#
#   scripts/sign-release.sh vYY.MM.DD.HH.MM
#
# The key comes from VPSW_SIGNING_KEY (default ~/.config/vps-watcher/release-signing-key.pem).
set -eu

die() { printf 'error: %s\n' "$*" >&2; exit 1; }

TAG="${1:?usage: scripts/sign-release.sh vYY.MM.DD.HH.MM}"
KEY="${VPSW_SIGNING_KEY:-$HOME/.config/vps-watcher/release-signing-key.pem}"
REPO="C3ZIZ/vps-watcher"
FILES="vps-watcher-x86_64-linux vps-watcher-aarch64-linux vps-watcher.service vps-watcher.env vps-watcher-update.service vps-watcher-update.timer install.sh"

[ -f "$KEY" ] || die "signing key not found at $KEY (create it once with scripts/make-signing-key.sh)"
openssl pkey -in "$KEY" -noout 2>/dev/null || die "cannot read $KEY with $(command -v openssl); OpenSSL 3 or newer is needed"
command -v gh >/dev/null || die "the GitHub CLI (gh) is required"
if command -v sha256sum >/dev/null; then SHA="sha256sum"; else SHA="shasum -a 256"; fi

DIR="$(mktemp -d)"
trap 'rm -rf "$DIR"' EXIT
for f in $FILES; do
  gh release download "$TAG" --repo "$REPO" --pattern "$f" --pattern "$f.sha256" --dir "$DIR" --clobber >/dev/null 2>&1 || true
  [ -f "$DIR/$f" ] || die "$TAG has no $f yet; wait for the release workflow to finish"
done

cd "$DIR"
# The binaries must match the checksums the build job published next to them.
for f in vps-watcher-x86_64-linux vps-watcher-aarch64-linux; do
  [ "$($SHA "$f" | cut -d' ' -f1)" = "$(cut -d' ' -f1 "$f.sha256")" ] || die "$f does not match $f.sha256"
done

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
echo "Signed $TAG ($(wc -l < SHA256SUMS | tr -d ' ') lines in SHA256SUMS)"
