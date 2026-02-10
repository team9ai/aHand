#!/bin/bash
# One-line installer for aHand.
# Usage: curl -fsSL https://raw.githubusercontent.com/team9ai/aHand/main/scripts/dist/install.sh | bash
#
# Environment variables:
#   AHAND_VERSION — install a specific version (default: latest)
#   AHAND_DIR     — install directory (default: ~/.ahand)

set -e

GITHUB_REPO="team9ai/aHand"
INSTALL_DIR="${AHAND_DIR:-$HOME/.ahand}"
BIN_DIR="$INSTALL_DIR/bin"

# ── Detect platform ────────────────────────────────────────────────

detect_platform() {
  OS=$(uname -s | tr '[:upper:]' '[:lower:]')
  ARCH=$(uname -m)

  case "$OS" in
    linux) ;;
    darwin) ;;
    *)
      echo "ERROR: Unsupported OS: $OS"
      exit 1
      ;;
  esac

  case "$ARCH" in
    x86_64|amd64) ARCH="x64" ;;
    aarch64|arm64) ARCH="arm64" ;;
    *)
      echo "ERROR: Unsupported architecture: $ARCH"
      exit 1
      ;;
  esac

  SUFFIX="${OS}-${ARCH}"
  echo "Platform: ${SUFFIX}"
}

# ── Resolve version ───────────────────────────────────────────────
# Each component has its own tag (rust-v*, admin-v*, browser-v*).
# AHAND_VERSION sets the shared version number; tags are derived.

resolve_version() {
  if [ -n "$AHAND_VERSION" ]; then
    VERSION="$AHAND_VERSION"
  else
    echo "Fetching latest release..."
    # Use rust release as the canonical version (first released component)
    local tag
    tag=$(curl -fsSL "https://api.github.com/repos/${GITHUB_REPO}/releases" \
      | grep '"tag_name"' | grep 'rust-v' | head -1 | sed 's/.*"rust-v\([^"]*\)".*/\1/')
    VERSION="$tag"
  fi

  if [ -z "$VERSION" ]; then
    echo "ERROR: Could not determine version"
    exit 1
  fi

  echo "Version: ${VERSION}"
}

# ── Download helper ───────────────────────────────────────────────

download() {
  local url="$1"
  local dest="$2"
  echo "  Downloading $(basename "$dest")..."
  curl -fSL "$url" -o "$dest"
}

# ── Main ──────────────────────────────────────────────────────────

main() {
  echo "==> Installing aHand..."
  echo

  detect_platform
  resolve_version

  RUST_URL="https://github.com/${GITHUB_REPO}/releases/download/rust-v${VERSION}"
  ADMIN_URL="https://github.com/${GITHUB_REPO}/releases/download/admin-v${VERSION}"
  BROWSER_URL="https://github.com/${GITHUB_REPO}/releases/download/browser-v${VERSION}"

  # Create directories
  mkdir -p "$BIN_DIR"
  mkdir -p "$INSTALL_DIR/admin/dist"

  # Download Rust binaries
  echo
  echo "==> Downloading binaries..."
  download "${RUST_URL}/ahandd-${SUFFIX}" "$BIN_DIR/ahandd"
  chmod +x "$BIN_DIR/ahandd"
  download "${RUST_URL}/ahandctl-${SUFFIX}" "$BIN_DIR/ahandctl"
  chmod +x "$BIN_DIR/ahandctl"

  # Download admin SPA
  echo
  echo "==> Downloading admin panel..."
  download "${ADMIN_URL}/admin-spa.tar.gz" "/tmp/ahand-admin-spa.tar.gz"
  tar xzf /tmp/ahand-admin-spa.tar.gz -C "$INSTALL_DIR/admin/dist/"
  rm /tmp/ahand-admin-spa.tar.gz

  # Download scripts
  echo
  echo "==> Downloading scripts..."
  download "${BROWSER_URL}/setup-browser.sh" "$BIN_DIR/setup-browser.sh"
  chmod +x "$BIN_DIR/setup-browser.sh"

  # Write version marker
  echo "$VERSION" > "$INSTALL_DIR/version"

  echo
  echo "==> aHand installed successfully!"
  echo
  echo "Installation directory: $INSTALL_DIR"
  echo "Binaries:              $BIN_DIR"
  echo
  echo "Add to your PATH:"
  echo "  export PATH=\"$BIN_DIR:\$PATH\""
  echo
  echo "Then run:"
  echo "  ahandctl configure    # Open admin panel"
  echo "  ahandctl browser-init # Install browser automation deps"
  echo
}

main
