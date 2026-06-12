#!/bin/bash
# Entry point: bootstrap bats, generate fixtures, run all tests.
set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

bash "$SCRIPT_DIR/bootstrap.sh"
bash "$SCRIPT_DIR/fixtures/generate-fixtures.sh"

# Use parallel if GNU parallel is available, otherwise run sequentially.
# Bare `command -v parallel` also matches the moreutils variant, which makes
# bats-core abort with "Cannot execute jobs without GNU parallel".  Detect the
# real thing by checking the version string.
JOBS_FLAG=""
if parallel --version 2>/dev/null | grep -q 'GNU parallel'; then
  JOBS_FLAG="--jobs 3"
  echo "Running bats in parallel (--jobs 3)"
else
  echo "Running bats sequentially (GNU parallel not found)"
fi

"$SCRIPT_DIR/.bats/bats-core/bin/bats" \
  $JOBS_FLAG \
  --timing \
  "$SCRIPT_DIR"/*.bats
