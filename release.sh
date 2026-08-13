#!/usr/bin/env bash
set -euo pipefail

# Infigraph release builder — multi-platform append workflow
#
# Usage:
#   ./release.sh v0.10.0          # Build all targets for current OS, upload to release
#
# Multi-platform workflow:
#   Mac:     ./release.sh v0.10.0  → uploads aarch64-apple-darwin + x86_64-apple-darwin
#   Windows: ./release.sh v0.10.0  → uploads x86_64-pc-windows-msvc.zip (appends to same release)
#   Either:  install.sh            → auto-detects platform, downloads correct binary

GHE_HOST="${INFIGRAPH_GH_HOST:-github.com}"
GHE_REPO="${INFIGRAPH_GH_OWNER:-intuit}/infigraph"

VERSION="${1:-}"
if [ -z "$VERSION" ]; then
  echo "Usage: $0 <version>   (e.g. $0 v0.10.0)"
  exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$SCRIPT_DIR"

# Guard: the tag must match the workspace version, or the binaries built
# below will report a version (via clap's `version` from CARGO_PKG_VERSION)
# that disagrees with the release tag they're uploaded under.
CARGO_VERSION="$(grep -m1 '^version = ' Cargo.toml | sed -E 's/version = "(.*)"/\1/')"
TAG_VERSION="${VERSION#v}"
if [ "$TAG_VERSION" != "$CARGO_VERSION" ]; then
  echo "Error: release tag ${VERSION} does not match [workspace.package].version ${CARGO_VERSION} in Cargo.toml."
  echo "Bump the workspace version to ${TAG_VERSION}, or run: $0 v${CARGO_VERSION}"
  exit 1
fi

# Detect current platform
OS="$(uname -s)"

# Ensure cmake available
if ! command -v cmake &>/dev/null; then
  case "$OS" in
    Darwin)
      echo "Installing cmake..."
      brew install cmake
      ;;
    MINGW*|MSYS*|CYGWIN*)
      echo "Error: cmake not found. Install via:"
      echo "  winget install Kitware.CMake"
      echo "  — or — choco install cmake"
      exit 1
      ;;
    *)
      echo "Error: cmake not found. Install it first."
      exit 1
      ;;
  esac
fi

# Collect targets to build
TARGETS=()
case "$OS" in
  Darwin)
    # arm64 only — x86_64 cross-compile fails with macOS version mismatch in C++ deps
    TARGETS=("aarch64-apple-darwin")
    ARCHIVE_EXT="tar.gz"
    BIN_SUFFIX=""
    ;;
  MINGW*|MSYS*|CYGWIN*)
    TARGETS=("x86_64-pc-windows-msvc")
    ARCHIVE_EXT="zip"
    BIN_SUFFIX=".exe"
    ;;
  Linux)
    ARCH="$(uname -m)"
    TARGETS=("${ARCH}-unknown-linux-gnu")
    ARCHIVE_EXT="tar.gz"
    BIN_SUFFIX=""
    ;;
  *)
    echo "Unsupported OS: $OS"; exit 1 ;;
esac

echo "Building Infigraph ${VERSION} for ${#TARGETS[@]} target(s): ${TARGETS[*]}"
echo "================================"

# Create release if it doesn't exist
echo ""
echo "Ensuring GitHub release ${VERSION} exists..."
if ! GH_HOST="$GHE_HOST" gh release view "$VERSION" --repo "$GHE_REPO" &>/dev/null; then
  echo "  Creating release ${VERSION}..."
  GH_HOST="$GHE_HOST" gh release create "$VERSION" \
    --repo "$GHE_REPO" \
    --title "Infigraph ${VERSION}" \
    --notes "Infigraph ${VERSION}"
fi

# Build, sign, package, upload each target
for TARGET in "${TARGETS[@]}"; do
  echo ""
  echo "→ Building ${TARGET}..."
  rustup target add "$TARGET" 2>/dev/null || true
  cargo clean -p infigraph-cli -p infigraph-mcp -p infigraph-core -p infigraph-languages --target "$TARGET" 2>/dev/null || true
  cargo build --release --target "$TARGET" -p infigraph-cli -p infigraph-mcp --features remote

  # Sign (macOS only)
  if [[ "$OS" == "Darwin" ]]; then
    echo "  Signing..."
    codesign --force --deep --sign - "target/${TARGET}/release/infigraph"
    codesign --force --deep --sign - "target/${TARGET}/release/infigraph-mcp"
  fi

  # Package
  echo "  Packaging..."
  ARCHIVE="infigraph-${TARGET}.${ARCHIVE_EXT}"
  cp "target/${TARGET}/release/infigraph${BIN_SUFFIX}" .
  cp "target/${TARGET}/release/infigraph-mcp${BIN_SUFFIX}" .
  if [[ "$ARCHIVE_EXT" == "zip" ]]; then
    rm -f "$ARCHIVE"
    if command -v zip &>/dev/null; then
      zip -r "$ARCHIVE" "infigraph${BIN_SUFFIX}" "infigraph-mcp${BIN_SUFFIX}" models/
    else
      powershell -NoProfile -Command "Compress-Archive -Path 'infigraph${BIN_SUFFIX}','infigraph-mcp${BIN_SUFFIX}','models' -DestinationPath '${ARCHIVE}' -Force"
    fi
  else
    tar -czf "$ARCHIVE" "infigraph${BIN_SUFFIX}" "infigraph-mcp${BIN_SUFFIX}" models/
  fi
  rm -f "infigraph${BIN_SUFFIX}" "infigraph-mcp${BIN_SUFFIX}"
  echo "  → ${ARCHIVE}"

  # Upload
  echo "  Uploading ${ARCHIVE}..."
  GH_HOST="$GHE_HOST" gh release upload "$VERSION" "$ARCHIVE" --clobber \
    --repo "$GHE_REPO"
  rm -f "$ARCHIVE"
  echo "  ✓ ${TARGET} done"
done

# Show all assets in this release
echo ""
echo "=============================="
echo "Released ${VERSION}"
echo ""
echo "Assets in ${VERSION}:"
GH_HOST="$GHE_HOST" gh release view "$VERSION" --repo "$GHE_REPO" --json assets --jq '.assets[].name' 2>/dev/null || true
echo ""
echo "https://${GHE_HOST}/${GHE_REPO}/releases/tag/${VERSION}"
echo "=============================="
