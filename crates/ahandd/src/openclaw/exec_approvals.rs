//! OpenClaw-style exec approvals file management.
//!
//! Manages ~/.ahand/exec-approvals.json for OpenClaw mode.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

use super::protocol::{ExecApprovalsFile, ExecApprovalsSnapshot};

const EXEC_APPROVALS_FILE: &str = "exec-approvals.json";

/// Get the default exec approvals file path
pub fn default_exec_approvals_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".ahand")
        .join(EXEC_APPROVALS_FILE)
}

/// Compute hash of file contents
fn compute_hash(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Read exec approvals snapshot
pub fn read_exec_approvals_snapshot(path: &Path) -> Result<ExecApprovalsSnapshot> {
    let exists = path.exists();
    let (file, hash) = if exists {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let hash = compute_hash(&content);
        let file: ExecApprovalsFile = serde_json::from_str(&content)
            .with_context(|| format!("failed to parse {}", path.display()))?;
        (file, hash)
    } else {
        (ExecApprovalsFile::default(), String::new())
    };

    Ok(ExecApprovalsSnapshot {
        path: path.to_string_lossy().to_string(),
        exists,
        hash,
        file,
    })
}

/// Save exec approvals file
pub fn save_exec_approvals(path: &Path, file: &ExecApprovalsFile) -> Result<()> {
    // Ensure parent directory exists
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create directory {}", parent.display()))?;
    }

    let content = serde_json::to_string_pretty(file)
        .context("failed to serialize exec approvals")?;

    std::fs::write(path, format!("{}\n", content))
        .with_context(|| format!("failed to write {}", path.display()))?;

    // Set file permissions to 0600 (user read/write only)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        let _ = std::fs::set_permissions(path, perms);
    }

    Ok(())
}

/// Normalize exec approvals file
pub fn normalize_exec_approvals(file: ExecApprovalsFile) -> ExecApprovalsFile {
    ExecApprovalsFile {
        version: file.version.or(Some(1)),
        security: file.security,
        ask: file.ask,
        allowlist: file.allowlist.map(|list| {
            list.into_iter()
                .filter(|entry| !entry.pattern.is_empty())
                .collect()
        }),
        auto_allow_skills: file.auto_allow_skills,
    }
}

/// Redact sensitive fields from exec approvals (for responses)
pub fn redact_exec_approvals(file: ExecApprovalsFile) -> ExecApprovalsFile {
    // Currently no sensitive fields to redact, but keep this for future use
    file
}
