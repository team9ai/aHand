#!/bin/bash

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

echo "Building admin panel..."
cd "$SCRIPT_DIR/../apps/admin"
pnpm build

echo "Deploying to ~/.ahand/admin/dist/..."
mkdir -p ~/.ahand/admin/dist
rm -rf ~/.ahand/admin/dist/*
cp -r dist/* ~/.ahand/admin/dist/

echo "Deploying scripts to ~/.ahand/bin/..."
mkdir -p ~/.ahand/bin
cp "$SCRIPT_DIR/dist/setup-browser.sh" ~/.ahand/bin/setup-browser.sh
chmod +x ~/.ahand/bin/setup-browser.sh

if [ -f "$SCRIPT_DIR/dist/upgrade.sh" ]; then
  cp "$SCRIPT_DIR/dist/upgrade.sh" ~/.ahand/bin/upgrade.sh
  chmod +x ~/.ahand/bin/upgrade.sh
fi

echo "Admin panel deployed successfully!"
echo "Run: ahandctl configure"
