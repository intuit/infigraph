#!/usr/bin/env bash
set -euo pipefail

# Install locally built infigraph binaries into ~/.local/bin.
#
# Unlike install.sh (which downloads upstream release binaries), this builds
# from the current working tree — including any local, uncommitted changes.
#
# Usage:
#   ./scripts/install-local.sh              # cargo build --release + install
#   ./scripts/install-local.sh --no-build   # install existing target/release binaries

INSTALL_DIR="${INFIGRAPH_INSTALL_DIR:-$HOME/.local/bin}"
SCRIPT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "$SCRIPT_DIR"

if [[ "${1:-}" != "--no-build" ]]; then
  echo "Building release binaries..."
  cargo build --release -p infigraph-cli -p infigraph-mcp
fi

for bin in infigraph infigraph-mcp; do
  src="target/release/$bin"
  [ -f "$src" ] || { echo "error: $src not found (run without --no-build)"; exit 1; }
done

mkdir -p "$INSTALL_DIR"

# Same trick as install.sh: a running binary can't be overwritten in place,
# but it can be renamed — the running process keeps the old inode.
move_running_binary() {
  local bin="$1"
  if [ -f "$bin" ]; then
    rm -f "${bin}.old"
    mv "$bin" "${bin}.old" 2>/dev/null || true
  fi
}

for bin in infigraph infigraph-mcp; do
  move_running_binary "$INSTALL_DIR/$bin"
  cp "target/release/$bin" "$INSTALL_DIR/$bin"
done

echo "Installed to $INSTALL_DIR:"
"$INSTALL_DIR/infigraph" --version
"$INSTALL_DIR/infigraph-mcp" --version 2>/dev/null || true
echo "(old copies kept as *.old until next install; running processes keep working)"
