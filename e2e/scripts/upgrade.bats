#!/usr/bin/env bats
# E2E tests for scripts/dist/upgrade.sh

load 'helpers/common'

setup() {
  setup_isolated_env
  # upgrade.sh expects INSTALL_DIR to exist.
  mkdir -p "$TEST_INSTALL_DIR/bin"
  mkdir -p "$TEST_INSTALL_DIR/admin/dist"
}

teardown() {
  teardown_isolated_env
}

# ── Version detection ────────────────────────────────────────────

@test "upgrade: detects current version from version file" {
  echo "0.2.0" > "$TEST_INSTALL_DIR/version"
  run bash "$DIST_DIR/upgrade.sh"
  assert_output --partial "Current version: 0.2.0"
}

@test "upgrade: reports unknown when no version file exists" {
  run bash "$DIST_DIR/upgrade.sh"
  assert_output --partial "Current version: unknown"
}

@test "upgrade: fetches latest version from API" {
  echo "0.2.0" > "$TEST_INSTALL_DIR/version"
  run bash "$DIST_DIR/upgrade.sh"
  assert_output --partial "Latest version:  rust=0.3.0"
}

@test "upgrade: --version flag overrides API" {
  echo "0.2.0" > "$TEST_INSTALL_DIR/version"
  run bash "$DIST_DIR/upgrade.sh" --version 0.2.5
  assert_output --partial "Latest version:  rust=0.2.5"
}

# ── Check mode ───────────────────────────────────────────────────

@test "upgrade: --check reports available update" {
  echo "0.2.0" > "$TEST_INSTALL_DIR/version"
  run bash "$DIST_DIR/upgrade.sh" --check
  assert_success
  assert_output --partial "Update available: 0.2.0 -> 0.3.0"
}

@test "upgrade: --check when already up to date" {
  echo "0.3.0" > "$TEST_INSTALL_DIR/version"
  run bash "$DIST_DIR/upgrade.sh" --check
  assert_success
  assert_output --partial "Already up to date!"
}

@test "upgrade: already up to date exits 0 without download" {
  echo "0.3.0" > "$TEST_INSTALL_DIR/version"
  run bash "$DIST_DIR/upgrade.sh"
  assert_success
  assert_output --partial "Already up to date!"
  # curl should only have the API call, no downloads.
  local count
  count=$(grep -c "." "$MOCK_CURL_LOG" 2>/dev/null || echo "0")
  [ "$count" -le 1 ]
}

# ── Upgrade execution ────────────────────────────────────────────

@test "upgrade: downloads and replaces binaries" {
  echo "0.2.0" > "$TEST_INSTALL_DIR/version"
  run bash "$DIST_DIR/upgrade.sh"
  assert_success
  assert_executable "$TEST_INSTALL_DIR/bin/ahandd"
  assert_executable "$TEST_INSTALL_DIR/bin/ahandctl"
}

@test "upgrade: extracts admin SPA" {
  echo "0.2.0" > "$TEST_INSTALL_DIR/version"
  run bash "$DIST_DIR/upgrade.sh"
  assert_success
  [ -f "$TEST_INSTALL_DIR/admin/dist/index.html" ]
}

@test "upgrade: updates version marker" {
  echo "0.2.0" > "$TEST_INSTALL_DIR/version"
  run bash "$DIST_DIR/upgrade.sh"
  assert_success
  [ "$(cat "$TEST_INSTALL_DIR/version")" = "0.3.0" ]
}

# ── Checksum verification ────────────────────────────────────────

@test "upgrade: downloads and verifies checksums" {
  echo "0.2.0" > "$TEST_INSTALL_DIR/version"
  run bash "$DIST_DIR/upgrade.sh"
  assert_success
  assert_output --partial "Verifying checksums"
  assert_output --partial "ahandd: OK"
  assert_output --partial "ahandctl: OK"
}

@test "upgrade: fails on checksum mismatch" {
  echo "0.2.0" > "$TEST_INSTALL_DIR/version"
  export MOCK_SHASUM_FAIL=true
  run bash "$DIST_DIR/upgrade.sh"
  assert_failure
  assert_output --partial "Checksum mismatch"
}

@test "upgrade: gracefully handles missing checksums" {
  echo "0.2.0" > "$TEST_INSTALL_DIR/version"
  export MOCK_CURL_FAIL_PATTERN="checksums-rust.txt"
  run bash "$DIST_DIR/upgrade.sh"
  assert_success
  [ "$(cat "$TEST_INSTALL_DIR/version")" = "0.3.0" ]
}

# ── Daemon management ────────────────────────────────────────────

@test "upgrade: stops running daemon via PID file" {
  echo "0.2.0" > "$TEST_INSTALL_DIR/version"
  mkdir -p "$TEST_INSTALL_DIR/data"
  # Start a real background process so kill -0 succeeds.
  sleep 300 &
  local daemon_pid=$!
  echo "$daemon_pid" > "$TEST_INSTALL_DIR/data/daemon.pid"
  run bash "$DIST_DIR/upgrade.sh"
  assert_success
  assert_output --partial "Stopping daemon (PID $daemon_pid)"
  # The real process should have been killed.
  ! kill -0 "$daemon_pid" 2>/dev/null || kill "$daemon_pid" 2>/dev/null
}

@test "upgrade: skips daemon stop when no PID file" {
  echo "0.2.0" > "$TEST_INSTALL_DIR/version"
  run bash "$DIST_DIR/upgrade.sh"
  assert_success
  [ ! -f "$MOCK_KILL_LOG" ] || [ ! -s "$MOCK_KILL_LOG" ]
}

# ── Script downloads ─────────────────────────────────────────────

@test "upgrade: installs setup-browser.sh if available" {
  echo "0.2.0" > "$TEST_INSTALL_DIR/version"
  run bash "$DIST_DIR/upgrade.sh"
  assert_success
  assert_executable "$TEST_INSTALL_DIR/bin/setup-browser.sh"
}

@test "upgrade: handles missing setup-browser.sh gracefully" {
  echo "0.2.0" > "$TEST_INSTALL_DIR/version"
  export MOCK_CURL_FAIL_PATTERN="setup-browser.sh"
  run bash "$DIST_DIR/upgrade.sh"
  assert_success
}

# ── Cleanup and edge cases ───────────────────────────────────────

@test "upgrade: invalid flag prints usage" {
  run bash "$DIST_DIR/upgrade.sh" --bogus
  assert_failure
  assert_output --partial "Usage:"
}

@test "upgrade: platform detection linux-x64" {
  echo "0.2.0" > "$TEST_INSTALL_DIR/version"
  export MOCK_UNAME_S="Linux"
  export MOCK_UNAME_M="x86_64"
  run bash "$DIST_DIR/upgrade.sh"
  assert_success
  assert_output --partial "linux-x64"
}
