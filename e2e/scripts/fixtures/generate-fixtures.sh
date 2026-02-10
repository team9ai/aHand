#!/bin/bash
# Generates dynamic fixtures (tar archives + checksums).
# Idempotent — safe to re-run.
set -e

FIXTURE_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$FIXTURE_DIR"

# ── admin-spa.tar.gz ─────────────────────────────────────────────
if [ ! -f admin-spa.tar.gz ]; then
  TMP=$(mktemp -d)
  echo '<html><body>admin panel</body></html>' > "$TMP/index.html"
  tar czf admin-spa.tar.gz -C "$TMP" .
  rm -rf "$TMP"
  echo "  Generated admin-spa.tar.gz"
fi

# ── daemon-bundle.tar.gz ─────────────────────────────────────────
if [ ! -f daemon-bundle.tar.gz ]; then
  TMP=$(mktemp -d)
  echo 'console.log("daemon-fake")' > "$TMP/daemon.js"
  tar czf daemon-bundle.tar.gz -C "$TMP" .
  rm -rf "$TMP"
  echo "  Generated daemon-bundle.tar.gz"
fi

# ── node-fake.tar.xz ─────────────────────────────────────────────
# Needs a top-level directory to work with --strip-components=1.
if [ ! -f node-fake.tar.xz ]; then
  TMP=$(mktemp -d)
  mkdir -p "$TMP/node-fake/bin"
  cat > "$TMP/node-fake/bin/node" <<'NODEEOF'
#!/bin/bash
if [ "$1" = "-v" ]; then echo "v24.13.0"; else echo "node-installed-fake $*"; fi
NODEEOF
  chmod +x "$TMP/node-fake/bin/node"
  cat > "$TMP/node-fake/bin/npx" <<'NPXEOF'
#!/bin/bash
echo "npx-installed-fake $*"
NPXEOF
  chmod +x "$TMP/node-fake/bin/npx"
  tar cJf node-fake.tar.xz -C "$TMP" node-fake
  rm -rf "$TMP"
  echo "  Generated node-fake.tar.xz"
fi

# ── checksums-rust.txt ────────────────────────────────────────────
# Compute real SHA256 sums of the fake binaries so checksum verification
# passes. Generate for all platform suffixes.
if [ ! -f checksums-rust.txt ]; then
  AHANDD_HASH=$(shasum -a 256 ahandd-fake | awk '{print $1}')
  AHANDCTL_HASH=$(shasum -a 256 ahandctl-fake | awk '{print $1}')
  cat > checksums-rust.txt <<EOF
${AHANDD_HASH}  ahandd-darwin-arm64
${AHANDD_HASH}  ahandd-darwin-x64
${AHANDD_HASH}  ahandd-linux-x64
${AHANDD_HASH}  ahandd-linux-arm64
${AHANDCTL_HASH}  ahandctl-darwin-arm64
${AHANDCTL_HASH}  ahandctl-darwin-x64
${AHANDCTL_HASH}  ahandctl-linux-x64
${AHANDCTL_HASH}  ahandctl-linux-arm64
EOF
  echo "  Generated checksums-rust.txt"
fi

echo "Fixtures ready."
