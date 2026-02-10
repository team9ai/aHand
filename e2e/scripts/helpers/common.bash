# Shared helpers for distribution script e2e tests.
# Loaded by each .bats file via: load 'helpers/common'

TESTS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIST_DIR="$(cd "$TESTS_DIR/../../scripts/dist" && pwd)"
MOCKS_DIR="$TESTS_DIR/mocks"
FIXTURES_DIR="$TESTS_DIR/fixtures"

# Load bats helpers
load "${TESTS_DIR}/.bats/bats-support/load"
load "${TESTS_DIR}/.bats/bats-assert/load"

# Create an isolated environment per test.
setup_isolated_env() {
  TEST_HOME="$(mktemp -d)"
  TEST_INSTALL_DIR="$TEST_HOME/.ahand"

  # Save and override PATH so mocks shadow real commands.
  export ORIGINAL_PATH="$PATH"
  export PATH="$MOCKS_DIR:$PATH"

  # Point scripts at isolated directory.
  export HOME="$TEST_HOME"
  export AHAND_DIR="$TEST_INSTALL_DIR"
  export AHAND_DATA_DIR="$TEST_INSTALL_DIR"

  # Tell mock curl where to find fixtures.
  export MOCK_CURL_FIXTURE_DIR="$FIXTURES_DIR"
  # Per-test curl log.
  export MOCK_CURL_LOG="$TEST_HOME/curl-calls.log"

  # Default platform: macOS arm64.
  export MOCK_UNAME_S="Darwin"
  export MOCK_UNAME_M="arm64"

  # Clear optional overrides.
  unset AHAND_VERSION
  unset MOCK_SHASUM_FAIL
  unset MOCK_NODE_ABSENT
  unset MOCK_NODE_VERSION
  unset MOCK_CURL_FAIL_PATTERN

  # Per-test kill log.
  export MOCK_KILL_LOG="$TEST_HOME/kill-calls.log"
}

teardown_isolated_env() {
  rm -rf "$TEST_HOME"
  export PATH="$ORIGINAL_PATH"
}

# Assert a file exists and is executable.
assert_executable() {
  local file="$1"
  [ -f "$file" ] || { echo "File not found: $file"; return 1; }
  [ -x "$file" ] || { echo "File not executable: $file"; return 1; }
}

# Assert file content matches a string.
assert_file_contains() {
  local file="$1"
  local expected="$2"
  grep -q "$expected" "$file" || {
    echo "File $file does not contain: $expected"
    echo "Actual: $(cat "$file")"
    return 1
  }
}

# Assert mock curl was called with a URL matching a pattern.
assert_curl_called_with() {
  local pattern="$1"
  grep -q "$pattern" "$MOCK_CURL_LOG" || {
    echo "curl was not called with URL matching: $pattern"
    echo "Actual calls:"
    cat "$MOCK_CURL_LOG" 2>/dev/null || echo "(none)"
    return 1
  }
}

# Assert mock curl was NOT called with a URL matching a pattern.
refute_curl_called_with() {
  local pattern="$1"
  if grep -q "$pattern" "$MOCK_CURL_LOG" 2>/dev/null; then
    echo "curl was unexpectedly called with URL matching: $pattern"
    return 1
  fi
}

# Count curl calls.
curl_call_count() {
  wc -l < "$MOCK_CURL_LOG" 2>/dev/null | tr -d ' '
}
