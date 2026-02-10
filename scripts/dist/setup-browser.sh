#!/bin/bash
# ── aHand Browser Setup ──────────────────────────────────────────────
# Installs browser automation dependencies for aHand.
#
# Usage:
#   setup-browser.sh                          # default install
#   setup-browser.sh --from-release           # download daemon bundle from GitHub releases
#   setup-browser.sh --from-release 0.1.0     # specific release version
#   setup-browser.sh --clean                  # kill daemons + remove runtime files
#   setup-browser.sh --purge                  # remove entire browser installation
#
# What it does:
#   1. Ensures Node.js >= 20 (installs prebuilt LTS if missing)
#   2. Downloads agent-browser CLI binary (pinned version from Vercel Labs)
#   3. Deploys ncc-bundled daemon.js
#   4. Creates socket directory
#   5. Detects system Chrome (or installs Chromium as fallback)
#   6. Kills any stale daemon processes
set -e

AHAND_DIR="${AHAND_DATA_DIR:-$HOME/.ahand}"
BROWSER_DIR="$AHAND_DIR/browser"
BIN_DIR="$AHAND_DIR/bin"
NODE_DIR="$AHAND_DIR/node"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$SCRIPT_DIR/.."

GITHUB_REPO="team9ai/aHand"
AGENT_BROWSER_REPO="vercel-labs/agent-browser"
# Pinned versions
AGENT_BROWSER_VERSION="0.9.1"
NODE_MIN_VERSION=20
NODE_LTS_VERSION="24.13.0"

FROM_RELEASE=false
RELEASE_VERSION=""

STEPS=6

# ── Parse arguments ──────────────────────────────────────────────────
while [ $# -gt 0 ]; do
  case "$1" in
    --clean)
      echo "Cleaning browser runtime..."
      pkill -f "$BROWSER_DIR/dist/daemon.js" 2>/dev/null && echo "  Killed daemon process" || true
      pkill -f "agent-browser.*daemon" 2>/dev/null || true
      rm -rf "$BROWSER_DIR/sockets"
      echo "  Cleaned sockets"
      echo "Done. (Binary and daemon bundle preserved)"
      exit 0
      ;;
    --purge)
      echo "Purging browser installation..."
      pkill -f "$BROWSER_DIR/dist/daemon.js" 2>/dev/null || true
      pkill -f "agent-browser.*daemon" 2>/dev/null || true
      rm -rf "$BROWSER_DIR" "$BIN_DIR/agent-browser"
      echo "  Purged $BROWSER_DIR and $BIN_DIR/agent-browser"
      exit 0
      ;;
    --from-release)
      FROM_RELEASE=true
      if [ -n "$2" ] && [ "${2:0:1}" != "-" ]; then
        RELEASE_VERSION="$2"
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

# ── Kill stale daemons first ──────────────────────────────────────────
echo "Cleaning stale daemons..."
pkill -f "$BROWSER_DIR/dist/daemon.js" 2>/dev/null && echo "  Killed old daemon" || true
rm -f "$BROWSER_DIR/sockets/"*.sock 2>/dev/null || true

# ── Resolve release version ──────────────────────────────────────────
if [ "$FROM_RELEASE" = true ] && [ -z "$RELEASE_VERSION" ]; then
  echo "Fetching latest browser release version..."
  RELEASE_VERSION=$(curl -s "https://api.github.com/repos/$GITHUB_REPO/releases" \
    | grep '"tag_name"' | grep 'browser-v' | head -1 | sed 's/.*"browser-v\([^"]*\)".*/\1/')
  if [ -z "$RELEASE_VERSION" ]; then
    echo "Error: Could not determine latest browser release version"
    exit 1
  fi
  echo "  Latest version: $RELEASE_VERSION"
fi

# ── 1. Node.js ───────────────────────────────────────────────────────
# Daemon.js requires Node.js >= 20. Check system node first,
# then fall back to our locally-installed copy, or install one.

ensure_node() {
  # Check if we already installed node locally
  if [ -x "$NODE_DIR/bin/node" ]; then
    NODE_BIN="$NODE_DIR/bin/node"
    NPX_BIN="$NODE_DIR/bin/npx"
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
      NPX_BIN=$(command -v npx 2>/dev/null || true)
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
  NPX_BIN="$NODE_DIR/bin/npx"
  echo "[1/$STEPS] Node.js: $("$NODE_BIN" -v) (installed to $NODE_DIR)"
}

ensure_node

# ── 2. Agent-browser CLI binary ──────────────────────────────────────
mkdir -p "$BIN_DIR"

BINARY="agent-browser-${OS}-${ARCH}"
echo "  Downloading agent-browser v${AGENT_BROWSER_VERSION} (${OS}-${ARCH})..."
curl -fsSL "https://github.com/${AGENT_BROWSER_REPO}/releases/download/v${AGENT_BROWSER_VERSION}/${BINARY}" \
  -o "$BIN_DIR/agent-browser"
