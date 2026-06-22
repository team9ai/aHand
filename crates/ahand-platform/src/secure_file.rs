//! Owner-only secret-file writes.
//!
//! Unix: concurrent writers each get a private tmp file (`.{name}.{pid}.{N}.tmp`);
//! the last-rename-wins but every rename carries a fully-written file. A best-effort
//! sweep removes sibling stale tmps before writing. Files are created exclusively
//! (`O_CREAT|O_EXCL`) with mode `0o600` so the mode always applies to a fresh file.
//! fsync, atomic rename. Windows: write a temp file, strip ACL inheritance and grant
//! only the current user via `icacls`, then rename into place; the temp file briefly
//! exists with default ACLs inside the target directory, which is itself under the
//! user profile — accepted and documented in the design spec ("Behavioral decisions").
//! Parent directories are created with mode `0o700` on Unix (newly created dirs only;
//! existing dirs are untouched).

use anyhow::{Context, Result};
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

/// Per-process monotonic counter used to build unique tmp names.
static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Write `contents` to `path` with owner-only permissions, atomically.
///
/// - Creates parent directories if they do not exist (Unix: mode `0o700`;
///   existing dirs are untouched).
/// - Unix: a unique tmp file `.{name}.{pid}.{N}.tmp` is created exclusively
///   (`O_CREAT|O_EXCL`) with mode `0o600` before any bytes are written — the
///   mode always applies to a fresh file and a pre-existing tmp can never
///   donate its permissions. Before creating the tmp, sibling files matching
///   `.{name}.*.tmp` are removed best-effort (stale tmps from crashed runs or
///   other concurrent writers are swept; errors are ignored). Concurrent
///   writers each hold a private tmp; the last `rename` wins but always
///   publishes a COMPLETE file.
/// - Windows: a temp file is created in the same directory, then
///   `icacls /inheritance:r /grant:r <USERNAME>:F` is applied to strip
///   inheritance and grant only the current user full control. If `icacls`
///   fails the temp file is removed and an error is returned — a secret file
///   with default ACLs is never left in place silently.
/// - Both platforms: the temp file is renamed over `path` atomically (on
///   Windows this is a remove-then-rename because Windows cannot atomically
///   replace a file that another process holds open; a concurrent reader may
///   observe a missing file during the window — documented M1 accepted window
///   in the design spec "Behavioral decisions").
pub fn write_secure_file(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("secure file path has no parent: {}", path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)
            .with_context(|| format!("failed to create directory {}", parent.display()))?;
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(parent)
        .with_context(|| format!("failed to create directory {}", parent.display()))?;

    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();

    // Build a unique tmp name: .{name}.{pid}.{counter}.tmp
    let pid = std::process::id();
    let counter = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = parent.join(format!(".{file_name}.{pid}.{counter}.tmp"));

    // Best-effort sweep of sibling stale tmps (same prefix/suffix) — ignore errors.
    sweep_stale_tmps(parent, &file_name, &tmp);

    {
        // Create exclusively: with O_EXCL/create_new the file is guaranteed
        // fresh, so the 0o600 mode (Unix) always applies and we never inherit
        // foreign permissions or contents.
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts
            .open(&tmp)
            .with_context(|| format!("failed to create {}", tmp.display()))?;
        f.write_all(contents)
            .with_context(|| format!("failed to write {}", tmp.display()))?;
        f.sync_all()
            .with_context(|| format!("failed to fsync {}", tmp.display()))?;
    }

    #[cfg(windows)]
    if let Err(e) = restrict_to_owner(&tmp) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }

    // Windows: rename-over-existing of a file another process holds open can
    // fail; remove-then-rename is acceptable for these secret files — a
    // concurrent reader may observe a missing file during the window
    // (documented M1 accepted window in the design spec "Behavioral
    // decisions"; concurrent writers use unique tmp names so last complete
    // write wins).
    #[cfg(windows)]
    if path.exists() {
        std::fs::remove_file(path)
            .with_context(|| format!("failed to replace {}", path.display()))?;
    }

    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e)
            .with_context(|| format!("failed to rename {} -> {}", tmp.display(), path.display()));
    }
    Ok(())
}

/// Remove sibling tmp files matching `.{name}.*.tmp` in `parent`, skipping
/// `own_tmp` and any file modified within the last 10 seconds (to avoid
/// racing with other currently-active writers). All errors are ignored
/// (best-effort cleanup only).
fn sweep_stale_tmps(parent: &Path, file_name: &str, own_tmp: &Path) {
    let prefix = format!(".{file_name}.");
    let suffix = ".tmp";
    let now = std::time::SystemTime::now();
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten() {
        let entry_path = entry.path();
        if entry_path == own_tmp {
            continue;
        }
        let fname = entry.file_name();
        let s = fname.to_string_lossy();
        if !s.starts_with(&prefix) || !s.ends_with(suffix) {
            continue;
        }
        // Skip recently-modified files: they may belong to an active writer.
        // If we cannot determine the age (unsupported mtime, clock skew, etc.)
        // we conservatively skip the file rather than risk deleting an active tmp.
        let is_old = match entry.metadata() {
            Ok(meta) => match meta.modified() {
                Ok(modified) => match now.duration_since(modified) {
                    Ok(age) => age.as_secs() >= 10,
                    Err(_) => false, // modified is in the future — very recent, skip
                },
                Err(_) => false, // mtime unavailable — skip conservatively
            },
            Err(_) => false, // can't stat — skip conservatively
        };
        if !is_old {
            continue;
        }
        let _ = std::fs::remove_file(&entry_path);
    }
}

