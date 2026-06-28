use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

use super::path_policy::resolve_existing_sandbox_path;
use super::registry::SandboxRegistry;
use super::types::{
    CommitResult, FileVersion, FileVersionStatus, HostFileRef, RegisterVersionRequest,
    SandboxError, SandboxFile, SandboxPermissionMode, SandboxResult,
};

pub fn import_file(
    registry: &mut SandboxRegistry,
    session_id: &str,
    file_ref: HostFileRef,
) -> SandboxResult<SandboxFile> {
    let session = registry.session_mut(session_id)?;
    let input_dir = session
        .workspace_root
        .join("input")
        .join(&file_ref.file_ref_id);
    fs::create_dir_all(&input_dir).map_err(|e| {
        SandboxError::unavailable(format!("failed to create sandbox input dir: {e}"))
    })?;
    let sandbox_path = input_dir.join(safe_file_name(&file_ref.display_name));
    fs::copy(&file_ref.source_path, &sandbox_path).map_err(|e| {
        SandboxError::unavailable(format!("failed to import host file into sandbox: {e}"))
    })?;
    let size = fs::metadata(&sandbox_path)
        .map_err(|e| SandboxError::unavailable(format!("failed to stat imported file: {e}")))?
        .len();
    let sandbox_file = SandboxFile {
        sandbox_file_id: format!(
            "sandbox-file-{}",
            sha256_hex(file_ref.file_ref_id.as_bytes())
        ),
        file_ref_id: file_ref.file_ref_id.clone(),
        sandbox_path,
        size,
    };
    session
        .host_file_refs
        .insert(file_ref.file_ref_id.clone(), file_ref.clone());
    session
        .imported_files
        .insert(file_ref.file_ref_id, sandbox_file.clone());
    Ok(sandbox_file)
}

pub fn register_file_version(
    registry: &mut SandboxRegistry,
    session_id: &str,
    request: RegisterVersionRequest,
) -> SandboxResult<FileVersion> {
    let session = registry.session(session_id)?;
    let sandbox_path = resolve_existing_sandbox_path(
        &session.workspace_root,
        &request.sandbox_path.to_string_lossy(),
    )?;
    let metadata = fs::metadata(&sandbox_path).map_err(|e| {
        SandboxError::invalid_sandbox_path(format!("failed to stat sandbox file: {e}"))
    })?;
    if !metadata.is_file() {
        return Err(SandboxError::invalid_sandbox_path(
            "registered sandbox path must be a file",
        ));
    }
    let hash = sha256_file(&sandbox_path)?;

    let version = FileVersion {
        version_id: format!("version-{hash}"),
        sandbox_path,
        source_file_ref_id: request.source_file_ref_id,
        size: metadata.len(),
        hash,
        status: FileVersionStatus::Candidate,
    };

    registry
        .session_mut(session_id)?
        .file_versions
        .insert(version.version_id.clone(), version.clone());

    Ok(version)
}

pub fn list_file_versions(
    registry: &SandboxRegistry,
    session_id: &str,
) -> SandboxResult<Vec<FileVersion>> {
    Ok(registry
        .session(session_id)?
        .file_versions
        .values()
        .cloned()
        .collect())
}

pub fn commit_file_version(
    registry: &mut SandboxRegistry,
    session_id: &str,
    version_id: &str,
) -> SandboxResult<CommitResult> {
    commit_registered_version(registry, session_id, version_id, CommitTarget::Source, true)
}

pub fn confirm_file_version_overwrite(
    registry: &mut SandboxRegistry,
    session_id: &str,
    version_id: &str,
) -> SandboxResult<CommitResult> {
    commit_registered_version(
        registry,
        session_id,
        version_id,
        CommitTarget::Source,
        false,
    )
}

pub fn save_file_version_as(
    registry: &mut SandboxRegistry,
    session_id: &str,
    version_id: &str,
    target_path: PathBuf,
) -> SandboxResult<CommitResult> {
    if !target_path.is_absolute() {
        return Err(SandboxError::invalid_sandbox_path(
            "save-as target path must be absolute",
        ));
    }

    commit_registered_version(
        registry,
        session_id,
        version_id,
        CommitTarget::ExplicitPath(target_path),
        false,
    )
}

enum CommitTarget {
    Source,
    ExplicitPath(PathBuf),
}

