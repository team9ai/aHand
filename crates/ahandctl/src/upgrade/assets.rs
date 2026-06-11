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

    // ── 3. Download admin SPA + checksums (optional) ───────────────────────
    //
    // release-admin.yml publishes `checksums-admin.txt` alongside
    // `admin-spa.tar.gz` (format: `{sha256}  admin-spa.tar.gz` as written by
    // `shasum -a 256`).  We download it first and verify BEFORE extracting.
    // A 404 on the checksums file is tolerated (older releases, network
    // hiccups) — integrity check is skipped in that case, mirroring the
    // lenient behaviour of the Rust binary path.
    let admin_bytes = if let Some(admin_ver) = info.admin.as_deref() {
        println!("==> Downloading admin panel (admin-v{admin_ver})...");
        let admin_base = format!("{download_base}/admin-v{admin_ver}");
        let admin_url = format!("{admin_base}/admin-spa.tar.gz");
        let admin_checksums_url = format!("{admin_base}/checksums-admin.txt");

        println!("  Downloading admin-spa.tar.gz...");
        match ahandd::updater::download_binary(&admin_url).await {
            Ok(b) => {
                // Download checksums (optional — 404 → skip verify).
                let admin_checksum_text = download_optional(&admin_checksums_url).await;
                if let Some(ref cs_text) = admin_checksum_text {
                    println!("==> Verifying admin-spa.tar.gz checksum...");
                    verify_binary_checksum(cs_text, "admin-spa.tar.gz", &b)
                        .context("admin-spa.tar.gz checksum mismatch")?;
                }
                Some(b)
            }
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
    // (No setup-browser.sh on non-unix; the unix-only install step below is
    // the sole consumer, so no binding is needed elsewhere.)

    // ── 5. Verify Rust binary checksums BEFORE any install ──────────────────
    if let Some(ref cs_text) = checksum_text {
        println!();
        println!("==> Verifying checksums...");
        verify_binary_checksum(cs_text, &ahandd_filename, &ahandd_bytes)?;
        verify_binary_checksum(cs_text, &ahandctl_filename, &ahandctl_bytes)?;
    }

    // ── 6. Stop daemon ──────────────────────────────────────────────────────
    println!();
    println!("==> Stopping daemon...");
    if let Err(e) = crate::daemon::stop_at(&ahand_home.join("data")).await {
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

/// Look up the expected SHA-256 for `filename` in `checksum_text` and verify.
///
/// Accepted line formats (covers `shasum`, `sha256sum`, binary-mode `*` prefix):
/// - `{hex}  {filename}`       — double-space (`shasum -a 256`)
/// - `{hex} *{filename}`       — single-space + binary-mode marker (`sha256sum -b`)
/// - `{hex} {filename}`        — single-space
///
/// Parsing: split each line by whitespace → first token = hex digest, last
/// token = filename with any leading `*` stripped.
///
/// If no matching line exists, the check is silently skipped (same behaviour
/// as upgrade.sh's `if [ -n "$expected" ]` guard).
///
/// Leniency is intentional: parity with `upgrade.sh` which skips verification
/// when the expected hash is absent rather than failing the upgrade.
fn verify_binary_checksum(checksum_text: &str, filename: &str, data: &[u8]) -> anyhow::Result<()> {
    let expected_hex = checksum_text.lines().find_map(|line| {
        let mut tokens = line.split_whitespace();
        let hex = tokens.next()?;
        // Last token is the filename; strip a leading '*' (binary-mode marker).
        let fname = tokens.last()?.trim_start_matches('*');
        if fname == filename {
            Some(hex.to_string())
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

/// Extract `tar.gz` bytes into `dist_dir` using a crash-safe rename-aside swap.
///
/// CRASH-SAFE SWAP:
///   1. Extraction happens into a fresh sibling temp directory
///      (`{dist_parent}/.dist.tmp-{pid}`).
///   2. Only after a FULLY successful extraction: the existing `dist_dir` (if
///      any) is renamed aside to `{dist_parent}/.dist.old-{pid}`.
///   3. The temp dir is then renamed into place as `dist_dir`.
///   4. If the final rename (step 3) fails, the old dir is renamed back from
///      `.dist.old-{pid}` to `dist_dir` to restore the previous state.  Both
///      the rename-in error and any rollback error are reported together.
///   5. On success, the `.dist.old-{pid}` directory is removed on a best-effort
///      basis (failure is logged but does not abort the upgrade).
///
/// This guarantees that a failed mid-swap never leaves `dist_dir` absent: at
/// any point in time either the old content or the new content is present.
///
/// PATH-TRAVERSAL GUARD: any entry whose normalised path escapes the temp dir
/// (contains `..` components or is absolute) is rejected with an error before
/// any bytes are written.
///
/// LINK REJECTION: symlinks and hard links are explicitly rejected.  The dist
/// directory is served verbatim by `warp::fs::dir`; a crafted symlink could
/// point outside the dist tree and expose arbitrary filesystem content to HTTP
/// clients.  Only regular files, directories, and PAX-extension metadata
/// entries are accepted.
pub fn extract_admin_spa(data: &[u8], dist_dir: &Path) -> anyhow::Result<()> {
    let dist_parent = dist_dir
        .parent()
        .ok_or_else(|| anyhow::anyhow!("dist_dir has no parent: {}", dist_dir.display()))?;

    // Ensure the parent directory exists so we can create the temp dir beside it.
    std::fs::create_dir_all(dist_parent)
        .with_context(|| format!("failed to create parent {}", dist_parent.display()))?;

    let pid = std::process::id();

    // Create a fresh temp dir sibling for safe extraction.
    let tmp_dir = dist_parent.join(format!(".dist.tmp-{pid}"));

    // Remove any stale temp dir from a previous aborted run.
    if tmp_dir.exists() {
        std::fs::remove_dir_all(&tmp_dir)
            .with_context(|| format!("failed to remove stale temp dir {}", tmp_dir.display()))?;
    }
    std::fs::create_dir_all(&tmp_dir)
        .with_context(|| format!("failed to create temp dir {}", tmp_dir.display()))?;

    // Decompress + untar into the temp dir.
    let gz = flate2::read::GzDecoder::new(std::io::Cursor::new(data));
    let mut archive = tar::Archive::new(gz);

    let extract_result = (|| -> anyhow::Result<()> {
        for entry in archive.entries().context("failed to read tar entries")? {
            let mut entry = entry.context("bad tar entry")?;
            let entry_type = entry.header().entry_type();

            // Reject symlinks and hard links — the dist tree is served by
            // warp::fs::dir, so a link could expose files outside dist.
            if entry_type.is_symlink() || entry_type.is_hard_link() {
                anyhow::bail!(
                    "archive contains a link entry (type {:?}) which is not allowed in admin-spa archives",
                    entry_type
                );
            }

            // Accept only regular files, directories, and extension metadata
            // entries (PAX local/global, GNU long-name/link).  Any other
            // exotic entry type (character device, block device, FIFO,
            // contiguous, etc.) is also rejected for safety.
            if !entry_type.is_file()
                && !entry_type.is_dir()
                && !entry_type.is_pax_local_extensions()
                && !entry_type.is_pax_global_extensions()
                && !entry_type.is_gnu_longname()
                && !entry_type.is_gnu_longlink()
            {
                anyhow::bail!(
                    "archive contains an unsupported entry type ({:?}); only files and directories are allowed",
                    entry_type
                );
            }

            let raw_path = entry.path().context("bad entry path")?;

            // Path-traversal guard.
            guard_path_traversal(&raw_path)
                .with_context(|| format!("path traversal detected: {}", raw_path.display()))?;

            entry
                .unpack_in(&tmp_dir)
                .context("failed to unpack tar entry")?;
        }
        Ok(())
    })();

    if let Err(e) = extract_result {
        // Clean up the partial temp dir so it does not linger.
        let _ = std::fs::remove_dir_all(&tmp_dir);
        return Err(e);
    }

    // ── Crash-safe rename-aside swap ────────────────────────────────────────
    // Rename existing dist aside (if present), then rename tmp into place.
    // On final-rename failure, roll back by restoring the old dir.
    let old_dir = dist_parent.join(format!(".dist.old-{pid}"));

    let dist_existed = dist_dir.exists();
    if dist_existed {
        std::fs::rename(dist_dir, &old_dir).with_context(|| {
            format!(
                "failed to rename existing dist aside: {} -> {}",
                dist_dir.display(),
                old_dir.display()
            )
        })?;
    }

    if let Err(rename_err) = std::fs::rename(&tmp_dir, dist_dir) {
        let rename_err = anyhow::Error::new(rename_err).context(format!(
            "failed to rename {} -> {}",
            tmp_dir.display(),
            dist_dir.display()
        ));
        // Attempt to roll back the old dist.
        if dist_existed && let Err(rollback_err) = std::fs::rename(&old_dir, dist_dir) {
            return Err(rename_err.context(format!(
                "rollback also failed ({rollback_err}); old dist left at {}",
                old_dir.display()
            )));
        }
        let _ = std::fs::remove_dir_all(&tmp_dir);
        return Err(rename_err);
    }

    // Success — best-effort removal of the old aside dir.
    if dist_existed && let Err(e) = std::fs::remove_dir_all(&old_dir) {
        eprintln!(
            "  Warning: could not remove old dist backup {}: {e}",
            old_dir.display()
        );
    }

    Ok(())
}

/// Return `Err` if `p` contains any `..` component, is absolute, or is
/// rooted/prefixed. Component-level checks keep the policy platform-uniform:
/// on Windows `/etc/x` is "rooted" but NOT `is_absolute()` (no drive letter),
/// and `C:\x` carries a `Prefix` component — both must be rejected too.
fn guard_path_traversal(p: &Path) -> anyhow::Result<()> {
    for component in p.components() {
        match component {
            Component::ParentDir => {
                anyhow::bail!("parent-dir component (..) in archive path: {}", p.display());
            }
            Component::RootDir | Component::Prefix(_) => {
                anyhow::bail!("absolute path in archive: {}", p.display());
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod checksum_skip_tests {
    use super::*;

    /// A checksum file that contains valid lines ONLY for OTHER filenames must
    /// return `Ok(())` without touching the data bytes — this is the documented
    /// lenient behaviour that mirrors `upgrade.sh`'s
    /// `if [ -n "$expected" ]` guard.
    #[test]
    fn checksum_text_with_no_matching_filename_returns_ok() {
        // Lines for entirely different filenames; our target is absent.
        let checksum_text = "\
            abc123def456abc123def456abc123def456abc123def456abc123def456abc123  other-binary-linux-x64\n\
            deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef  yet-another-tool.exe\n";

        // The data bytes are intentionally wrong — if the fn is lenient it must
        // never reach the hash comparison for an absent filename.
        let result =
            verify_binary_checksum(checksum_text, "ahandd-linux-x64", b"totally-wrong-bytes");
        assert!(
            result.is_ok(),
            "expected Ok(()) when filename is absent from checksum text, got: {result:?}"
        );
    }

    /// Lines in `{hex} *{filename}` format (binary-mode marker from `sha256sum -b`)
    /// must be parsed correctly: the leading `*` is stripped from the filename.
    #[test]
    fn checksum_text_binary_mode_marker_is_parsed() {
        use sha2::{Digest, Sha256};
        let data = b"binary-mode-test";
        let hex = hex::encode(Sha256::digest(data));
        // Single-space + `*` prefix — the `sha256sum -b` format.
        let checksum_text = format!("{hex} *ahandd-linux-x64\n");
        let result = verify_binary_checksum(&checksum_text, "ahandd-linux-x64", data);
        assert!(
            result.is_ok(),
            "binary-mode (*) line should match: {result:?}"
        );
    }

    /// Lines with only a single space between hex and filename (no `*`) must
    /// also be accepted.
    #[test]
    fn checksum_text_single_space_variant_is_parsed() {
        use sha2::{Digest, Sha256};
        let data = b"single-space-test";
        let hex = hex::encode(Sha256::digest(data));
        // Single-space, no binary-mode marker.
        let checksum_text = format!("{hex} ahandd-linux-x64\n");
        let result = verify_binary_checksum(&checksum_text, "ahandd-linux-x64", data);
        assert!(result.is_ok(), "single-space line should match: {result:?}");
    }
}

#[cfg(test)]
mod extract_tests {
    use super::*;
    use std::io::Write as _;

    /// Build a minimal tar.gz in memory from a list of `(path, content)` pairs.
    fn make_tar_gz(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut tar_bytes = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut tar_bytes);
            for (path, content) in files {
                let mut header = tar::Header::new_gnu();
                header.set_size(content.len() as u64);
                header.set_mode(0o644);
                header.set_entry_type(tar::EntryType::Regular);
                header.set_cksum();
                builder.append_data(&mut header, path, *content).unwrap();
            }
            builder.finish().unwrap();
        }

        let mut gz_bytes = Vec::new();
        {
            let mut enc =
                flate2::write::GzEncoder::new(&mut gz_bytes, flate2::Compression::default());
            enc.write_all(&tar_bytes).unwrap();
        }
        gz_bytes
    }

    /// Build a tar.gz that contains a symlink entry.
    fn make_tar_gz_with_symlink(link_name: &str, link_target: &str) -> Vec<u8> {
        let mut tar_bytes = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut tar_bytes);
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_size(0);
            header.set_mode(0o777);
            header
                .set_link_name(std::path::Path::new(link_target))
                .unwrap();
            header.set_cksum();
            builder
                .append_data(&mut header, link_name, &[][..])
                .unwrap();
            builder.finish().unwrap();
        }

        let mut gz_bytes = Vec::new();
        {
            let mut enc =
                flate2::write::GzEncoder::new(&mut gz_bytes, flate2::Compression::default());
            enc.write_all(&tar_bytes).unwrap();
        }
        gz_bytes
    }

    /// A well-formed archive should extract successfully and the dist dir
    /// should contain the expected files.
    #[test]
    fn extract_admin_spa_normal_archive_succeeds() {
        let tmp = tempfile::tempdir().unwrap();
        let dist_dir = tmp.path().join("admin").join("dist");

        let archive = make_tar_gz(&[
            ("index.html", b"<html/>"),
            ("assets/app.js", b"console.log()"),
        ]);
        extract_admin_spa(&archive, &dist_dir).unwrap();

        assert!(dist_dir.join("index.html").exists());
        assert!(dist_dir.join("assets").join("app.js").exists());
    }

    /// An archive containing a symlink entry must be rejected with an error.
    #[test]
    fn extract_admin_spa_rejects_symlink_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let dist_dir = tmp.path().join("admin").join("dist");

        let archive = make_tar_gz_with_symlink("evil-link", "/etc/passwd");
        let result = extract_admin_spa(&archive, &dist_dir);

        assert!(
            result.is_err(),
            "symlink entry in archive must be rejected, got Ok"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("link") || msg.contains("symlink") || msg.contains("not allowed"),
            "error should mention link/symlink: {msg}"
        );
    }

    /// A failed extraction (e.g. corrupt archive or link rejection) must leave
    /// the original dist directory intact — pre-populate dist with a sentinel
    /// file and confirm it is still present after the failure.
    #[test]
    fn extract_admin_spa_failed_extraction_leaves_original_dist_intact() {
        let tmp = tempfile::tempdir().unwrap();
        let dist_dir = tmp.path().join("admin").join("dist");

        // Pre-populate the dist dir with a sentinel file (simulates existing dist).
        std::fs::create_dir_all(&dist_dir).unwrap();
        let sentinel = dist_dir.join("sentinel.html");
        std::fs::write(&sentinel, b"old content").unwrap();

        // Feed a symlink archive — extraction must fail.
        let archive = make_tar_gz_with_symlink("evil-link", "/etc/passwd");
        let result = extract_admin_spa(&archive, &dist_dir);

        assert!(result.is_err(), "corrupt/link archive should return Err");

        // The original dist must still be intact.
        assert!(
            sentinel.exists(),
            "original dist sentinel must still exist after failed extraction"
        );
        assert_eq!(
            std::fs::read(&sentinel).unwrap(),
            b"old content",
            "original dist content must be unchanged after failed extraction"
        );
    }

    /// An archive with a path-traversal entry (`../evil`) must be rejected.
    ///
    /// The `tar` crate's builder refuses to encode `..` path components, so we
    /// construct a minimal raw POSIX tar header with a `../escape.txt` name
    /// injected directly into the 100-byte name field, then gzip-compress it.
    /// This mirrors what a malicious/manually-crafted archive would look like.
    #[test]
    fn extract_admin_spa_rejects_path_traversal() {
        let tmp = tempfile::tempdir().unwrap();
        let dist_dir = tmp.path().join("admin").join("dist");

        // Build a raw tar header with a traversal path.  A POSIX tar header is
        // 512 bytes; the first 100 bytes are the name field (NUL-terminated).
        let archive = make_raw_tar_gz_with_path("../escape.txt");
        let result = extract_admin_spa(&archive, &dist_dir);

        assert!(
            result.is_err(),
            "path-traversal entry must be rejected, got Ok"
        );
    }

    /// Build a gzip-compressed tar containing exactly ONE regular-file entry
    /// with the provided raw path string (which may contain `..`).
    fn make_raw_tar_gz_with_path(path: &str) -> Vec<u8> {
        use std::io::Write as _;
        // A POSIX tar header is 512 bytes.
        let mut header = [0u8; 512];
        // Name field: bytes 0..100.
        let name_bytes = path.as_bytes();
        let copy_len = name_bytes.len().min(99);
        header[..copy_len].copy_from_slice(&name_bytes[..copy_len]);
        // Mode: bytes 100..108 (octal "0000644\0").
        b"0000644\0"
            .iter()
            .enumerate()
            .for_each(|(i, &b)| header[100 + i] = b);
        // UID/GID: bytes 108..124.
        b"0000000\0"
            .iter()
            .enumerate()
            .for_each(|(i, &b)| header[108 + i] = b);
        b"0000000\0"
            .iter()
            .enumerate()
            .for_each(|(i, &b)| header[116 + i] = b);
        // Size: bytes 124..136 (zero, no data).
        b"00000000000\0"
            .iter()
            .enumerate()
            .for_each(|(i, &b)| header[124 + i] = b);
        // Mtime: bytes 136..148.
        b"00000000000\0"
            .iter()
            .enumerate()
            .for_each(|(i, &b)| header[136 + i] = b);
        // Type flag: byte 156 — '0' = regular file.
        header[156] = b'0';
        // Magic: bytes 257..263 "ustar\0".
        b"ustar\0"
            .iter()
            .enumerate()
            .for_each(|(i, &b)| header[257 + i] = b);
        // Version: bytes 263..265 "00".
        header[263] = b'0';
        header[264] = b'0';
        // Compute checksum (bytes 148..156).
        header[148..156].fill(b' ');
        let cksum: u32 = header.iter().map(|&b| b as u32).sum();
        let cksum_str = format!("{:06o}\0 ", cksum);
        cksum_str
            .as_bytes()
            .iter()
            .enumerate()
            .for_each(|(i, &b)| header[148 + i] = b);

        // tar = header block + two 512-byte zero end-of-archive blocks.
        let mut tar_bytes = Vec::with_capacity(512 * 3);
        tar_bytes.extend_from_slice(&header);
        tar_bytes.extend_from_slice(&[0u8; 512]); // EOF block 1
        tar_bytes.extend_from_slice(&[0u8; 512]); // EOF block 2

        // Gzip-compress.
        let mut gz_bytes = Vec::new();
        let mut enc = flate2::write::GzEncoder::new(&mut gz_bytes, flate2::Compression::default());
        enc.write_all(&tar_bytes).unwrap();
        drop(enc);
        gz_bytes
    }

    /// A successful swap must:
    ///   - leave `dist_dir` with the NEW archive content, and
    ///   - leave NO `.dist.old-*` residue in the parent directory.
    #[test]
    fn extract_admin_spa_successful_swap_leaves_no_old_residue() {
        let tmp = tempfile::tempdir().unwrap();
        let dist_dir = tmp.path().join("admin").join("dist");

        // Pre-populate dist with old content.
        std::fs::create_dir_all(&dist_dir).unwrap();
        std::fs::write(dist_dir.join("old.html"), b"old").unwrap();

        // New archive has different content.
        let archive = make_tar_gz(&[("index.html", b"<html>new</html>")]);
        extract_admin_spa(&archive, &dist_dir).unwrap();

        // dist must contain the new content.
        assert!(
            dist_dir.join("index.html").exists(),
            "new index.html must be present after successful swap"
        );
        assert_eq!(
            std::fs::read(dist_dir.join("index.html")).unwrap(),
            b"<html>new</html>",
            "dist content must be new archive content"
        );
        // Old file must not be present in dist.
        assert!(
            !dist_dir.join("old.html").exists(),
            "old content must not be present in dist after successful swap"
        );

        // No .dist.old-* residue in parent.
        let dist_parent = dist_dir.parent().unwrap();
        let residue: Vec<_> = std::fs::read_dir(dist_parent)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with(".dist.old-"))
            .collect();
        assert!(
            residue.is_empty(),
            "no .dist.old-* residue should remain after successful swap, found: {:?}",
            residue.iter().map(|e| e.file_name()).collect::<Vec<_>>()
        );
    }

    /// Verify that the admin-spa checksum check passes with a correct hash
    /// and fails with an incorrect hash.
    #[test]
    fn admin_spa_checksum_correct_passes() {
        use sha2::{Digest, Sha256};
        let data = b"fake-spa-bytes";
        let hex = hex::encode(Sha256::digest(data));
        let checksum_text = format!("{hex}  admin-spa.tar.gz\n");
        verify_binary_checksum(&checksum_text, "admin-spa.tar.gz", data)
            .expect("correct checksum should pass");
    }

    #[test]
    fn admin_spa_checksum_mismatch_errors() {
        let data = b"fake-spa-bytes";
        let wrong_hex = "0000000000000000000000000000000000000000000000000000000000000000";
        let checksum_text = format!("{wrong_hex}  admin-spa.tar.gz\n");
        let result = verify_binary_checksum(&checksum_text, "admin-spa.tar.gz", data);
        assert!(result.is_err(), "wrong checksum must return Err");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("checksum") || msg.contains("expected") || msg.contains("got"),
            "error should describe mismatch: {msg}"
        );
    }
}
