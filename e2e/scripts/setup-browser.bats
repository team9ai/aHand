#!/usr/bin/env bats
# E2E tests for scripts/dist/setup-browser.sh

load 'helpers/common'

setup() {
  setup_isolated_env
  mkdir -p "$TEST_INSTALL_DIR/bin"
  # setup-browser.sh uses AHAND_DATA_DIR (already set by common.bash).
  # Set PROJECT_ROOT for local-build-mode tests.
  export MOCK_NPX_LOG="$TEST_HOME/npx-calls.log"
  export MOCK_PKILL_LOG="$TEST_HOME/pkill-calls.log"
}

teardown() {
  teardown_isolated_env
}

# ── Directory structure ──────────────────────────────────────────

@test "setup-browser: creates browser directory structure" {
  run bash "$DIST_DIR/setup-browser.sh" --from-release 0.3.0
  assert_success
  [ -d "$TEST_INSTALL_DIR/browser/dist" ]
  [ -d "$TEST_INSTALL_DIR/browser/sockets" ]
  [ -d "$TEST_INSTALL_DIR/bin" ]
}

# ── Node.js detection ────────────────────────────────────────────

@test "setup-browser: detects system node >= 20" {
  export MOCK_NODE_VERSION="22.0.0"
  run bash "$DIST_DIR/setup-browser.sh" --from-release 0.3.0
  assert_success
  assert_output --partial "(system)"
}

@test "setup-browser: rejects system node < 20" {
  export MOCK_NODE_VERSION="18.0.0"
  run bash "$DIST_DIR/setup-browser.sh" --from-release 0.3.0
  assert_success
  assert_output --partial "need >= v20"
}

@test "setup-browser: installs node when missing" {
  export MOCK_NODE_ABSENT="true"
  run bash "$DIST_DIR/setup-browser.sh" --from-release 0.3.0
  assert_success
  assert_output --partial "Installing Node.js"
  [ -x "$TEST_INSTALL_DIR/node/bin/node" ]
}

@test "setup-browser: uses locally-installed node" {
  # Pre-install node at the expected local path.
  mkdir -p "$TEST_INSTALL_DIR/node/bin"
  cat > "$TEST_INSTALL_DIR/node/bin/node" <<'EOF'
#!/bin/bash
if [ "$1" = "-v" ]; then echo "v24.13.0"; else echo "local-node $*"; fi
EOF
  chmod +x "$TEST_INSTALL_DIR/node/bin/node"
  cat > "$TEST_INSTALL_DIR/node/bin/npx" <<'EOF'
#!/bin/bash
echo "local-npx $*"
EOF
  chmod +x "$TEST_INSTALL_DIR/node/bin/npx"

  run bash "$DIST_DIR/setup-browser.sh" --from-release 0.3.0
  assert_success
  assert_output --partial "(local:"
}

# ── Agent-browser binary ────────────────────────────────────────

@test "setup-browser: downloads agent-browser binary" {
  run bash "$DIST_DIR/setup-browser.sh" --from-release 0.3.0
  assert_success
  assert_executable "$TEST_INSTALL_DIR/bin/agent-browser"
}

# ── Daemon bundle ────────────────────────────────────────────────

@test "setup-browser: --from-release downloads daemon bundle" {
  run bash "$DIST_DIR/setup-browser.sh" --from-release 0.3.0
  assert_success
  [ -f "$TEST_INSTALL_DIR/browser/dist/daemon.js" ]
  assert_file_contains "$TEST_INSTALL_DIR/browser/dist/package.json" '"type":"module"'
}

@test "setup-browser: --from-release without version fetches latest" {
  run bash "$DIST_DIR/setup-browser.sh" --from-release
  assert_success
  assert_curl_called_with "api.github.com"
}

@test "setup-browser: local build mode copies daemon.js" {
  # setup-browser.sh resolves PROJECT_ROOT="$SCRIPT_DIR/.."
  # SCRIPT_DIR = scripts/dist → PROJECT_ROOT = scripts/
  # It looks for: $PROJECT_ROOT/packages/browser-bridge/dist/daemon.js
  local project_root
  project_root="$(cd "$DIST_DIR/.." && pwd)"
  mkdir -p "$project_root/packages/browser-bridge/dist"
  echo 'console.log("local-build")' > "$project_root/packages/browser-bridge/dist/daemon.js"

  run bash "$DIST_DIR/setup-browser.sh"
  assert_success
  assert_output --partial "from local build"

  # Clean up test file (don't leave artifacts in scripts/packages/).
  rm -f "$project_root/packages/browser-bridge/dist/daemon.js"
  rmdir "$project_root/packages/browser-bridge/dist" 2>/dev/null || true
  rmdir "$project_root/packages/browser-bridge" 2>/dev/null || true
  rmdir "$project_root/packages" 2>/dev/null || true
}

@test "setup-browser: local build mode warns when daemon.js missing" {
  # PROJECT_ROOT = scripts/, and scripts/packages/... doesn't exist,
  # so the script should report NOT FOUND.
  local project_root
  project_root="$(cd "$DIST_DIR/.." && pwd)"
  local bridge_dist="$project_root/packages/browser-bridge/dist/daemon.js"

  # Ensure the test artifact from the previous test doesn't linger.
  rm -f "$bridge_dist" 2>/dev/null || true

  run bash "$DIST_DIR/setup-browser.sh"
  assert_success
  assert_output --partial "NOT FOUND"
}

