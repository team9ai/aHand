#!/usr/bin/env bats
# Unit tests for e2e/scripts/lib helpers (run.sh's sourceable pieces).

load 'helpers/common'

setup() {
  LIB_DIR="$(cd "$(dirname "$BATS_TEST_FILENAME")/lib" && pwd)"
  # Isolated dir holding a stub `parallel` that shadows the real binary.
  STUB_DIR="$(mktemp -d)"
  STUB_PATH_SAVE="$PATH"
  export PATH="$STUB_DIR:$PATH"
}

teardown() {
  export PATH="$STUB_PATH_SAVE"
  rm -rf "$STUB_DIR"
}

# Write a stub `parallel` whose `--version` prints $1.
write_parallel_stub() {
  cat > "$STUB_DIR/parallel" <<EOF
#!/bin/bash
if [ "\$1" = "--version" ]; then
  echo '$1'
fi
EOF
  chmod +x "$STUB_DIR/parallel"
}

@test "detect_parallel_jobs_flag: GNU parallel -> --jobs 3" {
  write_parallel_stub "GNU parallel 20231122"
  source "$LIB_DIR/parallel.sh"
  run detect_parallel_jobs_flag
  assert_success
  assert_output "--jobs 3"
}

@test "detect_parallel_jobs_flag: moreutils parallel -> empty (no --jobs)" {
  write_parallel_stub "parallel from moreutils"
  source "$LIB_DIR/parallel.sh"
  run detect_parallel_jobs_flag
  assert_success
  assert_output ""
}

@test "detect_parallel_jobs_flag: parallel absent -> empty (no --jobs)" {
  # No stub written, and restrict PATH to stub dir + system bins (no Homebrew)
  # so any real GNU `parallel` is invisible — `parallel` is genuinely absent.
  # `grep` still resolves via /usr/bin so the helper itself can run.
  export PATH="$STUB_DIR:/usr/bin:/bin"
  source "$LIB_DIR/parallel.sh"
  run detect_parallel_jobs_flag
  assert_success
  assert_output ""
}
