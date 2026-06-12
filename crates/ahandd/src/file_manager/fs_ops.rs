//! Filesystem operation handlers: stat, list, glob, mkdir, and (later) mutation ops.
//!
//! Each handler returns `Result<ResultType, FileError>`. The dispatch layer in
//! `file_manager::mod` is responsible for running policy checks *before* invoking
//! these handlers.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::fs::Metadata;
use std::io;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use ahand_protocol::{
    DeleteMode, FileChmod, FileChmodResult, FileCopy, FileCopyResult, FileCreateSymlink,
    FileCreateSymlinkResult, FileDelete, FileDeleteResult, FileEntry, FileError, FileErrorCode,
    FileGlob, FileGlobResult, FileList, FileListResult, FileMkdir, FileMkdirResult, FileMove,
    FileMoveResult, FileStat, FileStatResult, FileType, UnixPermission, file_chmod,
};
// AclEntry and AclEntryType are used in cfg(windows) and cfg(any(windows,test)) code paths.
#[cfg(any(windows, test))]
use ahand_protocol::{AclEntry, AclEntryType};

use super::file_error;

const DEFAULT_LIST_MAX: u32 = 1000;
const DEFAULT_GLOB_MAX: u32 = 1000;

/// Safety margin on top of `offset + max_results` retained in the list heap.
/// Small extra room so near-tie mtimes don't break pagination boundaries.
const LIST_HEAP_SAFETY_MARGIN: usize = 64;
/// Absolute ceiling on retained entries. Prevents a pathological
/// `max_results = u32::MAX` request from pre-allocating all of memory.
const LIST_HEAP_RETAIN_CAP: usize = 100_000;

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
///
/// Security / DoS properties:
/// - Entry metadata is fetched via `symlink_metadata` so a symlink inside
///   `resolved` never leaks its target's type, size, or mtime into the listing.
///   The listing reports the symlink itself (type = Symlink) plus the target
///   path string via `read_link`.
/// - Entries are streamed into a bounded min-heap keyed by mtime. Only
///   `offset + max_results + margin` entries are retained in memory, so a
///   pathologically large directory cannot blow the heap. `total_count`
///   still reflects every non-hidden entry scanned so pagination metadata
///   (`has_more`) stays correct.
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

    let max_results = req.max_results.unwrap_or(DEFAULT_LIST_MAX) as usize;
    let offset = req.offset.unwrap_or(0) as usize;
    // C4: refuse pagination requests we structurally cannot answer
    // correctly. The bounded heap retains at most LIST_HEAP_RETAIN_CAP
    // entries, so anything past that window can't be served accurately —
    // we'd silently return an empty page with `has_more = true`, leaving
    // the caller to spin forever. Return InvalidArgument so the client
    // can surface a proper error instead.
    if offset.saturating_add(max_results) > LIST_HEAP_RETAIN_CAP {
        return Err(file_error(
            FileErrorCode::InvalidPath,
            &req.path,
            format!(
                "offset+max_results exceeds the directory listing window of {} entries; \
                 narrow the listing or use FileGlob for deep pagination",
                LIST_HEAP_RETAIN_CAP
            ),
        ));
    }
    // We need to retain offset+max_results entries to serve this page, plus a
    // small safety margin so near-tie mtimes at the boundary don't drop a
    // valid entry. Capped at LIST_HEAP_RETAIN_CAP regardless of request.
    let retain = offset
        .saturating_add(max_results)
        .saturating_add(LIST_HEAP_SAFETY_MARGIN)
        .min(LIST_HEAP_RETAIN_CAP);
    // Pre-allocating the heap to `retain` is fine in the common case, but we
    // still cap initial capacity so a huge `max_results` doesn't allocate
    // eagerly before we've even seen one entry.
    let initial_capacity = retain.min(1024);

    // Min-heap (via `Reverse`) keyed by (mtime, name). The smallest mtime sits
    // at the top; when the heap is full we peek the top and only push newer
    // entries (evicting the oldest). The `name` tiebreaker keeps eviction
    // deterministic when mtimes collide.
    //
    // `FileEntry` is a protobuf type and does not implement `Ord`, so the
    // payload is stored in a parallel `Vec` and the heap only carries the
    // comparable key plus an index into that vec. When the heap evicts an
    // entry we "tombstone" its slot (`None`) rather than paying to compact
    // the vec, then filter out tombstones when draining at the end.
    let mut heap: BinaryHeap<Reverse<(u64, String, usize)>> =
        BinaryHeap::with_capacity(initial_capacity);
    let mut payload: Vec<Option<FileEntry>> = Vec::with_capacity(initial_capacity);
    let mut total_count: u32 = 0;

    let mut read_dir = tokio::fs::read_dir(resolved)
        .await
        .map_err(|e| io_to_file_error(e, resolved))?;
    while let Some(entry) = read_dir
        .next_entry()
        .await
        .map_err(|e| io_to_file_error(e, resolved))?
    {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !req.include_hidden && name.starts_with('.') {
            continue;
        }
        // symlink_metadata never follows the entry's symlink, so a symlink
        // pointing at /etc/passwd reports the link itself, not the target.
        // tokio's `DirEntry` doesn't expose symlink_metadata directly, so we
        // call it on the full path.
        let Ok(metadata) = tokio::fs::symlink_metadata(entry.path()).await else {
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

        // Count every non-hidden entry we successfully stat'd, even if it
        // later gets evicted from the heap. This keeps `has_more`/`total_count`
        // accurate for pagination.
        total_count = total_count.saturating_add(1);

        let mtime = system_time_to_ms(metadata.modified().ok());
        let file_entry = FileEntry {
            name: name.clone(),
            file_type: map_file_type(&metadata) as i32,
            size: metadata.len(),
            modified_ms: mtime,
            symlink_target,
        };

        if heap.len() < retain {
            let idx = payload.len();
            payload.push(Some(file_entry));
            heap.push(Reverse((mtime, name, idx)));
        } else if let Some(Reverse((top_mtime, top_name, _))) = heap.peek() {
            // Evict the oldest if this entry is newer. Ties are broken by
            // name so eviction is deterministic.
            if (mtime, &name) > (*top_mtime, top_name) {
                if let Some(Reverse((_, _, evict_idx))) = heap.pop() {
                    payload[evict_idx] = None;
                }
                let idx = payload.len();
                payload.push(Some(file_entry));
                heap.push(Reverse((mtime, name, idx)));
            }
        }
    }

    // Drain the heap and sort by mtime desc (tiebreaker: name asc) for a
    // stable, user-facing order. Evicted slots are tombstoned `None`; we
    // walk the payload vec in index order (which is heap insertion order)
    // and filter those out before sorting.
    let mut sorted: Vec<FileEntry> = payload.into_iter().flatten().collect();
    sorted.sort_by(|a, b| {
        b.modified_ms
            .cmp(&a.modified_ms)
            .then_with(|| a.name.cmp(&b.name))
    });

    let total = total_count;
    // Pagination. Note: `sorted` only holds up to `retain` entries, so `offset`
    // beyond that clamps to the end of the retained window. `has_more` is
    // computed against the full scanned count so callers know more pages exist.
    let start = offset.min(sorted.len());
    let end = offset.saturating_add(max_results).min(sorted.len());
    let paged: Vec<FileEntry> = sorted[start..end].to_vec();
    let has_more = offset.saturating_add(max_results) < total as usize;

    Ok(FileListResult {
        entries: paged,
        total_count: total,
        has_more,
    })
}

/// Match glob pattern against files under the (optional) base path.
///
/// Every matched path is re-checked against `policy` before being returned —
/// without this filter, a `**` pattern rooted inside the allowlist could still
/// surface symlinks or follow resolution paths whose canonical target lies
/// outside the allowlist. The dispatcher also rejects obviously hostile
/// patterns (absolute paths, `..` components) before calling us.
pub async fn handle_glob(
    req: &FileGlob,
    base: Option<&Path>,
    policy: &super::policy::FilePolicyChecker,
) -> Result<FileGlobResult, FileError> {
    let max_results = req.max_results.unwrap_or(DEFAULT_GLOB_MAX) as usize;

    // Resolve pattern relative to base_path if provided. `Path::join` with an
    // absolute `req.pattern` would discard the base entirely, so absolute
    // patterns are rejected in the dispatcher before we get here.
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
        // Re-check every matched path against policy. Paths that resolve
        // outside the allowlist (via symlinks inside an allowed directory,
        // for example) are silently excluded — we neither leak metadata for
        // them nor error out, because the caller asked for a pattern match,
        // not a specific file.
        let path_str = path.to_string_lossy();
        if policy.check_path(&path_str, false, false).is_err() {
            continue;
        }
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
    entries.sort_by_key(|b| Reverse(b.modified_ms));

    let has_more = total_matches as usize > entries.len();

    Ok(FileGlobResult {
        entries,
        total_matches,
        has_more,
    })
}

