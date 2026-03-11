#!/bin/bash
# ── aHand Browser Setup ──────────────────────────────────────────────
# Installs browser automation dependencies for aHand.
#
# Usage:
#   setup-browser.sh                          # default install
#   setup-browser.sh --clean                  # remove runtime files
#   setup-browser.sh --purge                  # remove entire browser installation
#
# What it does:
#   1. Ensures Node.js >= 20 (installs prebuilt LTS if missing)
#   2. Installs @playwright/cli via npm
set -e

AHAND_DIR="${AHAND_DATA_DIR:-$HOME/.ahand}"
BROWSER_DIR="$AHAND_DIR/browser"
BIN_DIR="$AHAND_DIR/bin"
NODE_DIR="$AHAND_DIR/node"

# Pinned versions
PLAYWRIGHT_CLI_VERSION="0.1.1"
NODE_MIN_VERSION=20
NODE_LTS_VERSION="24.13.0"

STEPS=2

# ── Parse arguments ──────────────────────────────────────────────────
while [ $# -gt 0 ]; do
  case "$1" in
    --clean)
      echo "Cleaning browser runtime..."
      echo "  (no daemon processes to kill — playwright-cli manages its own lifecycle)"
      echo "Done."
      exit 0
      ;;
    --purge)
      echo "Purging browser installation..."
      rm -rf "$BROWSER_DIR"
      # Remove playwright-cli from node globals
      if [ -x "$NODE_DIR/bin/npm" ]; then
        "$NODE_DIR/bin/npm" uninstall -g @playwright/cli 2>/dev/null || true
      fi
      echo "  Purged $BROWSER_DIR and @playwright/cli"
      exit 0
      ;;
    --from-release)
      # Kept for backwards compat — ignored (no daemon bundle to download)
      if [ -n "$2" ] && [ "${2:0:1}" != "-" ]; then
        shift
      fi
      ;;
    *)
      echo "Unknown option: $1"
      exit 1
      ;;
  esac
  shift
done

# ── Detect platform ──────────────────────────────────────────────────
OS=$(uname -s | tr '[:upper:]' '[:lower:]')
ARCH=$(uname -m)
case "$ARCH" in
  x86_64)        ARCH="x64" ;;
  aarch64|arm64) ARCH="arm64" ;;
esac

# ── 1. Node.js ───────────────────────────────────────────────────────
ensure_node() {
  # Check if we already installed node locally
  if [ -x "$NODE_DIR/bin/node" ]; then
    NODE_BIN="$NODE_DIR/bin/node"
    NPM_BIN="$NODE_DIR/bin/npm"
    local ver
    ver=$("$NODE_BIN" -v 2>/dev/null | sed 's/v//' | cut -d. -f1)
    if [ "$ver" -ge "$NODE_MIN_VERSION" ] 2>/dev/null; then
      echo "[1/$STEPS] Node.js: $("$NODE_BIN" -v) (local: $NODE_DIR)"
      return 0
    fi
  fi

  # Check system node
  local sys_node
  sys_node=$(command -v node 2>/dev/null || true)
  if [ -n "$sys_node" ]; then
    local ver
    ver=$("$sys_node" -v 2>/dev/null | sed 's/v//' | cut -d. -f1)
    if [ "$ver" -ge "$NODE_MIN_VERSION" ] 2>/dev/null; then
      NODE_BIN="$sys_node"
      NPM_BIN=$(command -v npm 2>/dev/null || true)
      echo "[1/$STEPS] Node.js: $("$NODE_BIN" -v) (system)"
      return 0
    fi
    echo "  System node is v$("$sys_node" -v), need >= v${NODE_MIN_VERSION}"
  fi

  # Install prebuilt Node.js LTS
  echo "  Installing Node.js v${NODE_LTS_VERSION}..."
  local node_arch="$ARCH"
  local tarball="node-v${NODE_LTS_VERSION}-${OS}-${node_arch}.tar.xz"
  local url="https://nodejs.org/dist/v${NODE_LTS_VERSION}/${tarball}"
  local tmp="/tmp/ahand-node-$$.tar.xz"

  curl -fsSL "$url" -o "$tmp"
  mkdir -p "$NODE_DIR"
  tar xJf "$tmp" -C "$NODE_DIR" --strip-components=1
  rm -f "$tmp"

  NODE_BIN="$NODE_DIR/bin/node"
  NPM_BIN="$NODE_DIR/bin/npm"
  echo "[1/$STEPS] Node.js: $("$NODE_BIN" -v) (installed to $NODE_DIR)"
}

ensure_node

# ── 2. playwright-cli ────────────────────────────────────────────────
# Use the aHand-managed npm to install globally so binary lands at
# ~/.ahand/node/bin/playwright-cli (no sudo needed).
echo "  Installing @playwright/cli@${PLAYWRIGHT_CLI_VERSION}..."
"$NPM_BIN" install -g "@playwright/cli@${PLAYWRIGHT_CLI_VERSION}" --silent 2>/dev/null || \
  "$NPM_BIN" install -g "@playwright/cli@${PLAYWRIGHT_CLI_VERSION}"

PLAYWRIGHT_CLI="$NODE_DIR/bin/playwright-cli"
if [ ! -x "$PLAYWRIGHT_CLI" ]; then
  # Fallback: check if npm linked it somewhere else
  PLAYWRIGHT_CLI=$(command -v playwright-cli 2>/dev/null || true)
fi

if [ -x "$PLAYWRIGHT_CLI" ]; then
  echo "[2/$STEPS] playwright-cli: $("$PLAYWRIGHT_CLI" --version 2>/dev/null || echo 'installed') ($PLAYWRIGHT_CLI)"
else
  echo "[2/$STEPS] playwright-cli: FAILED to install"
  echo "      Try: $NPM_BIN install -g @playwright/cli@${PLAYWRIGHT_CLI_VERSION}"
  exit 1
fi

# ── Summary ──────────────────────────────────────────────────────────
echo ""
echo "Setup complete!"
echo "  Node.js:        $("$NODE_BIN" -v) ($NODE_BIN)"
echo "  playwright-cli: $PLAYWRIGHT_CLI"
echo ""
echo "playwright-cli will use the browser installed on your system (Chrome, Edge, etc.)."
