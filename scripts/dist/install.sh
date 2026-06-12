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

# ── Checksum helpers ──────────────────────────────────────────────
# Detect the available SHA-256 tool: `shasum -a 256` (macOS) or
# `sha256sum` (Linux). Mirrors the fallback logic in upgrade.sh and
# the test mock. Prints the hex digest of "$1" on stdout.

sha256_of() {
  local file="$1"
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$file" | awk '{print $1}'
  elif command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$file" | awk '{print $1}'
  else
    echo "ERROR: No SHA-256 tool found (need 'shasum' or 'sha256sum')" >&2
    exit 1
  fi
}

# Verify a downloaded file against a checksum file (FAIL-CLOSED).
#   $1 = path to the local downloaded file
#   $2 = name to look up inside the checksum file (the released filename)
#   $3 = path to the checksum file (format: "<hex>  <filename>")
# Aborts the install on a missing/unreadable checksum or a mismatch.

verify_checksum() {
  local file="$1"
  local checksum_name="$2"
  local checksum_file="$3"

  if [ ! -r "$checksum_file" ]; then
    echo "ERROR: Checksum file not available for ${checksum_name} — refusing to install unverified artifact."
    exit 1
  fi

  # Exact filename match (no regex): the checksum filename is compared
  # literally, so `.` in names like `admin-spa.tar.gz` cannot act as a
  # wildcard and select a decoy line. Handles both `shasum` (`<hash>  <file>`,
  # two spaces) and `sha256sum` binary mode (`<hash> *<file>`).
  local expected
  expected=$(awk -v n="$checksum_name" '$2==n || $2=="*"n {print $1; exit}' "$checksum_file" 2>/dev/null)
  if [ -z "$expected" ]; then
    echo "ERROR: No checksum entry for ${checksum_name} in checksum file — refusing to install unverified artifact."
    exit 1
  fi

  local actual
  actual=$(sha256_of "$file")
  if [ "$expected" != "$actual" ]; then
    echo "ERROR: Checksum mismatch for ${checksum_name}"
    echo "  Expected: $expected"
    echo "  Actual:   $actual"
    exit 1
  fi
  echo "  Checksum OK: ${checksum_name}"
}

# ── Main ──────────────────────────────────────────────────────────

main() {
  echo "==> Installing aHand..."
  echo

  detect_platform
  resolve_versions

  RUST_URL="https://github.com/${GITHUB_REPO}/releases/download/rust-v${RUST_VERSION}"

  # Per-invocation temp dir (parallel-safe; auto-cleaned on exit).
  # Use an explicit template so TMPDIR is honoured on macOS (bare `mktemp -d`
  # on macOS ignores TMPDIR in favour of CS_DARWIN_USER_TEMP_DIR).
  TMP_DIR=$(mktemp -d "${TMPDIR:-/tmp}/ahand-install-XXXXXX") || { echo "ERROR: failed to create temp dir" >&2; exit 1; }
  trap 'rm -rf "$TMP_DIR"' EXIT

  # Create directories
  mkdir -p "$BIN_DIR"

  # Download Rust binaries (required) into temp, then verify before installing
  echo
  echo "==> Downloading binaries (rust-v${RUST_VERSION})..."
  download "${RUST_URL}/ahandd-${SUFFIX}" "$TMP_DIR/ahandd"
  download "${RUST_URL}/ahandctl-${SUFFIX}" "$TMP_DIR/ahandctl"

  # Verify SHA-256 checksums (fail-closed) before installing anything.
  # The checksum download is tolerant of network failure so that a missing
  # checksum surfaces verify_checksum's clear fail-closed error rather than an
  # opaque curl error. verify_checksum still aborts when the file is absent.
  #
  # INTENTIONAL FAIL-CLOSED ASYMMETRY: install.sh treats a missing or
  # unreadable checksum as a hard abort (first-run trust boundary — we have
  # no previously-verified install to fall back on, so a checksum-strip MITM
  # attack must be rejected).  upgrade.sh and install.ps1 are intentionally
  # lenient-on-missing because the existing install is already trusted.
  # Do NOT relax this to match the lenient siblings.
  echo
  echo "==> Verifying checksums..."
  download "${RUST_URL}/checksums-rust.txt" "$TMP_DIR/checksums-rust.txt" || true
  verify_checksum "$TMP_DIR/ahandd"   "ahandd-${SUFFIX}"   "$TMP_DIR/checksums-rust.txt"
  verify_checksum "$TMP_DIR/ahandctl" "ahandctl-${SUFFIX}" "$TMP_DIR/checksums-rust.txt"

  # Install verified binaries.
  cp "$TMP_DIR/ahandd" "$BIN_DIR/ahandd"
  chmod +x "$BIN_DIR/ahandd"
  cp "$TMP_DIR/ahandctl" "$BIN_DIR/ahandctl"
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
    download "${ADMIN_URL}/admin-spa.tar.gz" "$TMP_DIR/admin-spa.tar.gz"
    download "${ADMIN_URL}/checksums-admin.txt" "$TMP_DIR/checksums-admin.txt" || true
    verify_checksum "$TMP_DIR/admin-spa.tar.gz" "admin-spa.tar.gz" "$TMP_DIR/checksums-admin.txt"
    tar xzf "$TMP_DIR/admin-spa.tar.gz" -C "$INSTALL_DIR/admin/dist/"
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
