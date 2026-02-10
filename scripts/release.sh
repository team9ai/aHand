#!/bin/bash
# Local release build script — produces release artifacts for testing.
# Usage: bash scripts/release.sh

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
RELEASE_DIR="$ROOT_DIR/release"

# Read version from Cargo.toml
VERSION=$(grep '^version' "$ROOT_DIR/Cargo.toml" | head -1 | sed 's/version = "\(.*\)"/\1/')
echo "Building release v${VERSION}"
echo

# Detect platform
OS=$(uname -s | tr '[:upper:]' '[:lower:]')
ARCH=$(uname -m)
case "$ARCH" in
  x86_64|amd64) ARCH="x64" ;;
  aarch64|arm64) ARCH="arm64" ;;
esac
SUFFIX="${OS}-${ARCH}"

# Clean release dir
rm -rf "$RELEASE_DIR"
mkdir -p "$RELEASE_DIR"

# Step 1: Build Rust binaries
echo "==> Building Rust binaries..."
cd "$ROOT_DIR"
cargo build --release -p ahandd -p ahandctl
cp target/release/ahandd "$RELEASE_DIR/ahandd-${SUFFIX}"
cp target/release/ahandctl "$RELEASE_DIR/ahandctl-${SUFFIX}"
echo "    ahandd-${SUFFIX}"
echo "    ahandctl-${SUFFIX}"

# Step 2: Build admin SPA
echo "==> Building admin SPA..."
cd "$ROOT_DIR/apps/admin"
pnpm build
cd dist && tar czf "$RELEASE_DIR/admin-spa.tar.gz" . && cd ..
echo "    admin-spa.tar.gz"

# Step 3: Build browser-bridge daemon bundle
if [ -d "$ROOT_DIR/packages/browser-bridge" ]; then
  echo "==> Building browser-bridge daemon bundle..."
  cd "$ROOT_DIR/packages/browser-bridge"
  pnpm build
  cd dist && tar czf "$RELEASE_DIR/daemon-bundle.tar.gz" . && cd ..
  echo "    daemon-bundle.tar.gz"
fi

# Step 4: Copy scripts
echo "==> Copying scripts..."
cp "$SCRIPT_DIR/dist/setup-browser.sh" "$RELEASE_DIR/setup-browser.sh"
if [ -f "$SCRIPT_DIR/dist/upgrade.sh" ]; then
  cp "$SCRIPT_DIR/dist/upgrade.sh" "$RELEASE_DIR/upgrade.sh"
fi
cp "$SCRIPT_DIR/dist/install.sh" "$RELEASE_DIR/install.sh"

# Step 5: Generate checksums
echo "==> Generating checksums..."
cd "$RELEASE_DIR"
shasum -a 256 * > checksums.txt

echo
echo "Release artifacts in $RELEASE_DIR:"
ls -lh "$RELEASE_DIR"
echo
echo "Done! Version: v${VERSION}"