chmod +x "$BIN_DIR/agent-browser"
echo "[2/$STEPS] CLI binary: $BIN_DIR/agent-browser"

# ── 3. Daemon bundle (ncc-built) ─────────────────────────────────────
mkdir -p "$BROWSER_DIR/dist"

if [ "$FROM_RELEASE" = true ]; then
  echo "  Downloading daemon bundle..."
  TMP_BUNDLE="/tmp/ahand-daemon-bundle-$$.tar.gz"
  curl -fsSL "https://github.com/$GITHUB_REPO/releases/download/browser-v${RELEASE_VERSION}/daemon-bundle.tar.gz" \
    -o "$TMP_BUNDLE"
  tar xzf "$TMP_BUNDLE" -C "$BROWSER_DIR/dist/"
  rm -f "$TMP_BUNDLE"
  echo '{"type":"module"}' > "$BROWSER_DIR/dist/package.json"
  echo "[3/$STEPS] Daemon bundle: $BROWSER_DIR/dist/"
else
  BRIDGE_DIST="$PROJECT_ROOT/packages/browser-bridge/dist/daemon.js"
  if [ -f "$BRIDGE_DIST" ]; then
    cp "$BRIDGE_DIST" "$BROWSER_DIR/dist/daemon.js"
    for chunk in "$PROJECT_ROOT/packages/browser-bridge/dist/"*.index.js; do
      [ -f "$chunk" ] && cp "$chunk" "$BROWSER_DIR/dist/"
    done
    echo '{"type":"module"}' > "$BROWSER_DIR/dist/package.json"
    echo "[3/$STEPS] Daemon bundle: $BROWSER_DIR/dist/ (from local build)"
  else
    echo "[3/$STEPS] Daemon bundle: NOT FOUND at $BRIDGE_DIST"
    echo "      Run: cd packages/browser-bridge && pnpm install && pnpm build"
    echo "      Then re-run this script."
  fi
fi

# ── 4. Socket directory ──────────────────────────────────────────────
mkdir -p "$BROWSER_DIR/sockets"
echo "[4/$STEPS] Socket directory: $BROWSER_DIR/sockets"

# ── 5. Browser detection ─────────────────────────────────────────────
CHROME_PATH=""
if [ "$(uname -s)" = "Darwin" ]; then
  for candidate in \
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" \
    "/Applications/Google Chrome Dev.app/Contents/MacOS/Google Chrome Dev" \
    "/Applications/Google Chrome Canary.app/Contents/MacOS/Google Chrome Canary" \
    "/Applications/Chromium.app/Contents/MacOS/Chromium"; do
    if [ -x "$candidate" ]; then
      CHROME_PATH="$candidate"
      break
    fi
  done
else
  for candidate in \
    /usr/bin/google-chrome \
    /usr/bin/google-chrome-stable \
    /usr/bin/chromium \
    /usr/bin/chromium-browser; do
    if [ -x "$candidate" ]; then
      CHROME_PATH="$candidate"
      break
    fi
  done
fi

if [ -n "$CHROME_PATH" ]; then
  echo "[5/$STEPS] Browser: $CHROME_PATH (system)"
else
  echo "[5/$STEPS] Browser: no system Chrome found — installing Chromium..."
  mkdir -p "$BROWSER_DIR/browsers"
  PLAYWRIGHT_BROWSERS_PATH="$BROWSER_DIR/browsers" "$NPX_BIN" playwright install chromium
  echo "      Chromium installed to $BROWSER_DIR/browsers"
fi

# ── 6. Write runtime config ──────────────────────────────────────────
# Save resolved paths so the daemon knows where to find node/chrome.
cat > "$BROWSER_DIR/env.sh" <<ENVEOF
# Auto-generated by setup-browser.sh — do not edit.
NODE_BIN="$NODE_BIN"
AGENT_BROWSER_BIN="$BIN_DIR/agent-browser"
AGENT_BROWSER_VERSION="$AGENT_BROWSER_VERSION"
CHROME_PATH="${CHROME_PATH:-}"
BROWSER_DIR="$BROWSER_DIR"
ENVEOF
echo "[6/$STEPS] Runtime config: $BROWSER_DIR/env.sh"

# ── Summary ──────────────────────────────────────────────────────────
echo ""
echo "Setup complete!"
echo "  Node.js:  $("$NODE_BIN" -v) ($NODE_BIN)"
echo "  Binary:   $BIN_DIR/agent-browser"
echo "  Daemon:   $BROWSER_DIR/dist/daemon.js"
echo "  Sockets:  $BROWSER_DIR/sockets"
[ -n "$CHROME_PATH" ] && echo "  Chrome:   $CHROME_PATH"
echo ""
echo "Config example (toml):"
echo "  [browser]"
echo "  enabled = true"
echo "  headed = true    # show browser window (optional)"
