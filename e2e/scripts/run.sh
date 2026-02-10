#!/bin/bash
# Entry point: bootstrap bats, generate fixtures, run all tests.
set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

bash "$SCRIPT_DIR/bootstrap.sh"
bash "$SCRIPT_DIR/fixtures/generate-fixtures.sh"

# Use parallel if available, otherwise run sequentially.
JOBS_FLAG=""
if command -v parallel &>/dev/null; then
  JOBS_FLAG="--jobs 3"
fi

"$SCRIPT_DIR/.bats/bats-core/bin/bats" \
  $JOBS_FLAG \
  --timing \
  "$SCRIPT_DIR"/*.bats
