//! Filesystem operation handlers: stat, list, glob, mkdir, and (later) mutation ops.
//!
//! Each handler returns `Result<ResultType, FileError>`. The dispatch layer in
//! `file_manager::mod` is responsible for running policy checks *before* invoking
//! these handlers.

use std::fs::Metadata;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use ahand_protocol::{
    FileEntry, FileError, FileErrorCode, FileGlob, FileGlobResult, FileList, FileListResult,
    FileMkdir, FileMkdirResult, FileStat, FileStatResult, FileType, UnixPermission,
};

use super::file_error;

const DEFAULT_LIST_MAX: u32 = 1000;
const DEFAULT_GLOB_MAX: u32 = 1000;

/// Stat a file/directory/symlink and return its metadata.
pub async fn handle_stat(req: &FileStat, resolved: &Path) -> Result<FileStatResult, FileError> {
    let metadata = if req.no_follow_symlink {
        tokio::fs::symlink_metadata(resolved).await
    } else {
        tokio::fs::metadata(resolved).await
    }
    .map_err(|e| io_to_file_error(e, resolved))?;

    let file_type = map_file_type(&metadata);

    let symlink_target = if metadata.file_type().is_symlink() {
        tokio::fs::read_link(resolved)
            .await
            .ok()
            .map(|p| p.to_string_lossy().into_owned())
    } else {
        None
    };

    Ok(FileStatResult {
        path: resolved.to_string_lossy().into_owned(),
        file_type: file_type as i32,
        size: metadata.len(),
        modified_ms: system_time_to_ms(metadata.modified().ok()),
        created_ms: system_time_to_ms(metadata.created().ok()),
        accessed_ms: system_time_to_ms(metadata.accessed().ok()),
        unix_permission: Some(unix_permission_from_metadata(&metadata)),
        windows_acl: None,
        symlink_target,
    })
}

/// List entries in a directory, sorted by mtime desc with pagination.
pub async fn handle_list(req: &FileList, resolved: &Path) -> Result<FileListResult, FileError> {
    let metadata = tokio::fs::metadata(resolved)
        .await
        .map_err(|e| io_to_file_error(e, resolved))?;
    if !metadata.is_dir() {
        return Err(file_error(
            FileErrorCode::NotADirectory,
            &req.path,
            "path is not a directory",
        ));
    }

    let mut read_dir = tokio::fs::read_dir(resolved)
        .await
        .map_err(|e| io_to_file_error(e, resolved))?;
    let mut entries: Vec<FileEntry> = Vec::new();
    while let Some(entry) = read_dir
        .next_entry()
        .await
        .map_err(|e| io_to_file_error(e, resolved))?
    {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !req.include_hidden && name.starts_with('.') {
            continue;
        }
        let Ok(metadata) = entry.metadata().await else {
            continue;
        };
        let symlink_target = if metadata.file_type().is_symlink() {
            tokio::fs::read_link(entry.path())
                .await
                .ok()
                .map(|p| p.to_string_lossy().into_owned())
        } else {
            None
        };
        entries.push(FileEntry {
            name,
            file_type: map_file_type(&metadata) as i32,
            size: metadata.len(),
            modified_ms: system_time_to_ms(metadata.modified().ok()),
            symlink_target,
        });
    }

    // Sort by mtime desc (most recent first).
    entries.sort_by(|a, b| b.modified_ms.cmp(&a.modified_ms));

    let total = entries.len() as u32;
    let offset = req.offset.unwrap_or(0) as usize;
    let max_results = req.max_results.unwrap_or(DEFAULT_LIST_MAX) as usize;

    let end = offset.saturating_add(max_results).min(entries.len());
    let start = offset.min(entries.len());
    let paged: Vec<FileEntry> = entries[start..end].to_vec();
    let has_more = end < entries.len();

    Ok(FileListResult {
        entries: paged,
        total_count: total,
        has_more,
    })
}

