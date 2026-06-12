#!/bin/bash
# Generates dynamic fixtures (tar archives + checksums).
# Idempotent — safe to re-run.
set -e

FIXTURE_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$FIXTURE_DIR"

# ── Portable SHA-256 helper ───────────────────────────────────────
# `shasum` is macOS/Perl-only; Linux uses `sha256sum`. Mirror the
# fallback logic from install.sh's sha256_of() so fixtures generate
# correctly on both platforms.
sha256_fixture() {
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  else
    sha256sum "$1" | awk '{print $1}'
  fi
}

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
# Includes node, npm, and npx stubs. The npm stub creates a playwright-cli
# stub in its own bin directory when called with `install -g @playwright/cli`.
if [ ! -f node-fake.tar.xz ]; then
  TMP=$(mktemp -d)
  mkdir -p "$TMP/node-fake/bin"
  cat > "$TMP/node-fake/bin/node" <<'NODEEOF'
#!/bin/bash
if [ "$1" = "-v" ]; then echo "v24.13.0"; else echo "node-installed-fake $*"; fi
NODEEOF
  chmod +x "$TMP/node-fake/bin/node"
  cat > "$TMP/node-fake/bin/npm" <<'NPMEOF'
#!/bin/bash
# Fake npm: create playwright-cli stub on `install -g @playwright/cli*`.
if [ "$1" = "install" ] && [ "$2" = "-g" ]; then
  BIN_DIR="$(dirname "$0")"
  cat > "$BIN_DIR/playwright-cli" <<'PCEOF'
#!/bin/bash
if [ "$1" = "--version" ]; then echo "0.1.1"; else echo "playwright-cli-fake $*"; fi
PCEOF
  chmod +x "$BIN_DIR/playwright-cli"
fi
echo "npm-installed-fake $*"
NPMEOF
  chmod +x "$TMP/node-fake/bin/npm"
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
  AHANDD_HASH=$(sha256_fixture ahandd-fake)
  AHANDCTL_HASH=$(sha256_fixture ahandctl-fake)
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

# ── checksums-admin.txt ──────────────────────────────────────────
# Real SHA256 of admin-spa.tar.gz so install.sh checksum verification
# passes. Regenerated every run to stay in sync with the tar on disk
# (gzip output is not byte-stable across regenerations).
ADMIN_HASH=$(sha256_fixture admin-spa.tar.gz)
printf '%s  admin-spa.tar.gz\n' "$ADMIN_HASH" > checksums-admin.txt
echo "  Generated checksums-admin.txt"

echo "Fixtures ready."
