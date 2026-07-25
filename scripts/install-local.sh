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

NO_BUILD=0
if [[ $# -eq 1 && "$1" == "--no-build" ]]; then
  NO_BUILD=1
elif [[ $# -gt 0 ]]; then
  echo "usage: $0 [--no-build]" >&2
  exit 1
fi

if [[ "$NO_BUILD" -eq 0 ]]; then
  echo "Building release binaries..."
  cargo build --release -p infigraph-cli -p infigraph-mcp
fi

for bin in infigraph infigraph-mcp; do
  src="target/release/$bin"
  [ -f "$src" ] || { echo "error: $src not found (run without --no-build)"; exit 1; }
done

mkdir -p "$INSTALL_DIR"

# Atomic replacement: stage BOTH binaries in the destination directory first,
# so a failed copy cannot leave a mixed-version install. Then rename each over
# its target — rename(2) replaces the directory entry even when the old binary
# is running (the running process keeps the old inode).
cleanup() { rm -f "$INSTALL_DIR"/.infigraph.tmp.$$ "$INSTALL_DIR"/.infigraph-mcp.tmp.$$; }
trap cleanup EXIT

for bin in infigraph infigraph-mcp; do
  tmp="$INSTALL_DIR/.$bin.tmp.$$"
  cp "target/release/$bin" "$tmp"
  chmod +x "$tmp"
done

# Validate the staged binaries BEFORE replacing the active install, so a
# broken build never displaces a working one. (infigraph-mcp ignores
# --version and starts its server loop, but exits 0 immediately once stdin
# hits EOF — so </dev/null both prevents a hang and still proves the binary
# loads and runs.)
"$INSTALL_DIR/.infigraph.tmp.$$" --version >/dev/null
"$INSTALL_DIR/.infigraph-mcp.tmp.$$" --version </dev/null >/dev/null 2>&1

for bin in infigraph infigraph-mcp; do
  mv -f "$INSTALL_DIR/.$bin.tmp.$$" "$INSTALL_DIR/$bin"
done

echo "Installed to $INSTALL_DIR:"
"$INSTALL_DIR/infigraph" --version
echo "(running processes keep working on the old inode until restarted)"