fn commit_registered_version(
    registry: &mut SandboxRegistry,
    session_id: &str,
    version_id: &str,
    target: CommitTarget,
    require_full_permission: bool,
) -> SandboxResult<CommitResult> {
    let snapshot = registry.permission_snapshot(session_id)?;
    if require_full_permission && snapshot.mode != SandboxPermissionMode::Full {
        return Err(SandboxError::permission_denied(
            "full permission is required to commit a sandbox file version",
        ));
    }
    let (version, workspace_root, source_file_ref_id, target_path, supersede_source_versions) = {
        let session = registry.session(session_id)?;
        let version = session
            .file_versions
            .get(version_id)
            .cloned()
            .ok_or_else(|| {
                SandboxError::unknown_version(format!("unknown version '{version_id}'"))
            })?;
        let source_file_ref_id = match target {
            CommitTarget::Source => {
                let source_file_ref_id = version.source_file_ref_id.clone().ok_or_else(|| {
                    SandboxError::unknown_file_ref(format!(
                        "version '{version_id}' has no source file reference"
                    ))
                })?;
                let source_path = session
                    .host_file_refs
                    .get(&source_file_ref_id)
                    .map(|file_ref| file_ref.source_path.clone())
                    .ok_or_else(|| {
                        SandboxError::unknown_file_ref(format!(
                            "unknown source file reference '{source_file_ref_id}'"
                        ))
                    })?;
                (source_file_ref_id, source_path, true)
            }
            CommitTarget::ExplicitPath(target_path) => (
                target_path.to_string_lossy().to_string(),
                target_path,
                false,
            ),
        };

        (
            version,
            session.workspace_root.clone(),
            source_file_ref_id.0,
            source_file_ref_id.1,
            source_file_ref_id.2,
        )
    };

    let copy_result = copy_version_to_target(&workspace_root, &version, &target_path)?;
    mark_version_committed(
        registry,
        session_id,
        version_id,
        supersede_source_versions
            .then_some(version.source_file_ref_id.as_deref())
            .flatten(),
    )?;

    Ok(CommitResult {
        version_id: version_id.to_string(),
        source_file_ref_id,
        backup_id: copy_result.backup_id,
        old_hash: copy_result.old_hash,
        new_hash: copy_result.new_hash,
        bytes_written: copy_result.bytes_written,
        permission_mode: snapshot.mode,
        permission_version: snapshot.version,
    })
}

struct CopyResult {
    backup_id: Option<String>,
    old_hash: Option<String>,
    new_hash: String,
    bytes_written: u64,
}

fn copy_version_to_target(
    workspace_root: &Path,
    version: &FileVersion,
    target_path: &Path,
) -> SandboxResult<CopyResult> {
    if let Some(parent) = target_path.parent() {
        fs::create_dir_all(parent).map_err(|e| {
            SandboxError::unavailable(format!("failed to create target directory: {e}"))
        })?;
    }

    let (backup_id, old_hash) = if target_path.exists() {
        (
            Some(create_backup(
                workspace_root,
                &version.version_id,
                target_path,
            )?),
            Some(sha256_file(target_path)?),
        )
    } else {
        (None, None)
    };

    fs::copy(&version.sandbox_path, target_path).map_err(|e| {
        SandboxError::unavailable(format!("failed to copy sandbox file to target: {e}"))
    })?;
    let bytes_written = fs::metadata(target_path)
        .map_err(|e| SandboxError::unavailable(format!("failed to stat target file: {e}")))?
        .len();
    let new_hash = sha256_file(target_path)?;

    Ok(CopyResult {
        backup_id,
        old_hash,
        new_hash,
        bytes_written,
    })
}

fn create_backup(
    workspace_root: &Path,
    version_id: &str,
    source_path: &Path,
) -> SandboxResult<String> {
    let backups_dir = workspace_root.join("backups");
    fs::create_dir_all(&backups_dir)
        .map_err(|e| SandboxError::unavailable(format!("failed to create backups dir: {e}")))?;
    let timestamp_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| SandboxError::unavailable(format!("failed to read system time: {e}")))?
        .as_millis();
    let backup_id = format!(
        "backup-{timestamp_ms}-{}",
        sha256_hex(format!("{version_id}:{}", source_path.display()).as_bytes())
    );
    let file_name = safe_file_name(
        source_path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("file"),
    );
    let backup_path = backups_dir.join(format!("{backup_id}-{file_name}"));

    fs::copy(source_path, backup_path)
        .map_err(|e| SandboxError::unavailable(format!("failed to backup target file: {e}")))?;

    Ok(backup_id)
}