/// Create a directory (and optionally parents). Respects `mode` on Unix.
///
/// On Unix, the actual `mkdirat` (or chained mkdirat for `recursive=true`)
/// is routed through [`super::io_safe`] so the kernel resolves the parent
/// chain through dirfds the policy has just validated, rather than
/// re-walking the path string. That closes the R10 TOCTOU window where an
/// attacker could swap an ancestor for a symlink between
/// `policy.check_path` and the syscall and redirect the new directory
/// outside the allowlist.
///
/// On Windows there is no equivalent API; the syscall stays path-based and
/// the race window remains. Daemon deployments on Windows assume a
/// single-tenant host where this attacker class is out of model. The
/// existing post-create verification still catches the case after the
/// fact.
pub async fn handle_mkdir(req: &FileMkdir, resolved: &Path) -> Result<FileMkdirResult, FileError> {
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

    #[cfg(unix)]
    {
        let resolved_owned = resolved.to_path_buf();
        let mode = req.mode;
        let recursive = req.recursive;
        tokio::task::spawn_blocking(move || -> Result<(), FileError> {
            // mkdirat applies umask, so we always pass the protocol mode
            // (or 0o755 default) to the create call AND then explicitly
            // re-chmod via fchmodat when the request specifies a mode —
            // matching the legacy "create_dir + set_permissions" sequence.
            let create_mode = mode.unwrap_or(0o755);
            if recursive {
                super::io_safe::safe_mkdirp(&resolved_owned, create_mode)
                    .map_err(|e| super::io_safe::io_to_file_error(e, &resolved_owned))?;
            } else {
                let handle = super::io_safe::safe_open_parent_dirfd_for(&resolved_owned)?;
                super::io_safe::mkdirat(&handle.fd, &handle.basename, create_mode)
                    .map_err(|e| super::io_safe::io_to_file_error(e, &resolved_owned))?;
            }
            if let Some(explicit_mode) = mode {
                // Re-open the parent dirfd safely (still race-proof) and
                // chmod the leaf. We don't reuse the fd from above because
                // for recursive=true we never held one to the parent.
                let handle = super::io_safe::safe_open_parent_dirfd_for(&resolved_owned)?;
                super::io_safe::fchmodat(&handle.fd, &handle.basename, explicit_mode)
                    .map_err(|e| super::io_safe::io_to_file_error(e, &resolved_owned))?;
            }
            Ok(())
        })
        .await
        .map_err(|e| {
            file_error(
                FileErrorCode::Io,
                &req.path,
                format!("mkdir join error: {e}"),
            )
        })??;
    }
    #[cfg(not(unix))]
    {
        if req.recursive {
            tokio::fs::create_dir_all(resolved)
                .await
                .map_err(|e| io_to_file_error(e, resolved))?;
        } else {
            tokio::fs::create_dir(resolved)
                .await
                .map_err(|e| io_to_file_error(e, resolved))?;
        }
        // On non-Unix platforms, `mode` is advisory and silently ignored.
        let _ = req.mode;
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
            // C5: TRASH was previously a "soft" delete that ignored the
            // `recursive` flag entirely — sending a TRASH delete on a
            // non-empty directory would silently move the whole tree
            // even when `recursive = false`. PERMANENT delete enforces
            // this guard; TRASH must too. Otherwise a caller checking
            // "is this a single-file delete?" via `recursive=false`
            // can't trust the call won't take a whole subtree.
            let mut items_deleted: u32 = 1;
            if metadata.is_dir() {
                let count = count_recursive(resolved).await;
                if !req.recursive && count > 1 {
                    return Err(file_error(
                        FileErrorCode::NotEmpty,
                        &req.path,
                        "directory not empty (use recursive=true)",
                    ));
                }
                items_deleted = count;
            }

            let path_str = resolved.to_string_lossy().into_owned();
            tokio::task::block_in_place(|| trash::delete(resolved)).map_err(|e| {
                file_error(
                    FileErrorCode::Io,
                    &req.path,
                    format!("failed to move to trash: {e}"),
                )
            })?;
            // The `trash` crate (5.2.x) does not expose the post-delete
            // destination: on macOS the `resultingItemURL` out-param from
            // `NSFileManager::trashItemAtURL` is discarded, the Finder
            // AppleScript path returns nothing, and the `os_limited` module
            // (which can `list()` trash contents on freedesktop/Windows) is
            // not compiled on macOS. So we fall back to a best-effort guess
            // of the home trash location. This is a hint only — the trash
            // system may rename the item to resolve name collisions, and on
            // non-home volumes the real location is a per-volume
            // `/Volumes/.../.Trashes/<uid>/` directory we don't try to
            // detect. On unsupported platforms (Windows, other Unixes) we
            // return `None` so callers can detect "unknown" rather than
            // relying on a fabricated path.
            let trash_path = guess_trash_path(resolved);
            Ok(FileDeleteResult {
                path: path_str,
                mode: DeleteMode::Trash as i32,
                items_deleted,
                trash_path,
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

/// Best-effort guess of where the home trash places an item after a
/// `trash::delete` call.
///
/// The `trash` crate (5.2.x) does not return the destination path on any
/// platform — see the comment in [`handle_delete`]'s TRASH branch for
/// details. This helper reproduces the Freedesktop/macOS "home trash"
/// location so the `FileDeleteResult.trash_path` field can carry a
/// human-useful hint instead of always being `None`.
///
/// **This is a hint, not a guarantee.** Concretely:
/// - The trash system may rename the item when another file with the same
///   basename already exists (e.g. macOS appends " 2", freedesktop appends
///   numeric suffixes to the `.trashinfo` file).
/// - On macOS, items deleted from non-boot volumes are placed in a
///   per-volume `/Volumes/<vol>/.Trashes/<uid>/` directory rather than
///   `~/.Trash`. We do not try to detect this.
/// - On Linux, items on a separate mount point land in that mount's
///   `.Trash-<uid>/files/` directory, not the home trash.
///
/// Returns `None` on platforms where no stable user-visible path exists
/// (currently Windows and any Unix other than macOS / freedesktop-compatible
/// Linux) or when the required environment variables (`HOME`,
/// `XDG_DATA_HOME`) are unset.
// The `return` statements inside cfg-gated blocks are load-bearing: each
// platform-specific block must use explicit `return` rather than a tail
// expression so the block itself doesn't become a statement whose value gets
// silently dropped when the subsequent cfg blocks are invisible to the
// compiler. The alternative would be nesting all platforms in a single
// `cfg_if!` macro (which isn't a direct dependency).
#[allow(clippy::needless_return)]
pub fn guess_trash_path(original: &Path) -> Option<String> {
    let basename = original.file_name()?;

    #[cfg(target_os = "macos")]
    {
        // Follows the default `DeleteMethod::Finder` path: moves into the
        // user's home trash at `~/.Trash`.
        let home = std::env::var_os("HOME")?;
        if home.is_empty() {
            return None;
        }
        let mut p = PathBuf::from(home);
        p.push(".Trash");
        p.push(basename);
        return Some(p.to_string_lossy().into_owned());
    }

    #[cfg(all(
        unix,
        not(target_os = "macos"),
        not(target_os = "ios"),
        not(target_os = "android")
    ))]
    {
        // Freedesktop Trash spec 1.0: the "home trash" is
        // `$XDG_DATA_HOME/Trash` (default `$HOME/.local/share/Trash`), and
        // deleted payloads live under its `files/` subdirectory.
        let trash_root = if let Some(data_home) = std::env::var_os("XDG_DATA_HOME") {
            if data_home.is_empty() {
                None
            } else {
                let mut p = PathBuf::from(data_home);
                p.push("Trash");
                Some(p)
            }
        } else {
            None
        };
        let trash_root = trash_root.or_else(|| {
            let home = std::env::var_os("HOME")?;
            if home.is_empty() {
                return None;
            }
            let mut p = PathBuf::from(home);
            p.push(".local/share/Trash");
            Some(p)
        })?;
        let mut p = trash_root;
        p.push("files");
        p.push(basename);
        return Some(p.to_string_lossy().into_owned());
    }

    #[cfg(not(any(
        target_os = "macos",
        all(
            unix,
            not(target_os = "macos"),
            not(target_os = "ios"),
            not(target_os = "android")
        )
    )))]
    {
        // Windows Recycle Bin does not expose a stable user-visible path;
        // other platforms have no standard home trash location.
        let _ = basename;
        None
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

/// Copy from `source_resolved` to `dest_resolved`. Files and directories
/// (with `req.recursive`) are both supported.
///
/// On Unix the **outermost** copy syscall — the file write for a single-file
/// copy, or the destination top-level mkdir for a recursive copy — runs
/// through dirfds opened safely from each side's parent (see
/// [`super::io_safe`]). That closes the R10 TOCTOU window where an attacker
/// could swap an ancestor of source or destination for a symlink between
/// `policy.check_path` and the syscall.
///
/// **Recursive copy residual:** the inner walk inside the destination
/// subtree remains path-based — every `entry.path()` op re-resolves the
/// path. After the safe top-level mkdirat the destination leaf inode is
/// pinned at the validated location, so an attacker would need to swap a
/// **subdirectory** of the leaf (which we just created or about to create)
/// during a microsecond window to redirect a sub-write. This is bounded
/// to attacker-writable subtrees and is documented (rather than fully
/// eliminated) in this round; the cost of an fd-based recursive walker
/// using `fdopendir` + `*at` for every nested entry was judged
/// disproportionate next to the `verify_post_create` belt-and-suspenders
/// already in place. See `io_safe::safe_open_parent_dirfd_for` for the
/// part of the race that **is** closed.
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
        copy_single_file(source_resolved, dest_resolved, req.overwrite).await?;
        1
    };

    Ok(FileCopyResult {
        source: source_resolved.to_string_lossy().into_owned(),
        destination: dest_resolved.to_string_lossy().into_owned(),
        items_copied,
    })
}

/// Single-file copy. On Unix this is fully fd-based — both source and
/// destination flow through safely-opened parent dirfds, then the bytes
/// move through `OwnedFd`-wrapped `std::fs::File` handles. On Windows it
/// falls back to `tokio::fs::copy` (race-prone, daemon assumes
/// single-tenant host).
async fn copy_single_file(source: &Path, dest: &Path, overwrite: bool) -> Result<(), FileError> {
    #[cfg(unix)]
    {
        let source = source.to_path_buf();
        let dest = dest.to_path_buf();
        tokio::task::spawn_blocking(move || -> Result<(), FileError> {
            // Open source parent + leaf with NOFOLLOW. NOFOLLOW on the
            // leaf protects against a symlink-leaf swap; the parent walk
            // protects against ancestor swaps. If source is itself a
            // symlink the legacy `tokio::fs::copy` would have followed
            // it; we deliberately reject here, because a symlink leaf
            // means the policy validated the link but the read would
            // have landed on the target — exactly the bug class this
            // PR addresses. Callers who want symlink-following copy
            // should resolve the source themselves first.
            let src_handle = super::io_safe::safe_open_parent_dirfd_for(&source)?;
            let src_fd = super::io_safe::openat_read_nofollow(&src_handle.fd, &src_handle.basename)
                .map_err(|e| super::io_safe::io_to_file_error(e, &source))?;
            let dst_handle = super::io_safe::safe_open_parent_dirfd_for(&dest)?;
            // truncate when overwriting; exclusive when not (so a race
            // that creates dest between try_exists and openat surfaces
            // as AlreadyExists rather than silently overwriting).
            let dst_fd = super::io_safe::openat_create_write(
                &dst_handle.fd,
                &dst_handle.basename,
                overwrite,
                !overwrite,
                0o644,
            )
            .map_err(|e| super::io_safe::io_to_file_error(e, &dest))?;

            // Move bytes. std::fs::File::from(OwnedFd) is a zero-cost
            // conversion; std::io::copy then chooses the best kernel
            // copy primitive (sendfile / copy_file_range on Linux).
            let mut src_file = std::fs::File::from(src_fd);
            let mut dst_file = std::fs::File::from(dst_fd);
            std::io::copy(&mut src_file, &mut dst_file).map_err(|e| io_to_file_error(e, &dest))?;
            Ok(())
        })
        .await
        .map_err(|e| file_error(FileErrorCode::Io, "", format!("copy join error: {e}")))??;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = overwrite; // on Windows the caller checks existence before calling us
        tokio::fs::copy(source, dest)
            .await
            .map_err(|e| io_to_file_error(e, dest))?;
        Ok(())
    }
}

async fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<u32, FileError> {
    let src = src.to_path_buf();
    let dst = dst.to_path_buf();
    tokio::task::spawn_blocking(move || -> Result<u32, FileError> {
        let mut count = 0u32;
        // Create the destination chain through `safe_mkdirp` on Unix —
        // each component is opened with O_NOFOLLOW so an attacker cannot
        // redirect the top-level dest creation by swapping an ancestor
        // for a symlink during the race window. EEXIST on the leaf is
        // tolerated (overwrite case).
        //
        // The inner `copy_dir_sync` walk is path-based; see the
        // residual-race note in `handle_copy`'s docstring. Bounded to
        // the validated subtree and covered by `verify_post_create`'s
        // RemoveTreeAll cleanup.
        #[cfg(unix)]
        {
            super::io_safe::safe_mkdirp(&dst, 0o755)
                .map_err(|e| super::io_safe::io_to_file_error(e, &dst))?;
        }
        #[cfg(not(unix))]
        {
            std::fs::create_dir_all(&dst).map_err(|e| io_to_file_error(e, &dst))?;
        }
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
                std::os::unix::fs::symlink(&target, &to).map_err(|e| io_to_file_error(e, &to))?;
                *count += 1;
            }
        }
    }
    Ok(())
}

/// Move (`rename(2)`) source onto destination. Same-filesystem rename is
/// the fast path; cross-fs `EXDEV` falls back to `cross_device_move_fallback`
/// which copies-then-removes.
///
/// On Unix we route the rename through [`super::io_safe::renameat`] using
/// dirfds opened safely from each side's parent. That closes the R10
/// TOCTOU window where an attacker could swap an ancestor of either path
/// for a symlink and redirect the rename outside the allowlist. The
/// cross-device fallback path stays path-based — it has its own race
/// window, but a) it runs only when a same-fs rename returned EXDEV, so
/// the source is still in place (no data-loss compounding), and b) the
/// existing post-create verifier already covers it.
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

    // Same-fs rename via renameat through safely-opened parent dirfds
    // (Unix), or path-based tokio rename on non-Unix. The "outcome"
    // enum keeps the safe-open failure path (PolicyDenied — the R10
    // case we care about) distinct from the rename's own io::Error
    // (which the caller inspects for EXDEV → cross-device fallback).
    // Wrapping safe-open errors in io::Error and unpacking them later
    // would lose the policy-denied code.
    match rename_safely(source_resolved, dest_resolved).await? {
        RenameOutcome::Done => {}
        RenameOutcome::CrossDevice => {
            tracing::info!(
                source = %source_resolved.display(),
                destination = %dest_resolved.display(),
                "rename hit cross-device error; falling back to copy+delete"
            );
            cross_device_move_fallback(req, source_resolved, dest_resolved).await?;
        }
    }

    Ok(FileMoveResult {
        source: source_resolved.to_string_lossy().into_owned(),
        destination: dest_resolved.to_string_lossy().into_owned(),
    })
}

/// Result of the same-fs rename attempt. EXDEV is split out from
/// `Done`/error so the caller can opt into the cross-device fallback
/// without re-deriving the EXDEV detection.
enum RenameOutcome {
    Done,
    CrossDevice,
}

/// Same-filesystem rename, dirfd-routed on Unix. Failures from the
/// safe-open layer propagate as [`FileError`] (carrying their original
/// code — typically `PolicyDenied`); rename's own non-EXDEV io errors
/// are mapped via [`io_to_file_error`].
async fn rename_safely(source: &Path, dest: &Path) -> Result<RenameOutcome, FileError> {
    #[cfg(unix)]
    {
        let source = source.to_path_buf();
        let dest = dest.to_path_buf();
        tokio::task::spawn_blocking(move || -> Result<RenameOutcome, FileError> {
            let src_handle = super::io_safe::safe_open_parent_dirfd_for(&source)?;
            let dst_handle = super::io_safe::safe_open_parent_dirfd_for(&dest)?;
            match super::io_safe::renameat(
                &src_handle.fd,
                &src_handle.basename,
                &dst_handle.fd,
                &dst_handle.basename,
            ) {
                Ok(()) => Ok(RenameOutcome::Done),
                Err(e) if is_cross_device_error(&e) => Ok(RenameOutcome::CrossDevice),
                Err(e) => Err(io_to_file_error(e, &source)),
            }
        })
        .await
        .map_err(|e| file_error(FileErrorCode::Io, "", format!("rename join error: {e}")))?
    }
    #[cfg(not(unix))]
    {
        match tokio::fs::rename(source, dest).await {
            Ok(()) => Ok(RenameOutcome::Done),
            Err(e) if is_cross_device_error(&e) => Ok(RenameOutcome::CrossDevice),
            Err(e) => Err(io_to_file_error(e, source)),
        }
    }
}

/// Cross-filesystem move payload. Extracted from `handle_move` so the
/// fallback can be unit-tested directly: simulating the EXDEV trigger
/// (`tokio::fs::rename` returning `CrossesDevices`) without a real
/// multi-FS test environment requires either platform-specific tricks
/// or runtime injection that's not worth the harness cost. Splitting
/// the trigger from the payload lets us test each independently:
/// `is_cross_device_error` covers detection, this helper covers the
/// copy+delete sequence.
async fn cross_device_move_fallback(
    req: &FileMove,
    source_resolved: &Path,
    dest_resolved: &Path,
) -> Result<(), FileError> {
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
    Ok(())
}

/// I6: detect "cross-device" rename errors portably.
///
/// The previous check tested `raw_os_error() == Some(18)`, which is EXDEV on
/// Linux/macOS but does NOT match Windows' `ERROR_NOT_SAME_DEVICE = 17`. On
/// Windows, a cross-volume `MoveFile` would therefore skip the copy+delete
/// fallback and surface as a generic IO error. We now prefer the stable
/// `io::ErrorKind::CrossesDevices` (Rust 1.85+, mapped per platform by std)
/// and keep the numeric checks as a defensive fallback for libc/Windows
/// codes that std might not classify.
fn is_cross_device_error(e: &io::Error) -> bool {
    if e.kind() == io::ErrorKind::CrossesDevices {
        return true;
    }
    match e.raw_os_error() {
        // Unix: EXDEV (Linux, macOS, BSDs) is 18.
        #[cfg(unix)]
        Some(18) => true,
        // Windows: ERROR_NOT_SAME_DEVICE = 17 (winerror.h). Distinct from
        // the Unix EXDEV value despite the proximity.
        #[cfg(windows)]
        Some(17) => true,
        _ => false,
    }
}

/// Create a symlink at `link_resolved` pointing at `req.target`.
///
/// On Unix the actual `symlinkat` runs through [`super::io_safe`] using a
/// dirfd opened safely for the link's parent — the kernel does not re-walk
/// the link path during the syscall, closing the R10 TOCTOU window. The
/// target string is stored verbatim; whether it resolves to an
/// allowlisted path is the dispatch layer's concern (see
/// `policy.check_path` for the link's parent + `R2` for the target).
// Explicit `return` in each cfg arm is load-bearing (same pattern as
// `guess_trash_path` above — see its comment for the full explanation).
#[allow(clippy::needless_return)]
pub async fn handle_create_symlink(
    req: &FileCreateSymlink,
    link_resolved: &Path,
) -> Result<FileCreateSymlinkResult, FileError> {
    #[cfg(unix)]
    {
        let link = link_resolved.to_path_buf();
        let target = req.target.clone();
        tokio::task::spawn_blocking(move || -> Result<(), FileError> {
            let handle = super::io_safe::safe_open_parent_dirfd_for(&link)?;
            super::io_safe::symlinkat(std::ffi::OsStr::new(&target), &handle.fd, &handle.basename)
                .map_err(|e| super::io_safe::io_to_file_error(e, &link))?;
            Ok(())
        })
        .await
        .map_err(|e| {
            file_error(
                FileErrorCode::Io,
                &req.link_path,
                format!("symlink join error: {e}"),
            )
        })??;
    }
    // The explicit `return` in the unix arm is load-bearing: each cfg block
    // appears as a statement to the compiler on the other platform (the
    // cfg-eliminated block leaves a `()` hole), so a plain tail expression
    // would yield `()` instead of the declared return type. `return` forces
    // the function to return from inside the block on the matching platform.
    #[cfg(unix)]
    {
        return Ok(FileCreateSymlinkResult {
            link_path: link_resolved.to_string_lossy().into_owned(),
            target: req.target.clone(),
        });
    }

    // Windows arm: use std::os::windows::fs::{symlink_file, symlink_dir}.
    // Choice of which to call is made by inspecting the resolved target:
    //   - target EXISTS and IS a directory → symlink_dir
    //   - otherwise (file, dangling/nonexistent, or any other type) → symlink_file
    //     (a dangling target defaults to file-symlink, matching the behaviour of
    //     mklink, ln, and most tooling on Windows)
    // Requires Developer Mode or elevation; ERROR_PRIVILEGE_NOT_HELD (1314) is
    // mapped to a human-readable error with remediation advice via map_symlink_error.
    #[cfg(windows)]
    {
        use std::os::windows::fs as win_fs;

        let link = link_resolved.to_path_buf();
        let target = req.target.clone();
        let link_path_str = req.link_path.clone();

        tokio::task::spawn_blocking(move || -> Result<(), FileError> {
            // Determine whether the target is an existing directory so we can
            // choose symlink_dir vs symlink_file.  We use std::fs::metadata
            // (which follows symlinks) so that a symlink-to-dir target gives
            // us symlink_dir, matching how the OS resolves it.  If stat fails
            // (e.g. dangling target), we fall back to symlink_file.
            let target_is_dir = std::fs::metadata(&target)
                .map(|m| m.is_dir())
                .unwrap_or(false);

            let result = if target_is_dir {
                win_fs::symlink_dir(&target, &link)
            } else {
                win_fs::symlink_file(&target, &link)
            };

            result.map_err(|e| map_symlink_error(&e, &link_path_str))
        })
        .await
        .map_err(|e| {
            file_error(
                FileErrorCode::Io,
                &req.link_path,
                format!("symlink join error: {e}"),
            )
        })??;
    }
    #[cfg(windows)]
    {
        return Ok(FileCreateSymlinkResult {
            link_path: link_resolved.to_string_lossy().into_owned(),
            target: req.target.clone(),
        });
    }

    // Non-Windows, non-Unix fallback — symlinks not yet supported.
    #[cfg(not(any(unix, windows)))]
    {
        let _ = link_resolved;
        return Err(file_error(
            FileErrorCode::Unspecified,
            &req.link_path,
            "symlinks are not supported on this platform",
        ));
    }
}

