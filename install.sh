#!/bin/sh
# vps-watcher installer: installs or upgrades the binary, a system user and a hardened systemd service.
#
#   curl -fsSL https://raw.githubusercontent.com/C3ZIZ/vps-watcher/main/install.sh | sudo sh
#
# Options (after `sh -s --` when piping):
#   --version vYY.MM.DD.HH.MM   install a specific release instead of the latest
#   --local DIR                 install from DIR (binary + packaging files) without downloading
#   --uninstall                 remove the service and binary, keep data in /var/lib/vps-watcher
#   --purge                     like --uninstall, and also delete data and settings
set -eu

REPO="C3ZIZ/vps-watcher"
BIN=/usr/local/bin/vps-watcher
UNIT=/etc/systemd/system/vps-watcher.service
CONF_DIR=/etc/vps-watcher
CONF="$CONF_DIR/vps-watcher.env"
DATA=/var/lib/vps-watcher
USER_NAME=vps-watcher

VERSION=latest
LOCAL=""
ACTION=install

say() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
die() { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

while [ $# -gt 0 ]; do
  case "$1" in
    --version) VERSION="${2:?--version needs a value}"; shift 2 ;;
    --local) LOCAL="${2:?--local needs a directory}"; shift 2 ;;
    --uninstall) ACTION=uninstall; shift ;;
    --purge) ACTION=purge; shift ;;
    -h|--help) sed -n '2,12p' "$0"; exit 0 ;;
    *) die "unknown option: $1" ;;
  esac
done

[ "$(id -u)" -eq 0 ] || die "run as root (for example: curl ... | sudo sh)"
[ "$(uname -s)" = "Linux" ] || die "vps-watcher runs on Linux only"
command -v systemctl >/dev/null 2>&1 || die "systemd is required"

uninstall() {
  say "Stopping and removing the service"
  systemctl disable --now vps-watcher >/dev/null 2>&1 || true
  rm -f "$UNIT" "$BIN"
  systemctl daemon-reload
  if [ "$1" = purge ]; then
    say "Deleting data and settings"
    rm -rf "$DATA" "$CONF_DIR"
    userdel "$USER_NAME" >/dev/null 2>&1 || true
  else
    say "Kept $DATA and $CONF (use --purge to delete them)"
  fi
  say "vps-watcher removed"
}

case "$ACTION" in
  uninstall) uninstall keep; exit 0 ;;
  purge) uninstall purge; exit 0 ;;
esac

case "$(uname -m)" in
  x86_64|amd64) ARCH=x86_64 ;;
  aarch64|arm64) ARCH=aarch64 ;;
  *) die "unsupported CPU: $(uname -m) (x86_64 and aarch64 are supported)" ;;
esac
ASSET="vps-watcher-$ARCH-linux"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

fetch() {
  if command -v curl >/dev/null 2>&1; then curl -fsSL --proto '=https' --tlsv1.2 -o "$2" "$1"
  elif command -v wget >/dev/null 2>&1; then wget -q -O "$2" "$1"
  else die "curl or wget is required"; fi
}

if [ -n "$LOCAL" ]; then
  say "Installing from $LOCAL"
  cp "$LOCAL/$ASSET" "$TMP/vps-watcher" 2>/dev/null || cp "$LOCAL/vps-watcher" "$TMP/vps-watcher"
  cp "$LOCAL/vps-watcher.service" "$LOCAL/vps-watcher.env" "$TMP/"
else
  if [ "$VERSION" = latest ]; then BASE="https://github.com/$REPO/releases/latest/download"
  else BASE="https://github.com/$REPO/releases/download/$VERSION"; fi
  say "Downloading $ASSET ($VERSION)"
  for f in "$ASSET" "$ASSET.sha256" vps-watcher.service vps-watcher.env; do
    fetch "$BASE/$f" "$TMP/$f" || die "download failed: $BASE/$f"
  done
  say "Verifying checksum"
  (cd "$TMP" && sha256sum -c "$ASSET.sha256" >/dev/null) || die "checksum mismatch; aborting"
  mv "$TMP/$ASSET" "$TMP/vps-watcher"
fi

"$TMP/vps-watcher" version >/dev/null 2>&1 || die "the downloaded binary does not run on this machine"

if ! id "$USER_NAME" >/dev/null 2>&1; then
  say "Creating system user $USER_NAME"
  useradd --system --no-create-home --home-dir "$DATA" --shell /usr/sbin/nologin "$USER_NAME"
fi
# docker: list containers and read logs. systemd-journal/adm: read service and system logs.
for g in docker systemd-journal adm; do
  if getent group "$g" >/dev/null 2>&1; then usermod -aG "$g" "$USER_NAME"; fi
done

UPGRADE=no
[ -f "$BIN" ] && UPGRADE=yes
install -m 0755 "$TMP/vps-watcher" "$BIN"
install -m 0644 "$TMP/vps-watcher.service" "$UNIT"
install -d -m 0750 -o root -g "$USER_NAME" "$CONF_DIR"
if [ ! -f "$CONF" ]; then
  install -m 0640 -o root -g "$USER_NAME" "$TMP/vps-watcher.env" "$CONF"
  say "Wrote default settings to $CONF"
else
  say "Kept your settings in $CONF"
fi

systemctl daemon-reload
if [ "$UPGRADE" = yes ]; then
  systemctl restart vps-watcher
  say "Upgraded to $("$BIN" version)"
else
  systemctl enable --now vps-watcher >/dev/null
  say "Installed $("$BIN" version)"
fi

sleep 1
systemctl is-active --quiet vps-watcher || die "the service did not start; see: journalctl -u vps-watcher -n 50"

LISTEN="$(sed -n 's/^VPSW_LISTEN=//p' "$CONF" | tail -n1)"
PORT="${LISTEN##*:}"
[ -n "$PORT" ] || PORT=9100
cat <<EOF

  vps-watcher is running on 127.0.0.1:$PORT (not public).

  1. From your computer, open a tunnel:   ssh -L $PORT:127.0.0.1:$PORT you@this-server
  2. Visit http://localhost:$PORT and create the admin account.

  Go public on your domain:
     set VPSW_DOMAIN in $CONF, then run
     sudo vps-watcher proxy caddy     (or: sudo vps-watcher proxy nginx)

  Logs: journalctl -u vps-watcher -f      Settings: $CONF
EOF