fn mark_version_committed(
    registry: &mut SandboxRegistry,
    session_id: &str,
    version_id: &str,
    source_file_ref_id: Option<&str>,
) -> SandboxResult<()> {
    let session = registry.session_mut(session_id)?;
    for version in session.file_versions.values_mut() {
        if version.version_id == version_id {
            version.status = FileVersionStatus::Committed;
        } else if source_file_ref_id.is_some()
            && version.source_file_ref_id.as_deref() == source_file_ref_id
            && version.status == FileVersionStatus::Candidate
        {
            version.status = FileVersionStatus::Superseded;
        }
    }

    Ok(())
}

fn safe_file_name(name: &str) -> String {
    Path::new(name)
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("input")
        .to_string()
}

fn sha256_file(path: &Path) -> SandboxResult<String> {
    let mut file = fs::File::open(path)
        .map_err(|e| SandboxError::unavailable(format!("failed to open file for hashing: {e}")))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|e| SandboxError::unavailable(format!("failed to hash file: {e}")))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }

    Ok(format!("{:x}", hasher.finalize()))
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::registry::SandboxRegistry;
    use crate::sandbox::types::{
        FileVersionStatus, HostFileRef, NetworkPolicy, RegisterVersionRequest,
        SandboxPermissionMode, SandboxSessionConfig,
    };
    use std::fs;

    fn registry_with_session(root: &std::path::Path) -> SandboxRegistry {
        let mut registry = SandboxRegistry::default();
        registry
            .create_session(SandboxSessionConfig {
                session_id: "session-1".to_string(),
                permission_mode: SandboxPermissionMode::Readonly,
                workspace_root: root.to_path_buf(),
                network: NetworkPolicy::Enabled,
                mounts: Vec::new(),
            })
            .unwrap();
        registry
    }

    #[test]
    fn import_file_copies_source_into_session_input() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("sandbox");
        let source = temp.path().join("source.txt");
        fs::create_dir_all(&root).unwrap();
        fs::write(&source, "hello").unwrap();
        let mut registry = registry_with_session(&root);

        let file = import_file(
            &mut registry,
            "session-1",
            HostFileRef {
                file_ref_id: "file-ref-1".to_string(),
                source_path: source.clone(),
                display_name: "source.txt".to_string(),
                size: 5,
                mtime_ms: None,
                conversation_id: None,
            },
        )
        .unwrap();

        assert!(
            file.sandbox_path
                .starts_with(root.canonicalize().unwrap().join("input"))
        );
        assert_eq!(fs::read_to_string(file.sandbox_path).unwrap(), "hello");
    }

    #[test]
    fn import_file_strips_path_components_from_display_name() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("sandbox");
        let source = temp.path().join("source.txt");
        fs::create_dir_all(&root).unwrap();
        fs::write(&source, "hello").unwrap();
        let mut registry = registry_with_session(&root);

        let file = import_file(
            &mut registry,
            "session-1",
            HostFileRef {
                file_ref_id: "file-ref-1".to_string(),
                source_path: source,
                display_name: "../source.txt".to_string(),
                size: 5,
                mtime_ms: None,
                conversation_id: None,
            },
        )
        .unwrap();

        assert!(
            file.sandbox_path
                .starts_with(root.canonicalize().unwrap().join("input"))
        );
        assert_eq!(file.sandbox_path.file_name().unwrap(), "source.txt");
    }

    #[test]
    fn register_file_version_returns_candidate_hash_and_size() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("sandbox");
        fs::create_dir_all(root.join("workspace")).unwrap();
        fs::write(root.join("workspace/out.txt"), "updated").unwrap();
        let mut registry = registry_with_session(&root);

        let version = register_file_version(
            &mut registry,
            "session-1",
            RegisterVersionRequest {
                sandbox_path: std::path::PathBuf::from("workspace/out.txt"),
                source_file_ref_id: Some("file-ref-1".to_string()),
            },
        )
        .unwrap();

        assert_eq!(version.size, 7);
        assert_eq!(version.hash.len(), 64);
        assert_eq!(version.version_id, format!("version-{}", version.hash));
        assert_eq!(version.status, FileVersionStatus::Candidate);
    }

    #[test]
    fn commit_requires_full_permission() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("sandbox");
        fs::create_dir_all(root.join("workspace")).unwrap();
        fs::write(root.join("workspace/out.txt"), "updated").unwrap();
        let mut registry = registry_with_session(&root);

        let version = register_file_version(
            &mut registry,
            "session-1",
            RegisterVersionRequest {
                sandbox_path: std::path::PathBuf::from("workspace/out.txt"),
                source_file_ref_id: Some("file-ref-1".to_string()),
            },
        )
        .unwrap();
        let err = commit_file_version(&mut registry, "session-1", &version.version_id).unwrap_err();

        assert_eq!(err.code, "PERMISSION_DENIED");
    }
}
