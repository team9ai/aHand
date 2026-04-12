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
    file_chmod, DeleteMode, FileChmod, FileChmodResult, FileCopy, FileCopyResult,
    FileCreateSymlink, FileCreateSymlinkResult, FileDelete, FileDeleteResult, FileEntry, FileError,
    FileErrorCode, FileGlob, FileGlobResult, FileList, FileListResult, FileMkdir, FileMkdirResult,
    FileMove, FileMoveResult, FileStat, FileStatResult, FileType, UnixPermission,
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

// ── Delete ─────────────────────────────────────────────────────────────────

pub async fn handle_delete(
    req: &FileDelete,
    resolved: &Path,
) -> Result<FileDeleteResult, FileError> {
    let metadata = if req.no_follow_symlink {
        tokio::fs::symlink_metadata(resolved).await
    } else {
        tokio::fs::metadata(resolved).await
    }
    .map_err(|e| io_to_file_error(e, resolved))?;

    let mode = DeleteMode::try_from(req.mode).unwrap_or(DeleteMode::Trash);

    match mode {
        DeleteMode::Trash => {
            let path_str = resolved.to_string_lossy().into_owned();
            tokio::task::block_in_place(|| trash::delete(resolved)).map_err(|e| {
                file_error(
                    FileErrorCode::Io,
                    &req.path,
                    format!("failed to move to trash: {e}"),
                )
            })?;
            Ok(FileDeleteResult {
                path: path_str,
                mode: DeleteMode::Trash as i32,
                items_deleted: 1,
                trash_path: None, // OS-specific — not always discoverable
            })
        }
        DeleteMode::Permanent => {
            if metadata.is_dir() {
                if !req.recursive {
                    // Check if empty.
                    let mut entries = tokio::fs::read_dir(resolved)
                        .await
                        .map_err(|e| io_to_file_error(e, resolved))?;
                    if entries
                        .next_entry()
                        .await
                        .map_err(|e| io_to_file_error(e, resolved))?
                        .is_some()
                    {
                        return Err(file_error(
                            FileErrorCode::NotEmpty,
                            &req.path,
                            "directory not empty (use recursive=true)",
                        ));
                    }
                    tokio::fs::remove_dir(resolved)
                        .await
                        .map_err(|e| io_to_file_error(e, resolved))?;
                    Ok(FileDeleteResult {
                        path: resolved.to_string_lossy().into_owned(),
                        mode: DeleteMode::Permanent as i32,
                        items_deleted: 1,
                        trash_path: None,
                    })
                } else {
                    let count = count_recursive(resolved).await;
                    tokio::fs::remove_dir_all(resolved)
                        .await
                        .map_err(|e| io_to_file_error(e, resolved))?;
                    Ok(FileDeleteResult {
                        path: resolved.to_string_lossy().into_owned(),
                        mode: DeleteMode::Permanent as i32,
                        items_deleted: count,
                        trash_path: None,
                    })
                }
            } else {
                tokio::fs::remove_file(resolved)
                    .await
                    .map_err(|e| io_to_file_error(e, resolved))?;
                Ok(FileDeleteResult {
                    path: resolved.to_string_lossy().into_owned(),
                    mode: DeleteMode::Permanent as i32,
                    items_deleted: 1,
                    trash_path: None,
                })
            }
        }
    }
}

/// Count files + directories under a path recursively (including the root).
async fn count_recursive(path: &Path) -> u32 {
    fn walk(path: &std::path::Path, count: &mut u32) {
        *count += 1;
        if let Ok(entries) = std::fs::read_dir(path) {
            for entry in entries.flatten() {
                let Ok(ft) = entry.file_type() else {
                    continue;
                };
                if ft.is_dir() {
                    walk(&entry.path(), count);
                } else {
                    *count += 1;
                }
            }
        }
    }
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut count = 0u32;
        walk(&path, &mut count);
        count
    })
    .await
    .unwrap_or(1)
}

// ── Copy / Move / Symlink ──────────────────────────────────────────────────

