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
  # The version check fires before any downloads, so missing download fixtures
  # are never reached — no need to copy them here.
  run bash "$DIST_DIR/install.sh"
  assert_failure
  assert_output --partial "Could not determine Rust release version"
}

@test "install: cleans up temp files (no leftover temp dir)" {
  # Point install.sh's mktemp at an isolated, per-test TMPDIR so this is
  # parallel-safe (no shared global /tmp path). After a successful install
  # the EXIT trap must have removed the per-invocation temp dir, leaving
  # this dir empty.
  #
  # NOTE: `VAR=val run cmd` does NOT pass the env-prefix into bats `run`
  # (run is a shell function, not an external command). Use `run env VAR=val`
  # so TMPDIR is actually forwarded to install.sh's mktemp.
  local isolated_tmp="$TEST_HOME/temp"
  mkdir -p "$isolated_tmp"
  run env TMPDIR="$isolated_tmp" bash "$DIST_DIR/install.sh"
  assert_success
  # After a successful install the EXIT trap must have removed the
  # per-invocation temp dir, leaving isolated_tmp empty.
  [ -z "$(ls -A "$isolated_tmp")" ]
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

# ── SHA-256 verification (cross-platform parity with install.ps1) ─

@test "install: verifies checksums on happy path" {
  run bash "$DIST_DIR/install.sh"
  assert_success
  assert_output --partial "Verifying checksums"
  assert_output --partial "Checksum OK: ahandd-darwin-arm64"
  assert_output --partial "Checksum OK: ahandctl-darwin-arm64"
  assert_output --partial "Checksum OK: admin-spa.tar.gz"
  assert_executable "$TEST_INSTALL_DIR/bin/ahandd"
}

@test "install: aborts on tampered artifact (checksum mismatch)" {
  # Force the local digest to differ from the published checksum: the
  # mock shasum returns a bogus hash, simulating a tampered/corrupt binary.
  export MOCK_SHASUM_FAIL=true
  run bash "$DIST_DIR/install.sh"
  assert_failure
  assert_output --partial "Checksum mismatch"
  # Fail-closed: the unverified binary must NOT be installed.
  [ ! -f "$TEST_INSTALL_DIR/bin/ahandd" ]
}

@test "install: fails closed when checksum file is missing" {
  # The .sha256/checksum download 404s; install must abort, not skip.
  export MOCK_CURL_FAIL_PATTERN="checksums-rust.txt"
  run bash "$DIST_DIR/install.sh"
  assert_failure
  assert_output --partial "Checksum file not available"
  # Fail-closed: nothing installed without integrity verification.
  [ ! -f "$TEST_INSTALL_DIR/bin/ahandd" ]
}

@test "install: fails closed when checksum entry is missing for artifact" {
  # The checksum file downloads fine (present + readable, valid format) but
  # contains NO line for the requested artifact — verify_checksum must still
  # abort (the "no entry" fail-closed branch), not silently skip verification.
  export MOCK_CURL_CHECKSUM_NOMATCH=1
  run bash "$DIST_DIR/install.sh"
  assert_failure
  assert_output --partial "No checksum entry"
  # Fail-closed: the unverified binary must NOT be installed.
  [ ! -f "$TEST_INSTALL_DIR/bin/ahandd" ]
}
