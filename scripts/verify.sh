#!/bin/sh
# Local check before a pull request: format, lint, tests, UI syntax and packaging sanity.
set -eu
cd "$(dirname "$0")/.."

step() { printf '\n\033[1;34m==> %s\033[0m\n' "$*"; }

step "Format"
cargo fmt --all -- --check

step "Lint"
cargo clippy --all-targets -- -D warnings

step "Tests"
cargo test --quiet

step "Web UI syntax"
if command -v node >/dev/null 2>&1; then node --check web/app.js; else echo "node not found, skipped"; fi

step "Shell scripts"
sh -n install.sh scripts/*.sh
if command -v shellcheck >/dev/null 2>&1; then shellcheck install.sh scripts/*.sh; else echo "shellcheck not found, skipped"; fi

step "Release build"
cargo build --release --quiet
echo "binary size: $(du -h "${CARGO_TARGET_DIR:-target}/release/vps-watcher" | cut -f1)"

printf '\n\033[1;32mAll checks passed.\033[0m\n'
