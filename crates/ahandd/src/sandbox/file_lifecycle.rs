use std::fs;
use std::io::Read;
use std::path::Path;

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

    Ok(FileVersion {
        version_id: format!("version-{hash}"),
        sandbox_path,
        source_file_ref_id: request.source_file_ref_id,
        size: metadata.len(),
        hash,
        status: FileVersionStatus::Candidate,
    })
}

pub fn commit_file_version(
    registry: &mut SandboxRegistry,
    session_id: &str,
    version_id: &str,
) -> SandboxResult<CommitResult> {
    let snapshot = registry.permission_snapshot(session_id)?;
    if snapshot.mode != SandboxPermissionMode::Full {
        return Err(SandboxError::permission_denied(
            "full permission is required to commit a sandbox file version",
        ));
    }
    Err(SandboxError::unavailable(format!(
        "version '{version_id}' cannot be committed until persistent version storage is wired"
    )))
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

        assert!(file.sandbox_path.starts_with(root.join("input")));
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

        assert!(file.sandbox_path.starts_with(root.join("input")));
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