pub async fn handle_copy(
    req: &FileCopy,
    source_resolved: &Path,
    dest_resolved: &Path,
) -> Result<FileCopyResult, FileError> {
    let source_metadata = tokio::fs::symlink_metadata(source_resolved)
        .await
        .map_err(|e| io_to_file_error(e, source_resolved))?;

    if tokio::fs::try_exists(dest_resolved).await.unwrap_or(false) && !req.overwrite {
        return Err(file_error(
            FileErrorCode::AlreadyExists,
            &req.destination,
            "destination exists (use overwrite=true)",
        ));
    }

    let items_copied = if source_metadata.is_dir() {
        if !req.recursive {
            return Err(file_error(
                FileErrorCode::IsADirectory,
                &req.source,
                "source is a directory (use recursive=true)",
            ));
        }
        copy_dir_recursive(source_resolved, dest_resolved).await?
    } else {
        tokio::fs::copy(source_resolved, dest_resolved)
            .await
            .map_err(|e| io_to_file_error(e, dest_resolved))?;
        1
    };

    Ok(FileCopyResult {
        source: source_resolved.to_string_lossy().into_owned(),
        destination: dest_resolved.to_string_lossy().into_owned(),
        items_copied,
    })
}

async fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<u32, FileError> {
    let src = src.to_path_buf();
    let dst = dst.to_path_buf();
    tokio::task::spawn_blocking(move || -> Result<u32, FileError> {
        let mut count = 0u32;
        std::fs::create_dir_all(&dst).map_err(|e| io_to_file_error(e, &dst))?;
        copy_dir_sync(&src, &dst, &mut count)?;
        Ok(count)
    })
    .await
    .map_err(|e| {
        file_error(
            FileErrorCode::Io,
            "",
            format!("recursive copy join error: {e}"),
        )
    })?
}

fn copy_dir_sync(src: &Path, dst: &Path, count: &mut u32) -> Result<(), FileError> {
    for entry in std::fs::read_dir(src).map_err(|e| io_to_file_error(e, src))? {
        let entry = entry.map_err(|e| io_to_file_error(e, src))?;
        let ty = entry.file_type().map_err(|e| io_to_file_error(e, src))?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if ty.is_dir() {
            std::fs::create_dir_all(&to).map_err(|e| io_to_file_error(e, &to))?;
            *count += 1;
            copy_dir_sync(&from, &to, count)?;
        } else if ty.is_file() {
            std::fs::copy(&from, &to).map_err(|e| io_to_file_error(e, &to))?;
            *count += 1;
        } else if ty.is_symlink() {
            #[cfg(unix)]
            {
                let target = std::fs::read_link(&from).map_err(|e| io_to_file_error(e, &from))?;
                std::os::unix::fs::symlink(&target, &to)
                    .map_err(|e| io_to_file_error(e, &to))?;
                *count += 1;
            }
        }
    }
    Ok(())
}

pub async fn handle_move(
    req: &FileMove,
    source_resolved: &Path,
    dest_resolved: &Path,
) -> Result<FileMoveResult, FileError> {
    if tokio::fs::try_exists(dest_resolved).await.unwrap_or(false) && !req.overwrite {
        return Err(file_error(
            FileErrorCode::AlreadyExists,
            &req.destination,
            "destination exists (use overwrite=true)",
        ));
    }

    // Try rename first (fast path, same filesystem). Fall back to copy+delete
    // on cross-device renames.
    match tokio::fs::rename(source_resolved, dest_resolved).await {
        Ok(_) => {}
        Err(e) if e.raw_os_error() == Some(18) /* EXDEV */ => {
            // Cross-filesystem: copy then delete.
            let copy_req = FileCopy {
                source: req.source.clone(),
                destination: req.destination.clone(),
                recursive: true,
                overwrite: req.overwrite,
            };
            handle_copy(&copy_req, source_resolved, dest_resolved).await?;
            let meta = tokio::fs::symlink_metadata(source_resolved)
                .await
                .map_err(|e| io_to_file_error(e, source_resolved))?;
            if meta.is_dir() {
                tokio::fs::remove_dir_all(source_resolved)
                    .await
                    .map_err(|e| io_to_file_error(e, source_resolved))?;
            } else {
                tokio::fs::remove_file(source_resolved)
                    .await
                    .map_err(|e| io_to_file_error(e, source_resolved))?;
            }
        }
        Err(e) => return Err(io_to_file_error(e, source_resolved)),
    }

    Ok(FileMoveResult {
        source: source_resolved.to_string_lossy().into_owned(),
        destination: dest_resolved.to_string_lossy().into_owned(),
    })
}