/// Map a symlink creation I/O error to a `FileError`, with special handling for
/// ERROR_PRIVILEGE_NOT_HELD (Windows raw OS error 1314).
///
/// On Windows, creating symlinks requires either Developer Mode to be enabled
/// or the process to be elevated (run as Administrator). When this privilege is
/// absent, the OS returns error code 1314. This function surfaces that condition
/// with a clear, actionable message.
///
/// This is a pure function over a `std::io::Error` reference so it can be unit-tested
/// without actually triggering a privilege error on the target machine.  The `link_path`
/// string is embedded in the returned `FileError::path` field for diagnostics.
///
/// Compiled on Windows (production use) and on all platforms under `cfg(test)`
/// so that unit tests can feed synthetic errors without triggering the real
/// privilege check.
#[cfg(any(windows, test))]
pub fn map_symlink_error(err: &std::io::Error, link_path: &str) -> FileError {
    // ERROR_PRIVILEGE_NOT_HELD = 1314 (0x522)
    if err.raw_os_error() == Some(1314) {
        return file_error(
            FileErrorCode::PermissionDenied,
            link_path,
            "creating symlinks requires Developer Mode or elevation: \
             enable Developer Mode in Windows Settings → System → For Developers, \
             or re-run as Administrator",
        );
    }
    // All other errors: map using the same logic as io_to_file_error but taking
    // a reference (io_to_file_error consumes the error; we only have a reference here).
    let code = match err.kind() {
        io::ErrorKind::NotFound => FileErrorCode::NotFound,
        io::ErrorKind::PermissionDenied => FileErrorCode::PermissionDenied,
        io::ErrorKind::AlreadyExists => FileErrorCode::AlreadyExists,
        _ => FileErrorCode::Io,
    };
    file_error(code, link_path, err.to_string())
}

// ── Chmod ──────────────────────────────────────────────────────────────────

pub async fn handle_chmod(req: &FileChmod, resolved: &Path) -> Result<FileChmodResult, FileError> {
    let Some(permission) = &req.permission else {
        return Err(file_error(
            FileErrorCode::Unspecified,
            &req.path,
            "no permission specified",
        ));
    };

    super::reject_if_final_component_is_symlink(resolved, &req.path, req.no_follow_symlink).await?;

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
            #[cfg(unix)]
            {
                let items = set_unix_mode(resolved, mode, req.recursive).await?;
                Ok(FileChmodResult {
                    path: resolved.to_string_lossy().into_owned(),
                    items_modified: items,
                })
            }
            #[cfg(not(unix))]
            {
                let _ = (mode, req.recursive);
                Err(file_error(
                    FileErrorCode::Unspecified,
                    &req.path,
                    "Unix mode chmod not supported on this platform",
                ))
            }
        }
        file_chmod::Permission::Windows(acl) => {
            #[cfg(not(windows))]
            {
                let _ = acl;
                Err(file_error(
                    FileErrorCode::Unspecified,
                    &req.path,
                    "Windows ACLs are not supported on this platform",
                ))
            }
            #[cfg(windows)]
            {
                // Apply Windows ACLs via the Win32 security API
                // (`SetNamedSecurityInfoW`) using a PROTECTED DACL — a TRUE full
                // DACL replacement.
                //
                // Why not icacls (the original M4 approach): icacls cannot
                // achieve a true DACL replacement. `/inheritance:r` removes only
                // INHERITED ACEs, and `/grant:r` only replaces the *same*
                // principal's grant — any PRE-EXISTING EXPLICIT ACE on the file
                // (e.g. an attacker-set `Everyone:F`) SURVIVES. So an
                // "owner-only" WindowsAcl was not actually owner-only
                // (SECURITY-HIGH#1, Codex review). icacls has no way to
                // enumerate+remove unknown explicit ACEs.
                //
                // Fix: build the COMPLETE DACL from the supplied entries as an
                // SDDL string (`D:P(...)...`), convert it to a SECURITY_DESCRIPTOR,
                // extract its DACL, and call `SetNamedSecurityInfoW` with
                // `DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION`.
                // PROTECTED strips inheritance AND, because we supply the entire
                // DACL, the OS replaces ALL explicit ACEs — so the file's
                // effective DACL becomes EXACTLY the supplied entries. This is
                // the true replacement icacls could not provide.
                //
                // Semantics: `WindowsAcl` is a FULL DACL REPLACEMENT — the
                // file's effective DACL becomes EXACTLY the supplied entries (a
                // set/replace, matching chmod). "owner-only" (a single `owner:F`
                // allow) really yields owner-only; a DENY is meaningful because
                // no inherited/pre-existing broad ALLOW survives. DENY masks are
                // exact-bit — the caller controls the complete effective ACL.
                //
                // Recursion (SECURITY-HIGH#2 fix): the Win32 API has no safe
                // recursive mode (`icacls /T` followed junctions/symlinks out of
                // the allowlist). We self-walk the tree and SKIP reparse points
                // (symlinks/junctions) — matching the Unix recursive arm's
                // explicit symlink skip — so a reparse point inside an allowed
                // dir can never redirect the recursive chmod to external files.
                let path_str = resolved.to_string_lossy().into_owned();

                // Resolve every principal to an SDDL SID string and validate the
                // entries BEFORE touching the filesystem. `resolve_acl_to_sddl`
                // calls into the Win32 account-lookup API, so it runs on the
                // blocking pool; we hand it owned data.
                let entries = acl.entries.clone();
                let recursive = req.recursive;
                let path_owned = resolved.to_path_buf();
                let path_str_for_blocking = path_str.clone();
                let items = tokio::task::spawn_blocking(move || -> Result<u32, FileError> {
                    // Build the complete-DACL SDDL once; reused for every path
                    // in the (possibly recursive) walk.
                    let sddl = resolve_acl_to_sddl(&entries, &path_str_for_blocking)?;
                    apply_protected_dacl_walk(&path_owned, &sddl, recursive, &path_str_for_blocking)
                })
                .await
                .map_err(|e| {
                    file_error(
                        FileErrorCode::Io,
                        &req.path,
                        format!("Windows ACL chmod join error: {e}"),
                    )
                })??;

                Ok(FileChmodResult {
                    path: resolved.to_string_lossy().into_owned(),
                    items_modified: items,
                })
            }
        }
    }
}

#[cfg(unix)]
async fn set_unix_mode(path: &Path, mode: u32, recursive: bool) -> Result<u32, FileError> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || -> Result<u32, FileError> {
        let mut count = 0u32;
        // R10 (this PR): the *leaf* chmod is the one the policy layer
        // gated. Routing it through `safe_open_parent_dirfd_for` +
        // `fchmodat` closes the TOCTOU race against an attacker swapping
        // an ancestor of the leaf for a symlink between
        // `policy.check_path` and the syscall. Once the leaf has been
        // chmod'd (and, for recursive=true, only after we've descended
        // into it), the inner walk operates on entries returned by
        // `read_dir` of the validated leaf — bounded to the validated
        // subtree, which keeps the inner-race window from escalating
        // beyond what the attacker already controls.
        let handle = super::io_safe::safe_open_parent_dirfd_for(&path)?;
        super::io_safe::fchmodat(&handle.fd, &handle.basename, mode)
            .map_err(|e| super::io_safe::io_to_file_error(e, &path))?;
        count += 1;
        if recursive {
            let metadata =
                std::fs::symlink_metadata(&path).map_err(|e| io_to_file_error(e, &path))?;
            if metadata.is_dir() {
                set_unix_mode_recursive(&path, mode, &mut count)?;
            }
        }
        Ok(count)
    })
    .await
    .map_err(|e| file_error(FileErrorCode::Io, "", format!("chmod join error: {e}")))?
}

/// Recursive descent inside an already-validated leaf directory.
/// Path-based — see the parent docstring for why the inner race is bounded.
#[cfg(unix)]
fn set_unix_mode_recursive(path: &Path, mode: u32, count: &mut u32) -> Result<(), FileError> {
    let perms = std::fs::Permissions::from_mode(mode);
    for entry in std::fs::read_dir(path).map_err(|e| io_to_file_error(e, path))? {
        let entry = entry.map_err(|e| io_to_file_error(e, path))?;
        let ty = entry.file_type().map_err(|e| io_to_file_error(e, path))?;
        if ty.is_symlink() {
            continue; // Don't follow symlinks.
        }
        let child = entry.path();
        std::fs::set_permissions(&child, perms.clone()).map_err(|e| io_to_file_error(e, &child))?;
        *count += 1;
        if ty.is_dir() {
            set_unix_mode_recursive(&child, mode, count)?;
        }
    }
    Ok(())
}

// ── Windows ACL (true PROTECTED-DACL replacement) ───────────────────────────
//
// The file's effective DACL is set to EXACTLY the supplied `AclEntry` list via
// the Win32 security API. We build an SDDL string `D:P(...)...` (one ACE per
// entry), convert it to a SECURITY_DESCRIPTOR, extract the DACL, and call
// `SetNamedSecurityInfoW` with `DACL_SECURITY_INFORMATION |
// PROTECTED_DACL_SECURITY_INFORMATION`. PROTECTED + a complete DACL is what
// makes this a TRUE replacement: the OS drops inherited ACEs AND all
// pre-existing explicit ACEs (the bug icacls could not fix).
//
// The SDDL builder (`build_dacl_sddl`) and principal classifier
// (`sddl_principal_passthrough`) are pure and compiled cross-platform under
// cfg(test) so they can be unit-tested on macOS without Windows.

