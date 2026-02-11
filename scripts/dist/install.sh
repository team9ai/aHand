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

# ── Resolve versions ──────────────────────────────────────────────
# Each component has its own tag (rust-v*, admin-v*, browser-v*).
# AHAND_VERSION pins all components to a single version;
# otherwise each component resolves its own latest independently.

resolve_versions() {
  if [ -n "$AHAND_VERSION" ]; then
    RUST_VERSION="$AHAND_VERSION"
    ADMIN_VERSION="$AHAND_VERSION"
    BROWSER_VERSION="$AHAND_VERSION"
  else
    echo "Fetching latest releases..."
    local releases
    releases=$(curl -fsSL "https://api.github.com/repos/${GITHUB_REPO}/releases")

    RUST_VERSION=$(echo "$releases" | grep '"tag_name"' | grep 'rust-v' | head -1 | sed 's/.*"rust-v\([^"]*\)".*/\1/')
    ADMIN_VERSION=$(echo "$releases" | grep '"tag_name"' | grep 'admin-v' | head -1 | sed 's/.*"admin-v\([^"]*\)".*/\1/')
    BROWSER_VERSION=$(echo "$releases" | grep '"tag_name"' | grep 'browser-v' | head -1 | sed 's/.*"browser-v\([^"]*\)".*/\1/')
  fi

  if [ -z "$RUST_VERSION" ]; then
    echo "ERROR: Could not determine Rust release version"
    exit 1
  fi

  echo "Versions: rust=${RUST_VERSION} admin=${ADMIN_VERSION:-none} browser=${BROWSER_VERSION:-none}"
}

# ── Download helper ───────────────────────────────────────────────

download() {
  local url="$1"
  local dest="$2"
  echo "  Downloading $(basename "$dest")..."
  curl -fsSL "$url" -o "$dest"
}

# ── Main ──────────────────────────────────────────────────────────

main() {
  echo "==> Installing aHand..."
  echo

  detect_platform
  resolve_versions

  RUST_URL="https://github.com/${GITHUB_REPO}/releases/download/rust-v${RUST_VERSION}"

  # Create directories
  mkdir -p "$BIN_DIR"

  # Download Rust binaries (required)
  echo
  echo "==> Downloading binaries (rust-v${RUST_VERSION})..."
  download "${RUST_URL}/ahandd-${SUFFIX}" "$BIN_DIR/ahandd"
  chmod +x "$BIN_DIR/ahandd"
  download "${RUST_URL}/ahandctl-${SUFFIX}" "$BIN_DIR/ahandctl"
  chmod +x "$BIN_DIR/ahandctl"

  # Remove macOS quarantine attribute (Gatekeeper)
  if [ "$OS" = "darwin" ]; then
    xattr -d com.apple.quarantine "$BIN_DIR/ahandd" 2>/dev/null || true
    xattr -d com.apple.quarantine "$BIN_DIR/ahandctl" 2>/dev/null || true
  fi

  # Download admin SPA (optional — skip if no admin release exists)
  if [ -n "$ADMIN_VERSION" ]; then
    ADMIN_URL="https://github.com/${GITHUB_REPO}/releases/download/admin-v${ADMIN_VERSION}"
    echo
    echo "==> Downloading admin panel (admin-v${ADMIN_VERSION})..."
    mkdir -p "$INSTALL_DIR/admin/dist"
    download "${ADMIN_URL}/admin-spa.tar.gz" "/tmp/ahand-admin-spa.tar.gz"
    tar xzf /tmp/ahand-admin-spa.tar.gz -C "$INSTALL_DIR/admin/dist/"
    rm /tmp/ahand-admin-spa.tar.gz
  fi

  # Download scripts (optional — skip if no browser release exists)
  if [ -n "$BROWSER_VERSION" ]; then
    BROWSER_URL="https://github.com/${GITHUB_REPO}/releases/download/browser-v${BROWSER_VERSION}"
    echo
    echo "==> Downloading scripts (browser-v${BROWSER_VERSION})..."
    download "${BROWSER_URL}/setup-browser.sh" "$BIN_DIR/setup-browser.sh"
    chmod +x "$BIN_DIR/setup-browser.sh"
  fi

  # Write version marker (rust version is canonical)
  echo "$RUST_VERSION" > "$INSTALL_DIR/version"

  echo
  echo "==> aHand installed successfully!"
  echo
  echo "Get started:"
  echo

  # Detect shell and output appropriate PATH command
  case "${SHELL:-}" in
    */zsh)
      echo "  1. Add to PATH (paste and run):"
      echo
      echo "     echo 'export PATH=\"\$HOME/.ahand/bin:\$PATH\"' >> ~/.zshrc && source ~/.zshrc"
      ;;
    */bash)
      if [ "$OS" = "darwin" ]; then
        SHELL_RC="~/.bash_profile"
      else
        SHELL_RC="~/.bashrc"
      fi
      echo "  1. Add to PATH (paste and run):"
      echo
      echo "     echo 'export PATH=\"\$HOME/.ahand/bin:\$PATH\"' >> ${SHELL_RC} && source ${SHELL_RC}"
      ;;
    */fish)
      echo "  1. Add to PATH (paste and run):"
      echo
      echo "     fish_add_path ~/.ahand/bin"
      ;;
    *)
      echo "  1. Add to PATH:"
      echo
      echo "     export PATH=\"$BIN_DIR:\$PATH\""
      echo
      echo "     Add the line above to your shell profile to make it permanent."
      ;;
  esac

  echo
  echo "  2. Start setup:"
  echo
  echo "     ahandctl configure"
  echo
}

main
