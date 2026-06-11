//! Full native upgrade execution: download, verify, stop daemon, swap binaries,
//! extract admin SPA, write version marker.

use std::path::{Component, Path};

use anyhow::Context as _;

use super::release::ReleaseInfo;

// ── Public entry point ──────────────────────────────────────────────────────

/// Perform the full upgrade described by `info`.
///
/// Asset URL layout (mirrors `upgrade.sh`):
/// - Rust binaries: `{download_base}/rust-v{ver}/{asset}`
/// - Admin SPA:     `{download_base}/admin-v{ver}/admin-spa.tar.gz`
/// - Browser setup: `{download_base}/browser-v{ver}/setup-browser.sh`
pub async fn perform_upgrade(
    info: &ReleaseInfo,
    current: &str,
    download_base: &str,
    ahand_home: &Path,
) -> anyhow::Result<()> {
    let latest = info
        .rust
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("Could not determine latest version"))?;

    let suffix = ahand_platform::paths::release_suffix();
    let rust_base = format!("{download_base}/rust-v{latest}");

    println!("Upgrading: {current} -> {latest}");
    println!();

    // ── 1. Download checksums (optional) ───────────────────────────────────
    println!("==> Downloading checksums...");
    let checksums_url = format!("{rust_base}/checksums-rust.txt");
    let checksum_text = download_optional(&checksums_url).await;

    // ── 2. Download binaries (required) ────────────────────────────────────
    println!("==> Downloading binaries (rust-v{latest})...");

    let ahandd_filename = format!("ahandd-{suffix}{}", if cfg!(windows) { ".exe" } else { "" });
    let ahandctl_filename = format!(
        "ahandctl-{suffix}{}",
        if cfg!(windows) { ".exe" } else { "" }
    );

    let ahandd_url = format!("{rust_base}/{ahandd_filename}");
    let ahandctl_url = format!("{rust_base}/{ahandctl_filename}");

    println!("  Downloading {ahandd_filename}...");
    let ahandd_bytes = ahandd::updater::download_binary(&ahandd_url)
        .await
        .with_context(|| format!("failed to download {ahandd_url}"))?;

    println!("  Downloading {ahandctl_filename}...");
    let ahandctl_bytes = ahandd::updater::download_binary(&ahandctl_url)
        .await
        .with_context(|| format!("failed to download {ahandctl_url}"))?;

    // ── 3. Download admin SPA (optional) ───────────────────────────────────
    let admin_bytes = if let Some(admin_ver) = info.admin.as_deref() {
        println!("==> Downloading admin panel (admin-v{admin_ver})...");
        let admin_url = format!("{download_base}/admin-v{admin_ver}/admin-spa.tar.gz");
        println!("  Downloading admin-spa.tar.gz...");
        match ahandd::updater::download_binary(&admin_url).await {
            Ok(b) => Some(b),
            Err(e) => {
                eprintln!("  Warning: could not download admin-spa.tar.gz: {e}");
                None
            }
        }
    } else {
        None
    };

    // ── 4. Download setup-browser.sh (optional, unix only) ─────────────────
    #[cfg(unix)]
    let browser_bytes = if let Some(browser_ver) = info.browser.as_deref() {
        println!("==> Downloading scripts (browser-v{browser_ver})...");
        let browser_url = format!("{download_base}/browser-v{browser_ver}/setup-browser.sh");
        println!("  Downloading setup-browser.sh...");
        match ahandd::updater::download_binary(&browser_url).await {
            Ok(b) => Some(b),
            Err(e) => {
                eprintln!("  Warning: could not download setup-browser.sh: {e}");
                None
            }
        }
    } else {
        None
    };
    #[cfg(not(unix))]
    let browser_bytes: Option<Vec<u8>> = None;

    // ── 5. Verify checksums BEFORE any install ──────────────────────────────
    if let Some(ref cs_text) = checksum_text {
        println!();
        println!("==> Verifying checksums...");
        verify_binary_checksum(cs_text, &ahandd_filename, &ahandd_bytes)?;
        verify_binary_checksum(cs_text, &ahandctl_filename, &ahandctl_bytes)?;
    }

    // ── 6. Stop daemon ──────────────────────────────────────────────────────
    println!();
    println!("==> Stopping daemon...");
    if let Err(e) = crate::daemon::stop().await {
        eprintln!("  Note: daemon stop returned: {e} (continuing)");
    }

    // ── 7. Swap binaries ────────────────────────────────────────────────────
    println!();
    println!("==> Installing binaries...");
    let bin_dir = ahand_home.join("bin");
    std::fs::create_dir_all(&bin_dir).context("failed to create bin dir")?;

    ahandd::updater::swap_binary_into(&bin_dir, "ahandd", &ahandd_bytes)
        .context("failed to install ahandd")?;
    println!("  ahandd: OK");

    ahandd::updater::swap_binary_into(&bin_dir, "ahandctl", &ahandctl_bytes).context(
        "ahandd was updated but ahandctl could not be replaced; \
         the daemon is stopped and the version marker was not written \
         — re-run `ahandctl upgrade` to complete",
    )?;
    println!("  ahandctl: OK");

    // ── 8. macOS: remove quarantine xattr ──────────────────────────────────
    #[cfg(target_os = "macos")]
    {
        let ahandd_bin = bin_dir.join(ahand_platform::paths::exe_name("ahandd"));
        let ahandctl_bin = bin_dir.join(ahand_platform::paths::exe_name("ahandctl"));
        for p in [&ahandd_bin, &ahandctl_bin] {
            let _ = std::process::Command::new("xattr")
                .args(["-d", "com.apple.quarantine"])
                .arg(p)
                .status();
        }
    }

    // ── 9. Extract admin SPA ────────────────────────────────────────────────
    if let Some(ref bytes) = admin_bytes {
        println!("==> Installing admin panel...");
        let dist_dir = ahand_home.join("admin").join("dist");
        extract_admin_spa(bytes, &dist_dir).context("failed to extract admin-spa.tar.gz")?;
        println!("  admin SPA: OK");
    }

    // ── 10. Install setup-browser.sh ────────────────────────────────────────
    #[cfg(unix)]
    if let Some(ref bytes) = browser_bytes {
        let script_path = bin_dir.join("setup-browser.sh");
        std::fs::write(&script_path, bytes).context("failed to write setup-browser.sh")?;
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755))
                .context("failed to chmod setup-browser.sh")?;
        }
        println!("  setup-browser.sh: OK");
    }

    // ── 11. Write version marker ─────────────────────────────────────────────
    ahand_platform::paths::write_version_marker(ahand_home, latest)
        .context("failed to write version marker")?;

    // ── 12. Final message ───────────────────────────────────────────────────
    println!();
    println!("==> Upgrade complete!");
    println!("  {current} -> {latest}");
    println!();
    println!("Restart the daemon to use the new version:");
    println!("  ahandctl restart");

    Ok(())
}