/// Render a 32-bit access mask as the hex form an SDDL ACE rights field accepts,
/// e.g. `0x001F01FF`. SDDL also accepts the two-letter right abbreviations
/// (`FA`, `FR`, …), but the explicit hex mask is unambiguous and preserves the
/// caller's exact bits — `deny R` denies exactly the read bits and nothing more.
///
/// Pure; compiled on all platforms (used by `build_dacl_sddl`).
#[cfg(any(windows, test))]
fn acl_mask_to_sddl_rights(access_mask: u32) -> String {
    format!("0x{access_mask:08X}")
}

/// Reject a principal token that cannot be safely turned into an SDDL ACE.
///
/// The resolved SID is embedded as the final, `;`-separated field of an SDDL
/// ACE — `(A;;<rights>;;;<sid>)`. Even though we never interpolate the raw
/// principal into the SDDL (we resolve it to a SID first, or pass through only
/// after a strict alias/SID-string check), we still reject obviously malformed
/// tokens up front as defense-in-depth and to give a clear, early error:
/// a principal containing a `:` (e.g. `Everyone:F`) is the classic
/// perm-smuggling attempt; whitespace is never part of a valid account token;
/// a leading `/` looks like a flag. These are rejected before any SID lookup or
/// filesystem touch.
///
/// Returns `Ok(())` for a well-formed principal (e.g. `BUILTIN\Users`,
/// `DOMAIN\user`, `Everyone`, the SDDL aliases `BA`/`SY`/`WD`/`OW`, or a literal
/// SID string `S-1-5-…`) and a `FileError` (InvalidPath, matching the file's
/// existing path-style validation errors) otherwise.
#[cfg(any(windows, test))]
fn validate_principal(principal: &str, path_for_error: &str) -> Result<(), FileError> {
    if principal.is_empty() {
        return Err(file_error(
            FileErrorCode::Unspecified,
            path_for_error,
            "ACL entry has an empty principal — a valid Windows user or group name is required",
        ));
    }
    if principal.contains(':')
        || principal.contains(';')
        || principal.contains('(')
        || principal.contains(')')
        || principal.chars().any(char::is_whitespace)
        || principal.starts_with('/')
    {
        return Err(file_error(
            FileErrorCode::InvalidPath,
            path_for_error,
            format!(
                "ACL entry has a malformed principal '{principal}' — a principal must not \
                 contain ':', ';', '(', ')' or whitespace or start with '/' (these corrupt \
                 the SDDL ACE token)"
            ),
        ));
    }
    Ok(())
}

/// Decide whether a principal is a recognised SDDL alias (e.g. `BA`, `SY`,
/// `WD`, `OW`) or a literal SID string (`S-1-…`) that can be embedded into an
/// SDDL ACE verbatim, WITHOUT going through `LookupAccountNameW`.
///
/// SDDL two-letter SID aliases are well-defined and case-insensitive in
/// practice, but we accept only the canonical upper-case spelling to keep the
/// passthrough conservative — anything else falls through to a real account
/// lookup. Literal SID strings are accepted (case-insensitive `S-1-` prefix);
/// SDDL embeds them directly.
///
/// Pure; compiled cross-platform for unit testing. The caller has already run
/// `validate_principal`, so the token contains no SDDL-breaking characters.
#[cfg(any(windows, test))]
fn sddl_principal_passthrough(principal: &str) -> Option<&str> {
    // Canonical SDDL SID-string aliases we allow through. This is a curated
    // subset (the common ones a caller would reasonably use); unknown aliases
    // intentionally fall through to LookupAccountNameW, which also resolves
    // them on a real system.
    const ALIASES: &[&str] = &[
        "BA", // Built-in Administrators
        "BU", // Built-in Users
        "SY", // Local System
        "LS", // Local Service
        "NS", // Network Service
        "WD", // Everyone (World)
        "OW", // Owner Rights
        "AU", // Authenticated Users
        "AN", // Anonymous
        "IU", // Interactive Users
        "CO", // Creator Owner
        "CG", // Creator Group
    ];
    if ALIASES.contains(&principal) {
        return Some(principal);
    }
    // Literal SID string, e.g. "S-1-5-18". SDDL accepts these directly.
    let upper = principal.to_ascii_uppercase();
    if upper.starts_with("S-1-")
        && principal
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        return Some(principal);
    }
    None
}

/// Build the SDDL DACL string that, when applied as a PROTECTED DACL, makes the
/// file's effective DACL EXACTLY the supplied entries — a true full replacement.
///
/// Output shape: `D:P(ace)(ace)…` where
///   - `D:` introduces the DACL.
///   - `P` marks it PROTECTED: no ACEs are inherited from the parent. Combined
///     with applying the COMPLETE DACL via `PROTECTED_DACL_SECURITY_INFORMATION`,
///     this drops BOTH inherited ACEs AND any pre-existing explicit ACEs on the
///     file — so "owner-only" really is owner-only and a DENY is meaningful.
///   - each ACE is `(A;;<rights-hex>;;;<sid>)` for ALLOW or `(D;;…)` for DENY.
///     The rights field is the caller's exact 32-bit mask in hex, so a `deny R`
///     denies only the read bits and nothing more.
///
/// Each entry's `principal` must already be resolved to an SDDL-embeddable SID
/// string (a literal `S-1-…` or a two-letter alias). Resolution from a friendly
/// name (`DOMAIN\user`, `Everyone`) to a SID happens in the Windows-only
/// `resolve_acl_to_sddl`; this builder is pure and assembles the final string.
///
/// `sids` is parallel to `entries` (same length, same order). The DENY-before-
/// ALLOW evaluation order is the Windows default and is preserved as given.
///
/// Returns an error if `entries` is empty (nothing to apply is an error, not a
/// no-op) or if `entries.len() != sids.len()` (a programming error). Pure,
/// compiled on all platforms for unit testing.
#[cfg(any(windows, test))]
pub fn build_dacl_sddl(
    entries: &[AclEntry],
    sids: &[String],
    path_for_error: &str,
) -> Result<String, FileError> {
    if entries.is_empty() {
        return Err(file_error(
            FileErrorCode::Unspecified,
            path_for_error,
            "WindowsAcl entries list is empty — at least one ACE is required",
        ));
    }
    if entries.len() != sids.len() {
        // Defensive: callers always pass parallel slices. Treat a mismatch as an
        // internal error rather than silently producing a malformed DACL.
        return Err(file_error(
            FileErrorCode::Unspecified,
            path_for_error,
            "internal error: ACL entry/SID count mismatch",
        ));
    }

    // `D:P` — DACL, PROTECTED (no inheritance; the complete DACL replaces all
    // existing ACEs when applied with PROTECTED_DACL_SECURITY_INFORMATION).
    let mut sddl = String::from("D:P");
    for (entry, sid) in entries.iter().zip(sids.iter()) {
        // AclEntryType::Allow = 0, AclEntryType::Deny = 1; any non-Deny is Allow.
        let ace_type = if entry.entry_type == AclEntryType::Deny as i32 {
            "D"
        } else {
            "A"
        };
        let rights = acl_mask_to_sddl_rights(entry.access_mask);
        // (ace_type;ace_flags;rights;object_guid;inherit_object_guid;account_sid)
        // We leave flags / object GUIDs empty.
        sddl.push_str(&format!("({ace_type};;{rights};;;{sid})"));
    }
    Ok(sddl)
}

/// Validate every entry, resolve each principal to an SDDL SID string, and
/// assemble the final PROTECTED-DACL SDDL via [`build_dacl_sddl`].
///
/// Principal resolution (per entry):
///   1. `validate_principal` rejects empty/malformed tokens early.
///   2. `sddl_principal_passthrough` accepts canonical SDDL aliases (`BA`,
///      `SY`, `WD`, `OW`, …) and literal SID strings (`S-1-…`) verbatim.
///   3. Otherwise `resolve_principal_to_sid` calls `LookupAccountNameW`
///      (handles `DOMAIN\user`, bare names, well-known names) and stringifies
///      the resulting SID. An unresolvable name maps to PermissionDenied.
///
/// Done once per chmod request (before the per-path walk) so the SDDL — and
/// thus the account lookups — are computed a single time even for recursive
/// applications.
#[cfg(windows)]
fn resolve_acl_to_sddl(entries: &[AclEntry], path_for_error: &str) -> Result<String, FileError> {
    if entries.is_empty() {
        return Err(file_error(
            FileErrorCode::Unspecified,
            path_for_error,
            "WindowsAcl entries list is empty — at least one ACE is required",
        ));
    }
    let mut sids: Vec<String> = Vec::with_capacity(entries.len());
    for entry in entries {
        validate_principal(&entry.principal, path_for_error)?;
        let sid = match sddl_principal_passthrough(&entry.principal) {
            Some(s) => s.to_string(),
            None => resolve_principal_to_sid(&entry.principal, path_for_error)?,
        };
        sids.push(sid);
    }
    build_dacl_sddl(entries, &sids, path_for_error)
}

