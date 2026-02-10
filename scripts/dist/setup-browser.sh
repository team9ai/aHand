#!/bin/bash
# ── aHand Browser Setup ──────────────────────────────────────────────
# Installs agent-browser CLI + daemon bundle for browser automation.
#
# Usage:
#   ./scripts/setup-browser.sh                     # install from built artifacts (dev)
#   ./scripts/setup-browser.sh --from-release      # download from GitHub releases
#   ./scripts/setup-browser.sh --from-release 0.1.0  # specific version
#   ./scripts/setup-browser.sh --clean             # kill daemons + remove runtime files
#
# What it does:
#   1. Copies/downloads agent-browser CLI binary to ~/.ahand/bin/
#   2. Deploys ncc-bundled daemon.js to ~/.ahand/browser/dist/
#   3. Creates socket directory
#   4. Detects system Chrome (or installs Chromium as fallback)
#   5. Kills any stale daemon processes
set -e

AHAND_DIR="${AHAND_DATA_DIR:-$HOME/.ahand}"
BROWSER_DIR="$AHAND_DIR/browser"
BIN_DIR="$AHAND_DIR/bin"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$SCRIPT_DIR/.."

GITHUB_REPO="team9ai/aHand"
AGENT_BROWSER_REPO="vercel-labs/agent-browser"
# Pinned agent-browser version (from Vercel Labs)
AGENT_BROWSER_VERSION="0.9.1"

FROM_RELEASE=false
RELEASE_VERSION=""

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

# ── 1. Agent-browser CLI binary ──────────────────────────────────────
mkdir -p "$BIN_DIR"

if [ "$FROM_RELEASE" = true ]; then
  # Download from GitHub releases
  BINARY="agent-browser-${OS}-${ARCH}"
  echo "  Downloading agent-browser v${AGENT_BROWSER_VERSION} (${OS}-${ARCH})..."
  curl -fSL "https://github.com/${AGENT_BROWSER_REPO}/releases/download/v${AGENT_BROWSER_VERSION}/${BINARY}" \
    -o "$BIN_DIR/agent-browser" 2>&1 || {
    echo "  Warning: Could not download agent-browser binary, trying npm..."
    npm install -g "agent-browser@${AGENT_BROWSER_VERSION}" 2>/dev/null || true
    LOCAL_BIN=$(command -v agent-browser 2>/dev/null || true)
    if [ -n "$LOCAL_BIN" ] && [ -x "$LOCAL_BIN" ]; then
      cp "$LOCAL_BIN" "$BIN_DIR/agent-browser"
    fi
  }
  chmod +x "$BIN_DIR/agent-browser" 2>/dev/null || true
  echo "[1/4] CLI binary: installed to $BIN_DIR/agent-browser"
else
  # Source mode: prefer locally-installed binary (from npm/pnpm).
  LOCAL_BIN=$(find "$PROJECT_ROOT/node_modules/.pnpm" -name "agent-browser" -path "*/bin/*" -type f 2>/dev/null | head -1)
  if [ -z "$LOCAL_BIN" ]; then
    LOCAL_BIN=$(command -v agent-browser 2>/dev/null || true)
  fi

  if [ -n "$LOCAL_BIN" ] && [ -x "$LOCAL_BIN" ]; then
    cp "$LOCAL_BIN" "$BIN_DIR/agent-browser"
    chmod +x "$BIN_DIR/agent-browser"
    echo "[1/4] CLI binary: copied from $LOCAL_BIN"
  elif [ -x "$BIN_DIR/agent-browser" ]; then
    echo "[1/4] CLI binary: already installed at $BIN_DIR/agent-browser"
  else
    # Fallback: download from GitHub releases.
    BINARY="agent-browser-${OS}-${ARCH}"
    echo "  Downloading agent-browser v${AGENT_BROWSER_VERSION} (${OS}-${ARCH})..."
    curl -fSL "https://github.com/${AGENT_BROWSER_REPO}/releases/download/v${AGENT_BROWSER_VERSION}/${BINARY}" \
      -o "$BIN_DIR/agent-browser"
    chmod +x "$BIN_DIR/agent-browser"
    echo "[1/4] CLI binary: downloaded to $BIN_DIR/agent-browser"
  fi
fi

# ── 2. Daemon bundle (ncc-built) ─────────────────────────────────────
mkdir -p "$BROWSER_DIR/dist"

if [ "$FROM_RELEASE" = true ]; then
  # Download from GitHub releases
  echo "  Downloading daemon bundle..."
  TMP_BUNDLE="/tmp/ahand-daemon-bundle-$$.tar.gz"
  curl -fSL "https://github.com/$GITHUB_REPO/releases/download/browser-v${RELEASE_VERSION}/daemon-bundle.tar.gz" \
    -o "$TMP_BUNDLE" 2>&1 || {
    echo "  Warning: Could not download daemon bundle from release"
    echo "  Browser daemon may not work without daemon.js"
  }
  if [ -f "$TMP_BUNDLE" ]; then
    tar xzf "$TMP_BUNDLE" -C "$BROWSER_DIR/dist/"
    rm -f "$TMP_BUNDLE"
    echo '{"type":"module"}' > "$BROWSER_DIR/dist/package.json"
    echo "[2/4] Daemon bundle: deployed to $BROWSER_DIR/dist/"
  fi
else
  # Source mode: copy from local build artifacts
  BRIDGE_DIST="$PROJECT_ROOT/packages/browser-bridge/dist/daemon.js"
  if [ -f "$BRIDGE_DIST" ]; then
    cp "$BRIDGE_DIST" "$BROWSER_DIR/dist/daemon.js"
    # Copy chunks that daemon.js might dynamically import.
    for chunk in "$PROJECT_ROOT/packages/browser-bridge/dist/"*.index.js; do
      [ -f "$chunk" ] && cp "$chunk" "$BROWSER_DIR/dist/"
    done
    # Write clean package.json for ESM loading.
    echo '{"type":"module"}' > "$BROWSER_DIR/dist/package.json"
    echo "[2/4] Daemon bundle: deployed to $BROWSER_DIR/dist/"
  else
    echo "[2/4] Daemon bundle: NOT FOUND at $BRIDGE_DIST"
    echo "      Run: cd packages/browser-bridge && pnpm install && pnpm build"
    echo "      Then re-run this script."
  fi
fi

# ── 3. Socket directory ──────────────────────────────────────────────
mkdir -p "$BROWSER_DIR/sockets"
echo "[3/4] Socket directory: $BROWSER_DIR/sockets"

# ── 4. Browser detection ─────────────────────────────────────────────
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
  echo "[4/4] Browser: $CHROME_PATH (system)"
else
  echo "[4/4] Browser: no system Chrome found — installing Chromium..."
  mkdir -p "$BROWSER_DIR/browsers"
  PLAYWRIGHT_BROWSERS_PATH="$BROWSER_DIR/browsers" npx playwright install chromium
  echo "      Chromium installed to $BROWSER_DIR/browsers"
fi

# ── Summary ──────────────────────────────────────────────────────────
echo ""
echo "Setup complete!"
echo "  Binary:   $BIN_DIR/agent-browser"
echo "  Daemon:   $BROWSER_DIR/dist/daemon.js"
echo "  Sockets:  $BROWSER_DIR/sockets"
[ -n "$CHROME_PATH" ] && echo "  Chrome:   $CHROME_PATH"
echo ""
echo "Config example (toml):"
echo "  [browser]"
echo "  enabled = true"
echo "  headed = true    # show browser window (optional)"