// ── Private helpers ─────────────────────────────────────────────────────────

/// Download `url`; returns `None` on any HTTP or network error (treated as
/// optional resource).
async fn download_optional(url: &str) -> Option<String> {
    match ahandd::updater::download_binary(url).await {
        Ok(bytes) => Some(String::from_utf8_lossy(&bytes).into_owned()),
        Err(_) => None,
    }
}

/// Look up the expected SHA-256 for `filename` in `checksum_text` (format:
/// `{hex}  {filename}` per line, as written by `shasum -a 256`), and verify.
///
/// If no matching line exists, the check is silently skipped (same behaviour
/// as upgrade.sh's `if [ -n "$expected" ]` guard).
fn verify_binary_checksum(checksum_text: &str, filename: &str, data: &[u8]) -> anyhow::Result<()> {
    // Find a line that ends with two-spaces + filename (shasum format).
    // e.g. "abc123...  ahandd-linux-x64"
    let expected_hex = checksum_text.lines().find_map(|line| {
        // Split on the "  " separator (shasum uses two spaces).
        let (hex, fname) = line.split_once("  ")?;
        if fname.trim() == filename {
            Some(hex.trim().to_string())
        } else {
            None
        }
    });

    if let Some(ref hex) = expected_hex {
        ahandd::updater::verify_checksum(data, hex)
            .with_context(|| format!("checksum mismatch for {filename}"))?;
        println!("  {filename}: OK");
    }
    Ok(())
}

/// Clear `dist_dir` contents (but keep the directory itself), then extract
/// `tar.gz` bytes into it.
///
/// PATH-TRAVERSAL GUARD: any entry whose normalised path escapes `dist_dir`
/// (contains `..` components or is absolute) is rejected with an error.
///
/// SYMLINK-ESCAPE DEFENSE: the two-layer guard below stops both direct traversal
/// (`../evil`) and the "symlink + follow" attack (`link -> /tmp` then
/// `link/evil.txt`).  The manual [`guard_path_traversal`] check catches `..` and
/// absolute paths before anything is written.  The [`tar::Entry::unpack_in`] call
/// provides a second layer via its internal `validate_inside_dst` canonicalization,
/// which re-resolves the destination after each entry is written and rejects any
/// path that has escaped `dist_dir` — this includes symlink-based escapes that
/// were not visible from the raw header path alone.
pub fn extract_admin_spa(data: &[u8], dist_dir: &Path) -> anyhow::Result<()> {
    // Create or keep the dist dir; clear its contents.
    std::fs::create_dir_all(dist_dir)
        .with_context(|| format!("failed to create {}", dist_dir.display()))?;

    // Remove existing contents (not the dir itself).
    for entry in std::fs::read_dir(dist_dir)
        .with_context(|| format!("failed to read {}", dist_dir.display()))?
    {
        let entry = entry?;
        let p = entry.path();
        if p.is_dir() {
            std::fs::remove_dir_all(&p)
                .with_context(|| format!("failed to remove {}", p.display()))?;
        } else {
            std::fs::remove_file(&p)
                .with_context(|| format!("failed to remove {}", p.display()))?;
        }
    }

    // Decompress + untar.
    let gz = flate2::read::GzDecoder::new(std::io::Cursor::new(data));
    let mut archive = tar::Archive::new(gz);

    for entry in archive.entries().context("failed to read tar entries")? {
        let mut entry = entry.context("bad tar entry")?;
        let raw_path = entry.path().context("bad entry path")?;

        // Path-traversal guard.
        guard_path_traversal(&raw_path)
            .with_context(|| format!("path traversal detected: {}", raw_path.display()))?;

        entry
            .unpack_in(dist_dir)
            .context("failed to unpack tar entry")?;
    }

    Ok(())
}

/// Return `Err` if `p` contains any `..` component or is absolute.
fn guard_path_traversal(p: &Path) -> anyhow::Result<()> {
    if p.is_absolute() {
        anyhow::bail!("absolute path in archive: {}", p.display());
    }
    for component in p.components() {
        if matches!(component, Component::ParentDir) {
            anyhow::bail!("parent-dir component (..) in archive path: {}", p.display());
        }
    }
    Ok(())
}