/// Resolve a friendly account name (`DOMAIN\user`, `Everyone`, a bare username)
/// to its SDDL SID string (`S-1-…`) via `LookupAccountNameW` +
/// `ConvertSidToStringSidW`.
///
/// An unresolvable name (the API reports `ERROR_NONE_MAPPED`) is surfaced as a
/// `PermissionDenied` FileError — matching the prior icacls "invalid principal"
/// behaviour the integration tests assert on. Any other failure is `Io`.
///
/// # Memory safety
/// - `LookupAccountNameW` is called twice: first with null buffers to learn the
///   required SID and domain sizes, then with exactly-sized owned buffers. The
///   SID buffer is a `Vec<u8>` sized to `cb_sid`; the domain buffer a
///   `Vec<u16>`. Both outlive the call.
/// - The string SID from `ConvertSidToStringSidW` is `LocalAlloc`'d and is freed
///   exactly once via a `LocalFree` guard on every return path.
#[cfg(windows)]
fn resolve_principal_to_sid(principal: &str, path_for_error: &str) -> Result<String, FileError> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_NONE_MAPPED, LocalFree};
    use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
    use windows_sys::Win32::Security::{LookupAccountNameW, PSID, SID_NAME_USE};

    // NUL-terminated UTF-16 account name for the W API.
    let name_wide: Vec<u16> = std::ffi::OsStr::new(principal)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    // First call: discover the required SID and domain buffer sizes. With null
    // out-buffers the API is expected to fail with ERROR_INSUFFICIENT_BUFFER and
    // fill cb_sid / cch_domain.
    let mut cb_sid: u32 = 0;
    let mut cch_domain: u32 = 0;
    let mut sid_use: SID_NAME_USE = 0;

    // SAFETY: `name_wide` is a valid NUL-terminated UTF-16 buffer alive for the
    // call. We pass null for the system name (local machine), null SID and
    // domain out-buffers with zeroed size out-params, so the call only reports
    // the required sizes via cb_sid / cch_domain. It does not write through the
    // null pointers.
    let probe = unsafe {
        LookupAccountNameW(
            std::ptr::null(),
            name_wide.as_ptr(),
            std::ptr::null_mut(),
            &mut cb_sid,
            std::ptr::null_mut(),
            &mut cch_domain,
            &mut sid_use,
        )
    };
    // The size-probe is expected to FAIL with ERROR_INSUFFICIENT_BUFFER. If it
    // somehow succeeds (cb_sid == 0) or fails for another reason, classify it.
    if probe != 0 {
        return Err(file_error(
            FileErrorCode::Io,
            path_for_error,
            format!("LookupAccountNameW size-probe unexpectedly succeeded for '{principal}'"),
        ));
    }
    let probe_err = std::io::Error::last_os_error();
    if probe_err.raw_os_error() == Some(ERROR_NONE_MAPPED as i32) {
        return Err(file_error(
            FileErrorCode::PermissionDenied,
            path_for_error,
            format!(
                "invalid principal — ACL entry references an unknown user or group name \
                 '{principal}' (no SID mapping)"
            ),
        ));
    }
    if probe_err.raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER as i32) || cb_sid == 0 {
        return Err(file_error(
            FileErrorCode::Io,
            path_for_error,
            format!("LookupAccountNameW size-probe failed for '{principal}': {probe_err}"),
        ));
    }

    // Second call: exactly-sized owned buffers. Clamp the domain buffer to at
    // least 1 element: for some built-in accounts the probe can report
    // cch_domain == 0, but LookupAccountNameW still writes a NUL terminator, so
    // a zero-length buffer would be an out-of-bounds write. `cch_domain` itself
    // is left at the API-reported value (the size the call uses); only the
    // allocation is widened.
    let mut sid_buf: Vec<u8> = vec![0u8; cb_sid as usize];
    let mut domain_buf: Vec<u16> = vec![0u16; (cch_domain as usize).max(1)];

    // SAFETY: `name_wide` is still alive. `sid_buf`/`domain_buf` are owned,
    // exactly the size the probe requested, and outlive the call. cb_sid /
    // cch_domain hold those sizes. The API writes the binary SID into sid_buf
    // and the domain name into domain_buf; neither pointer is retained.
    let ok = unsafe {
        LookupAccountNameW(
            std::ptr::null(),
            name_wide.as_ptr(),
            sid_buf.as_mut_ptr() as PSID,
            &mut cb_sid,
            domain_buf.as_mut_ptr(),
            &mut cch_domain,
            &mut sid_use,
        )
    };
    if ok == 0 {
        let err = std::io::Error::last_os_error();
        let code = if err.raw_os_error() == Some(ERROR_NONE_MAPPED as i32) {
            FileErrorCode::PermissionDenied
        } else {
            FileErrorCode::Io
        };
        return Err(file_error(
            code,
            path_for_error,
            format!("LookupAccountNameW failed for '{principal}': {err}"),
        ));
    }

    // Stringify the binary SID. ConvertSidToStringSidW LocalAlloc's the result;
    // we free it exactly once via the guard below.
    let mut str_sid_ptr: windows_sys::core::PWSTR = std::ptr::null_mut();
    // SAFETY: `sid_buf` holds a valid SID written by the successful
    // LookupAccountNameW above and is alive for this call. `str_sid_ptr` is a
    // valid out-pointer; on success the API sets it to a LocalAlloc'd
    // NUL-terminated UTF-16 string that we own and free.
    let conv_ok = unsafe { ConvertSidToStringSidW(sid_buf.as_mut_ptr() as PSID, &mut str_sid_ptr) };
    if conv_ok == 0 || str_sid_ptr.is_null() {
        let err = std::io::Error::last_os_error();
        return Err(file_error(
            FileErrorCode::Io,
            path_for_error,
            format!("ConvertSidToStringSidW failed for '{principal}': {err}"),
        ));
    }

    // RAII free of the LocalAlloc'd string SID on every path below.
    struct LocalStr(windows_sys::core::PWSTR);
    impl Drop for LocalStr {
        fn drop(&mut self) {
            // SAFETY: `self.0` is the non-null LocalAlloc'd pointer from
            // ConvertSidToStringSidW; LocalFree is the matching deallocator and
            // runs exactly once. After this the pointer is not used.
            unsafe {
                LocalFree(self.0.cast());
            }
        }
    }
    let guard = LocalStr(str_sid_ptr);

    // Read the NUL-terminated wide string into a Rust String.
    // SAFETY: `guard.0` points at a valid NUL-terminated UTF-16 string owned by
    // the guard for the duration of this read.
    let sid_string = unsafe {
        let mut len = 0usize;
        while *guard.0.add(len) != 0 {
            len += 1;
        }
        let slice = std::slice::from_raw_parts(guard.0, len);
        String::from_utf16_lossy(slice)
    };
    // `guard` drops here (LocalFree), after the string has been copied out.
    Ok(sid_string)
}

/// Apply the COMPLETE, PROTECTED DACL described by `sddl` to a single path via
/// `SetNamedSecurityInfoW`. This is the true full-DACL replacement: PROTECTED +
/// the entire DACL means the OS drops both inherited and pre-existing explicit
/// ACEs, leaving the path's effective DACL equal to exactly `sddl`'s entries.
///
/// # Memory safety
/// - The SDDL is converted to a heap SECURITY_DESCRIPTOR via
///   `ConvertStringSecurityDescriptorToSecurityDescriptorW`; the descriptor is
///   owned by a `LocalFree` guard (`SecDesc`) and outlives the
///   `SetNamedSecurityInfoW` call that reads through its DACL pointer.
/// - `GetSecurityDescriptorDacl` yields a borrowed pointer INTO that descriptor,
///   so the descriptor must (and does) stay alive across the set call.
/// - The path is a NUL-terminated UTF-16 buffer that lives for the call.
#[cfg(windows)]
fn set_protected_dacl(path: &Path, sddl: &str, path_for_error: &str) -> Result<(), FileError> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{BOOL, ERROR_SUCCESS, LocalFree};
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1, SE_FILE_OBJECT,
        SetNamedSecurityInfoW,
    };
    use windows_sys::Win32::Security::{
        ACL, DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl,
        PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
    };

    // RAII guard freeing the LocalAlloc'd SECURITY_DESCRIPTOR. Mirrors the T6
    // (ipc.rs) pattern: tie LocalFree to scope exit so there is no leak, UAF, or
    // double-free across early returns.
    struct SecDesc(PSECURITY_DESCRIPTOR);
    impl Drop for SecDesc {
        fn drop(&mut self) {
            // SAFETY: `self.0` is the non-null descriptor from the converter
            // (LocalAlloc'd); LocalFree is the matching deallocator, run once.
            unsafe {
                LocalFree(self.0.cast());
            }
        }
    }

    let sddl_wide: Vec<u16> = std::ffi::OsStr::new(sddl)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut psd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();

    // SAFETY: `sddl_wide` is a valid NUL-terminated UTF-16 buffer alive for the
    // call; `psd` is a valid out-pointer. On success the API LocalAlloc's a
    // descriptor we own (freed via SecDesc). Null size out-param is allowed.
    let conv_ok = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl_wide.as_ptr(),
            SDDL_REVISION_1,
            &mut psd,
            std::ptr::null_mut(),
        )
    };
    // Capture the OS error BEFORE constructing the guard (Drop runs LocalFree,
    // which would clobber the thread's last-error). Wrap `psd` in the guard
    // FIRST, then check the result: if the converter failed but still set a
    // non-null `psd` (the failure state is API-unspecified), the guard still
    // frees it on the early return — no leak.
    let conv_err = std::io::Error::last_os_error();
    let sd = if psd.is_null() {
        None
    } else {
        Some(SecDesc(psd))
    };
    if conv_ok == 0 || sd.is_none() {
        return Err(file_error(
            FileErrorCode::Io,
            path_for_error,
            format!("failed to build security descriptor from DACL SDDL '{sddl}': {conv_err}"),
        ));
    }
    // Safe: the `sd.is_none()` case returned above.
    let sd = sd.expect("descriptor guard present after success check");

    // Extract the DACL pointer from the descriptor. It borrows INTO `sd`, so
    // `sd` must stay alive through SetNamedSecurityInfoW (it does — dropped at
    // end of scope).
    let mut dacl_present: BOOL = 0;
    let mut dacl_ptr: *mut ACL = std::ptr::null_mut();
    let mut dacl_defaulted: BOOL = 0;
    // SAFETY: `sd.0` is a valid descriptor. The three out-params are valid
    // locals. On success `dacl_ptr` points into the descriptor owned by `sd`.
    let get_ok = unsafe {
        GetSecurityDescriptorDacl(sd.0, &mut dacl_present, &mut dacl_ptr, &mut dacl_defaulted)
    };
    if get_ok == 0 {
        let err = std::io::Error::last_os_error();
        return Err(file_error(
            FileErrorCode::Io,
            path_for_error,
            format!("GetSecurityDescriptorDacl failed: {err}"),
        ));
    }
    if dacl_present == 0 || dacl_ptr.is_null() {
        // Our SDDL always contains `D:P(...)`, so a present, non-null DACL is
        // expected. A NULL DACL would grant Everyone full access — refuse to
        // apply rather than weaken the file.
        return Err(file_error(
            FileErrorCode::Io,
            path_for_error,
            "built security descriptor unexpectedly had no DACL — refusing to apply",
        ));
    }

    // NUL-terminated UTF-16 path for the W API.
    let path_wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    // Apply the DACL as PROTECTED → strips inheritance AND replaces all explicit
    // ACEs with exactly our DACL. owner/group/sacl are unchanged (null).
    // SAFETY: `path_wide` is a valid NUL-terminated UTF-16 path alive for the
    // call; `dacl_ptr` points into the live descriptor owned by `sd` (kept alive
    // below). owner/group/sacl null means "do not change". The function copies
    // the DACL it needs during the call and does not retain our pointers.
    let rc = unsafe {
        SetNamedSecurityInfoW(
            path_wide.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            dacl_ptr,
            std::ptr::null_mut(),
        )
    };

    // Keep the descriptor alive until strictly AFTER the set call returns: the
    // DACL pointer borrowed into it during the call. Explicit so the ordering is
    // part of the code, not scope luck.
    drop(sd);

    if rc != ERROR_SUCCESS {
        let err = std::io::Error::from_raw_os_error(rc as i32);
        // ERROR_FILE_NOT_FOUND (2) / ERROR_PATH_NOT_FOUND (3) → NotFound.
        let code = match rc {
            2 | 3 => FileErrorCode::NotFound,
            // ERROR_ACCESS_DENIED (5) → PermissionDenied.
            5 => FileErrorCode::PermissionDenied,
            _ => FileErrorCode::Io,
        };
        return Err(file_error(
            code,
            path_for_error,
            format!("SetNamedSecurityInfoW failed (win32 {rc}): {err}"),
        ));
    }
    Ok(())
}

