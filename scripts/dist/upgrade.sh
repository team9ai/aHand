#!/bin/bash
# Upgrades ahandd, ahandctl, and admin SPA from GitHub releases.
# Usage: upgrade.sh [--check] [--version 0.2.0]

set -e

GITHUB_REPO="team9ai/aHand"
INSTALL_DIR="${AHAND_DIR:-$HOME/.ahand}"
BIN_DIR="$INSTALL_DIR/bin"

CHECK_ONLY=false
TARGET_VERSION=""

# ── Parse arguments ───────────────────────────────────────────────

while [[ $# -gt 0 ]]; do
  case "$1" in
    --check)
      CHECK_ONLY=true
      shift
      ;;
    --version)
      TARGET_VERSION="$2"
      shift 2
      ;;
    *)
      echo "Usage: upgrade.sh [--check] [--version 0.2.0]"
      exit 1
      ;;
  esac
done

# ── Detect platform ──────────────────────────────────────────────

detect_platform() {
  OS=$(uname -s | tr '[:upper:]' '[:lower:]')
  ARCH=$(uname -m)

  case "$ARCH" in
    x86_64|amd64) ARCH="x64" ;;
    aarch64|arm64) ARCH="arm64" ;;
    *)
      echo "ERROR: Unsupported architecture: $ARCH"
      exit 1
      ;;
  esac

  SUFFIX="${OS}-${ARCH}"
}

# ── Get current version ──────────────────────────────────────────

get_current_version() {
  if [ -f "$INSTALL_DIR/version" ]; then
    CURRENT_VERSION=$(cat "$INSTALL_DIR/version" | tr -d '[:space:]')
  elif [ -x "$BIN_DIR/ahandctl" ]; then
    CURRENT_VERSION=$("$BIN_DIR/ahandctl" --version 2>/dev/null | awk '{print $2}' || echo "unknown")
  else
    CURRENT_VERSION="unknown"
  fi
}

# ── Get latest version ───────────────────────────────────────────
# Uses rust release as canonical version source.

get_latest_version() {
  if [ -n "$TARGET_VERSION" ]; then
    LATEST_VERSION="$TARGET_VERSION"
  else
    LATEST_VERSION=$(curl -fsSL "https://api.github.com/repos/${GITHUB_REPO}/releases" \
      | grep '"tag_name"' | grep 'rust-v' | head -1 | sed 's/.*"rust-v\([^"]*\)".*/\1/')
  fi

  if [ -z "$LATEST_VERSION" ]; then
    echo "ERROR: Could not determine latest version"
    exit 1
  fi
}

# ── Download helper ──────────────────────────────────────────────

download() {
  local url="$1"
  local dest="$2"
  echo "  Downloading $(basename "$dest")..."
  curl -fSL "$url" -o "$dest"
}

# ── Main ─────────────────────────────────────────────────────────

detect_platform
get_current_version
get_latest_version

echo "Current version: ${CURRENT_VERSION}"
echo "Latest version:  ${LATEST_VERSION}"
echo "Platform:        ${SUFFIX}"
echo

if [ "$CURRENT_VERSION" = "$LATEST_VERSION" ]; then
  echo "Already up to date!"
  exit 0
fi

if [ "$CHECK_ONLY" = true ]; then
  echo "Update available: ${CURRENT_VERSION} -> ${LATEST_VERSION}"
  echo "Run: ahandctl upgrade"
  exit 0
fi

echo "Upgrading: ${CURRENT_VERSION} -> ${LATEST_VERSION}"
echo

RUST_URL="https://github.com/${GITHUB_REPO}/releases/download/rust-v${LATEST_VERSION}"
ADMIN_URL="https://github.com/${GITHUB_REPO}/releases/download/admin-v${LATEST_VERSION}"
BROWSER_URL="https://github.com/${GITHUB_REPO}/releases/download/browser-v${LATEST_VERSION}"
TMP_DIR=$(mktemp -d)
trap "rm -rf $TMP_DIR" EXIT

# Download checksums for verification
echo "==> Downloading checksums..."
download "${RUST_URL}/checksums-rust.txt" "$TMP_DIR/checksums-rust.txt" 2>/dev/null || true

# Download binaries
echo "==> Downloading binaries..."
download "${RUST_URL}/ahandd-${SUFFIX}" "$TMP_DIR/ahandd"
download "${RUST_URL}/ahandctl-${SUFFIX}" "$TMP_DIR/ahandctl"

# Download admin SPA
echo "==> Downloading admin panel..."
download "${ADMIN_URL}/admin-spa.tar.gz" "$TMP_DIR/admin-spa.tar.gz"

# Download scripts
echo "==> Downloading scripts..."
download "${BROWSER_URL}/setup-browser.sh" "$TMP_DIR/setup-browser.sh" 2>/dev/null || true

# Verify checksums if available
if [ -f "$TMP_DIR/checksums-rust.txt" ]; then
  echo
  echo "==> Verifying checksums..."
  cd "$TMP_DIR"
  # Check binary checksums
  for f in ahandd ahandctl; do
    expected=$(grep "${f}-${SUFFIX}" checksums-rust.txt 2>/dev/null | awk '{print $1}' || echo "")
    if [ -n "$expected" ]; then
      actual=$(shasum -a 256 "$f" | awk '{print $1}')
      if [ "$expected" != "$actual" ]; then
        echo "ERROR: Checksum mismatch for $f"
        echo "  Expected: $expected"
        echo "  Actual:   $actual"
        exit 1
      fi
      echo "  $f: OK"
    fi
  done
fi

# Stop daemon if running
DAEMON_PID=""
if [ -f "$INSTALL_DIR/data/daemon.pid" ]; then
  DAEMON_PID=$(cat "$INSTALL_DIR/data/daemon.pid" 2>/dev/null || echo "")
  if [ -n "$DAEMON_PID" ] && kill -0 "$DAEMON_PID" 2>/dev/null; then
    echo
    echo "==> Stopping daemon (PID $DAEMON_PID)..."
    kill "$DAEMON_PID" 2>/dev/null || true
    sleep 2
  fi
fi

# Install binaries
echo
echo "==> Installing binaries..."
mkdir -p "$BIN_DIR"
cp "$TMP_DIR/ahandd" "$BIN_DIR/ahandd"
chmod +x "$BIN_DIR/ahandd"
cp "$TMP_DIR/ahandctl" "$BIN_DIR/ahandctl"
chmod +x "$BIN_DIR/ahandctl"

# Install admin SPA
echo "==> Installing admin panel..."
mkdir -p "$INSTALL_DIR/admin/dist"
rm -rf "$INSTALL_DIR/admin/dist/*"
tar xzf "$TMP_DIR/admin-spa.tar.gz" -C "$INSTALL_DIR/admin/dist/"

# Install scripts
if [ -f "$TMP_DIR/setup-browser.sh" ]; then
  cp "$TMP_DIR/setup-browser.sh" "$BIN_DIR/setup-browser.sh"
  chmod +x "$BIN_DIR/setup-browser.sh"
fi

# Write version marker
echo "$LATEST_VERSION" > "$INSTALL_DIR/version"

echo
echo "==> Upgrade complete!"
echo "  ${CURRENT_VERSION} -> ${LATEST_VERSION}"
echo
echo "Restart the daemon to use the new version:"
echo "  ahandd --config ~/.ahand/config.toml"
