# CI/CD Workflows

aHand uses per-component release workflows. Each component is released independently with its own tag prefix.

## Workflows Overview

| Workflow | File | Trigger | Artifacts |
|----------|------|---------|-----------|
| Release Rust Binaries | `release-rust.yml` | `rust-v*` tag | `ahandd-{os}-{arch}`, `ahandctl-{os}-{arch}` |
| Release Admin SPA | `release-admin.yml` | `admin-v*` tag | `admin-spa.tar.gz` |
| Release Browser Bundle | `release-browser.yml` | `browser-v*` tag | `daemon-bundle.tar.gz`, `setup-browser.sh` |

All workflows also support `workflow_dispatch` for manual triggering.

## Releasing a New Version

### Release all components (e.g. v0.2.0)

```bash
git tag rust-v0.2.0 && git push origin rust-v0.2.0
git tag admin-v0.2.0 && git push origin admin-v0.2.0
git tag browser-v0.2.0 && git push origin browser-v0.2.0
```

### Release a single component

Only push the tag for the changed component:

```bash
# Only Rust binaries changed
git tag rust-v0.2.1 && git push origin rust-v0.2.1
```

### Pre-release / alpha

Use semver pre-release suffix:

```bash
git tag rust-v0.1.0-alpha && git push origin rust-v0.1.0-alpha
```

### Manual trigger (without tag)

Go to Actions > select workflow > "Run workflow" and enter the tag name.

## Release Artifacts

### rust-v* (4 platform matrix)

Builds `ahandd` and `ahandctl` for:
- `linux-x64` (ubuntu, native)
- `linux-arm64` (ubuntu, cross-compiled via `cross`)
- `darwin-arm64` (macos, native)
- `darwin-x64` (macos, native)

Cross-compilation config is in `/Cross.toml`.

**System dependencies:**
- Linux: `protobuf-compiler`, `libssl-dev`, `pkg-config` (installed in workflow)
- macOS: `protobuf` (installed via Homebrew)
- Linux arm64 (cross): `libssl-dev:arm64`, `protobuf-compiler` (installed via `Cross.toml` pre-build)

### admin-v*

Builds the Solid.js admin panel SPA (`apps/admin/`), packages as `admin-spa.tar.gz`.

### browser-v*

Builds the ncc-bundled browser daemon (`packages/browser-bridge/`), packages as `daemon-bundle.tar.gz` + copies `scripts/dist/setup-browser.sh`.

## Version Convention

- All components share the same version number (e.g. `0.2.0`), but can be released independently.
- `install.sh` and `upgrade.sh` use the `rust-v*` release as the canonical version source to discover the latest version.
- The version number comes from `Cargo.toml` workspace `version` field.

## Adding a New Platform (Rust)

Edit `release-rust.yml` matrix:

```yaml
matrix:
  include:
    - os: ubuntu-latest
      target: x86_64-unknown-linux-musl  # new target
      suffix: linux-musl-x64
```

For cross-compilation targets, add `cross: true` and update `Cross.toml`.

## Maintenance Notes

- **pnpm version**: Read from `package.json` `packageManager` field. Do NOT set `version` in `pnpm/action-setup`.
- **Node version**: Pinned to 20 in admin and browser workflows.
- **Rust toolchain**: Uses `dtolnay/rust-toolchain@stable`.
- **Checksums**: Each workflow generates its own `checksums-{component}.txt` using `shasum -a 256`.
- **GitHub Release**: Created automatically via `softprops/action-gh-release@v2`. Multiple workflows can upload to the same release (if tags match), or create separate releases (current setup).

## Local Release Build

For testing release artifacts locally without CI:

```bash
bash scripts/release.sh
# Output in release/ directory
```

## Related Scripts

| Script | Purpose |
|--------|---------|
| `scripts/release.sh` | Local release build (all components) |
| `scripts/dist/install.sh` | One-line installer (downloads from GitHub releases) |
| `scripts/dist/upgrade.sh` | Self-upgrade (downloads from GitHub releases) |
| `scripts/dist/setup-browser.sh` | Browser dependency installer |
| `scripts/deploy-admin.sh` | Dev: deploy admin SPA to `~/.ahand/` |
