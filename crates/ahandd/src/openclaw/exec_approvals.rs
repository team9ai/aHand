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
    let content =
        serde_json::to_string_pretty(file).context("failed to serialize exec approvals")?;

    ahand_platform::secure_file::write_secure_file(path, format!("{}\n", content).as_bytes())
        .with_context(|| format!("failed to write {}", path.display()))?;

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

#[cfg(test)]
mod tests {
    use super::super::protocol::AllowlistEntry;
    use super::*;

    fn make_test_path(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir()
            .join(format!(
                "ahandd-exec-approvals-{}-{}",
                tag,
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ))
            .join("exec-approvals.json")
    }

    #[cfg(unix)]
    #[test]
    fn save_exec_approvals_uses_0600_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let path = make_test_path("perms");
        let file = ExecApprovalsFile {
            version: Some(1),
            security: None,
            ask: None,
            allowlist: None,
            auto_allow_skills: None,
        };

        save_exec_approvals(&path, &file).unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "exec-approvals.json must be owner-read/write only"
        );

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(path.parent().unwrap());
    }

    #[test]
    fn save_and_load_exec_approvals_round_trips() {
        let path = make_test_path("roundtrip");
        let file = ExecApprovalsFile {
            version: Some(1),
            security: Some("strict".to_string()),
            ask: Some("always".to_string()),
            allowlist: Some(vec![AllowlistEntry {
                pattern: "cargo test".to_string(),
                agent_id: Some("agent-abc".to_string()),
                last_used_ms: Some(1_000_000),
                use_count: Some(3),
            }]),
            auto_allow_skills: Some(true),
        };

        save_exec_approvals(&path, &file).unwrap();

        let snapshot = read_exec_approvals_snapshot(&path).unwrap();
        assert!(snapshot.exists);
        assert!(!snapshot.hash.is_empty());
        let loaded = snapshot.file;
        assert_eq!(loaded.version, file.version);
        assert_eq!(loaded.security, file.security);
        assert_eq!(loaded.ask, file.ask);
        assert_eq!(loaded.auto_allow_skills, file.auto_allow_skills);
        let entries = loaded.allowlist.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].pattern, "cargo test");
        assert_eq!(entries[0].agent_id.as_deref(), Some("agent-abc"));

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(path.parent().unwrap());
    }
}