/// Apply the PROTECTED DACL `sddl` to `path` and, when `recursive`, to every
/// non-reparse-point descendant.
///
/// SECURITY (HIGH#2): the recursive walk SKIPS reparse points
/// (symlinks/junctions) — it neither descends into nor applies the DACL to them
/// — exactly like the Unix recursive arm's symlink skip. This prevents a reparse
/// point planted inside an allowed directory from redirecting the recursive
/// chmod to files OUTSIDE the allowlist (the `icacls /T` escape). The leaf
/// itself was already validated by the policy layer and the
/// `reject_if_final_component_is_symlink` guard upstream, so applying to the
/// leaf is safe.
///
/// Returns the number of paths whose DACL was set. Fails the whole operation on
/// any per-path error (no `/C`-style continue) so a partial failure is visible.
#[cfg(windows)]
fn apply_protected_dacl_walk(
    path: &Path,
    sddl: &str,
    recursive: bool,
    path_for_error: &str,
) -> Result<u32, FileError> {
    // Always apply to the (already-validated) leaf.
    set_protected_dacl(path, sddl, path_for_error)?;
    let mut count = 1u32;

    if recursive {
        let metadata = std::fs::symlink_metadata(path).map_err(|e| io_to_file_error(e, path))?;
        // If the leaf is itself a reparse point we must NOT have followed it —
        // the upstream no-follow guard handles the explicit no_follow case, but
        // for defense-in-depth do not descend through a reparse-point leaf.
        if metadata.is_dir() && !metadata.file_type().is_symlink() {
            apply_protected_dacl_recursive(path, sddl, &mut count, path_for_error)?;
        }
    }
    Ok(count)
}