pub async fn handle_create_symlink(
    req: &FileCreateSymlink,
    link_resolved: &Path,
) -> Result<FileCreateSymlinkResult, FileError> {
    #[cfg(unix)]
    {
        tokio::fs::symlink(&req.target, link_resolved)
            .await
            .map_err(|e| io_to_file_error(e, link_resolved))?;
    }
    #[cfg(not(unix))]
    {
        return Err(file_error(
            FileErrorCode::Unspecified,
            &req.link_path,
            "symlinks are not supported on this platform",
        ));
    }
    Ok(FileCreateSymlinkResult {
        link_path: link_resolved.to_string_lossy().into_owned(),
        target: req.target.clone(),
    })
}

// ── Chmod ──────────────────────────────────────────────────────────────────

pub async fn handle_chmod(
    req: &FileChmod,
    resolved: &Path,
) -> Result<FileChmodResult, FileError> {
    let Some(permission) = &req.permission else {
        return Err(file_error(
            FileErrorCode::Unspecified,
            &req.path,
            "no permission specified",
        ));
    };

    match permission {
        file_chmod::Permission::Unix(unix) => {
            if unix.owner.is_some() || unix.group.is_some() {
                return Err(file_error(
                    FileErrorCode::PermissionDenied,
                    &req.path,
                    "chown is not yet supported by this daemon",
                ));
            }
            let Some(mode) = unix.mode else {
                return Err(file_error(
                    FileErrorCode::Unspecified,
                    &req.path,
                    "unix permission mode is required",
                ));
            };
            let items = set_unix_mode(resolved, mode, req.recursive).await?;
            Ok(FileChmodResult {
                path: resolved.to_string_lossy().into_owned(),
                items_modified: items,
            })
        }
        file_chmod::Permission::Windows(_acl) => {
            #[cfg(not(windows))]
            {
                Err(file_error(
                    FileErrorCode::Unspecified,
                    &req.path,
                    "Windows ACLs are not supported on this platform",
                ))
            }
            #[cfg(windows)]
            {
                // TODO: wire up real Windows ACL setting. For now, report
                // that only mode-based chmod is implemented.
                Err(file_error(
                    FileErrorCode::Unspecified,
                    &req.path,
                    "Windows ACL chmod is not yet implemented",
                ))
            }
        }
    }
}

async fn set_unix_mode(path: &Path, mode: u32, recursive: bool) -> Result<u32, FileError> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || -> Result<u32, FileError> {
        let mut count = 0u32;
        set_unix_mode_sync(&path, mode, recursive, &mut count)?;
        Ok(count)
    })
    .await
    .map_err(|e| file_error(FileErrorCode::Io, "", format!("chmod join error: {e}")))?
}

fn set_unix_mode_sync(
    path: &Path,
    mode: u32,
    recursive: bool,
    count: &mut u32,
) -> Result<(), FileError> {
    let perms = std::fs::Permissions::from_mode(mode);
    std::fs::set_permissions(path, perms).map_err(|e| io_to_file_error(e, path))?;
    *count += 1;
    if recursive {
        let metadata = std::fs::symlink_metadata(path).map_err(|e| io_to_file_error(e, path))?;
        if metadata.is_dir() {
            for entry in std::fs::read_dir(path).map_err(|e| io_to_file_error(e, path))? {
                let entry = entry.map_err(|e| io_to_file_error(e, path))?;
                let ty = entry.file_type().map_err(|e| io_to_file_error(e, path))?;
                if ty.is_symlink() {
                    continue; // Don't follow symlinks.
                }
                set_unix_mode_sync(&entry.path(), mode, recursive, count)?;
            }
        }
    }
    Ok(())
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