/// Restrict `path` to the current user only (Windows). Hard error on failure:
/// a secret file with default ACLs must never be left in place silently.
#[cfg(windows)]
pub fn restrict_to_owner(path: &Path) -> Result<()> {
    let user = std::env::var("USERNAME").context("USERNAME is not set")?;
    let output = std::process::Command::new("icacls")
        .arg(path)
        .args(["/inheritance:r", "/grant:r", &format!("{user}:F")])
        .output()
        .context("failed to run icacls")?;
    if !output.status.success() {
        anyhow::bail!(
            "icacls failed on {}: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_contents_and_creates_parents() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("deep").join("nested").join("secret.json");
        write_secure_file(&path, b"{\"k\":1}\n").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"{\"k\":1}\n");
    }

    #[cfg(unix)]
    #[test]
    fn unix_mode_is_0600() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("secret");
        write_secure_file(&path, b"s").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[cfg(windows)]
    #[test]
    fn windows_acl_is_restricted_to_owner() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("secret");
        write_secure_file(&path, b"s").unwrap();
        // After /inheritance:r + single /grant:r <user>:F there must be no
        // broad principals left on the ACL.
        let out = std::process::Command::new("icacls")
            .arg(&path)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout).to_lowercase();
        let user = std::env::var("USERNAME").unwrap().to_lowercase();
        assert!(text.contains(&user), "ACL output missing user: {text}");
        assert!(
            !text.contains("builtin\\users"),
            "world-readable ACL: {text}"
        );
        assert!(!text.contains("everyone"), "world-readable ACL: {text}");
    }

    #[test]
    fn overwrites_existing_file_atomically() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("secret");
        write_secure_file(&path, b"one").unwrap();
        write_secure_file(&path, b"two").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"two");
    }

    #[cfg(unix)]
    #[test]
    fn preexisting_world_readable_tmp_cannot_leak_into_secret() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("secret");
        // Stage a world-readable tmp file the way an attacker (or crashed
        // run) would. Security property: even if the staged file is NOT swept
        // (the sweep uses a time threshold to avoid racing active writers),
        // the secret must still be created with 0o600 because `write_secure_file`
        // uses `create_new(true)` with `mode(0o600)` — the staged file's mode
        // can never propagate to the freshly-created tmp or to the secret.
        let staged = tmp.path().join(".secret.stale.tmp");
        std::fs::write(&staged, b"stale").unwrap();
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o666)).unwrap();
        write_secure_file(&path, b"fresh").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "staged tmp permissions leaked");
        assert_eq!(std::fs::read(&path).unwrap(), b"fresh");
        // Note: the stale tmp may or may not be swept depending on its age
        // (sweep skips recently-modified files to avoid racing active writers).
        // The security invariant is that the SECRET's mode is 0o600, which
        // is verified above regardless of whether the staged file was swept.
    }

    #[cfg(unix)]
    #[test]
    fn newly_created_parent_dir_has_mode_0700() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("newdir").join("secret");
        write_secure_file(&path, b"s").unwrap();
        let mode = std::fs::metadata(tmp.path().join("newdir"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700, "newly created parent dir is not 0700");
    }

    // ── no-parent path (#6) ───────────────────────────────────────────────────
    //
    // `Path::new("/").parent()` returns `None` on both Unix and Windows, which
    // is the no-parent branch in `write_secure_file`. We use that as the
    // canonical "no parent" input. (A bare filename like `Path::new("file")`
    // also has no parent *component* but its `.parent()` returns `Some("")`,
    // so it does NOT exercise the `ok_or_else` branch.)
    #[test]
    fn write_secure_file_no_parent_errors() {
        // "/" has no parent — `.parent()` returns None — triggering the
        // "secure file path has no parent" error branch.
        let err = write_secure_file(std::path::Path::new("/"), b"x").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("no parent") || msg.contains("parent"),
            "error should mention 'no parent': {msg}"
        );
    }

    /// Spawn 8 threads, each writing 50 iterations of a distinct 64 KB pattern
    /// (byte = thread id) to the SAME path concurrently. After all threads
    /// finish, the final file must equal one of the patterns exactly — no
    /// partial writes, no corruption.
    #[cfg(unix)]
    #[test]
    fn concurrent_writers_never_publish_partial_content() {
        use std::sync::Arc;

        const THREAD_COUNT: u8 = 8;
        const ITERATIONS: usize = 50;
        const CHUNK_SIZE: usize = 64 * 1024; // 64 KB

        let tmp = tempfile::tempdir().unwrap();
        let path = Arc::new(tmp.path().join("concurrent_secret"));

        let handles: Vec<_> = (0..THREAD_COUNT)
            .map(|tid| {
                let p = Arc::clone(&path);
                std::thread::spawn(move || {
                    let pattern = vec![tid; CHUNK_SIZE];
                    for _ in 0..ITERATIONS {
                        write_secure_file(&p, &pattern).expect("write_secure_file failed");
                    }
                })
            })
            .collect();

        for h in handles {
            h.join().expect("thread panicked");
        }

        // The file must exist and contain exactly one valid pattern.
        let final_data = std::fs::read(&*path).expect("final file missing");
        assert_eq!(
            final_data.len(),
            CHUNK_SIZE,
            "final file length is not CHUNK_SIZE ({} != {})",
            final_data.len(),
            CHUNK_SIZE
        );
        let first_byte = final_data[0];
        assert!(
            first_byte < THREAD_COUNT,
            "unexpected byte value {first_byte} (expected 0..{THREAD_COUNT})"
        );
        assert!(
            final_data.iter().all(|&b| b == first_byte),
            "file contains mixed bytes — partial write detected"
        );
    }
}