/// Recursive descent for [`apply_protected_dacl_walk`]. Reparse points
/// (symlinks/junctions) are skipped — never applied to and never descended into
/// — so the recursion cannot escape the validated subtree via a planted
/// junction. Path-based, matching the Unix `set_unix_mode_recursive` arm.
#[cfg(windows)]
fn apply_protected_dacl_recursive(
    dir: &Path,
    sddl: &str,
    count: &mut u32,
    path_for_error: &str,
) -> Result<(), FileError> {
    for entry in std::fs::read_dir(dir).map_err(|e| io_to_file_error(e, dir))? {
        let entry = entry.map_err(|e| io_to_file_error(e, dir))?;
        let ty = entry.file_type().map_err(|e| io_to_file_error(e, dir))?;
        // SECURITY: skip reparse points (symlinks/junctions). On Windows a
        // directory junction reports `is_symlink() == true` via
        // symlink_metadata / DirEntry::file_type (both use the reparse tag), so
        // this skip covers junctions as well as name-surrogate symlinks. A
        // reparse point here could otherwise redirect the DACL set OUTSIDE the
        // allowlist (HIGH#2).
        if ty.is_symlink() {
            continue;
        }
        let child = entry.path();
        set_protected_dacl(&child, sddl, path_for_error)?;
        *count += 1;
        if ty.is_dir() {
            apply_protected_dacl_recursive(&child, sddl, count, path_for_error)?;
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
    #[cfg(unix)]
    {
        UnixPermission {
            mode: Some(metadata.permissions().mode()),
            owner: None,
            group: None,
        }
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        UnixPermission {
            mode: None,
            owner: None,
            group: None,
        }
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

#[cfg(test)]
mod tests {
    use super::{cross_device_move_fallback, is_cross_device_error};
    use ahand_protocol::{AclEntry, AclEntryType, FileMove};
    use std::io;
    use std::path::Path;

    #[test]
    fn is_cross_device_error_detects_crosses_devices_kind() {
        // I6 regression: the stable `io::ErrorKind::CrossesDevices` must
        // be classified as cross-device on every platform. Without this,
        // Windows cross-volume moves would skip the copy+delete fallback.
        let e = io::Error::new(io::ErrorKind::CrossesDevices, "synthetic");
        assert!(is_cross_device_error(&e));
    }

    #[cfg(unix)]
    #[test]
    fn is_cross_device_error_detects_unix_exdev_raw_code() {
        // EXDEV = 18 on Linux/macOS; std maps this to the CrossesDevices
        // kind on supported toolchains, but we keep the numeric fallback
        // for resilience.
        let e = io::Error::from_raw_os_error(18);
        assert!(is_cross_device_error(&e));
    }

    #[cfg(windows)]
    #[test]
    fn is_cross_device_error_detects_windows_not_same_device_raw_code() {
        // ERROR_NOT_SAME_DEVICE = 17 (winerror.h). Distinct from Unix
        // EXDEV (also 18 vs 17) — checking only the Unix value would
        // silently miss this on Windows.
        let e = io::Error::from_raw_os_error(17);
        assert!(is_cross_device_error(&e));
    }

    #[test]
    fn is_cross_device_error_rejects_unrelated_errors() {
        // Anything else must NOT trigger the copy+delete fallback,
        // otherwise we'd silently mask real failures (PermissionDenied,
        // NotFound, etc.).
        let cases = [
            io::Error::from(io::ErrorKind::NotFound),
            io::Error::from(io::ErrorKind::PermissionDenied),
            io::Error::from(io::ErrorKind::AlreadyExists),
            io::Error::from_raw_os_error(2),  // ENOENT
            io::Error::from_raw_os_error(13), // EACCES
        ];
        for e in &cases {
            assert!(
                !is_cross_device_error(e),
                "expected not-cross-device for {e:?}"
            );
        }
    }

    /// Build a FileMove with the strings filled in to match the resolved
    /// paths the helper sees in production. The helper only consumes
    /// `req.source` / `req.destination` / `req.overwrite` to forward into
    /// `handle_copy`, so the exact strings don't matter for correctness;
    /// keeping them aligned with the `Path` arguments avoids surprises in
    /// log lines.
    fn move_req(source: &Path, destination: &Path, overwrite: bool) -> FileMove {
        FileMove {
            source: source.to_string_lossy().into_owned(),
            destination: destination.to_string_lossy().into_owned(),
            overwrite,
        }
    }

    /// Cross-device payload for a single file: copy to destination, remove source.
    /// Trigger detection is covered separately via `is_cross_device_error_*`;
    /// this isolates the payload so we can verify it without staging a real
    /// multi-FS environment (which CI runners don't reliably provide).
    #[tokio::test]
    async fn cross_device_move_fallback_moves_a_single_file() {
        let dir = tempfile::TempDir::new().unwrap();
        // Production code always passes canonicalized paths into the
        // copy/move fallback (the dispatch layer routes through
        // `policy.check_path` first). On macOS `tempfile`'s `/var/...`
        // root resolves to `/private/var/...` only after canonicalize,
        // and the new dirfd-based safe-open path refuses to traverse
        // the `/var` symlink. Canonicalize here so the test exercises
        // the same precondition production has.
        let dir_canonical = dir.path().canonicalize().unwrap();
        let src = dir_canonical.join("source.txt");
        let dst = dir_canonical.join("dest.txt");
        std::fs::write(&src, b"payload").unwrap();

        let req = move_req(&src, &dst, false);
        cross_device_move_fallback(&req, &src, &dst).await.unwrap();

        assert!(!src.exists(), "source should be gone after fallback");
        assert_eq!(std::fs::read(&dst).unwrap(), b"payload");
    }

    /// Cross-device payload for a directory tree: recursive copy then
    /// `remove_dir_all` on the source. Verifies that `is_dir()`-branch
    /// in the fallback uses the right unlink primitive (a previous
    /// implementation could have called `remove_file` and silently
    /// failed for directories).
    #[tokio::test]
    async fn cross_device_move_fallback_moves_a_directory_tree() {
        let dir = tempfile::TempDir::new().unwrap();
        // See note in the single-file fallback test above for why we
        // canonicalize the temp root before deriving paths.
        let dir_canonical = dir.path().canonicalize().unwrap();
        let src = dir_canonical.join("src_tree");
        let dst = dir_canonical.join("dst_tree");
        std::fs::create_dir(&src).unwrap();
        std::fs::create_dir(src.join("nested")).unwrap();
        std::fs::write(src.join("a.txt"), b"a").unwrap();
        std::fs::write(src.join("nested/b.txt"), b"b").unwrap();

        let req = move_req(&src, &dst, false);
        cross_device_move_fallback(&req, &src, &dst).await.unwrap();

        assert!(!src.exists(), "source tree should be gone");
        assert_eq!(std::fs::read(dst.join("a.txt")).unwrap(), b"a");
        assert_eq!(std::fs::read(dst.join("nested/b.txt")).unwrap(), b"b");
    }

    // ── map_symlink_error unit tests ──────────────────────────────────────
    // These tests run cross-platform (the function is compiled under cfg(test)).
    // They feed synthetic io::Error values so no actual symlink privilege is needed.

    #[test]
    fn map_symlink_error_privilege_not_held_maps_to_permission_denied_with_remediation() {
        // ERROR_PRIVILEGE_NOT_HELD = 1314 (Windows) — synthesised cross-platform.
        let err = io::Error::from_raw_os_error(1314);
        let fe = super::map_symlink_error(&err, "/tmp/link");
        assert_eq!(
            fe.code,
            ahand_protocol::FileErrorCode::PermissionDenied as i32,
            "1314 must map to PermissionDenied"
        );
        // The message must contain actionable remediation text.
        assert!(
            fe.message.contains("Developer Mode") || fe.message.contains("elevated"),
            "remediation text must mention Developer Mode or elevation; got: {:?}",
            fe.message
        );
        assert_eq!(fe.path, "/tmp/link", "path field must be preserved");
    }

    #[test]
    fn map_symlink_error_generic_permission_denied_passes_through() {
        // A generic PermissionDenied error must NOT be confused with the Windows
        // 1314 privilege error — no Developer Mode messaging should appear.
        let err = io::Error::new(io::ErrorKind::PermissionDenied, "generic denied");
        let fe = super::map_symlink_error(&err, "/tmp/link2");
        assert_eq!(
            fe.code,
            ahand_protocol::FileErrorCode::PermissionDenied as i32,
            "PermissionDenied kind must map to PermissionDenied code"
        );
        assert!(
            !fe.message.contains("Developer Mode"),
            "generic error must not mention Developer Mode; got: {:?}",
            fe.message
        );
    }

    #[test]
    fn map_symlink_error_already_exists_passes_through() {
        let err = io::Error::new(io::ErrorKind::AlreadyExists, "already exists");
        let fe = super::map_symlink_error(&err, "/tmp/existing");
        assert_eq!(
            fe.code,
            ahand_protocol::FileErrorCode::AlreadyExists as i32,
            "AlreadyExists kind must map to AlreadyExists code"
        );
    }

    #[test]
    fn map_symlink_error_not_found_passes_through() {
        let err = io::Error::new(io::ErrorKind::NotFound, "not found");
        let fe = super::map_symlink_error(&err, "/tmp/missing");
        assert_eq!(
            fe.code,
            ahand_protocol::FileErrorCode::NotFound as i32,
            "NotFound kind must map to NotFound code"
        );
    }

    // ── Windows ACL SDDL builder unit tests ──────────────────────────────────
    // These run cross-platform because the builders are compiled under
    // cfg(any(windows, test)). They never touch the filesystem or the Win32 API
    // — they only test the pure SDDL-string assembly, mask rendering, principal
    // validation, and the alias/SID-string passthrough.

    // ── acl_mask_to_sddl_rights: exact-bit hex rendering ─────────────────────

    #[test]
    fn acl_mask_renders_full_control_hex() {
        assert_eq!(super::acl_mask_to_sddl_rights(0x001F_01FF), "0x001F01FF");
    }

    #[test]
    fn acl_mask_renders_read_hex() {
        assert_eq!(super::acl_mask_to_sddl_rights(0x0012_0089), "0x00120089");
    }

    #[test]
    fn acl_mask_renders_arbitrary_mask_preserving_bits() {
        // An unusual mask is preserved exactly (no lossy mapping to a letter).
        assert_eq!(super::acl_mask_to_sddl_rights(0xDEAD_BEEF), "0xDEADBEEF");
        assert_eq!(super::acl_mask_to_sddl_rights(0x0000_0001), "0x00000001");
        assert_eq!(super::acl_mask_to_sddl_rights(0), "0x00000000");
    }

    // ── sddl_principal_passthrough: aliases + literal SID strings ────────────

    #[test]
    fn passthrough_accepts_canonical_aliases() {
        for a in [
            "BA", "BU", "SY", "WD", "OW", "AU", "AN", "IU", "CO", "CG", "LS", "NS",
        ] {
            assert_eq!(
                super::sddl_principal_passthrough(a),
                Some(a),
                "alias {a} must pass through"
            );
        }
    }

    #[test]
    fn passthrough_accepts_literal_sid_string() {
        assert_eq!(
            super::sddl_principal_passthrough("S-1-5-18"),
            Some("S-1-5-18")
        );
        assert_eq!(
            super::sddl_principal_passthrough("S-1-5-32-544"),
            Some("S-1-5-32-544")
        );
    }

    #[test]
    fn passthrough_rejects_friendly_names() {
        // Friendly names must fall through to LookupAccountNameW (None here).
        assert_eq!(super::sddl_principal_passthrough("Everyone"), None);
        assert_eq!(super::sddl_principal_passthrough("DOMAIN\\user"), None);
        assert_eq!(super::sddl_principal_passthrough("Administrators"), None);
        // Lower-case alias is NOT in the curated set → falls through.
        assert_eq!(super::sddl_principal_passthrough("ba"), None);
        // A SID-looking token with an illegal char is not a literal SID.
        assert_eq!(super::sddl_principal_passthrough("S-1-5-1$8"), None);
    }

    // ── build_dacl_sddl: PROTECTED complete-DACL string assembly ─────────────

    #[test]
    fn build_dacl_sddl_empty_entries_returns_error() {
        let err = super::build_dacl_sddl(&[], &[], "/some/path").unwrap_err();
        assert_eq!(err.code, ahand_protocol::FileErrorCode::Unspecified as i32);
        assert!(
            err.message.contains("empty"),
            "error must mention 'empty': {}",
            err.message
        );
    }

    #[test]
    fn build_dacl_sddl_count_mismatch_returns_error() {
        let entry = AclEntry {
            principal: "BA".to_string(),
            access_mask: 0x001F_01FF,
            entry_type: AclEntryType::Allow as i32,
        };
        // One entry, zero SIDs → internal mismatch.
        let err = super::build_dacl_sddl(&[entry], &[], "/p").unwrap_err();
        assert_eq!(err.code, ahand_protocol::FileErrorCode::Unspecified as i32);
        assert!(err.message.contains("mismatch"), "got: {}", err.message);
    }

    #[test]
    fn build_dacl_sddl_single_allow_is_protected_complete_dacl() {
        let entry = AclEntry {
            principal: "S-1-5-21-1-2-3-1000".to_string(),
            access_mask: 0x001F_01FF, // Full control
            entry_type: AclEntryType::Allow as i32,
        };
        let sddl =
            super::build_dacl_sddl(&[entry], &["S-1-5-21-1-2-3-1000".to_string()], "/p").unwrap();
        // PROTECTED DACL: `D:P` prefix is the marker that strips inheritance AND
        // (when applied) replaces all explicit ACEs — the true-replacement fix.
        assert!(
            sddl.starts_with("D:P("),
            "must be a PROTECTED DACL (D:P): {sddl}"
        );
        assert_eq!(sddl, "D:P(A;;0x001F01FF;;;S-1-5-21-1-2-3-1000)");
    }

    #[test]
    fn build_dacl_sddl_deny_uses_d_ace_type_and_exact_mask() {
        let entry = AclEntry {
            principal: "WD".to_string(),
            access_mask: 0x0012_0089, // Read
            entry_type: AclEntryType::Deny as i32,
        };
        let sddl = super::build_dacl_sddl(&[entry], &["WD".to_string()], "/p").unwrap();
        // DENY → `D` ace type; exact-bit mask preserved.
        assert_eq!(sddl, "D:P(D;;0x00120089;;;WD)");
    }

    #[test]
    fn build_dacl_sddl_multiple_entries_preserve_order() {
        let entries = vec![
            AclEntry {
                principal: "BA".to_string(),
                access_mask: 0x001F_01FF,
                entry_type: AclEntryType::Allow as i32,
            },
            AclEntry {
                principal: "WD".to_string(),
                access_mask: 0x0012_0089,
                entry_type: AclEntryType::Deny as i32,
            },
        ];
        let sids = vec!["BA".to_string(), "WD".to_string()];
        let sddl = super::build_dacl_sddl(&entries, &sids, "/p").unwrap();
        assert_eq!(
            sddl, "D:P(A;;0x001F01FF;;;BA)(D;;0x00120089;;;WD)",
            "entry order must be preserved (DENY-before-ALLOW evaluation is the \
             Windows default; we apply entries as given)"
        );
    }

    // ── validate_principal: SDDL-breaking tokens rejected ────────────────────

    #[test]
    fn validate_principal_empty_returns_unspecified() {
        let err = super::validate_principal("", "/p").unwrap_err();
        assert_eq!(err.code, ahand_protocol::FileErrorCode::Unspecified as i32);
        assert!(
            err.message.contains("empty principal"),
            "got: {}",
            err.message
        );
    }

    #[test]
    fn validate_principal_rejects_colon() {
        // "Everyone:F" — classic perm-smuggling attempt.
        let err = super::validate_principal("Everyone:F", "/p").unwrap_err();
        assert_eq!(err.code, ahand_protocol::FileErrorCode::InvalidPath as i32);
        assert!(
            err.message.contains("malformed principal"),
            "got: {}",
            err.message
        );
    }

    #[test]
    fn validate_principal_rejects_sddl_breaking_chars() {
        // ';', '(', ')' would break out of / corrupt the SDDL ACE token.
        for bad in ["a;b", "a(b", "a)b", "WD;DA"] {
            let err = super::validate_principal(bad, "/p").unwrap_err();
            assert_eq!(
                err.code,
                ahand_protocol::FileErrorCode::InvalidPath as i32,
                "principal {bad:?} must be rejected"
            );
        }
    }

    #[test]
    fn validate_principal_rejects_whitespace_and_leading_slash() {
        assert_eq!(
            super::validate_principal("Local Service", "/p")
                .unwrap_err()
                .code,
            ahand_protocol::FileErrorCode::InvalidPath as i32
        );
        assert_eq!(
            super::validate_principal("/grant", "/p").unwrap_err().code,
            ahand_protocol::FileErrorCode::InvalidPath as i32
        );
    }

    #[test]
    fn validate_principal_accepts_valid_domain_account() {
        // A normal `DOMAIN\account` principal must NOT be rejected.
        assert!(super::validate_principal("BUILTIN\\Users", "/p").is_ok());
        assert!(super::validate_principal("DOMAIN\\user", "/p").is_ok());
        assert!(super::validate_principal("Everyone", "/p").is_ok());
        assert!(super::validate_principal("S-1-5-18", "/p").is_ok());
    }
}
