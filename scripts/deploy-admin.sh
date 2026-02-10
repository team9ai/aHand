#!/bin/bash
# Deploy admin SPA and scripts to ~/.ahand/
# Build is handled separately by turbo (admin#build → admin#deploy).

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ADMIN_DIST="$SCRIPT_DIR/../apps/admin/dist"

echo "Deploying admin SPA to ~/.ahand/admin/dist/..."
mkdir -p ~/.ahand/admin/dist
rm -rf ~/.ahand/admin/dist/*
cp -r "$ADMIN_DIST/"* ~/.ahand/admin/dist/

echo "Deploying scripts to ~/.ahand/bin/..."
mkdir -p ~/.ahand/bin
cp "$SCRIPT_DIR/dist/setup-browser.sh" ~/.ahand/bin/setup-browser.sh
chmod +x ~/.ahand/bin/setup-browser.sh

if [ -f "$SCRIPT_DIR/dist/upgrade.sh" ]; then
  cp "$SCRIPT_DIR/dist/upgrade.sh" ~/.ahand/bin/upgrade.sh
  chmod +x ~/.ahand/bin/upgrade.sh
fi

echo "Deploy complete!"