# ── Browser detection ────────────────────────────────────────────
# Note: Chrome detection uses hardcoded absolute paths that can't easily
# be overridden in tests. We test the fallback path (no Chrome found).

@test "setup-browser: falls back to Chromium install when no Chrome" {
  # On a test system without Chrome at the hardcoded paths, it should
  # attempt to install Chromium via npx playwright.
  run bash "$DIST_DIR/setup-browser.sh" --from-release 0.3.0
  # Either system Chrome is found (test machine has it) or Chromium install runs.
  # We just verify the script completes successfully either way.
  assert_success
  assert_output --partial "Browser:"
}

# ── Runtime config ───────────────────────────────────────────────

@test "setup-browser: writes env.sh with correct paths" {
  run bash "$DIST_DIR/setup-browser.sh" --from-release 0.3.0
  assert_success
  [ -f "$TEST_INSTALL_DIR/browser/env.sh" ]
  assert_file_contains "$TEST_INSTALL_DIR/browser/env.sh" "NODE_BIN="
  assert_file_contains "$TEST_INSTALL_DIR/browser/env.sh" "AGENT_BROWSER_BIN="
  assert_file_contains "$TEST_INSTALL_DIR/browser/env.sh" "BROWSER_DIR="
}

# ── Clean and purge modes ────────────────────────────────────────

@test "setup-browser: --clean kills daemon and removes sockets" {
  mkdir -p "$TEST_INSTALL_DIR/browser/sockets"
  touch "$TEST_INSTALL_DIR/browser/sockets/foo.sock"
  run bash "$DIST_DIR/setup-browser.sh" --clean
  assert_success
  assert_output --partial "Cleaned sockets"
  [ ! -f "$TEST_INSTALL_DIR/browser/sockets/foo.sock" ]
}

@test "setup-browser: --clean preserves binaries" {
  mkdir -p "$TEST_INSTALL_DIR/bin"
  mkdir -p "$TEST_INSTALL_DIR/browser/dist"
  echo "binary" > "$TEST_INSTALL_DIR/bin/agent-browser"
  echo "daemon" > "$TEST_INSTALL_DIR/browser/dist/daemon.js"
  mkdir -p "$TEST_INSTALL_DIR/browser/sockets"

  run bash "$DIST_DIR/setup-browser.sh" --clean
  assert_success
  [ -f "$TEST_INSTALL_DIR/bin/agent-browser" ]
  [ -f "$TEST_INSTALL_DIR/browser/dist/daemon.js" ]
}

@test "setup-browser: --purge removes browser dir entirely" {
  mkdir -p "$TEST_INSTALL_DIR/browser/dist"
  mkdir -p "$TEST_INSTALL_DIR/bin"
  echo "binary" > "$TEST_INSTALL_DIR/bin/agent-browser"
  echo "daemon" > "$TEST_INSTALL_DIR/browser/dist/daemon.js"

  run bash "$DIST_DIR/setup-browser.sh" --purge
  assert_success
  [ ! -d "$TEST_INSTALL_DIR/browser" ]
  [ ! -f "$TEST_INSTALL_DIR/bin/agent-browser" ]
}

# ── Error handling ───────────────────────────────────────────────

@test "setup-browser: unknown option exits with error" {
  run bash "$DIST_DIR/setup-browser.sh" --bogus
  assert_failure
  assert_output --partial "Unknown option"
}

# ── Platform detection ───────────────────────────────────────────

@test "setup-browser: platform detection darwin-arm64" {
  export MOCK_UNAME_S="Darwin"
  export MOCK_UNAME_M="arm64"
  run bash "$DIST_DIR/setup-browser.sh" --from-release 0.3.0
  assert_success
  assert_output --partial "darwin-arm64"
}

@test "setup-browser: platform detection linux-x64" {
  export MOCK_UNAME_S="Linux"
  export MOCK_UNAME_M="x86_64"
  run bash "$DIST_DIR/setup-browser.sh" --from-release 0.3.0
  assert_success
  assert_output --partial "linux-x64"
}

# ── Stale daemon cleanup ────────────────────────────────────────

@test "setup-browser: kills stale daemons on startup" {
  run bash "$DIST_DIR/setup-browser.sh" --from-release 0.3.0
  assert_success
  assert_output --partial "Cleaning stale daemons"
  grep -q "pkill" "$MOCK_PKILL_LOG"
}

@test "setup-browser: --from-release fails when API returns no version" {
  export MOCK_CURL_FIXTURE_DIR="$TEST_HOME/empty-fixtures"
  mkdir -p "$MOCK_CURL_FIXTURE_DIR"
  echo '[]' > "$MOCK_CURL_FIXTURE_DIR/github-releases.json"
  # Copy other fixtures for non-API curl calls.
  cp "$FIXTURES_DIR/agent-browser-fake" "$MOCK_CURL_FIXTURE_DIR/"
  run bash "$DIST_DIR/setup-browser.sh" --from-release
  assert_failure
  assert_output --partial "Could not determine"
}