/// Match glob pattern against files under the (optional) base path.
pub async fn handle_glob(
    req: &FileGlob,
    base: Option<&Path>,
) -> Result<FileGlobResult, FileError> {
    let max_results = req.max_results.unwrap_or(DEFAULT_GLOB_MAX) as usize;

    // Resolve pattern relative to base_path if provided.
    let full_pattern = match base {
        Some(b) => {
            let joined: PathBuf = b.join(&req.pattern);
            joined.to_string_lossy().into_owned()
        }
        None => req.pattern.clone(),
    };

    let glob_iter = glob::glob(&full_pattern).map_err(|e| {
        file_error(
            FileErrorCode::InvalidPath,
            &req.pattern,
            format!("invalid glob pattern: {e}"),
        )
    })?;

    let mut entries: Vec<FileEntry> = Vec::new();
    let mut total_matches: u32 = 0;
    for entry in glob_iter {
        let Ok(path) = entry else {
            continue;
        };
        total_matches = total_matches.saturating_add(1);
        if entries.len() >= max_results {
            continue;
        }
        let Ok(metadata) = tokio::fs::symlink_metadata(&path).await else {
            continue;
        };
        let symlink_target = if metadata.file_type().is_symlink() {
            tokio::fs::read_link(&path)
                .await
                .ok()
                .map(|p| p.to_string_lossy().into_owned())
        } else {
            None
        };
        entries.push(FileEntry {
            name: path.to_string_lossy().into_owned(),
            file_type: map_file_type(&metadata) as i32,
            size: metadata.len(),
            modified_ms: system_time_to_ms(metadata.modified().ok()),
            symlink_target,
        });
    }

    // Sort by mtime desc for consistency with `list`.
    entries.sort_by(|a, b| b.modified_ms.cmp(&a.modified_ms));

    let has_more = total_matches as usize > entries.len();

    Ok(FileGlobResult {
        entries,
        total_matches,
        has_more,
    })
}

/// Create a directory (and optionally parents). Respects `mode` on Unix.
pub async fn handle_mkdir(
    req: &FileMkdir,
    resolved: &Path,
) -> Result<FileMkdirResult, FileError> {
    if tokio::fs::try_exists(resolved).await.unwrap_or(false) {
        // Enforce "must be a directory" when the path exists.
        let metadata = tokio::fs::symlink_metadata(resolved)
            .await
            .map_err(|e| io_to_file_error(e, resolved))?;
        if !metadata.is_dir() {
            return Err(file_error(
                FileErrorCode::AlreadyExists,
                &req.path,
                "path exists and is not a directory",
            ));
        }
        return Ok(FileMkdirResult {
            path: resolved.to_string_lossy().into_owned(),
            already_existed: true,
        });
    }

    if req.recursive {
        tokio::fs::create_dir_all(resolved)
            .await
            .map_err(|e| io_to_file_error(e, resolved))?;
    } else {
        tokio::fs::create_dir(resolved)
            .await
            .map_err(|e| io_to_file_error(e, resolved))?;
    }

    if let Some(mode) = req.mode {
        let perms = std::fs::Permissions::from_mode(mode);
        tokio::fs::set_permissions(resolved, perms)
            .await
            .map_err(|e| io_to_file_error(e, resolved))?;
    }

    Ok(FileMkdirResult {
        path: resolved.to_string_lossy().into_owned(),
        already_existed: false,
    })
}

// ── Helpers ────────────────────────────────────────────────────────────────

pub fn map_file_type(metadata: &Metadata) -> FileType {
    let ft = metadata.file_type();
    if ft.is_dir() {
        FileType::Directory
    } else if ft.is_file() {
        FileType::File
    } else if ft.is_symlink() {
        FileType::Symlink
    } else {
        FileType::Other
    }
}

pub fn system_time_to_ms(time: Option<std::time::SystemTime>) -> u64 {
    time.and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn unix_permission_from_metadata(metadata: &Metadata) -> UnixPermission {
    UnixPermission {
        mode: Some(metadata.permissions().mode()),
        owner: None,
        group: None,
    }
}

pub fn io_to_file_error(err: io::Error, path: &Path) -> FileError {
    let path_str = path.to_string_lossy().into_owned();
    let code = match err.kind() {
        io::ErrorKind::NotFound => FileErrorCode::NotFound,
        io::ErrorKind::PermissionDenied => FileErrorCode::PermissionDenied,
        io::ErrorKind::AlreadyExists => FileErrorCode::AlreadyExists,
        _ => {
            // Map "not a directory" and "is a directory" via raw_os_error when possible.
            match err.raw_os_error() {
                Some(20) => FileErrorCode::NotADirectory, // ENOTDIR
                Some(21) => FileErrorCode::IsADirectory,  // EISDIR
                Some(39) => FileErrorCode::NotEmpty,      // ENOTEMPTY
                _ => FileErrorCode::Io,
            }
        }
    };
    FileError {
        code: code as i32,
        message: err.to_string(),
        path: path_str,
    }
}
