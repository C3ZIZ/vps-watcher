#!/bin/sh
# Creates the release signing key, once. The private key stays on this machine; never commit or upload it.
# The printed public key goes into RELEASE_KEY in src/update.rs.
set -eu
KEY="${VPSW_SIGNING_KEY:-$HOME/.config/vps-watcher/release-signing-key.pem}"
[ -e "$KEY" ] && { printf 'error: %s already exists; refusing to replace it\n' "$KEY" >&2; exit 1; }
umask 077
mkdir -p "$(dirname "$KEY")"
openssl genpkey -algorithm ed25519 -out "$KEY"
printf 'Created %s\n\nPublic key (base64, 32 bytes):\n' "$KEY"
openssl pkey -in "$KEY" -pubout -outform DER | tail -c 32 | base64
printf '\nBack the key file up somewhere offline. Without it, installed servers cannot auto-update.\n'
