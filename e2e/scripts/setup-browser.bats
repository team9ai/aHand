#!/usr/bin/env bats
# E2E tests for scripts/dist/setup-browser.sh (playwright-cli version)

load 'helpers/common'

setup() {
  setup_isolated_env
  mkdir -p "$TEST_INSTALL_DIR/bin"
  export MOCK_NPM_LOG="$TEST_HOME/npm-calls.log"
}

teardown() {
  teardown_isolated_env
}

# ── Node.js detection ────────────────────────────────────────────

@test "setup-browser: detects system node >= 20" {
  export MOCK_NODE_VERSION="22.0.0"
  run bash "$DIST_DIR/setup-browser.sh"
  assert_success
  assert_output --partial "(system)"
}

@test "setup-browser: rejects system node < 20" {
  export MOCK_NODE_VERSION="18.0.0"
  run bash "$DIST_DIR/setup-browser.sh"
  assert_success
  assert_output --partial "need >= v20"
}

@test "setup-browser: installs node when missing" {
  export MOCK_NODE_ABSENT="true"
  run bash "$DIST_DIR/setup-browser.sh"
  assert_success
  assert_output --partial "Installing Node.js"
  [ -x "$TEST_INSTALL_DIR/node/bin/node" ]
}

@test "setup-browser: uses locally-installed node" {
  mkdir -p "$TEST_INSTALL_DIR/node/bin"
  cat > "$TEST_INSTALL_DIR/node/bin/node" <<'EOF'
#!/bin/bash
if [ "$1" = "-v" ]; then echo "v24.13.0"; else echo "local-node $*"; fi
EOF
  chmod +x "$TEST_INSTALL_DIR/node/bin/node"
  cat > "$TEST_INSTALL_DIR/node/bin/npm" <<'EOF'
#!/bin/bash
echo "local-npm $*"
EOF
  chmod +x "$TEST_INSTALL_DIR/node/bin/npm"

  run bash "$DIST_DIR/setup-browser.sh"
  assert_success
  assert_output --partial "(local:"
}

# ── playwright-cli installation ──────────────────────────────────

@test "setup-browser: installs playwright-cli via npm" {
  run bash "$DIST_DIR/setup-browser.sh"
  assert_success
  assert_output --partial "playwright-cli"
  assert_output --partial "[2/2]"
}

# ── 2-step flow ──────────────────────────────────────────────────

@test "setup-browser: completes all 2 steps" {
  run bash "$DIST_DIR/setup-browser.sh"
  assert_success
  assert_output --partial "[1/2]"
  assert_output --partial "[2/2]"
  assert_output --partial "Setup complete!"
}

# ── Clean and purge modes ────────────────────────────────────────

@test "setup-browser: --clean succeeds" {
  run bash "$DIST_DIR/setup-browser.sh" --clean
  assert_success
  assert_output --partial "Cleaning browser runtime"
}

@test "setup-browser: --purge removes browser dir" {
  mkdir -p "$TEST_INSTALL_DIR/browser"
  run bash "$DIST_DIR/setup-browser.sh" --purge
  assert_success
  [ ! -d "$TEST_INSTALL_DIR/browser" ]
}

# ── Error handling ───────────────────────────────────────────────

@test "setup-browser: unknown option exits with error" {
  run bash "$DIST_DIR/setup-browser.sh" --bogus
  assert_failure
  assert_output --partial "Unknown option"
}

# ── Backwards compat ─────────────────────────────────────────────

@test "setup-browser: --from-release flag is accepted (ignored)" {
  run bash "$DIST_DIR/setup-browser.sh" --from-release 0.3.0
  assert_success
}
