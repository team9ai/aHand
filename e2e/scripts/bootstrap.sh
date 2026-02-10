#!/bin/bash
# Clones bats-core + helpers if not already present.
set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
BATS_DIR="$SCRIPT_DIR/.bats"

if [ -x "$BATS_DIR/bats-core/bin/bats" ]; then
  exit 0
fi

mkdir -p "$BATS_DIR"
echo "Cloning bats-core..."
git clone --depth 1 --branch v1.11.1 \
  https://github.com/bats-core/bats-core.git "$BATS_DIR/bats-core" 2>/dev/null
git clone --depth 1 \
  https://github.com/bats-core/bats-support.git "$BATS_DIR/bats-support" 2>/dev/null
git clone --depth 1 \
  https://github.com/bats-core/bats-assert.git "$BATS_DIR/bats-assert" 2>/dev/null
echo "bats-core ready."
