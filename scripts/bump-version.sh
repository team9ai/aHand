#!/bin/bash
# Bump version across all workspace files (Cargo.toml + package.json).
# Usage:
#   bash scripts/bump-version.sh patch          # 0.1.0 → 0.1.1
#   bash scripts/bump-version.sh minor          # 0.1.0 → 0.2.0
#   bash scripts/bump-version.sh major          # 0.1.0 → 1.0.0
#   bash scripts/bump-version.sh 1.2.3          # set exact version

set -e

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"

# ── Read current version from Cargo.toml (source of truth) ──────────
CURRENT=$(grep '^version' "$ROOT_DIR/Cargo.toml" | head -1 | sed 's/version = "\(.*\)"/\1/')
if [ -z "$CURRENT" ]; then
  echo "Error: cannot read version from Cargo.toml" >&2
  exit 1
fi

IFS='.' read -r MAJOR MINOR PATCH <<< "$CURRENT"

# ── Compute new version ─────────────────────────────────────────────
case "${1:-}" in
  patch) NEW_VERSION="$MAJOR.$MINOR.$((PATCH + 1))" ;;
  minor) NEW_VERSION="$MAJOR.$((MINOR + 1)).0" ;;
  major) NEW_VERSION="$((MAJOR + 1)).0.0" ;;
  "")
    echo "Usage: bump-version.sh <patch|minor|major|X.Y.Z>" >&2
    exit 1
    ;;
  *)
    if ! echo "$1" | grep -qE '^[0-9]+\.[0-9]+\.[0-9]+$'; then
      echo "Error: invalid version '$1' (expected X.Y.Z)" >&2
      exit 1
    fi
    NEW_VERSION="$1"
    ;;
esac

if [ "$NEW_VERSION" = "$CURRENT" ]; then
  echo "Already at v${CURRENT}, nothing to do."
  exit 0
fi

echo "Bumping version: ${CURRENT} → ${NEW_VERSION}"
echo

# ── Files to update ─────────────────────────────────────────────────
CARGO_FILE="$ROOT_DIR/Cargo.toml"

PACKAGE_FILES=(
  "$ROOT_DIR/package.json"
  "$ROOT_DIR/apps/admin/package.json"
  "$ROOT_DIR/apps/dashboard/package.json"
  "$ROOT_DIR/apps/dev-cloud/package.json"
  "$ROOT_DIR/crates/ahand-protocol/package.json"
  "$ROOT_DIR/crates/ahandctl/package.json"
  "$ROOT_DIR/crates/ahandd/package.json"
  "$ROOT_DIR/packages/browser-bridge/package.json"
  "$ROOT_DIR/packages/proto-ts/package.json"
  "$ROOT_DIR/packages/sdk/package.json"
)

# ── Update Cargo.toml ───────────────────────────────────────────────
sed -i '' "s/^version = \"${CURRENT}\"/version = \"${NEW_VERSION}\"/" "$CARGO_FILE"
echo "  updated  Cargo.toml"

# ── Update package.json files ───────────────────────────────────────
for f in "${PACKAGE_FILES[@]}"; do
  if [ -f "$f" ]; then
    sed -i '' "s/\"version\": \"${CURRENT}\"/\"version\": \"${NEW_VERSION}\"/" "$f"
    echo "  updated  ${f#"$ROOT_DIR"/}"
  else
    echo "  skipped  ${f#"$ROOT_DIR"/} (not found)"
  fi
done

echo
echo "Done! v${CURRENT} → v${NEW_VERSION}"
