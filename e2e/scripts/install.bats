#!/usr/bin/env bats
# E2E tests for scripts/dist/install.sh

load 'helpers/common'

setup() {
  setup_isolated_env
}

teardown() {
  teardown_isolated_env
}

@test "install: creates directory structure" {
  run bash "$DIST_DIR/install.sh"
  assert_success
  [ -d "$TEST_INSTALL_DIR/bin" ]
  [ -d "$TEST_INSTALL_DIR/admin/dist" ]
}

@test "install: downloads and installs ahandd binary" {
  run bash "$DIST_DIR/install.sh"
  assert_success
  assert_executable "$TEST_INSTALL_DIR/bin/ahandd"
}

@test "install: downloads and installs ahandctl binary" {
  run bash "$DIST_DIR/install.sh"
  assert_success
  assert_executable "$TEST_INSTALL_DIR/bin/ahandctl"
}

@test "install: extracts admin SPA from tar.gz" {
  run bash "$DIST_DIR/install.sh"
  assert_success
  [ -f "$TEST_INSTALL_DIR/admin/dist/index.html" ]
}

@test "install: downloads and installs setup-browser.sh" {
  run bash "$DIST_DIR/install.sh"
  assert_success
  assert_executable "$TEST_INSTALL_DIR/bin/setup-browser.sh"
}

@test "install: writes version marker" {
  run bash "$DIST_DIR/install.sh"
  assert_success
  [ -f "$TEST_INSTALL_DIR/version" ]
  [ "$(cat "$TEST_INSTALL_DIR/version")" = "0.3.0" ]
}

@test "install: AHAND_VERSION env overrides API fetch" {
  export AHAND_VERSION="0.2.0"
  run bash "$DIST_DIR/install.sh"
  assert_success
  [ "$(cat "$TEST_INSTALL_DIR/version")" = "0.2.0" ]
}

@test "install: AHAND_DIR env overrides install path" {
  CUSTOM_DIR="$TEST_HOME/custom-install"
  export AHAND_DIR="$CUSTOM_DIR"
  run bash "$DIST_DIR/install.sh"
  assert_success
  [ -d "$CUSTOM_DIR/bin" ]
  assert_executable "$CUSTOM_DIR/bin/ahandd"
  [ -f "$CUSTOM_DIR/version" ]
}

@test "install: detects darwin-arm64 platform" {
  export MOCK_UNAME_S="Darwin"
  export MOCK_UNAME_M="arm64"
  run bash "$DIST_DIR/install.sh"
  assert_success
  assert_output --partial "darwin-arm64"
}

@test "install: detects linux-x64 platform" {
  export MOCK_UNAME_S="Linux"
  export MOCK_UNAME_M="x86_64"
  run bash "$DIST_DIR/install.sh"
  assert_success
  assert_output --partial "linux-x64"
}

@test "install: detects linux-arm64 platform" {
  export MOCK_UNAME_S="Linux"
  export MOCK_UNAME_M="aarch64"
  run bash "$DIST_DIR/install.sh"
  assert_success
  assert_output --partial "linux-arm64"
}

@test "install: fails on unsupported OS" {
  export MOCK_UNAME_S="FreeBSD"
  run bash "$DIST_DIR/install.sh"
  assert_failure
  assert_output --partial "Unsupported OS"
}

@test "install: fails on unsupported architecture" {
  export MOCK_UNAME_M="riscv64"
  run bash "$DIST_DIR/install.sh"
  assert_failure
  assert_output --partial "Unsupported architecture"
}

@test "install: fails when version cannot be determined" {
  # Override fixture to return empty JSON.
  export MOCK_CURL_FIXTURE_DIR="$TEST_HOME/empty-fixtures"
  mkdir -p "$MOCK_CURL_FIXTURE_DIR"
  echo '[]' > "$MOCK_CURL_FIXTURE_DIR/github-releases.json"
  # Copy other fixtures so the script doesn't fail for other reasons.
  run bash "$DIST_DIR/install.sh"
  assert_failure
  assert_output --partial "Could not determine version"
}

@test "install: cleans up temp tar file" {
  run bash "$DIST_DIR/install.sh"
  assert_success
  [ ! -f "/tmp/ahand-admin-spa.tar.gz" ]
}

@test "install: outputs success message with PATH instructions" {
  run bash "$DIST_DIR/install.sh"
  assert_success
  assert_output --partial "aHand installed successfully"
  assert_output --partial "export PATH"
}

@test "install: is idempotent" {
  run bash "$DIST_DIR/install.sh"
  assert_success
  run bash "$DIST_DIR/install.sh"
  assert_success
  assert_executable "$TEST_INSTALL_DIR/bin/ahandd"
  [ "$(cat "$TEST_INSTALL_DIR/version")" = "0.3.0" ]
}
