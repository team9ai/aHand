//! Integration tests for daemon file operations.
//!
//! These tests drive the `FileManager::handle` entry point directly (no hub or
//! WebSocket involved), with a permissive policy scoped to a per-test temp dir.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ahand_protocol::{
    ByteRangeReplace, DeleteMode, FileAppend, FileChmod, FileCopy, FileCreateSymlink, FileDelete,
    FileEdit, FileErrorCode, FileGlob, FileList, FileMkdir, FileMove, FilePosition, FileReadBinary,
    FileReadImage, FileReadText, FileRequest, FileStat, FileType, FileWrite, FullWrite,
    ImageFormat, LineCol, LineRangeReplace, StopReason, StringReplace, UnixPermission, WriteAction,
    file_chmod, file_edit, file_position, file_read_text, file_request, file_response, file_write,
    full_write,
};
use ahandd::config::FilePolicyConfig;
use ahandd::file_manager::FileManager;
use tempfile::TempDir;

// ── Fixtures ───────────────────────────────────────────────────────────────

/// Set up a permissive file manager scoped to the given temp directory.
///
/// Returns `(manager, canonical_root)`. Tests should use the returned canonical
/// root (not `tmp.path()`) when constructing paths, since macOS
/// `/var/folders/...` becomes `/private/var/folders/...` after canonicalization
/// and our policy checker doesn't resolve symlinks.
fn test_manager(tmp: &TempDir) -> (FileManager, PathBuf) {
    let root = tmp
        .path()
        .canonicalize()
        .expect("tempdir canonicalization should succeed");
    let root_str = root.to_string_lossy().into_owned();
    let pattern = format!("{}/**", root_str.trim_end_matches('/'));
    let self_pattern = root_str;

    let mgr = FileManager::new(&FilePolicyConfig {
        enabled: true,
        path_allowlist: vec![pattern, self_pattern],
        path_denylist: vec![],
        max_read_bytes: 100_000_000,
        max_write_bytes: 100_000_000,
        dangerous_paths: vec![],
    });
    (mgr, root)
}

fn stat_request(path: &Path) -> FileRequest {
    FileRequest {
        request_id: "test".to_string(),
        operation: Some(file_request::Operation::Stat(FileStat {
            path: path.to_string_lossy().into_owned(),
            no_follow_symlink: false,
        })),
    }
}

fn stat_request_no_follow(path: &Path) -> FileRequest {
    FileRequest {
        request_id: "test".to_string(),
        operation: Some(file_request::Operation::Stat(FileStat {
            path: path.to_string_lossy().into_owned(),
            no_follow_symlink: true,
        })),
    }
}

fn expect_stat(resp: ahand_protocol::FileResponse) -> ahand_protocol::FileStatResult {
    match resp.result {
        Some(file_response::Result::Stat(r)) => r,
        other => panic!("expected stat result, got {other:?}"),
    }
}

fn expect_list(resp: ahand_protocol::FileResponse) -> ahand_protocol::FileListResult {
    match resp.result {
        Some(file_response::Result::List(r)) => r,
        other => panic!("expected list result, got {other:?}"),
    }
}

fn expect_glob(resp: ahand_protocol::FileResponse) -> ahand_protocol::FileGlobResult {
    match resp.result {
        Some(file_response::Result::Glob(r)) => r,
        other => panic!("expected glob result, got {other:?}"),
    }
}

fn expect_mkdir(resp: ahand_protocol::FileResponse) -> ahand_protocol::FileMkdirResult {
    match resp.result {
        Some(file_response::Result::Mkdir(r)) => r,
        other => panic!("expected mkdir result, got {other:?}"),
    }
}

fn expect_read_text(resp: ahand_protocol::FileResponse) -> ahand_protocol::FileReadTextResult {
    match resp.result {
        Some(file_response::Result::ReadText(r)) => r,
        other => panic!("expected read_text result, got {other:?}"),
    }
}

fn read_text_request(path: &Path) -> FileReadText {
    FileReadText {
        path: path.to_string_lossy().into_owned(),
        start: None,
        max_lines: None,
        max_bytes: None,
        target_end: None,
        max_line_width: None,
        encoding: None,
        line_numbers: true,
        no_follow_symlink: false,
    }
}

fn wrap_read_text(req: FileReadText) -> FileRequest {
    FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::ReadText(req)),
    }
}

fn expect_read_binary(resp: ahand_protocol::FileResponse) -> ahand_protocol::FileReadBinaryResult {
    match resp.result {
        Some(file_response::Result::ReadBinary(r)) => r,
        other => panic!("expected read_binary result, got {other:?}"),
    }
}

fn expect_read_image(resp: ahand_protocol::FileResponse) -> ahand_protocol::FileReadImageResult {
    match resp.result {
        Some(file_response::Result::ReadImage(r)) => r,
        other => panic!("expected read_image result, got {other:?}"),
    }
}

fn expect_write(resp: ahand_protocol::FileResponse) -> ahand_protocol::FileWriteResult {
    match resp.result {
        Some(file_response::Result::Write(r)) => r,
        other => panic!("expected write result, got {other:?}"),
    }
}

fn expect_edit(resp: ahand_protocol::FileResponse) -> ahand_protocol::FileEditResult {
    match resp.result {
        Some(file_response::Result::Edit(r)) => r,
        other => panic!("expected edit result, got {other:?}"),
    }
}

fn expect_delete(resp: ahand_protocol::FileResponse) -> ahand_protocol::FileDeleteResult {
    match resp.result {
        Some(file_response::Result::Delete(r)) => r,
        other => panic!("expected delete result, got {other:?}"),
    }
}

fn expect_copy(resp: ahand_protocol::FileResponse) -> ahand_protocol::FileCopyResult {
    match resp.result {
        Some(file_response::Result::Copy(r)) => r,
        other => panic!("expected copy result, got {other:?}"),
    }
}

fn expect_move(resp: ahand_protocol::FileResponse) -> ahand_protocol::FileMoveResult {
    match resp.result {
        Some(file_response::Result::MoveResult(r)) => r,
        other => panic!("expected move result, got {other:?}"),
    }
}

fn expect_symlink(resp: ahand_protocol::FileResponse) -> ahand_protocol::FileCreateSymlinkResult {
    match resp.result {
        Some(file_response::Result::CreateSymlink(r)) => r,
        other => panic!("expected create_symlink result, got {other:?}"),
    }
}

fn expect_chmod(resp: ahand_protocol::FileResponse) -> ahand_protocol::FileChmodResult {
    match resp.result {
        Some(file_response::Result::Chmod(r)) => r,
        other => panic!("expected chmod result, got {other:?}"),
    }
}

fn write_request_full(path: &Path, content: &[u8], create_parents: bool) -> FileRequest {
    FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::Write(FileWrite {
            path: path.to_string_lossy().into_owned(),
            create_parents,
            encoding: None,
            no_follow_symlink: false,
            method: Some(file_write::Method::FullWrite(FullWrite {
                source: Some(full_write::Source::Content(content.to_vec())),
                ..Default::default()
            })),
        })),
    }
}

fn expect_error(resp: ahand_protocol::FileResponse) -> ahand_protocol::FileError {
    match resp.result {
        Some(file_response::Result::Error(e)) => e,
        other => panic!("expected error, got {other:?}"),
    }
}

// ── FileStat tests ─────────────────────────────────────────────────────────

#[tokio::test]
async fn stat_file_returns_correct_type_and_size() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let file = root.join("hello.txt");
    fs::write(&file, "hello").unwrap();

    let resp = mgr.handle(&stat_request(&file)).await;
    let stat = expect_stat(resp);

    assert_eq!(stat.file_type, FileType::File as i32);
    assert_eq!(stat.size, 5);
    assert!(stat.unix_permission.is_some());
}

#[tokio::test]
async fn stat_directory() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let sub = root.join("sub");
    fs::create_dir(&sub).unwrap();

    let resp = mgr.handle(&stat_request(&sub)).await;
    let stat = expect_stat(resp);

    assert_eq!(stat.file_type, FileType::Directory as i32);
}

#[cfg(unix)]
#[tokio::test]
async fn stat_symlink_follow_returns_target_type() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let target = root.join("target.txt");
    fs::write(&target, "hi").unwrap();
    let link = root.join("link");
    std::os::unix::fs::symlink(&target, &link).unwrap();

    let resp = mgr.handle(&stat_request(&link)).await;
    let stat = expect_stat(resp);

    // follow: behaves like target
    assert_eq!(stat.file_type, FileType::File as i32);
    assert_eq!(stat.size, 2);
}

#[cfg(unix)]
#[tokio::test]
async fn stat_symlink_no_follow_returns_symlink_type() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let target = root.join("target.txt");
    fs::write(&target, "hi").unwrap();
    let link = root.join("link");
    std::os::unix::fs::symlink(&target, &link).unwrap();

    let resp = mgr.handle(&stat_request_no_follow(&link)).await;
    let stat = expect_stat(resp);

    assert_eq!(stat.file_type, FileType::Symlink as i32);
    assert!(stat.symlink_target.is_some());
}

#[tokio::test]
async fn stat_not_found_returns_error() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let missing = root.join("nope.txt");

    let resp = mgr.handle(&stat_request(&missing)).await;
    let err = expect_error(resp);
    assert_eq!(err.code, FileErrorCode::NotFound as i32);
}

// ── FileList tests ─────────────────────────────────────────────────────────

#[tokio::test]
async fn list_directory_basic() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    fs::write(root.join("a.txt"), "a").unwrap();
    fs::write(root.join("b.txt"), "bb").unwrap();
    fs::write(root.join("c.txt"), "ccc").unwrap();

    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::List(FileList {
            path: root.to_string_lossy().into_owned(),
            max_results: None,
            offset: None,
            include_hidden: false,
        })),
    };
    let resp = mgr.handle(&req).await;
    let list = expect_list(resp);
    assert_eq!(list.total_count, 3);
    assert_eq!(list.entries.len(), 3);
    assert!(!list.has_more);
}

#[tokio::test]
async fn list_offset_beyond_retain_cap_returns_invalid_path() {
    // C4 regression: the bounded heap can only retain 100_000 entries,
    // so a paginated request whose `offset + max_results` exceeds that
    // window cannot be served accurately. We must reject up front
    // rather than silently return an empty page with `has_more = true`,
    // which would loop the caller forever. The guard fires before any
    // directory walk, so this test is cheap regardless of dir size.
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    fs::write(root.join("a.txt"), "a").unwrap();

    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::List(FileList {
            path: root.to_string_lossy().into_owned(),
            // 100_000 + 1 > LIST_HEAP_RETAIN_CAP (100_000).
            max_results: Some(1),
            offset: Some(100_000),
            include_hidden: false,
        })),
    };
    let resp = mgr.handle(&req).await;
    let err = expect_error(resp);
    assert_eq!(err.code, FileErrorCode::InvalidPath as i32);
    assert!(
        err.message.contains("100000") || err.message.contains("listing window"),
        "error should mention the cap, got: {}",
        err.message
    );
}

#[tokio::test]
async fn list_pagination() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    for i in 0..10 {
        fs::write(root.join(format!("f{i}.txt")), "x").unwrap();
        // Stagger mtimes to make sort deterministic for pagination test.
        std::thread::sleep(Duration::from_millis(2));
    }

    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::List(FileList {
            path: root.to_string_lossy().into_owned(),
            max_results: Some(4),
            offset: Some(2),
            include_hidden: false,
        })),
    };
    let resp = mgr.handle(&req).await;
    let list = expect_list(resp);
    assert_eq!(list.total_count, 10);
    assert_eq!(list.entries.len(), 4);
    assert!(list.has_more);
}

#[tokio::test]
async fn list_hidden_files_filtered_by_default() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    fs::write(root.join(".hidden"), "x").unwrap();
    fs::write(root.join("visible.txt"), "x").unwrap();

    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::List(FileList {
            path: root.to_string_lossy().into_owned(),
            max_results: None,
            offset: None,
            include_hidden: false,
        })),
    };
    let resp = mgr.handle(&req).await;
    let list = expect_list(resp);
    assert_eq!(list.total_count, 1);
    assert_eq!(list.entries[0].name, "visible.txt");
}

#[tokio::test]
async fn list_include_hidden_returns_all() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    fs::write(root.join(".hidden"), "x").unwrap();
    fs::write(root.join("visible.txt"), "x").unwrap();

    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::List(FileList {
            path: root.to_string_lossy().into_owned(),
            max_results: None,
            offset: None,
            include_hidden: true,
        })),
    };
    let resp = mgr.handle(&req).await;
    let list = expect_list(resp);
    assert_eq!(list.total_count, 2);
}

#[tokio::test]
async fn list_not_a_directory_returns_error() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let file = root.join("a.txt");
    fs::write(&file, "x").unwrap();

    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::List(FileList {
            path: file.to_string_lossy().into_owned(),
            max_results: None,
            offset: None,
            include_hidden: false,
        })),
    };
    let resp = mgr.handle(&req).await;
    let err = expect_error(resp);
    assert_eq!(err.code, FileErrorCode::NotADirectory as i32);
}

/// Regression for R5: a symlink inside the listed directory must surface as
/// `FileType::Symlink` with a `symlink_target`, and must NOT leak the target
/// file's size/type/mtime. Previously `handle_list` called
/// `entry.metadata()` which follows symlinks, so a symlink pointing at
/// `/etc/hosts` would report `/etc/hosts`'s metadata to the caller.
#[cfg(unix)]
#[tokio::test]
async fn list_does_not_follow_symlink_into_target_metadata() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    // Create a real file with a known size inside the sandbox.
    let target = root.join("real.txt");
    fs::write(&target, "1234567890").unwrap();
    // Create a symlink in the same directory pointing OUTSIDE the sandbox.
    // /etc/hosts is a stable system file on macOS/Linux.
    let link = root.join("link-out");
    std::os::unix::fs::symlink("/etc/hosts", &link).unwrap();

    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::List(FileList {
            path: root.to_string_lossy().into_owned(),
            max_results: None,
            offset: None,
            include_hidden: false,
        })),
    };
    let resp = mgr.handle(&req).await;
    let list = expect_list(resp);
    let link_entry = list
        .entries
        .iter()
        .find(|e| e.name == "link-out")
        .expect("link-out must appear in listing");
    // The link must be reported as Symlink type, not File or Directory.
    assert_eq!(
        link_entry.file_type,
        FileType::Symlink as i32,
        "symlink must not be resolved to its target's file type"
    );
    // The target string must be populated so callers can see what it points at.
    assert_eq!(
        link_entry.symlink_target.as_deref(),
        Some("/etc/hosts"),
        "symlink_target should reflect the literal link target"
    );
    // And the real file must still be listed correctly alongside it.
    let real_entry = list
        .entries
        .iter()
        .find(|e| e.name == "real.txt")
        .expect("real.txt must appear in listing");
    assert_eq!(real_entry.file_type, FileType::File as i32);
    assert_eq!(real_entry.size, 10);
}

// ── FileGlob tests ─────────────────────────────────────────────────────────

#[tokio::test]
async fn glob_matches_pattern() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    fs::write(root.join("a.rs"), "x").unwrap();
    fs::write(root.join("b.rs"), "x").unwrap();
    fs::write(root.join("c.txt"), "x").unwrap();

    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::Glob(FileGlob {
            pattern: "*.rs".into(),
            base_path: Some(root.to_string_lossy().into_owned()),
            max_results: None,
        })),
    };
    let resp = mgr.handle(&req).await;
    let glob = expect_glob(resp);
    assert_eq!(glob.total_matches, 2);
    assert_eq!(glob.entries.len(), 2);
}

#[tokio::test]
async fn glob_recursive() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let sub = root.join("sub");
    fs::create_dir(&sub).unwrap();
    fs::write(root.join("top.rs"), "x").unwrap();
    fs::write(sub.join("deep.rs"), "x").unwrap();

    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::Glob(FileGlob {
            pattern: "**/*.rs".into(),
            base_path: Some(root.to_string_lossy().into_owned()),
            max_results: None,
        })),
    };
    let resp = mgr.handle(&req).await;
    let glob = expect_glob(resp);
    assert!(glob.total_matches >= 2);
}

#[tokio::test]
async fn glob_absolute_pattern_without_base_is_rejected() {
    // Without a base_path, an absolute pattern like `/etc/**` would let
    // glob iterate the entire filesystem. Must be rejected up front.
    let dir = TempDir::new().unwrap();
    let (mgr, _root) = test_manager(&dir);
    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::Glob(FileGlob {
            pattern: "/etc/**".into(),
            base_path: None,
            max_results: None,
        })),
    };
    let resp = mgr.handle(&req).await;
    let err = expect_error(resp);
    assert_eq!(err.code, FileErrorCode::InvalidPath as i32);
}

#[tokio::test]
async fn glob_traversal_pattern_is_rejected() {
    // `../` inside a pattern would let the matcher escape the base dir.
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::Glob(FileGlob {
            pattern: "../*.rs".into(),
            base_path: Some(root.to_string_lossy().into_owned()),
            max_results: None,
        })),
    };
    let resp = mgr.handle(&req).await;
    let err = expect_error(resp);
    assert_eq!(err.code, FileErrorCode::InvalidPath as i32);
}

#[cfg(unix)]
#[tokio::test]
async fn glob_skips_symlink_pointing_outside_allowlist() {
    // A `**` pattern inside the allowlist still matches symlinks whose
    // canonical target lies outside. handle_glob re-checks policy per match
    // and silently filters those out.
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    fs::write(root.join("inside.rs"), "x").unwrap();
    // Symlink inside root pointing at a real file outside the allowlist.
    std::os::unix::fs::symlink("/etc/hosts", root.join("escape.rs")).unwrap();

    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::Glob(FileGlob {
            pattern: "**/*.rs".into(),
            base_path: Some(root.to_string_lossy().into_owned()),
            max_results: None,
        })),
    };
    let resp = mgr.handle(&req).await;
    let glob = expect_glob(resp);
    // Only `inside.rs` should survive — `escape.rs` gets filtered out.
    assert_eq!(glob.total_matches, 1);
    assert!(glob.entries.iter().any(|e| e.name.ends_with("inside.rs")));
    assert!(glob.entries.iter().all(|e| !e.name.ends_with("escape.rs")));
}

// ── FileMkdir tests ────────────────────────────────────────────────────────

#[tokio::test]
async fn mkdir_creates_new_directory() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let new_dir = root.join("new_dir");

    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::Mkdir(FileMkdir {
            path: new_dir.to_string_lossy().into_owned(),
            recursive: false,
            mode: None,
        })),
    };
    let resp = mgr.handle(&req).await;
    let result = expect_mkdir(resp);
    assert!(!result.already_existed);
    assert!(new_dir.is_dir());
}

#[tokio::test]
async fn mkdir_recursive_creates_intermediate_directories() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let nested = root.join("a/b/c/d");

    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::Mkdir(FileMkdir {
            path: nested.to_string_lossy().into_owned(),
            recursive: true,
            mode: None,
        })),
    };
    let resp = mgr.handle(&req).await;
    expect_mkdir(resp);
    assert!(nested.is_dir());
}

#[tokio::test]
async fn mkdir_already_exists_marks_flag() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let existing = root.join("existing");
    fs::create_dir(&existing).unwrap();

    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::Mkdir(FileMkdir {
            path: existing.to_string_lossy().into_owned(),
            recursive: false,
            mode: None,
        })),
    };
    let resp = mgr.handle(&req).await;
    let result = expect_mkdir(resp);
    assert!(result.already_existed);
}

#[cfg(unix)]
#[tokio::test]
async fn mkdir_with_mode_sets_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let new_dir = root.join("perm_dir");

    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::Mkdir(FileMkdir {
            path: new_dir.to_string_lossy().into_owned(),
            recursive: false,
            mode: Some(0o700),
        })),
    };
    let resp = mgr.handle(&req).await;
    expect_mkdir(resp);
    let perms = fs::metadata(&new_dir).unwrap().permissions();
    assert_eq!(perms.mode() & 0o777, 0o700);
}

#[tokio::test]
async fn mkdir_path_exists_as_file_returns_error() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let existing_file = root.join("afile");
    fs::write(&existing_file, "x").unwrap();

    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::Mkdir(FileMkdir {
            path: existing_file.to_string_lossy().into_owned(),
            recursive: false,
            mode: None,
        })),
    };
    let resp = mgr.handle(&req).await;
    let err = expect_error(resp);
    assert_eq!(err.code, FileErrorCode::AlreadyExists as i32);
}

// ── Policy integration ────────────────────────────────────────────────────

#[tokio::test]
async fn stat_outside_allowlist_is_denied() {
    let dir = TempDir::new().unwrap();
    let (mgr, _root) = test_manager(&dir);

    // Try to stat a path outside the allowlist.
    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::Stat(FileStat {
            path: "/etc/hosts".into(),
            no_follow_symlink: false,
        })),
    };
    let resp = mgr.handle(&req).await;
    let err = expect_error(resp);
    assert_eq!(err.code, FileErrorCode::PolicyDenied as i32);
}

#[tokio::test]
async fn path_traversal_is_rejected() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);

    // Craft a path that passes through the allowlist directory textually but
    // includes a traversal component.
    let traversal = format!("{}/../../../etc/passwd", root.to_string_lossy());
    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::Stat(FileStat {
            path: traversal,
            no_follow_symlink: false,
        })),
    };
    let resp = mgr.handle(&req).await;
    let err = expect_error(resp);
    assert_eq!(err.code, FileErrorCode::InvalidPath as i32);
}

#[tokio::test]
async fn disabled_file_manager_rejects_everything() {
    let dir = TempDir::new().unwrap();
    let _ = dir;
    let mgr = FileManager::new(&FilePolicyConfig::default());

    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::Stat(FileStat {
            path: "/whatever".into(),
            no_follow_symlink: false,
        })),
    };
    let resp = mgr.handle(&req).await;
    let err = expect_error(resp);
    assert_eq!(err.code, FileErrorCode::PolicyDenied as i32);
}

// ── FileReadText tests ─────────────────────────────────────────────────────

#[tokio::test]
async fn read_text_basic_reads_all_lines() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let file = root.join("simple.txt");
    fs::write(&file, "line1\nline2\nline3\n").unwrap();

    let resp = mgr.handle(&wrap_read_text(read_text_request(&file))).await;
    let result = expect_read_text(resp);
    assert_eq!(result.lines.len(), 3);
    assert_eq!(result.lines[0].content, "line1");
    assert_eq!(result.lines[1].content, "line2");
    assert_eq!(result.lines[2].content, "line3");
    assert_eq!(result.stop_reason, StopReason::FileEnd as i32);
    assert_eq!(result.total_file_bytes, 18);
    assert_eq!(result.remaining_bytes, 0);
    assert_eq!(result.detected_encoding, "UTF-8");
}

#[tokio::test]
async fn read_text_respects_max_lines() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let file = root.join("hundred.txt");
    let content: String = (1..=100).map(|i| format!("line{i}\n")).collect();
    fs::write(&file, &content).unwrap();

    let mut req = read_text_request(&file);
    req.max_lines = Some(5);
    req.max_bytes = Some(10_000_000);

    let resp = mgr.handle(&wrap_read_text(req)).await;
    let result = expect_read_text(resp);
    assert_eq!(result.lines.len(), 5);
    assert_eq!(result.stop_reason, StopReason::MaxLines as i32);
    assert_eq!(result.lines[4].content, "line5");
    assert!(result.remaining_bytes > 0);
}

#[tokio::test]
async fn read_text_respects_max_bytes() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let file = root.join("fiftysome.txt");
    // 100 lines, each "x" * 50 = 51 bytes incl. newline → 5100 bytes total.
    let mut content = String::new();
    for _ in 0..100 {
        content.push_str(&"x".repeat(50));
        content.push('\n');
    }
    fs::write(&file, &content).unwrap();

    let mut req = read_text_request(&file);
    req.max_lines = Some(10_000);
    req.max_bytes = Some(120);

    let resp = mgr.handle(&wrap_read_text(req)).await;
    let result = expect_read_text(resp);
    assert_eq!(result.stop_reason, StopReason::MaxBytes as i32);
    assert!(result.lines.len() <= 3); // at most 2-3 lines
    assert!(result.lines.len() >= 1);
}

#[tokio::test]
async fn read_text_respects_target_end_line() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let file = root.join("target.txt");
    let content: String = (1..=10).map(|i| format!("line{i}\n")).collect();
    fs::write(&file, &content).unwrap();

    let mut req = read_text_request(&file);
    req.target_end = Some(FilePosition {
        position: Some(file_position::Position::Line(5)),
    });

    let resp = mgr.handle(&wrap_read_text(req)).await;
    let result = expect_read_text(resp);
    assert_eq!(result.stop_reason, StopReason::TargetEnd as i32);
    assert_eq!(result.lines.len(), 4); // lines 1..4 before target line 5
    assert_eq!(result.lines.last().unwrap().content, "line4");
}

#[tokio::test]
async fn read_text_start_at_line() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let file = root.join("startline.txt");
    let content: String = (1..=10).map(|i| format!("line{i}\n")).collect();
    fs::write(&file, &content).unwrap();

    let mut req = read_text_request(&file);
    req.start = Some(file_read_text::Start::StartLine(3));

    let resp = mgr.handle(&wrap_read_text(req)).await;
    let result = expect_read_text(resp);
    assert_eq!(result.lines.first().unwrap().content, "line3");
    assert_eq!(result.lines.first().unwrap().line_number, 3);
    assert_eq!(result.start_pos.unwrap().line, 3);
}

#[tokio::test]
async fn read_text_start_at_byte() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let file = root.join("startbyte.txt");
    fs::write(&file, "abcdefghij").unwrap();

    let mut req = read_text_request(&file);
    req.start = Some(file_read_text::Start::StartByte(4));

    let resp = mgr.handle(&wrap_read_text(req)).await;
    let result = expect_read_text(resp);
    assert_eq!(result.lines.len(), 1);
    assert_eq!(result.lines[0].content, "efghij");
    assert_eq!(result.start_pos.unwrap().byte_in_file, 4);
}

#[tokio::test]
async fn read_text_start_at_line_col() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let file = root.join("linecol.txt");
    fs::write(&file, "first\nsecond\nthird\n").unwrap();

    let mut req = read_text_request(&file);
    req.start = Some(file_read_text::Start::StartLineCol(LineCol {
        line: 2,
        col: 3,
    }));

    let resp = mgr.handle(&wrap_read_text(req)).await;
    let result = expect_read_text(resp);
    assert_eq!(result.lines.first().unwrap().content, "ond");
    let start = result.start_pos.unwrap();
    assert_eq!(start.line, 2);
    assert_eq!(start.byte_in_line, 3);
}

#[tokio::test]
async fn read_text_line_truncation_marks_flag_and_remaining() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let file = root.join("long.txt");
    let line = "x".repeat(1000);
    fs::write(&file, format!("{line}\n")).unwrap();

    let mut req = read_text_request(&file);
    req.max_line_width = Some(100);

    let resp = mgr.handle(&wrap_read_text(req)).await;
    let result = expect_read_text(resp);
    assert_eq!(result.lines.len(), 1);
    let line = &result.lines[0];
    assert!(line.truncated);
    assert_eq!(line.content.len(), 100);
    assert_eq!(line.remaining_bytes, 900);
}

#[tokio::test]
async fn read_text_truncates_gbk_in_raw_bytes_not_decoded_bytes() {
    // R0 regression: `truncate_line` must cut the raw on-disk slice at
    // `max_line_width` raw bytes, not the decoded UTF-8 length. For GBK
    // "你好世界" = 8 raw bytes (4 CJK chars × 2 bytes each) but 12 UTF-8
    // bytes after decoding — the old impl mixed these up and returned
    // wrong content + wrong remaining_bytes. With max_line_width=4 we
    // should keep the first 4 raw bytes (= 2 GBK chars = "你好") and
    // report remaining_bytes=4.
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let file = root.join("gbk-trunc.txt");
    let gbk: Vec<u8> = vec![0xC4, 0xE3, 0xBA, 0xC3, 0xCA, 0xC0, 0xBD, 0xE7];
    fs::write(&file, &gbk).unwrap();

    let mut req = read_text_request(&file);
    req.encoding = Some("gbk".into());
    req.max_line_width = Some(4);

    let resp = mgr.handle(&wrap_read_text(req)).await;
    let result = expect_read_text(resp);
    assert_eq!(result.lines.len(), 1);
    let line = &result.lines[0];
    assert!(line.truncated);
    assert_eq!(line.content, "你好");
    assert_eq!(line.remaining_bytes, 4);
}

#[tokio::test]
async fn read_text_empty_file() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let file = root.join("empty.txt");
    fs::write(&file, "").unwrap();

    let resp = mgr.handle(&wrap_read_text(read_text_request(&file))).await;
    let result = expect_read_text(resp);
    assert_eq!(result.lines.len(), 0);
    assert_eq!(result.stop_reason, StopReason::FileEnd as i32);
    assert_eq!(result.total_file_bytes, 0);
}

#[tokio::test]
async fn read_text_remaining_bytes_after_limit() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let file = root.join("remain.txt");
    // 10 lines of "abc\n" = 40 bytes total.
    let content = "abc\n".repeat(10);
    fs::write(&file, &content).unwrap();

    let mut req = read_text_request(&file);
    req.max_lines = Some(3);

    let resp = mgr.handle(&wrap_read_text(req)).await;
    let result = expect_read_text(resp);
    assert_eq!(result.lines.len(), 3);
    // After 3 lines (12 bytes), 28 bytes remain.
    assert_eq!(result.remaining_bytes, 28);
    assert_eq!(result.total_file_bytes, 40);
}

#[tokio::test]
async fn read_text_utf8_multibyte_not_split() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let file = root.join("utf8.txt");
    // "你好世界" is 12 UTF-8 bytes (3 bytes per char).
    fs::write(&file, "你好世界\n").unwrap();

    let mut req = read_text_request(&file);
    req.max_line_width = Some(7); // Would split in the middle of "世" (byte 6-8)

    let resp = mgr.handle(&wrap_read_text(req)).await;
    let result = expect_read_text(resp);
    let line = &result.lines[0];
    // Truncated content must be valid UTF-8 and not contain partial codepoints.
    assert!(line.truncated);
    assert!(line.content.chars().all(|c| c != '\u{FFFD}'));
}

#[tokio::test]
async fn read_text_encoding_forced_gbk() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let file = root.join("gbk.txt");
    // Encode "你好" in GBK manually.
    let gbk_bytes: [u8; 4] = [0xC4, 0xE3, 0xBA, 0xC3];
    fs::write(&file, gbk_bytes).unwrap();

    let mut req = read_text_request(&file);
    req.encoding = Some("gbk".into());

    let resp = mgr.handle(&wrap_read_text(req)).await;
    let result = expect_read_text(resp);
    assert_eq!(result.lines.len(), 1);
    assert_eq!(result.lines[0].content, "你好");
    assert!(
        result
            .detected_encoding
            .to_ascii_lowercase()
            .contains("gbk")
    );
}

#[tokio::test]
async fn read_text_nonexistent_file_returns_error() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let missing = root.join("missing.txt");

    let resp = mgr
        .handle(&wrap_read_text(read_text_request(&missing)))
        .await;
    let err = expect_error(resp);
    assert_eq!(err.code, FileErrorCode::NotFound as i32);
}

// ── FileReadBinary tests ───────────────────────────────────────────────────

fn binary_req(path: &Path, offset: u64, length: u64) -> FileRequest {
    FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::ReadBinary(FileReadBinary {
            path: path.to_string_lossy().into_owned(),
            byte_offset: offset,
            byte_length: length,
            max_bytes: None,
            no_follow_symlink: false,
        })),
    }
}

#[tokio::test]
async fn read_binary_full_file() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let file = root.join("bin.dat");
    let data: Vec<u8> = (0..100u8).collect();
    fs::write(&file, &data).unwrap();

    let resp = mgr.handle(&binary_req(&file, 0, 0)).await;
    let result = expect_read_binary(resp);
    assert_eq!(result.content, data);
    assert_eq!(result.bytes_read, 100);
    assert_eq!(result.total_file_bytes, 100);
    assert_eq!(result.remaining_bytes, 0);
}

#[tokio::test]
async fn read_binary_range() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let file = root.join("range.dat");
    let data: Vec<u8> = (0..100u8).collect();
    fs::write(&file, &data).unwrap();

    let resp = mgr.handle(&binary_req(&file, 20, 30)).await;
    let result = expect_read_binary(resp);
    assert_eq!(result.content, data[20..50].to_vec());
    assert_eq!(result.byte_offset, 20);
    assert_eq!(result.bytes_read, 30);
    assert_eq!(result.remaining_bytes, 50);
}

#[tokio::test]
async fn read_binary_respects_max_bytes() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let file = root.join("big.dat");
    let data = vec![0u8; 10_000];
    fs::write(&file, &data).unwrap();

    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::ReadBinary(FileReadBinary {
            path: file.to_string_lossy().into_owned(),
            byte_offset: 0,
            byte_length: 0,
            max_bytes: Some(1024),
            no_follow_symlink: false,
        })),
    };
    let resp = mgr.handle(&req).await;
    let result = expect_read_binary(resp);
    assert_eq!(result.bytes_read, 1024);
    assert_eq!(result.remaining_bytes, 10_000 - 1024);
}

#[tokio::test]
async fn read_binary_past_eof_returns_empty() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let file = root.join("small.dat");
    fs::write(&file, [1u8, 2, 3]).unwrap();

    let resp = mgr.handle(&binary_req(&file, 100, 10)).await;
    let result = expect_read_binary(resp);
    assert_eq!(result.bytes_read, 0);
    assert_eq!(result.content.len(), 0);
    assert_eq!(result.remaining_bytes, 0);
}

// ── FileReadImage tests ────────────────────────────────────────────────────

/// Write a small synthetic PNG (via the `image` crate) to disk for testing.
fn write_test_png(path: &Path, width: u32, height: u32) {
    use image::{ImageBuffer, Rgb};
    let img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_fn(width, height, |x, y| {
        Rgb([(x & 0xFF) as u8, (y & 0xFF) as u8, 0u8])
    });
    img.save_with_format(path, image::ImageFormat::Png)
        .expect("failed to write test png");
}

fn image_req(path: &Path, max_w: Option<u32>, max_h: Option<u32>) -> FileRequest {
    FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::ReadImage(FileReadImage {
            path: path.to_string_lossy().into_owned(),
            max_width: max_w,
            max_height: max_h,
            max_bytes: None,
            quality: None,
            output_format: None,
            no_follow_symlink: false,
        })),
    }
}

#[tokio::test]
async fn read_image_passthrough_returns_original_format() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let file = root.join("plain.png");
    write_test_png(&file, 100, 50);
    let original_size = fs::metadata(&file).unwrap().len();

    let resp = mgr.handle(&image_req(&file, None, None)).await;
    let result = expect_read_image(resp);
    assert_eq!(result.width, 100);
    assert_eq!(result.height, 50);
    assert_eq!(result.original_bytes, original_size);
    assert_eq!(result.format, ImageFormat::Png as i32);
    assert!(!result.content.is_empty());
}

#[tokio::test]
async fn read_image_resize_preserves_aspect_ratio() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let file = root.join("resize.png");
    write_test_png(&file, 1000, 800);

    let resp = mgr.handle(&image_req(&file, Some(500), None)).await;
    let result = expect_read_image(resp);
    assert_eq!(result.width, 500);
    assert_eq!(result.height, 400);
}

#[tokio::test]
async fn read_image_format_convert_png_to_jpeg() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let file = root.join("convert.png");
    write_test_png(&file, 64, 64);

    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::ReadImage(FileReadImage {
            path: file.to_string_lossy().into_owned(),
            max_width: None,
            max_height: None,
            max_bytes: None,
            quality: Some(80),
            output_format: Some(ImageFormat::Jpeg as i32),
            no_follow_symlink: false,
        })),
    };
    let resp = mgr.handle(&req).await;
    let result = expect_read_image(resp);
    assert_eq!(result.format, ImageFormat::Jpeg as i32);
    // JPEG files start with 0xFF 0xD8 (SOI marker).
    assert_eq!(result.content.first().copied(), Some(0xFF));
    assert_eq!(result.content.get(1).copied(), Some(0xD8));
}

#[tokio::test]
async fn read_image_max_bytes_reduces_jpeg_size() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let file = root.join("busy.png");
    write_test_png(&file, 400, 400);

    // First pass: no max_bytes at quality 100.
    let req_full = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::ReadImage(FileReadImage {
            path: file.to_string_lossy().into_owned(),
            max_width: None,
            max_height: None,
            max_bytes: None,
            quality: Some(100),
            output_format: Some(ImageFormat::Jpeg as i32),
            no_follow_symlink: false,
        })),
    };
    let full = expect_read_image(mgr.handle(&req_full).await);

    // Second pass: force a tight max_bytes that should iteratively drop quality.
    let budget = (full.output_bytes / 2).max(1000);
    let req_budget = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::ReadImage(FileReadImage {
            path: file.to_string_lossy().into_owned(),
            max_width: None,
            max_height: None,
            max_bytes: Some(budget),
            quality: Some(100),
            output_format: Some(ImageFormat::Jpeg as i32),
            no_follow_symlink: false,
        })),
    };
    let reduced = expect_read_image(mgr.handle(&req_budget).await);
    assert!(
        reduced.output_bytes < full.output_bytes,
        "expected reduced output smaller than full: reduced={} full={}",
        reduced.output_bytes,
        full.output_bytes
    );
}

#[tokio::test]
async fn read_image_not_an_image_returns_error() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let file = root.join("plain.txt");
    fs::write(&file, b"not an image").unwrap();

    let resp = mgr.handle(&image_req(&file, None, None)).await;
    let err = expect_error(resp);
    assert_eq!(err.code, FileErrorCode::Unspecified as i32);
}

#[tokio::test]
async fn read_image_input_size_exceeds_max_read_bytes_is_rejected() {
    // Use a tight policy budget and a real image whose on-disk size is
    // larger than the budget. We force the budget below the encoded PNG
    // size so the file-size check trips before the dimension guard.
    use image::{ImageBuffer, Rgb};
    let dir = TempDir::new().unwrap();
    let tmp_root = dir.path().canonicalize().unwrap();
    let root_str = tmp_root.to_string_lossy().into_owned();
    let mgr = ahandd::file_manager::FileManager::new(&ahandd::config::FilePolicyConfig {
        enabled: true,
        path_allowlist: vec![format!("{}/**", root_str), root_str.clone()],
        path_denylist: vec![],
        max_read_bytes: 200,
        max_write_bytes: 100_000_000,
        dangerous_paths: vec![],
    });
    let file = tmp_root.join("big.png");
    // 64x64 random-noise PNG won't compress to under 200 bytes. Use
    // wrapping arithmetic so we don't overflow u8 in debug builds.
    let img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_fn(64, 64, |x, y| {
        Rgb([
            ((x ^ y) as u8).wrapping_mul(7),
            (x.wrapping_mul(y) & 0xFF) as u8,
            ((x.wrapping_add(y)) as u8) ^ 0xAA,
        ])
    });
    img.save_with_format(&file, image::ImageFormat::Png)
        .unwrap();

    let resp = mgr.handle(&image_req(&file, None, None)).await;
    let err = expect_error(resp);
    assert_eq!(err.code, FileErrorCode::TooLarge as i32);
}

#[tokio::test]
async fn read_image_max_bytes_unreachable_returns_too_large() {
    // Generate a JPEG that cannot be compressed below ~1 KB even at the
    // quality floor, then ask for max_bytes=100. The handler must return
    // TooLarge instead of best-effort output.
    use image::{ImageBuffer, Rgb};
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let file = root.join("noise.png");
    // High-entropy image so JPEG can't compress it down.
    let img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_fn(256, 256, |x, y| {
        Rgb([
            (x as u8).wrapping_mul(17) ^ y as u8,
            (x as u8).wrapping_add(y as u8),
            0,
        ])
    });
    img.save_with_format(&file, image::ImageFormat::Png)
        .unwrap();

    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::ReadImage(FileReadImage {
            path: file.to_string_lossy().into_owned(),
            max_width: None,
            max_height: None,
            max_bytes: Some(100),
            quality: Some(100),
            output_format: Some(ImageFormat::Jpeg as i32),
            no_follow_symlink: false,
        })),
    };
    let resp = mgr.handle(&req).await;
    let err = expect_error(resp);
    assert_eq!(err.code, FileErrorCode::TooLarge as i32);
}

// ── FileWrite tests ────────────────────────────────────────────────────────

#[tokio::test]
async fn full_write_creates_new_file() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let file = root.join("new.txt");

    let resp = mgr
        .handle(&write_request_full(&file, b"hello world", false))
        .await;
    let result = expect_write(resp);
    assert_eq!(result.action, WriteAction::Created as i32);
    assert_eq!(result.bytes_written, 11);
    assert_eq!(result.final_size, 11);
    assert_eq!(fs::read_to_string(&file).unwrap(), "hello world");
}

#[tokio::test]
async fn full_write_create_parents_creates_intermediate_dirs() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let nested = root.join("a/b/c/file.txt");

    let resp = mgr.handle(&write_request_full(&nested, b"x", true)).await;
    let result = expect_write(resp);
    assert_eq!(result.action, WriteAction::Created as i32);
    assert!(nested.is_file());
}

#[tokio::test]
async fn full_write_overwrites_existing() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let file = root.join("ex.txt");
    fs::write(&file, "old").unwrap();

    let resp = mgr
        .handle(&write_request_full(&file, b"new content", false))
        .await;
    let result = expect_write(resp);
    assert_eq!(result.action, WriteAction::Overwritten as i32);
    assert_eq!(fs::read_to_string(&file).unwrap(), "new content");
}

#[tokio::test]
async fn file_append_appends_to_existing() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let file = root.join("append.txt");
    fs::write(&file, "hello").unwrap();

    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::Write(FileWrite {
            path: file.to_string_lossy().into_owned(),
            create_parents: false,
            encoding: None,
            no_follow_symlink: false,
            method: Some(file_write::Method::Append(FileAppend {
                content: b" world".to_vec(),
            })),
        })),
    };
    let resp = mgr.handle(&req).await;
    let result = expect_write(resp);
    assert_eq!(result.action, WriteAction::Appended as i32);
    assert_eq!(fs::read_to_string(&file).unwrap(), "hello world");
}

#[tokio::test]
async fn string_replace_write_single() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let file = root.join("sr.txt");
    fs::write(&file, "foo bar foo").unwrap();

    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::Write(FileWrite {
            path: file.to_string_lossy().into_owned(),
            create_parents: false,
            encoding: None,
            no_follow_symlink: false,
            method: Some(file_write::Method::StringReplace(StringReplace {
                old_string: "bar".into(),
                new_string: "BAZ".into(),
                replace_all: false,
            })),
        })),
    };
    let resp = mgr.handle(&req).await;
    let result = expect_write(resp);
    assert_eq!(result.replacements_made, Some(1));
    assert_eq!(fs::read_to_string(&file).unwrap(), "foo BAZ foo");
}

#[tokio::test]
async fn string_replace_write_multiple_without_replace_all_errors() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let file = root.join("sr.txt");
    fs::write(&file, "foo foo foo").unwrap();

    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::Write(FileWrite {
            path: file.to_string_lossy().into_owned(),
            create_parents: false,
            encoding: None,
            no_follow_symlink: false,
            method: Some(file_write::Method::StringReplace(StringReplace {
                old_string: "foo".into(),
                new_string: "BAR".into(),
                replace_all: false,
            })),
        })),
    };
    let resp = mgr.handle(&req).await;
    let err = expect_error(resp);
    assert_eq!(err.code, FileErrorCode::MultipleMatches as i32);
    // File content preserved.
    assert_eq!(fs::read_to_string(&file).unwrap(), "foo foo foo");
}

#[tokio::test]
async fn string_replace_write_replace_all_works() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let file = root.join("sr.txt");
    fs::write(&file, "foo foo foo").unwrap();

    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::Write(FileWrite {
            path: file.to_string_lossy().into_owned(),
            create_parents: false,
            encoding: None,
            no_follow_symlink: false,
            method: Some(file_write::Method::StringReplace(StringReplace {
                old_string: "foo".into(),
                new_string: "BAR".into(),
                replace_all: true,
            })),
        })),
    };
    let resp = mgr.handle(&req).await;
    let result = expect_write(resp);
    assert_eq!(result.replacements_made, Some(3));
    assert_eq!(fs::read_to_string(&file).unwrap(), "BAR BAR BAR");
}

#[tokio::test]
async fn line_range_replace_write() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let file = root.join("lr.txt");
    fs::write(&file, "one\ntwo\nthree\nfour\nfive\n").unwrap();

    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::Write(FileWrite {
            path: file.to_string_lossy().into_owned(),
            create_parents: false,
            encoding: None,
            no_follow_symlink: false,
            method: Some(file_write::Method::LineRangeReplace(LineRangeReplace {
                start_line: 2,
                end_line: 3,
                new_content: "TWO_AND_THREE".into(),
            })),
        })),
    };
    let resp = mgr.handle(&req).await;
    expect_write(resp);
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        "one\nTWO_AND_THREE\nfour\nfive\n"
    );
}

#[tokio::test]
async fn byte_range_replace_write() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let file = root.join("br.bin");
    fs::write(&file, b"0123456789").unwrap();

    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::Write(FileWrite {
            path: file.to_string_lossy().into_owned(),
            create_parents: false,
            encoding: None,
            no_follow_symlink: false,
            method: Some(file_write::Method::ByteRangeReplace(ByteRangeReplace {
                byte_offset: 5,
                byte_length: 3,
                new_content: b"XYZW".to_vec(),
            })),
        })),
    };
    let resp = mgr.handle(&req).await;
    expect_write(resp);
    assert_eq!(fs::read(&file).unwrap(), b"01234XYZW89");
}

#[tokio::test]
async fn write_exceeds_max_bytes_returns_too_large() {
    let dir = TempDir::new().unwrap();
    let _ = dir.path();
    // Use a custom manager with a tight max_write_bytes.
    let tmp_root = dir.path().canonicalize().unwrap();
    let root_str = tmp_root.to_string_lossy().into_owned();
    let mgr = ahandd::file_manager::FileManager::new(&ahandd::config::FilePolicyConfig {
        enabled: true,
        path_allowlist: vec![format!("{}/**", root_str), root_str.clone()],
        path_denylist: vec![],
        max_read_bytes: 100_000_000,
        max_write_bytes: 10,
        dangerous_paths: vec![],
    });
    let file = tmp_root.join("too_big.bin");

    let resp = mgr
        .handle(&write_request_full(&file, &vec![0u8; 100], false))
        .await;
    let err = expect_error(resp);
    assert_eq!(err.code, FileErrorCode::TooLarge as i32);
}

/// Build a tight-budget manager (max_write_bytes = `max`) over `root` so the
/// I3 tests can prove the existing-size pre-check fires before any read.
fn manager_with_max_write(tmp_root: &Path, max: u64) -> ahandd::file_manager::FileManager {
    let root_str = tmp_root.to_string_lossy().into_owned();
    ahandd::file_manager::FileManager::new(&ahandd::config::FilePolicyConfig {
        enabled: true,
        path_allowlist: vec![format!("{}/**", root_str), root_str.clone()],
        path_denylist: vec![],
        max_read_bytes: 100_000_000,
        max_write_bytes: max,
        dangerous_paths: vec![],
    })
}

#[tokio::test]
async fn write_string_replace_refuses_oversized_existing_file_before_read() {
    // I3 regression: `apply_string_replace` used to call
    // `tokio::fs::read_to_string` on the existing file *before* any size
    // check. A 100 GB file would OOM during the read even though
    // post-read `enforce_size_limit` was guaranteed to reject the result.
    // The fix is a stat-then-refuse pre-check; this test exercises it
    // through the Write StringReplace path (which goes through
    // `apply_string_replace`).
    let dir = TempDir::new().unwrap();
    let tmp_root = dir.path().canonicalize().unwrap();
    let mgr = manager_with_max_write(&tmp_root, 50);

    let file = tmp_root.join("big.txt");
    // 200 bytes > max_write_bytes (50). Tiny in absolute terms, but it
    // exercises the same code path that would OOM on 100 GB.
    fs::write(&file, vec![b'a'; 200]).unwrap();

    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::Write(FileWrite {
            path: file.to_string_lossy().into_owned(),
            create_parents: false,
            encoding: None,
            no_follow_symlink: false,
            method: Some(file_write::Method::StringReplace(StringReplace {
                old_string: "a".into(),
                new_string: "b".into(),
                replace_all: false,
            })),
        })),
    };
    let resp = mgr.handle(&req).await;
    let err = expect_error(resp);
    assert_eq!(err.code, FileErrorCode::TooLarge as i32);
    assert!(
        err.message.contains("existing file"),
        "expected the pre-read guard's message, got: {}",
        err.message
    );
}

#[tokio::test]
async fn edit_string_replace_refuses_oversized_existing_file_before_read() {
    // I3 regression via the Edit StringReplace path. Edit has its own
    // inline `read_to_string` (not delegated to apply_string_replace),
    // so the pre-check has to fire there independently.
    let dir = TempDir::new().unwrap();
    let tmp_root = dir.path().canonicalize().unwrap();
    let mgr = manager_with_max_write(&tmp_root, 50);

    let file = tmp_root.join("big.txt");
    fs::write(&file, vec![b'a'; 200]).unwrap();

    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::Edit(FileEdit {
            path: file.to_string_lossy().into_owned(),
            encoding: None,
            no_follow_symlink: false,
            method: Some(file_edit::Method::StringReplace(StringReplace {
                old_string: "a".into(),
                new_string: "b".into(),
                replace_all: false,
            })),
        })),
    };
    let resp = mgr.handle(&req).await;
    let err = expect_error(resp);
    assert_eq!(err.code, FileErrorCode::TooLarge as i32);
    assert!(
        err.message.contains("existing file"),
        "expected the pre-read guard's message, got: {}",
        err.message
    );
}

#[tokio::test]
async fn line_range_replace_refuses_oversized_existing_file_before_read() {
    // I3 regression via the Write LineRangeReplace path
    // (apply_line_range_replace also calls read_to_string on the full
    // file before any size check; the same OOM hazard applies).
    let dir = TempDir::new().unwrap();
    let tmp_root = dir.path().canonicalize().unwrap();
    let mgr = manager_with_max_write(&tmp_root, 50);

    let file = tmp_root.join("big.txt");
    fs::write(&file, vec![b'a'; 200]).unwrap();

    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::Write(FileWrite {
            path: file.to_string_lossy().into_owned(),
            create_parents: false,
            encoding: None,
            no_follow_symlink: false,
            method: Some(file_write::Method::LineRangeReplace(LineRangeReplace {
                start_line: 1,
                end_line: 1,
                new_content: "b".into(),
            })),
        })),
    };
    let resp = mgr.handle(&req).await;
    let err = expect_error(resp);
    assert_eq!(err.code, FileErrorCode::TooLarge as i32);
    assert!(
        err.message.contains("existing file"),
        "expected the pre-read guard's message, got: {}",
        err.message
    );
}

#[tokio::test]
async fn byte_range_replace_refuses_oversized_existing_file_before_read() {
    // I3 regression via the Write ByteRangeReplace path
    // (apply_byte_range_replace uses raw `tokio::fs::read` rather than
    // `read_to_string`, so the size guard has to live on this branch
    // separately even though the OOM hazard is identical).
    let dir = TempDir::new().unwrap();
    let tmp_root = dir.path().canonicalize().unwrap();
    let mgr = manager_with_max_write(&tmp_root, 50);

    let file = tmp_root.join("big.bin");
    fs::write(&file, vec![0u8; 200]).unwrap();

    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::Write(FileWrite {
            path: file.to_string_lossy().into_owned(),
            create_parents: false,
            encoding: None,
            no_follow_symlink: false,
            method: Some(file_write::Method::ByteRangeReplace(ByteRangeReplace {
                byte_offset: 0,
                byte_length: 1,
                new_content: vec![1u8],
            })),
        })),
    };
    let resp = mgr.handle(&req).await;
    let err = expect_error(resp);
    assert_eq!(err.code, FileErrorCode::TooLarge as i32);
    assert!(
        err.message.contains("existing file"),
        "expected the pre-read guard's message, got: {}",
        err.message
    );
}

// ── FileEdit tests ─────────────────────────────────────────────────────────

#[tokio::test]
async fn edit_nonexistent_file_returns_not_found() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let missing = root.join("nope.txt");

    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::Edit(FileEdit {
            path: missing.to_string_lossy().into_owned(),
            encoding: None,
            no_follow_symlink: false,
            method: Some(file_edit::Method::StringReplace(StringReplace {
                old_string: "x".into(),
                new_string: "y".into(),
                replace_all: false,
            })),
        })),
    };
    let resp = mgr.handle(&req).await;
    let err = expect_error(resp);
    assert_eq!(err.code, FileErrorCode::NotFound as i32);
}

#[tokio::test]
async fn edit_string_replace_missing_reports_match_error() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let file = root.join("ex.txt");
    fs::write(&file, "hello").unwrap();

    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::Edit(FileEdit {
            path: file.to_string_lossy().into_owned(),
            encoding: None,
            no_follow_symlink: false,
            method: Some(file_edit::Method::StringReplace(StringReplace {
                old_string: "world".into(),
                new_string: "friends".into(),
                replace_all: false,
            })),
        })),
    };
    let resp = mgr.handle(&req).await;
    let result = expect_edit(resp);
    assert_eq!(result.replacements_made, Some(0));
    assert!(result.match_error.is_some());
    // File content unchanged.
    assert_eq!(fs::read_to_string(&file).unwrap(), "hello");
}

#[tokio::test]
async fn edit_string_replace_multiple_reports_match_error() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let file = root.join("ex.txt");
    fs::write(&file, "foo foo foo").unwrap();

    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::Edit(FileEdit {
            path: file.to_string_lossy().into_owned(),
            encoding: None,
            no_follow_symlink: false,
            method: Some(file_edit::Method::StringReplace(StringReplace {
                old_string: "foo".into(),
                new_string: "bar".into(),
                replace_all: false,
            })),
        })),
    };
    let resp = mgr.handle(&req).await;
    let result = expect_edit(resp);
    assert!(result.match_error.unwrap().contains("multiple matches"));
    assert_eq!(fs::read_to_string(&file).unwrap(), "foo foo foo");
}

#[tokio::test]
async fn edit_string_replace_single_succeeds() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let file = root.join("ex.txt");
    fs::write(&file, "hello world").unwrap();

    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::Edit(FileEdit {
            path: file.to_string_lossy().into_owned(),
            encoding: None,
            no_follow_symlink: false,
            method: Some(file_edit::Method::StringReplace(StringReplace {
                old_string: "world".into(),
                new_string: "friend".into(),
                replace_all: false,
            })),
        })),
    };
    let resp = mgr.handle(&req).await;
    let result = expect_edit(resp);
    assert_eq!(result.replacements_made, Some(1));
    assert!(result.match_error.is_none());
    assert_eq!(fs::read_to_string(&file).unwrap(), "hello friend");
}

// ── FileDelete tests ───────────────────────────────────────────────────────

fn delete_req(path: &Path, permanent: bool, recursive: bool) -> FileRequest {
    FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::Delete(FileDelete {
            path: path.to_string_lossy().into_owned(),
            recursive,
            mode: if permanent {
                DeleteMode::Permanent as i32
            } else {
                DeleteMode::Trash as i32
            },
            no_follow_symlink: false,
        })),
    }
}

#[tokio::test]
async fn delete_permanent_removes_file() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let file = root.join("del.txt");
    fs::write(&file, "x").unwrap();

    let resp = mgr.handle(&delete_req(&file, true, false)).await;
    let result = expect_delete(resp);
    assert_eq!(result.items_deleted, 1);
    assert_eq!(result.mode, DeleteMode::Permanent as i32);
    assert!(!file.exists());
}

#[tokio::test]
async fn delete_permanent_recursive_directory() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let sub = root.join("recdir");
    fs::create_dir(&sub).unwrap();
    fs::write(sub.join("a.txt"), "a").unwrap();
    fs::write(sub.join("b.txt"), "b").unwrap();

    let resp = mgr.handle(&delete_req(&sub, true, true)).await;
    let result = expect_delete(resp);
    assert!(result.items_deleted >= 3); // dir + 2 files
    assert!(!sub.exists());
}

#[tokio::test]
async fn delete_non_recursive_on_non_empty_dir_returns_not_empty() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let sub = root.join("notempty");
    fs::create_dir(&sub).unwrap();
    fs::write(sub.join("x"), "x").unwrap();

    let resp = mgr.handle(&delete_req(&sub, true, false)).await;
    let err = expect_error(resp);
    assert_eq!(err.code, FileErrorCode::NotEmpty as i32);
    assert!(sub.exists());
}

#[tokio::test]
async fn delete_trash_non_recursive_on_non_empty_dir_returns_not_empty() {
    // C5 regression: TRASH used to ignore `recursive` entirely and
    // silently move the whole subtree to trash even when the caller
    // passed `recursive=false`. PERMANENT enforces this guard; TRASH
    // must too. The guard fires before `trash::delete()`, so this
    // test does NOT touch the system trash.
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let sub = root.join("notempty-trash");
    fs::create_dir(&sub).unwrap();
    fs::write(sub.join("x"), "x").unwrap();

    let resp = mgr.handle(&delete_req(&sub, false, false)).await;
    let err = expect_error(resp);
    assert_eq!(err.code, FileErrorCode::NotEmpty as i32);
    assert!(sub.exists(), "directory must remain when guard fires");
    assert!(sub.join("x").exists(), "child must remain when guard fires");
}

// ── Trash path guess (Option B fallback) ──────────────────────────────────

/// `guess_trash_path` must produce a user-visible hint ending in the
/// original basename on platforms where the trash crate actually has a
/// home trash concept (macOS + freedesktop Linux). This test exercises the
/// Option B fallback path that `handle_delete`'s TRASH branch now uses,
/// without actually touching the real system trash.
#[cfg(any(
    target_os = "macos",
    all(
        unix,
        not(target_os = "macos"),
        not(target_os = "ios"),
        not(target_os = "android")
    )
))]
#[test]
fn guess_trash_path_returns_basename_under_home_trash() {
    use ahandd::file_manager::fs_ops::guess_trash_path;

    let original = PathBuf::from("/tmp/some/where/trash-me.txt");
    let guess = guess_trash_path(&original).expect("home trash guess should be available");
    assert!(
        guess.ends_with("trash-me.txt"),
        "guessed path {guess:?} should end with the original basename",
    );

    #[cfg(target_os = "macos")]
    assert!(
        guess.contains("/.Trash/"),
        "on macOS the guess should live under ~/.Trash, got {guess:?}",
    );

    #[cfg(all(
        unix,
        not(target_os = "macos"),
        not(target_os = "ios"),
        not(target_os = "android")
    ))]
    assert!(
        guess.contains("/Trash/files/"),
        "on freedesktop Linux the guess should live under Trash/files, got {guess:?}",
    );
}

/// A path with no basename (e.g. `/`) can't produce a meaningful trash
/// hint. The helper must return `None` in that case rather than panic or
/// produce a garbage path.
#[test]
fn guess_trash_path_returns_none_for_rootless_path() {
    use ahandd::file_manager::fs_ops::guess_trash_path;

    // `Path::file_name` returns `None` for `/` and for empty paths.
    assert!(guess_trash_path(Path::new("/")).is_none());
    assert!(guess_trash_path(Path::new("")).is_none());
}

/// End-to-end check that the TRASH delete mode actually populates
/// `trash_path` via the Option B fallback. This test moves a real file
/// into the user's system trash, so it's gated behind `#[ignore]` — run
/// manually with `cargo test -p ahandd --test file_ops -- --ignored
/// delete_trash_populates_trash_path`.
///
/// Uses the multi-thread tokio flavor because `handle_delete` calls
/// `tokio::task::block_in_place` for the blocking trash move, which
/// panics on the default current-thread runtime.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "touches the real system trash; run manually"]
async fn delete_trash_populates_trash_path() {
    // Guarded by #[ignore] because it actually moves a file to the
    // user's system trash. Run manually with `cargo test -- --ignored`.
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let file = root.join("trash-me.txt");
    fs::write(&file, "goodbye").unwrap();

    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::Delete(FileDelete {
            path: file.to_string_lossy().into_owned(),
            recursive: false,
            mode: DeleteMode::Trash as i32,
            no_follow_symlink: false,
        })),
    };
    let resp = mgr.handle(&req).await;
    let result = expect_delete(resp);
    assert_eq!(result.mode, DeleteMode::Trash as i32);
    assert!(
        result.trash_path.is_some(),
        "trash_path should be populated"
    );
    assert!(!file.exists(), "original file should be gone");
}

// ── FileCopy tests ─────────────────────────────────────────────────────────

fn copy_req(src: &Path, dst: &Path, recursive: bool, overwrite: bool) -> FileRequest {
    FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::Copy(FileCopy {
            source: src.to_string_lossy().into_owned(),
            destination: dst.to_string_lossy().into_owned(),
            recursive,
            overwrite,
        })),
    }
}

#[tokio::test]
async fn copy_file_creates_destination() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let src = root.join("src.txt");
    let dst = root.join("dst.txt");
    fs::write(&src, "hello").unwrap();

    let resp = mgr.handle(&copy_req(&src, &dst, false, false)).await;
    let result = expect_copy(resp);
    assert_eq!(result.items_copied, 1);
    assert_eq!(fs::read_to_string(&dst).unwrap(), "hello");
    assert!(src.exists());
}

#[tokio::test]
async fn copy_without_overwrite_fails_when_destination_exists() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let src = root.join("src.txt");
    let dst = root.join("dst.txt");
    fs::write(&src, "new").unwrap();
    fs::write(&dst, "old").unwrap();

    let resp = mgr.handle(&copy_req(&src, &dst, false, false)).await;
    let err = expect_error(resp);
    assert_eq!(err.code, FileErrorCode::AlreadyExists as i32);
    assert_eq!(fs::read_to_string(&dst).unwrap(), "old");
}

#[tokio::test]
async fn copy_recursive_directory() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let src_dir = root.join("src");
    let dst_dir = root.join("dst");
    fs::create_dir(&src_dir).unwrap();
    fs::write(src_dir.join("a.txt"), "a").unwrap();
    fs::create_dir(src_dir.join("sub")).unwrap();
    fs::write(src_dir.join("sub/b.txt"), "b").unwrap();

    let resp = mgr.handle(&copy_req(&src_dir, &dst_dir, true, false)).await;
    let result = expect_copy(resp);
    assert!(result.items_copied >= 3);
    assert_eq!(fs::read_to_string(dst_dir.join("a.txt")).unwrap(), "a");
    assert_eq!(fs::read_to_string(dst_dir.join("sub/b.txt")).unwrap(), "b");
}

// ── FileMove tests ─────────────────────────────────────────────────────────

#[tokio::test]
async fn move_file_removes_source_and_creates_destination() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let src = root.join("src.txt");
    let dst = root.join("moved.txt");
    fs::write(&src, "content").unwrap();

    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::Move(FileMove {
            source: src.to_string_lossy().into_owned(),
            destination: dst.to_string_lossy().into_owned(),
            overwrite: false,
        })),
    };
    let resp = mgr.handle(&req).await;
    expect_move(resp);
    assert!(!src.exists());
    assert_eq!(fs::read_to_string(&dst).unwrap(), "content");
}

#[tokio::test]
async fn move_with_overwrite_replaces_destination() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let src = root.join("src.txt");
    let dst = root.join("dst.txt");
    fs::write(&src, "new").unwrap();
    fs::write(&dst, "old").unwrap();

    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::Move(FileMove {
            source: src.to_string_lossy().into_owned(),
            destination: dst.to_string_lossy().into_owned(),
            overwrite: true,
        })),
    };
    let resp = mgr.handle(&req).await;
    expect_move(resp);
    assert!(!src.exists());
    assert_eq!(fs::read_to_string(&dst).unwrap(), "new");
}

// ── FileCreateSymlink tests ────────────────────────────────────────────────

#[cfg(unix)]
#[tokio::test]
async fn create_symlink_creates_link() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let target = root.join("target.txt");
    let link = root.join("link");
    fs::write(&target, "x").unwrap();

    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::CreateSymlink(FileCreateSymlink {
            target: target.to_string_lossy().into_owned(),
            link_path: link.to_string_lossy().into_owned(),
        })),
    };
    let resp = mgr.handle(&req).await;
    expect_symlink(resp);
    let metadata = fs::symlink_metadata(&link).unwrap();
    assert!(metadata.file_type().is_symlink());
    assert_eq!(fs::read_link(&link).unwrap(), target);
}

#[cfg(unix)]
#[tokio::test]
async fn create_symlink_with_relative_target_escaping_parent_is_normalized() {
    // Round 3 spec/quality follow-up: the dispatch path used to call
    // `parent.join(&req.target)` directly without lexical normalization.
    // For a relative `target` like "../sibling.txt", the resulting
    // policy-check string carries a raw `..` component and
    // `policy.check_path` rejects it with InvalidPath — even though
    // approval (which DID lexically normalize via `collect_request_paths`)
    // has already shown the operator a clean canonical path. Without
    // this fix every approved relative symlink that escapes its own
    // parent would silently fail at execution time.
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);

    // Layout: <root>/sub/escape -> ../target.txt  ⇒ <root>/target.txt
    let target = root.join("target.txt");
    fs::write(&target, "x").unwrap();
    let sub = root.join("sub");
    fs::create_dir(&sub).unwrap();
    let link = sub.join("escape");

    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::CreateSymlink(FileCreateSymlink {
            target: "../target.txt".into(),
            link_path: link.to_string_lossy().into_owned(),
        })),
    };
    let resp = mgr.handle(&req).await;
    expect_symlink(resp);

    let metadata = fs::symlink_metadata(&link).unwrap();
    assert!(metadata.file_type().is_symlink());
    // The on-disk symlink target is preserved as-is (we don't rewrite
    // the user's relative target string), so reading it back should
    // give exactly what they asked for.
    assert_eq!(
        fs::read_link(&link).unwrap(),
        std::path::PathBuf::from("../target.txt")
    );
}

// ── FileChmod tests ────────────────────────────────────────────────────────

#[cfg(unix)]
#[tokio::test]
async fn chmod_sets_unix_mode() {
    use std::os::unix::fs::PermissionsExt;
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let file = root.join("modmode.txt");
    fs::write(&file, "x").unwrap();

    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::Chmod(FileChmod {
            path: file.to_string_lossy().into_owned(),
            recursive: false,
            no_follow_symlink: false,
            permission: Some(file_chmod::Permission::Unix(UnixPermission {
                mode: Some(0o600),
                owner: None,
                group: None,
            })),
        })),
    };
    let resp = mgr.handle(&req).await;
    let result = expect_chmod(resp);
    assert_eq!(result.items_modified, 1);
    assert_eq!(
        fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[cfg(unix)]
#[tokio::test]
async fn chmod_recursive_applies_to_children() {
    use std::os::unix::fs::PermissionsExt;
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let sub = root.join("chmodrec");
    fs::create_dir(&sub).unwrap();
    fs::write(sub.join("a.txt"), "x").unwrap();
    fs::write(sub.join("b.txt"), "x").unwrap();

    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::Chmod(FileChmod {
            path: sub.to_string_lossy().into_owned(),
            recursive: true,
            no_follow_symlink: false,
            permission: Some(file_chmod::Permission::Unix(UnixPermission {
                mode: Some(0o700),
                owner: None,
                group: None,
            })),
        })),
    };
    let resp = mgr.handle(&req).await;
    let result = expect_chmod(resp);
    assert_eq!(result.items_modified, 3);
    assert_eq!(
        fs::metadata(sub.join("a.txt"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
}

#[cfg(unix)]
#[tokio::test]
async fn chmod_chown_not_supported_returns_permission_denied() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let file = root.join("own.txt");
    fs::write(&file, "x").unwrap();

    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::Chmod(FileChmod {
            path: file.to_string_lossy().into_owned(),
            recursive: false,
            no_follow_symlink: false,
            permission: Some(file_chmod::Permission::Unix(UnixPermission {
                mode: None,
                owner: Some("root".into()),
                group: None,
            })),
        })),
    };
    let resp = mgr.handle(&req).await;
    let err = expect_error(resp);
    assert_eq!(err.code, FileErrorCode::PermissionDenied as i32);
}

// ── T18: additional write/edit/fs edge tests ─────────────────────────────

#[tokio::test]
async fn file_write_string_replace_not_found_returns_error() {
    // Round 1 #23: FileWrite (not FileEdit) path wasn't covered for the
    // "old_string not found" case. FileWrite errors, FileEdit uses
    // match_error instead.
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let file = root.join("nf.txt");
    fs::write(&file, "hello world").unwrap();

    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::Write(FileWrite {
            path: file.to_string_lossy().into_owned(),
            create_parents: false,
            encoding: None,
            no_follow_symlink: false,
            method: Some(file_write::Method::StringReplace(StringReplace {
                old_string: "missing".into(),
                new_string: "x".into(),
                replace_all: false,
            })),
        })),
    };
    let resp = mgr.handle(&req).await;
    let err = expect_error(resp);
    assert_eq!(err.code, FileErrorCode::NotFound as i32);
    assert_eq!(fs::read_to_string(&file).unwrap(), "hello world");
}

#[tokio::test]
async fn line_range_replace_start_line_past_eof_errors() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let file = root.join("short.txt");
    fs::write(&file, "a\nb\nc\n").unwrap();

    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::Write(FileWrite {
            path: file.to_string_lossy().into_owned(),
            create_parents: false,
            encoding: None,
            no_follow_symlink: false,
            method: Some(file_write::Method::LineRangeReplace(LineRangeReplace {
                start_line: 99,
                end_line: 100,
                new_content: "x".into(),
            })),
        })),
    };
    let resp = mgr.handle(&req).await;
    let _err = expect_error(resp);
}

#[tokio::test]
async fn line_range_replace_end_line_clamped_past_total() {
    // end_line > total_lines should clamp to the last line rather than
    // erroring.
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let file = root.join("three.txt");
    fs::write(&file, "alpha\nbeta\ngamma\n").unwrap();

    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::Write(FileWrite {
            path: file.to_string_lossy().into_owned(),
            create_parents: false,
            encoding: None,
            no_follow_symlink: false,
            method: Some(file_write::Method::LineRangeReplace(LineRangeReplace {
                start_line: 2,
                end_line: 99,
                new_content: "REST".into(),
            })),
        })),
    };
    let resp = mgr.handle(&req).await;
    expect_write(resp);
    assert_eq!(fs::read_to_string(&file).unwrap(), "alpha\nREST\n");
}

#[tokio::test]
async fn byte_range_replace_at_eof_inserts() {
    // Pure insert at EOF: byte_offset == file.len(), byte_length == 0.
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let file = root.join("b.bin");
    fs::write(&file, b"hello").unwrap();

    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::Write(FileWrite {
            path: file.to_string_lossy().into_owned(),
            create_parents: false,
            encoding: None,
            no_follow_symlink: false,
            method: Some(file_write::Method::ByteRangeReplace(ByteRangeReplace {
                byte_offset: 5,
                byte_length: 0,
                new_content: b" world".to_vec(),
            })),
        })),
    };
    let resp = mgr.handle(&req).await;
    expect_write(resp);
    assert_eq!(fs::read(&file).unwrap(), b"hello world");
}

#[tokio::test]
async fn byte_range_replace_pure_insert_mid_file() {
    // byte_length == 0 in the middle of the file is a pure insertion.
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let file = root.join("b.bin");
    fs::write(&file, b"hello world").unwrap();

    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::Write(FileWrite {
            path: file.to_string_lossy().into_owned(),
            create_parents: false,
            encoding: None,
            no_follow_symlink: false,
            method: Some(file_write::Method::ByteRangeReplace(ByteRangeReplace {
                byte_offset: 5,
                byte_length: 0,
                new_content: b",".to_vec(),
            })),
        })),
    };
    let resp = mgr.handle(&req).await;
    expect_write(resp);
    assert_eq!(fs::read(&file).unwrap(), b"hello, world");
}

#[tokio::test]
async fn byte_range_replace_u64_overflow_returns_error() {
    // T9 regression: byte_offset + byte_length overflowing u64 must not
    // panic. The handler should return an InvalidPath error.
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let file = root.join("b.bin");
    fs::write(&file, b"hi").unwrap();

    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::Write(FileWrite {
            path: file.to_string_lossy().into_owned(),
            create_parents: false,
            encoding: None,
            no_follow_symlink: false,
            method: Some(file_write::Method::ByteRangeReplace(ByteRangeReplace {
                byte_offset: 5,
                byte_length: u64::MAX,
                new_content: Vec::new(),
            })),
        })),
    };
    let resp = mgr.handle(&req).await;
    let err = expect_error(resp);
    assert_eq!(err.code, FileErrorCode::InvalidPath as i32);
}

#[tokio::test]
async fn file_write_encoding_non_utf8_rejected() {
    // T14 regression: FileWrite.encoding other than utf-8 returns
    // FILE_ERROR_CODE_ENCODING without touching the filesystem.
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let file = root.join("enc.txt");
    fs::write(&file, "original").unwrap();

    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::Write(FileWrite {
            path: file.to_string_lossy().into_owned(),
            create_parents: false,
            encoding: Some("gbk".to_string()),
            no_follow_symlink: false,
            method: Some(file_write::Method::FullWrite(FullWrite {
                source: Some(full_write::Source::Content(b"new".to_vec())),
                ..Default::default()
            })),
        })),
    };
    let resp = mgr.handle(&req).await;
    let err = expect_error(resp);
    assert_eq!(err.code, FileErrorCode::Encoding as i32);
    // File unchanged.
    assert_eq!(fs::read_to_string(&file).unwrap(), "original");
}

#[tokio::test]
async fn file_edit_encoding_non_utf8_rejected() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let file = root.join("enc.txt");
    fs::write(&file, "foo").unwrap();

    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::Edit(FileEdit {
            path: file.to_string_lossy().into_owned(),
            encoding: Some("shift_jis".to_string()),
            no_follow_symlink: false,
            method: Some(file_edit::Method::StringReplace(StringReplace {
                old_string: "foo".into(),
                new_string: "bar".into(),
                replace_all: false,
            })),
        })),
    };
    let resp = mgr.handle(&req).await;
    let err = expect_error(resp);
    assert_eq!(err.code, FileErrorCode::Encoding as i32);
    assert_eq!(fs::read_to_string(&file).unwrap(), "foo");
}

#[cfg(unix)]
#[tokio::test]
async fn write_refuses_symlink_when_no_follow_set() {
    // T11 regression: no_follow_symlink=true on a symlink must error
    // without touching the target.
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let target = root.join("target.txt");
    fs::write(&target, "original").unwrap();
    let link = root.join("link.txt");
    std::os::unix::fs::symlink(&target, &link).unwrap();

    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::Write(FileWrite {
            path: link.to_string_lossy().into_owned(),
            create_parents: false,
            encoding: None,
            no_follow_symlink: true,
            method: Some(file_write::Method::FullWrite(FullWrite {
                source: Some(full_write::Source::Content(b"hijacked".to_vec())),
                ..Default::default()
            })),
        })),
    };
    let resp = mgr.handle(&req).await;
    let err = expect_error(resp);
    assert_eq!(err.code, FileErrorCode::InvalidPath as i32);
    // Target must be untouched.
    assert_eq!(fs::read_to_string(&target).unwrap(), "original");
}

#[cfg(unix)]
#[tokio::test]
async fn edit_refuses_symlink_when_no_follow_set() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let target = root.join("target.txt");
    fs::write(&target, "foo").unwrap();
    let link = root.join("link.txt");
    std::os::unix::fs::symlink(&target, &link).unwrap();

    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::Edit(FileEdit {
            path: link.to_string_lossy().into_owned(),
            encoding: None,
            no_follow_symlink: true,
            method: Some(file_edit::Method::StringReplace(StringReplace {
                old_string: "foo".into(),
                new_string: "bar".into(),
                replace_all: false,
            })),
        })),
    };
    let resp = mgr.handle(&req).await;
    let err = expect_error(resp);
    assert_eq!(err.code, FileErrorCode::InvalidPath as i32);
    assert_eq!(fs::read_to_string(&target).unwrap(), "foo");
}

#[cfg(unix)]
#[tokio::test]
async fn chmod_refuses_symlink_when_no_follow_set() {
    use std::os::unix::fs::PermissionsExt;
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let target = root.join("target.txt");
    fs::write(&target, "x").unwrap();
    // Capture the original target mode.
    let original_mode = fs::metadata(&target).unwrap().permissions().mode() & 0o777;
    let link = root.join("link.txt");
    std::os::unix::fs::symlink(&target, &link).unwrap();

    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::Chmod(FileChmod {
            path: link.to_string_lossy().into_owned(),
            recursive: false,
            no_follow_symlink: true,
            permission: Some(file_chmod::Permission::Unix(UnixPermission {
                mode: Some(0o700),
                owner: None,
                group: None,
            })),
        })),
    };
    let resp = mgr.handle(&req).await;
    let err = expect_error(resp);
    assert_eq!(err.code, FileErrorCode::InvalidPath as i32);
    // Target mode must be unchanged.
    assert_eq!(
        fs::metadata(&target).unwrap().permissions().mode() & 0o777,
        original_mode
    );
}

#[cfg(unix)]
#[tokio::test]
async fn delete_symlink_no_follow_removes_link_not_target() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let target = root.join("target.txt");
    fs::write(&target, "x").unwrap();
    let link = root.join("link.txt");
    std::os::unix::fs::symlink(&target, &link).unwrap();

    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::Delete(FileDelete {
            path: link.to_string_lossy().into_owned(),
            recursive: false,
            mode: DeleteMode::Permanent as i32,
            no_follow_symlink: true,
        })),
    };
    let resp = mgr.handle(&req).await;
    expect_delete(resp);
    // Symlink gone, target survived.
    assert!(!link.exists());
    assert!(target.exists());
    assert_eq!(fs::read_to_string(&target).unwrap(), "x");
}

#[tokio::test]
async fn copy_recursive_overwrite_merges_into_existing_destination() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let src = root.join("src");
    let dst = root.join("dst");
    fs::create_dir(&src).unwrap();
    fs::create_dir(&dst).unwrap();
    fs::write(src.join("fresh.txt"), "new").unwrap();
    fs::write(dst.join("fresh.txt"), "old").unwrap();
    fs::write(dst.join("untouched.txt"), "keep").unwrap();

    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::Copy(FileCopy {
            source: src.to_string_lossy().into_owned(),
            destination: dst.to_string_lossy().into_owned(),
            recursive: true,
            overwrite: true,
        })),
    };
    let resp = mgr.handle(&req).await;
    expect_copy(resp);
    assert_eq!(fs::read_to_string(dst.join("fresh.txt")).unwrap(), "new");
    assert_eq!(
        fs::read_to_string(dst.join("untouched.txt")).unwrap(),
        "keep"
    );
}

#[tokio::test]
async fn file_request_with_no_operation_returns_unspecified_error() {
    // T20 regression: the operation-less request path in FileManager::handle
    // should produce FILE_ERROR_CODE_UNSPECIFIED rather than silently
    // dispatching to a default handler.
    let dir = TempDir::new().unwrap();
    let (mgr, _root) = test_manager(&dir);
    let req = FileRequest {
        request_id: "t".into(),
        operation: None,
    };
    let resp = mgr.handle(&req).await;
    let err = expect_error(resp);
    assert_eq!(err.code, FileErrorCode::Unspecified as i32);
}

// ── T19: text/image edge-case tests ──────────────────────────────────────

#[tokio::test]
async fn read_text_reports_total_lines_and_full_position_info() {
    // 5 short lines — confirm total_lines populated and PositionInfo fields
    // are populated (line, byte_in_file, byte_in_line).
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let file = root.join("lines.txt");
    fs::write(&file, "alpha\nbeta\ngamma\ndelta\nepsilon\n").unwrap();

    let resp = mgr.handle(&wrap_read_text(read_text_request(&file))).await;
    let result = expect_read_text(resp);
    assert_eq!(result.total_lines, 5);
    let start = result.start_pos.as_ref().unwrap();
    assert_eq!(start.line, 1);
    assert_eq!(start.byte_in_file, 0);
    assert_eq!(start.byte_in_line, 0);
    let end = result.end_pos.as_ref().unwrap();
    assert_eq!(end.line, 5);
    // File is 32 bytes total ("alpha\n"=6 + "beta\n"=5 + "gamma\n"=6 +
    // "delta\n"=6 + "epsilon\n"=8 = 31 bytes).
    assert_eq!(result.total_file_bytes, 31);
    assert_eq!(end.byte_in_file, 31);
}

#[tokio::test]
async fn read_text_start_line_col_reports_full_position_info() {
    // Starting at line 2 col 3 of "first\nsecond\nthird\n" should yield
    // byte_in_file = 6 (start of line 2) + 3 = 9, byte_in_line = 3.
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let file = root.join("linecol.txt");
    fs::write(&file, "first\nsecond\nthird\n").unwrap();

    let mut req = read_text_request(&file);
    req.start = Some(file_read_text::Start::StartLineCol(LineCol {
        line: 2,
        col: 3,
    }));
    let resp = mgr.handle(&wrap_read_text(req)).await;
    let result = expect_read_text(resp);
    let start = result.start_pos.unwrap();
    assert_eq!(start.line, 2);
    assert_eq!(start.byte_in_file, 9);
    assert_eq!(start.byte_in_line, 3);
}

#[tokio::test]
async fn read_text_gbk_autodetect_without_forced_encoding() {
    // T8 regression: when chardetng identifies the file as GBK and we did
    // NOT pass an explicit encoding, the handler still decodes correctly
    // and reports an encoding name (not UTF-8).
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let file = root.join("gbk-auto.txt");
    // "你好,世界" in GBK (with CJK punctuation to give chardetng enough
    // signal to lock in on GBK).
    let gbk: Vec<u8> = vec![0xC4, 0xE3, 0xBA, 0xC3, 0xA3, 0xAC, 0xCA, 0xC0, 0xBD, 0xE7];
    fs::write(&file, &gbk).unwrap();

    let req = read_text_request(&file);
    let resp = mgr.handle(&wrap_read_text(req)).await;
    let result = expect_read_text(resp);
    assert_eq!(result.lines.len(), 1);
    // Content should be the decoded CJK string.
    assert!(result.lines[0].content.contains("你好"));
    // Detected encoding must NOT be UTF-8.
    assert_ne!(result.detected_encoding.to_ascii_uppercase(), "UTF-8");
    // On-disk byte positions reported in raw bytes, not decoded.
    assert_eq!(result.total_file_bytes, gbk.len() as u64);
}

#[tokio::test]
async fn read_text_byte_positions_match_raw_on_disk_bytes_for_gbk() {
    // T8 regression: for GBK, PositionInfo.byte_in_file must be raw
    // on-disk bytes (not decoded UTF-8 offset). Start at raw byte 4
    // ("中间" after the first two characters) and confirm we get bytes
    // 4..end.
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let file = root.join("gbk-cursor.txt");
    // 2 GBK CJK chars = 4 bytes "你好", 2 more = 4 bytes "世界", total 8.
    let gbk: Vec<u8> = vec![0xC4, 0xE3, 0xBA, 0xC3, 0xCA, 0xC0, 0xBD, 0xE7];
    fs::write(&file, &gbk).unwrap();

    let mut req = read_text_request(&file);
    req.encoding = Some("gbk".into());
    req.start = Some(file_read_text::Start::StartByte(4));
    let resp = mgr.handle(&wrap_read_text(req)).await;
    let result = expect_read_text(resp);
    let start = result.start_pos.unwrap();
    // start_byte = 4 (raw), not 6 (decoded UTF-8 offset of "世").
    assert_eq!(start.byte_in_file, 4);
    let end = result.end_pos.unwrap();
    // end_byte = 8 (raw file length), not 12 (decoded UTF-8 length).
    assert_eq!(end.byte_in_file, 8);
}

#[tokio::test]
async fn read_text_empty_encoding_triggers_auto_detect() {
    // R7 regression: an empty `encoding` string means "auto-detect", NOT
    // "force UTF-8". A short CJK blob in GBK should round-trip through the
    // auto-detect path (BOM / chardetng), not be mis-decoded as UTF-8.
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let file = root.join("auto.txt");
    // "你好,世界" in GBK — same bytes as the gbk-autodetect test above, enough
    // signal for chardetng to lock onto GBK rather than falling back to UTF-8.
    let gbk: Vec<u8> = vec![0xC4, 0xE3, 0xBA, 0xC3, 0xA3, 0xAC, 0xCA, 0xC0, 0xBD, 0xE7];
    fs::write(&file, &gbk).unwrap();

    let mut req = read_text_request(&file);
    req.encoding = Some(String::new()); // explicit empty → auto-detect
    let resp = mgr.handle(&wrap_read_text(req)).await;
    let result = expect_read_text(resp);
    // Auto-detect should pick a non-UTF-8 encoding (GBK/GB18030) and decode
    // the bytes correctly.
    assert_ne!(result.detected_encoding.to_ascii_uppercase(), "UTF-8");
    assert_eq!(result.lines.len(), 1);
    assert!(result.lines[0].content.contains("你好"));
}

#[tokio::test]
async fn read_text_line_col_with_col_past_eol_clamps_to_line_end() {
    // R12 regression: an oversized `col` on a short line must clamp to the
    // current line's end (start of next line), not spill into the next line.
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let file = root.join("clamp.txt");
    fs::write(&file, "hi\nlong line here\n").unwrap();

    let mut req = read_text_request(&file);
    req.start = Some(file_read_text::Start::StartLineCol(LineCol {
        line: 1,
        col: 999, // way past "hi"
    }));
    let resp = mgr.handle(&wrap_read_text(req)).await;
    let result = expect_read_text(resp);
    // start_pos must remain on line 1, not spill into line 2.
    assert_eq!(result.start_pos.unwrap().line, 1);
}

#[tokio::test]
async fn read_text_max_bytes_zero_returns_no_lines() {
    // R16 regression: max_bytes=0 must return zero lines with
    // stop_reason=MaxBytes (previously it emitted a single empty TextLine).
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let file = root.join("zero.txt");
    fs::write(&file, "line1\nline2\n").unwrap();

    let mut req = read_text_request(&file);
    req.max_bytes = Some(0);
    let resp = mgr.handle(&wrap_read_text(req)).await;
    let result = expect_read_text(resp);
    assert_eq!(result.lines.len(), 0);
    assert_eq!(result.stop_reason, StopReason::MaxBytes as i32);
}

#[tokio::test]
async fn read_image_passthrough_is_byte_for_byte_identical() {
    // T10 regression: passthrough mode must return the original file bytes
    // without decode/encode.
    use image::{ImageBuffer, Rgb};
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let file = root.join("pass.png");
    let img: ImageBuffer<Rgb<u8>, Vec<u8>> =
        ImageBuffer::from_fn(32, 32, |x, y| Rgb([(x * 7) as u8, (y * 11) as u8, 13u8]));
    img.save_with_format(&file, image::ImageFormat::Png)
        .unwrap();

    let original = fs::read(&file).unwrap();

    let resp = mgr.handle(&image_req(&file, None, None)).await;
    let result = expect_read_image(resp);
    assert_eq!(
        result.content, original,
        "passthrough must be byte-identical"
    );
    assert_eq!(result.width, 32);
    assert_eq!(result.height, 32);
    assert_eq!(result.original_bytes, original.len() as u64);
    assert_eq!(result.output_bytes, original.len() as u64);
}

#[tokio::test]
async fn read_image_max_height_only_preserves_aspect_ratio() {
    use image::{ImageBuffer, Rgb};
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let file = root.join("tall.png");
    let img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_fn(800, 1000, |_, _| Rgb([0, 0, 0]));
    img.save_with_format(&file, image::ImageFormat::Png)
        .unwrap();

    // max_height=500, no max_width → height scaled to 500, width scaled
    // proportionally to 400.
    let resp = mgr.handle(&image_req(&file, None, Some(500))).await;
    let result = expect_read_image(resp);
    assert_eq!(result.height, 500);
    assert_eq!(result.width, 400);
}

#[tokio::test]
async fn read_image_both_axis_resize_preserves_aspect_ratio() {
    use image::{ImageBuffer, Rgb};
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let file = root.join("box.png");
    let img: ImageBuffer<Rgb<u8>, Vec<u8>> =
        ImageBuffer::from_fn(1000, 500, |_, _| Rgb([128, 128, 128]));
    img.save_with_format(&file, image::ImageFormat::Png)
        .unwrap();

    // Both axes set — smaller axis wins. max_width=500 → height=250.
    let resp = mgr.handle(&image_req(&file, Some(500), Some(400))).await;
    let result = expect_read_image(resp);
    assert!(result.width <= 500);
    assert!(result.height <= 400);
    // Aspect ratio 1000:500 = 2:1. Expected 500x250.
    assert_eq!(result.width, 500);
    assert_eq!(result.height, 250);
}

#[tokio::test]
async fn read_image_jpeg_source_can_be_read() {
    // T19 gap: only PNG sources were exercised. Write a JPEG and read it.
    use image::{ImageBuffer, Rgb};
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let file = root.join("src.jpg");
    let img: ImageBuffer<Rgb<u8>, Vec<u8>> =
        ImageBuffer::from_fn(64, 48, |x, _| Rgb([x as u8 * 4, 100, 200]));
    img.save_with_format(&file, image::ImageFormat::Jpeg)
        .unwrap();

    // Passthrough: the returned content should be exactly the JPEG bytes.
    let original = fs::read(&file).unwrap();
    let resp = mgr.handle(&image_req(&file, None, None)).await;
    let result = expect_read_image(resp);
    assert_eq!(result.content, original);
    assert_eq!(result.width, 64);
    assert_eq!(result.height, 48);
    // Format should be JPEG (not PNG).
    assert_eq!(result.format, ImageFormat::Jpeg as i32);
}

#[tokio::test]
async fn read_image_quality_clamp_accepts_out_of_range_values() {
    // Quality values outside [1, 100] must be clamped rather than rejected.
    use image::{ImageBuffer, Rgb};
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let file = root.join("clamp.png");
    let img: ImageBuffer<Rgb<u8>, Vec<u8>> =
        ImageBuffer::from_fn(64, 64, |_, _| Rgb([50, 100, 150]));
    img.save_with_format(&file, image::ImageFormat::Png)
        .unwrap();

    // quality = 0 (below minimum) → still produces valid JPEG output.
    let req_zero = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::ReadImage(FileReadImage {
            path: file.to_string_lossy().into_owned(),
            max_width: None,
            max_height: None,
            max_bytes: None,
            quality: Some(0),
            output_format: Some(ImageFormat::Jpeg as i32),
            no_follow_symlink: false,
        })),
    };
    let result_zero = expect_read_image(mgr.handle(&req_zero).await);
    assert_eq!(result_zero.format, ImageFormat::Jpeg as i32);
    assert!(!result_zero.content.is_empty());

    // quality = 200 (above maximum) → still produces valid JPEG output.
    let req_high = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::ReadImage(FileReadImage {
            path: file.to_string_lossy().into_owned(),
            max_width: None,
            max_height: None,
            max_bytes: None,
            quality: Some(200),
            output_format: Some(ImageFormat::Jpeg as i32),
            no_follow_symlink: false,
        })),
    };
    let result_high = expect_read_image(mgr.handle(&req_high).await);
    assert_eq!(result_high.format, ImageFormat::Jpeg as i32);
    assert!(!result_high.content.is_empty());
}

#[tokio::test]
async fn read_image_bomb_guard_rejects_oversized_dimensions() {
    // T10 regression: a PNG whose declared dimensions x 4 exceeds
    // max_read_bytes must be rejected BEFORE decoding. We simulate this
    // by setting max_read_bytes tight enough that the guard trips.
    use image::{ImageBuffer, Rgb};
    let dir = TempDir::new().unwrap();
    let tmp_root = dir.path().canonicalize().unwrap();
    let root_str = tmp_root.to_string_lossy().into_owned();
    // Tight budget: 1 MB read cap, but 1024x1024 RGBA = 4 MB decoded.
    let mgr = ahandd::file_manager::FileManager::new(&ahandd::config::FilePolicyConfig {
        enabled: true,
        path_allowlist: vec![format!("{}/**", root_str), root_str.clone()],
        path_denylist: vec![],
        max_read_bytes: 1_000_000,
        max_write_bytes: 100_000_000,
        dangerous_paths: vec![],
    });
    let file = tmp_root.join("big.png");
    let img: ImageBuffer<Rgb<u8>, Vec<u8>> =
        ImageBuffer::from_fn(1024, 1024, |_, _| Rgb([0, 0, 0]));
    img.save_with_format(&file, image::ImageFormat::Png)
        .unwrap();

    // The on-disk PNG is well under 1 MB (it compresses), so file-level
    // max_read_bytes wouldn't catch it. The dimension guard in
    // handle_read_image should.
    let resp = mgr.handle(&image_req(&file, None, None)).await;
    let err = expect_error(resp);
    assert_eq!(err.code, FileErrorCode::TooLarge as i32);
}

// ── R21: approval / dangerous_paths / recursive-delete tests ─────────────

/// Build a FileManager whose policy allowlist scopes to the temp dir AND
/// marks specific subpaths as `dangerous_paths`. Mirrors `test_manager`
/// but with the dangerous-paths slot populated.
fn manager_with_dangerous(tmp: &TempDir, dangerous: &[&str]) -> (FileManager, std::path::PathBuf) {
    let root = tmp
        .path()
        .canonicalize()
        .expect("tempdir canonicalization should succeed");
    let root_str = root.to_string_lossy().into_owned();
    let pattern = format!("{}/**", root_str.trim_end_matches('/'));
    let dangerous_abs: Vec<String> = dangerous
        .iter()
        .map(|d| format!("{}/{}", root_str.trim_end_matches('/'), d))
        .collect();
    let mgr = FileManager::new(&FilePolicyConfig {
        enabled: true,
        path_allowlist: vec![pattern, root_str],
        path_denylist: vec![],
        max_read_bytes: 100_000_000,
        max_write_bytes: 100_000_000,
        dangerous_paths: dangerous_abs,
    });
    (mgr, root)
}

#[tokio::test]
async fn check_request_approval_returns_true_for_dangerous_path_read() {
    // R21: a request that reads a dangerous-paths file must be flagged
    // for approval, regardless of session mode.
    let dir = TempDir::new().unwrap();
    let (mgr, root) = manager_with_dangerous(&dir, &["secret.txt"]);
    let secret = root.join("secret.txt");
    fs::write(&secret, "shhh").unwrap();

    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::ReadText(FileReadText {
            path: secret.to_string_lossy().into_owned(),
            start: None,
            max_lines: None,
            max_bytes: None,
            target_end: None,
            max_line_width: None,
            encoding: None,
            line_numbers: true,
            no_follow_symlink: false,
        })),
    };
    let escalation = mgr
        .check_request_approval(&req)
        .await
        .expect("dangerous path must not be denied");
    let escalation = escalation.expect("dangerous_paths read must require approval");
    assert_eq!(
        escalation.kind,
        ahandd::file_manager::EscalationKind::DangerousPath
    );
    assert_eq!(
        escalation.path.as_deref(),
        Some(secret.to_string_lossy().as_ref())
    );
}

#[tokio::test]
async fn check_request_approval_returns_false_for_normal_path() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = manager_with_dangerous(&dir, &["secret.txt"]);
    let normal = root.join("ordinary.txt");
    fs::write(&normal, "x").unwrap();

    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::Stat(FileStat {
            path: normal.to_string_lossy().into_owned(),
            no_follow_symlink: false,
        })),
    };
    let escalation = mgr.check_request_approval(&req).await.unwrap();
    assert!(
        escalation.is_none(),
        "non-dangerous path must NOT trigger approval, got {escalation:?}"
    );
}

#[tokio::test]
async fn check_request_approval_returns_true_for_recursive_permanent_delete() {
    // R9 + R21: spec rule design.md:635 — recursive PERMANENT delete
    // always escalates to STRICT approval regardless of which path is
    // involved. The check must fire even on a path NOT in dangerous_paths.
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let target = root.join("victim_dir");
    fs::create_dir(&target).unwrap();

    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::Delete(FileDelete {
            path: target.to_string_lossy().into_owned(),
            recursive: true,
            mode: DeleteMode::Permanent as i32,
            no_follow_symlink: false,
        })),
    };
    let escalation = mgr
        .check_request_approval(&req)
        .await
        .unwrap()
        .expect("recursive PERMANENT delete must require approval");
    assert_eq!(
        escalation.kind,
        ahandd::file_manager::EscalationKind::RecursivePermanentDelete
    );
}

#[tokio::test]
async fn check_request_approval_returns_false_for_non_recursive_permanent_delete() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let target = root.join("single.txt");
    fs::write(&target, "x").unwrap();

    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::Delete(FileDelete {
            path: target.to_string_lossy().into_owned(),
            recursive: false,
            mode: DeleteMode::Permanent as i32,
            no_follow_symlink: false,
        })),
    };
    let escalation = mgr.check_request_approval(&req).await.unwrap();
    assert!(
        escalation.is_none(),
        "non-recursive permanent delete should NOT require approval by itself, got {escalation:?}"
    );
}

#[tokio::test]
async fn check_request_approval_returns_true_for_dangerous_path_trash_delete() {
    // recursive=true + TRASH mode is NOT auto-escalated (only PERMANENT
    // is), but a dangerous path IS escalated independently.
    let dir = TempDir::new().unwrap();
    let (mgr, root) = manager_with_dangerous(&dir, &[".sensitive"]);
    let target = root.join(".sensitive");
    fs::create_dir(&target).unwrap();

    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::Delete(FileDelete {
            path: target.to_string_lossy().into_owned(),
            recursive: true,
            mode: DeleteMode::Trash as i32,
            no_follow_symlink: false,
        })),
    };
    let escalation = mgr
        .check_request_approval(&req)
        .await
        .unwrap()
        .expect("TRASH delete on a dangerous path must still require approval");
    assert_eq!(
        escalation.kind,
        ahandd::file_manager::EscalationKind::DangerousPath
    );
}

#[cfg(unix)]
#[tokio::test]
async fn check_request_approval_denies_symlink_target_outside_allowlist() {
    // R2 regression: FileCreateSymlink with a target pointing outside the
    // allowlist is rejected at the policy preflight (PolicyDenied), not
    // silently allowed.
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let link = root.join("escape");

    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::CreateSymlink(FileCreateSymlink {
            target: "/etc/passwd".into(),
            link_path: link.to_string_lossy().into_owned(),
        })),
    };
    let err = mgr.check_request_approval(&req).await.unwrap_err();
    assert_eq!(err.code, FileErrorCode::PolicyDenied as i32);
}

#[cfg(unix)]
#[tokio::test]
async fn check_request_approval_escalates_relative_symlink_target_into_dangerous_path() {
    // C2 regression: FileCreateSymlink with a *relative* target that
    // resolves into a path listed in `dangerous_paths` must escalate to
    // approval. Previously only absolute targets were checked, so a
    // crafted `target = "../secret.txt"` with `link_path` inside the
    // allowlist would slip past dangerous_paths entirely.
    let dir = TempDir::new().unwrap();
    let (mgr, root) = manager_with_dangerous(&dir, &["secret.txt"]);
    std::fs::write(root.join("secret.txt"), "shhh").unwrap();
    let sub = root.join("sub");
    std::fs::create_dir(&sub).unwrap();
    let link = sub.join("escape");

    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::CreateSymlink(FileCreateSymlink {
            target: "../secret.txt".into(),
            link_path: link.to_string_lossy().into_owned(),
        })),
    };
    let escalation = mgr
        .check_request_approval(&req)
        .await
        .expect("relative target inside allowlist must not be denied")
        .expect("relative target resolving into dangerous_paths must escalate");
    assert_eq!(
        escalation.kind,
        ahandd::file_manager::EscalationKind::DangerousPath
    );
}

/// RAII guard that lowers the glob-approval scan cap for the lifetime of a
/// test, restoring the original cap on drop (even on panic). Tests use this
/// so they don't have to materialize tens of thousands of files just to
/// exercise the "cap hit" branch.
struct GlobScanCapGuard {
    previous: usize,
}

impl GlobScanCapGuard {
    fn set(cap: usize) -> Self {
        Self {
            previous: ahandd::file_manager::set_glob_approval_scan_cap(cap),
        }
    }
}

impl Drop for GlobScanCapGuard {
    fn drop(&mut self) {
        ahandd::file_manager::set_glob_approval_scan_cap(self.previous);
    }
}

#[tokio::test]
async fn check_request_approval_glob_fails_closed_when_scan_cap_hit() {
    // C3 regression: when a glob pattern matches more than the safety cap,
    // the pre-flight scan can't prove the unscanned tail is safe — it must
    // escalate to approval rather than fail open.
    //
    // We override the production cap of 10_000 down to a tiny number so we
    // only have to create a handful of files; otherwise this test would
    // burn ~10k filesystem inodes per run.
    use ahand_protocol::FileGlob;

    let _cap_guard = GlobScanCapGuard::set(8);

    let dir = TempDir::new().unwrap();
    let (mgr, root) = manager_with_dangerous(&dir, &["needle.txt"]);
    std::fs::write(root.join("needle.txt"), "x").unwrap();
    let bulk = root.join("bulk");
    std::fs::create_dir(&bulk).unwrap();
    // 10 matches > cap of 8, so the iterator still has items past the cap
    // and we must hit the CapHit branch (not Clean).
    for i in 0..10 {
        std::fs::write(bulk.join(format!("f{i}.txt")), b"x").unwrap();
    }

    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::Glob(FileGlob {
            base_path: Some(bulk.to_string_lossy().into_owned()),
            pattern: "*.txt".into(),
            max_results: None,
        })),
    };
    let escalation = mgr
        .check_request_approval(&req)
        .await
        .expect("oversized glob must not be denied — it must be escalated")
        .expect("oversized glob (cap hit) must require approval");
    assert_eq!(
        escalation.kind,
        ahandd::file_manager::EscalationKind::GlobScanCapHit
    );
}

#[tokio::test]
async fn check_request_approval_glob_escalates_when_match_is_in_dangerous_paths() {
    // The sister branch of the glob fail-closed (C3) approval flow:
    // if the pre-flight scan finds a glob expansion that matches a
    // path listed in `dangerous_paths`, escalation kind must be
    // `DangerousGlobMatch` (not `GlobScanCapHit`, not bypassed).
    // Without this test, a regression that conflated the two branches
    // — or that silently failed the dangerous check while reporting
    // CapHit — would slip through.
    use ahand_protocol::FileGlob;

    let dir = TempDir::new().unwrap();
    let (mgr, root) = manager_with_dangerous(&dir, &["secrets/**"]);
    let secrets = root.join("secrets");
    std::fs::create_dir(&secrets).unwrap();
    std::fs::write(secrets.join("api_key.txt"), "shh").unwrap();
    std::fs::write(root.join("normal.txt"), "x").unwrap();

    // Pattern intentionally matches both the dangerous path and the
    // normal one — the scan must surface the dangerous match.
    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::Glob(FileGlob {
            base_path: Some(root.to_string_lossy().into_owned()),
            pattern: "**/*.txt".into(),
            max_results: None,
        })),
    };
    let escalation = mgr
        .check_request_approval(&req)
        .await
        .expect("dangerous glob must not be denied — it must be escalated")
        .expect("a glob matching dangerous_paths must require approval");
    assert_eq!(
        escalation.kind,
        ahandd::file_manager::EscalationKind::DangerousGlobMatch
    );
    // The escalation message should name the specific dangerous match
    // so the operator's approval prompt can show what tripped the gate.
    assert_eq!(
        escalation.path.as_deref(),
        Some(secrets.join("api_key.txt").to_string_lossy().as_ref())
    );
}

// ── Bad-case coverage gaps surfaced by test-completeness audit ──────────────
//
// Each test below pins one error path that the audit found uncovered.
// Failure mode is documented inline. None of these required production
// code changes — they only surface paths that already exist.

#[tokio::test]
async fn read_text_on_directory_returns_is_a_directory() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::ReadText(FileReadText {
            path: root.to_string_lossy().into_owned(),
            start: None,
            max_lines: None,
            max_bytes: None,
            target_end: None,
            max_line_width: None,
            encoding: None,
            line_numbers: false,
            no_follow_symlink: false,
        })),
    };
    let err = expect_error(mgr.handle(&req).await);
    assert_eq!(err.code, FileErrorCode::IsADirectory as i32);
}

#[tokio::test]
async fn read_text_exceeding_max_read_bytes_returns_too_large() {
    // ReadText must check file_size against max_read_bytes BEFORE
    // slurping. A file that exceeds the policy budget should return
    // TooLarge, never load into memory.
    let dir = TempDir::new().unwrap();
    let tmp_root = dir.path().canonicalize().unwrap();
    let root_str = tmp_root.to_string_lossy().into_owned();
    let mgr = ahandd::file_manager::FileManager::new(&ahandd::config::FilePolicyConfig {
        enabled: true,
        path_allowlist: vec![format!("{}/**", root_str), root_str.clone()],
        path_denylist: vec![],
        max_read_bytes: 50,
        max_write_bytes: 100_000_000,
        dangerous_paths: vec![],
    });
    let big = tmp_root.join("big.txt");
    fs::write(&big, vec![b'a'; 200]).unwrap();

    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::ReadText(FileReadText {
            path: big.to_string_lossy().into_owned(),
            start: None,
            max_lines: None,
            max_bytes: None,
            target_end: None,
            max_line_width: None,
            encoding: None,
            line_numbers: false,
            no_follow_symlink: false,
        })),
    };
    let err = expect_error(mgr.handle(&req).await);
    assert_eq!(err.code, FileErrorCode::TooLarge as i32);
}

#[tokio::test]
async fn read_binary_on_missing_file_returns_not_found() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let missing = root.join("nope.bin");
    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::ReadBinary(FileReadBinary {
            path: missing.to_string_lossy().into_owned(),
            byte_offset: 0,
            byte_length: 0,
            max_bytes: None,
            no_follow_symlink: false,
        })),
    };
    let err = expect_error(mgr.handle(&req).await);
    assert_eq!(err.code, FileErrorCode::NotFound as i32);
}

#[tokio::test]
async fn read_binary_on_directory_returns_is_a_directory() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::ReadBinary(FileReadBinary {
            path: root.to_string_lossy().into_owned(),
            byte_offset: 0,
            byte_length: 0,
            max_bytes: None,
            no_follow_symlink: false,
        })),
    };
    let err = expect_error(mgr.handle(&req).await);
    assert_eq!(err.code, FileErrorCode::IsADirectory as i32);
}

#[tokio::test]
async fn read_image_on_directory_returns_is_a_directory() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::ReadImage(FileReadImage {
            path: root.to_string_lossy().into_owned(),
            max_width: None,
            max_height: None,
            max_bytes: None,
            quality: None,
            output_format: None,
            no_follow_symlink: false,
        })),
    };
    let err = expect_error(mgr.handle(&req).await);
    assert_eq!(err.code, FileErrorCode::IsADirectory as i32);
}

#[tokio::test]
async fn glob_with_invalid_pattern_returns_invalid_path() {
    // The dispatcher rejects absolute / `..` patterns up front. A
    // syntactically invalid pattern like an unclosed character class
    // gets through to `glob::glob()` and must be reported as
    // `InvalidPath` rather than panicking or returning empty results.
    use ahand_protocol::FileGlob;
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::Glob(FileGlob {
            base_path: Some(root.to_string_lossy().into_owned()),
            pattern: "[unclosed".into(),
            max_results: None,
        })),
    };
    let err = expect_error(mgr.handle(&req).await);
    assert_eq!(err.code, FileErrorCode::InvalidPath as i32);
}

#[tokio::test]
async fn copy_directory_without_recursive_returns_is_a_directory() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let src = root.join("src_dir");
    fs::create_dir(&src).unwrap();
    let dst = root.join("dst");
    let req = copy_req(&src, &dst, false, false);
    let err = expect_error(mgr.handle(&req).await);
    assert_eq!(err.code, FileErrorCode::IsADirectory as i32);
}

#[tokio::test]
async fn move_to_existing_destination_without_overwrite_returns_already_exists() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let src = root.join("src.txt");
    let dst = root.join("dst.txt");
    fs::write(&src, "from").unwrap();
    fs::write(&dst, "stay").unwrap();
    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::Move(FileMove {
            source: src.to_string_lossy().into_owned(),
            destination: dst.to_string_lossy().into_owned(),
            overwrite: false,
        })),
    };
    let err = expect_error(mgr.handle(&req).await);
    assert_eq!(err.code, FileErrorCode::AlreadyExists as i32);
    // Source untouched because the guard fires before rename.
    assert_eq!(fs::read_to_string(&src).unwrap(), "from");
    assert_eq!(fs::read_to_string(&dst).unwrap(), "stay");
}

#[tokio::test]
async fn write_with_no_method_returns_unspecified_error() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let path = root.join("placeholder.txt");
    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::Write(FileWrite {
            path: path.to_string_lossy().into_owned(),
            create_parents: false,
            encoding: None,
            no_follow_symlink: false,
            method: None,
        })),
    };
    let err = expect_error(mgr.handle(&req).await);
    assert_eq!(err.code, FileErrorCode::Unspecified as i32);
}

#[tokio::test]
async fn edit_with_no_method_returns_unspecified_error() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let path = root.join("placeholder.txt");
    fs::write(&path, "x").unwrap();
    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::Edit(FileEdit {
            path: path.to_string_lossy().into_owned(),
            encoding: None,
            no_follow_symlink: false,
            method: None,
        })),
    };
    let err = expect_error(mgr.handle(&req).await);
    assert_eq!(err.code, FileErrorCode::Unspecified as i32);
}

#[tokio::test]
async fn full_write_with_no_source_returns_unspecified_error() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let path = root.join("nosource.txt");
    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::Write(FileWrite {
            path: path.to_string_lossy().into_owned(),
            create_parents: false,
            encoding: None,
            no_follow_symlink: false,
            method: Some(file_write::Method::FullWrite(FullWrite::default())),
        })),
    };
    let err = expect_error(mgr.handle(&req).await);
    assert_eq!(err.code, FileErrorCode::Unspecified as i32);
}

#[tokio::test]
async fn full_write_with_s3_object_key_but_no_download_url_returns_unspecified_error() {
    // The S3 large-file flow requires the hub to inject s3_download_url
    // before forwarding. If a daemon receives a FullWrite carrying only
    // s3_object_key with no URL, that's either a hub bug or an
    // old-hub/new-daemon mismatch — the daemon must reject loudly so
    // the operator gets a clear error rather than a silent empty write.
    // (The happy-path S3 fetch behavior lives in
    // tests/file_ops_s3_write.rs.)
    use ahand_protocol::full_write;
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let path = root.join("s3.bin");
    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::Write(FileWrite {
            path: path.to_string_lossy().into_owned(),
            create_parents: false,
            encoding: None,
            no_follow_symlink: false,
            method: Some(file_write::Method::FullWrite(FullWrite {
                source: Some(full_write::Source::S3ObjectKey("upload-key-123".into())),
                s3_download_url: None,
                s3_download_url_expires_ms: None,
            })),
        })),
    };
    let err = expect_error(mgr.handle(&req).await);
    assert_eq!(err.code, FileErrorCode::Unspecified as i32);
    assert!(
        err.message.to_lowercase().contains("s3"),
        "error must name the unsupported S3 source, got: {}",
        err.message
    );
}

#[tokio::test]
async fn append_exceeding_total_size_limit_returns_too_large() {
    // The size-limit check for Append must combine existing file size
    // and new content. An existing file at the limit + any non-empty
    // append must fail with TooLarge before any write happens.
    let dir = TempDir::new().unwrap();
    let tmp_root = dir.path().canonicalize().unwrap();
    let root_str = tmp_root.to_string_lossy().into_owned();
    let mgr = ahandd::file_manager::FileManager::new(&ahandd::config::FilePolicyConfig {
        enabled: true,
        path_allowlist: vec![format!("{}/**", root_str), root_str.clone()],
        path_denylist: vec![],
        max_read_bytes: 100_000_000,
        max_write_bytes: 50,
        dangerous_paths: vec![],
    });
    let path = tmp_root.join("at_limit.txt");
    fs::write(&path, vec![b'a'; 50]).unwrap();

    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::Write(FileWrite {
            path: path.to_string_lossy().into_owned(),
            create_parents: false,
            encoding: None,
            no_follow_symlink: false,
            method: Some(file_write::Method::Append(FileAppend {
                content: b"!".to_vec(),
            })),
        })),
    };
    let err = expect_error(mgr.handle(&req).await);
    assert_eq!(err.code, FileErrorCode::TooLarge as i32);
    // File on disk untouched.
    assert_eq!(fs::read(&path).unwrap().len(), 50);
}

#[tokio::test]
async fn chmod_with_no_permission_returns_unspecified_error() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let file = root.join("p.txt");
    fs::write(&file, "x").unwrap();
    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::Chmod(FileChmod {
            path: file.to_string_lossy().into_owned(),
            recursive: false,
            no_follow_symlink: false,
            permission: None,
        })),
    };
    let err = expect_error(mgr.handle(&req).await);
    assert_eq!(err.code, FileErrorCode::Unspecified as i32);
}

#[cfg(unix)]
#[tokio::test]
async fn chmod_with_unix_permission_but_no_mode_or_owner_returns_unspecified_error() {
    // The Unix branch requires AT LEAST one of mode / owner / group to
    // be Some. All three None must surface as a clear "nothing to do"
    // error rather than silently no-op.
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let file = root.join("p.txt");
    fs::write(&file, "x").unwrap();
    let req = FileRequest {
        request_id: "t".into(),
        operation: Some(file_request::Operation::Chmod(FileChmod {
            path: file.to_string_lossy().into_owned(),
            recursive: false,
            no_follow_symlink: false,
            permission: Some(file_chmod::Permission::Unix(UnixPermission {
                mode: None,
                owner: None,
                group: None,
            })),
        })),
    };
    let err = expect_error(mgr.handle(&req).await);
    assert_eq!(err.code, FileErrorCode::Unspecified as i32);
}

// ── R10 follow-up: PostCreateCleanup behavior ─────────────────────────────
//
// These tests directly drive `verify_post_create` (test-only doc-hidden
// API) to pin each cleanup mode independently of an actual TOCTOU race.
// The race window is microseconds and unreliable to trigger from a
// single-threaded integration test, so we verify the **cleanup
// contract** instead: given that a post-create policy check rejects,
// what does each mode do to the on-disk artifact?

/// Build a `FilePolicyChecker` whose allowlist contains exactly `tmp_root`
/// (and its descendants), so any path *outside* that root will be
/// rejected by `policy.check_path`.
fn restrictive_policy(tmp_root: &Path) -> ahandd::file_manager::policy::FilePolicyChecker {
    let root_str = tmp_root.to_string_lossy().into_owned();
    ahandd::file_manager::policy::FilePolicyChecker::new(&ahandd::config::FilePolicyConfig {
        enabled: true,
        path_allowlist: vec![format!("{}/**", root_str), root_str.clone()],
        path_denylist: vec![],
        max_read_bytes: 100_000_000,
        max_write_bytes: 100_000_000,
        dangerous_paths: vec![],
    })
}

#[tokio::test]
async fn verify_post_create_remove_file_or_dir_unlinks_a_rejected_file() {
    // Mode invariant for Mkdir / Write / Symlink: when post-check
    // rejects, the artifact at the rejected path is unlinked so the
    // caller's failure surface is "operation didn't happen".
    let allowed = TempDir::new().unwrap();
    let allowed_root = allowed.path().canonicalize().unwrap();
    let policy = restrictive_policy(&allowed_root);

    // The artifact lives outside the allowlist — the post-check will
    // reject it and the cleanup must unlink.
    let outside = TempDir::new().unwrap();
    let outside_root = outside.path().canonicalize().unwrap();
    let leaked = outside_root.join("leaked.txt");
    fs::write(&leaked, "ghost").unwrap();
    assert!(leaked.exists());

    let err = ahandd::file_manager::verify_post_create(
        &policy,
        &leaked,
        ahandd::file_manager::PostCreateCleanup::RemoveFileOrDir,
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, FileErrorCode::PolicyDenied as i32);
    assert!(
        !leaked.exists(),
        "RemoveFileOrDir must unlink the rejected file"
    );
}

#[tokio::test]
async fn verify_post_create_remove_tree_all_purges_a_rejected_directory_tree() {
    // Mode invariant for recursive Copy: a partially-populated
    // directory tree at the rejected path must be fully removed —
    // `remove_dir` cannot unlink non-empty dirs, so a cleanup that
    // used the file-or-dir variant would leak the partial tree on
    // disk after returning PolicyDenied.
    let allowed = TempDir::new().unwrap();
    let policy = restrictive_policy(&allowed.path().canonicalize().unwrap());

    let outside = TempDir::new().unwrap();
    let outside_root = outside.path().canonicalize().unwrap();
    let tree_root = outside_root.join("tree");
    fs::create_dir(&tree_root).unwrap();
    fs::create_dir(tree_root.join("nested")).unwrap();
    fs::write(tree_root.join("a.txt"), b"a").unwrap();
    fs::write(tree_root.join("nested/b.txt"), b"b").unwrap();
    assert!(tree_root.exists());

    let err = ahandd::file_manager::verify_post_create(
        &policy,
        &tree_root,
        ahandd::file_manager::PostCreateCleanup::RemoveTreeAll,
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, FileErrorCode::PolicyDenied as i32);
    assert!(
        !tree_root.exists(),
        "RemoveTreeAll must recursively unlink the rejected tree (no leaked partial copy)"
    );
}

#[tokio::test]
async fn verify_post_create_leave_preserves_data_at_rejected_path() {
    // Mode invariant for Move: rename has already destroyed the source
    // by the time post-check runs. If the destination's canonical
    // resolution then rejects, deleting the destination would compound
    // the data loss — the user's data would be gone from BOTH the
    // original source path (deleted by rename) AND the rejected
    // destination (deleted by us). `Leave` preserves the data so the
    // operator can recover by inspecting the rejected destination.
    let allowed = TempDir::new().unwrap();
    let policy = restrictive_policy(&allowed.path().canonicalize().unwrap());

    let outside = TempDir::new().unwrap();
    let outside_root = outside.path().canonicalize().unwrap();
    let preserved = outside_root.join("user_data.txt");
    fs::write(&preserved, b"do not delete").unwrap();

    let err = ahandd::file_manager::verify_post_create(
        &policy,
        &preserved,
        ahandd::file_manager::PostCreateCleanup::Leave,
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, FileErrorCode::PolicyDenied as i32);
    assert!(
        preserved.exists(),
        "Leave must NOT touch the rejected artifact — that is the entire point"
    );
    assert_eq!(
        fs::read(&preserved).unwrap(),
        b"do not delete",
        "Leave must preserve content byte-for-byte"
    );
}

#[tokio::test]
async fn verify_post_create_returns_ok_when_path_is_inside_allowlist() {
    // Sanity: every cleanup mode is a no-op when the post-check passes.
    // We verify this for the Leave variant explicitly because it's the
    // new one — Remove* modes were already exercised by the existing
    // integration suite via Mkdir / Write / Copy / Symlink.
    let allowed = TempDir::new().unwrap();
    let allowed_root = allowed.path().canonicalize().unwrap();
    let policy = restrictive_policy(&allowed_root);

    let inside = allowed_root.join("ok.txt");
    fs::write(&inside, b"safe").unwrap();

    ahandd::file_manager::verify_post_create(
        &policy,
        &inside,
        ahandd::file_manager::PostCreateCleanup::Leave,
    )
    .await
    .expect("inside-allowlist path must pass post-check");
    assert!(inside.exists());
    assert_eq!(fs::read(&inside).unwrap(), b"safe");
}

// ── R10 (this PR) — TOCTOU race-window closure ─────────────────────────────
//
// These tests exercise the dirfd-based safe-open layer added in
// `file_manager::io_safe`. We can't time the race itself in a
// single-threaded test, so we **simulate the post-race state**: the path
// passed to the handler has a symlink in an ancestor at the moment the
// handler runs. In production this could happen if an attacker swapped
// that ancestor between the policy check and the handler — exactly the
// bug class the fix targets.
//
// The handler must:
//   1. detect the swap (parent walk via openat2/chain-open with NOFOLLOW),
//   2. abort with `PolicyDenied`,
//   3. **not** mutate either side of the would-be operation (no escape,
//      no data loss).

#[cfg(unix)]
#[tokio::test]
async fn mkdir_with_symlinked_parent_returns_policy_denied_and_does_not_escape() {
    // Post-race shape: the daemon's policy layer canonicalized the
    // request's path through a real directory `/allowed/parent/`, but
    // by the time the handler runs the attacker has replaced
    // `/allowed/parent` with a symlink pointing into a directory the
    // attacker controls outside the allowlist. Without dirfd-based
    // safety, `tokio::fs::create_dir` would walk the new symlink and
    // create the new dir at the attacker's target. With safe-open, the
    // parent walk fails with ELOOP (Linux) or ENOTDIR (macOS) and the
    // handler returns PolicyDenied without touching the attacker side.
    let allowed = TempDir::new().unwrap();
    let allowed_root = allowed.path().canonicalize().unwrap();
    let attacker = TempDir::new().unwrap();
    let attacker_root = attacker.path().canonicalize().unwrap();

    let sentinel = attacker_root.join("sentinel.txt");
    fs::write(&sentinel, b"do_not_touch").unwrap();

    let parent_link = allowed_root.join("parent_link");
    std::os::unix::fs::symlink(&attacker_root, &parent_link).unwrap();
    let target = parent_link.join("new_dir");

    let req = FileMkdir {
        path: target.to_string_lossy().into_owned(),
        mode: None,
        recursive: false,
    };
    let err = ahandd::file_manager::fs_ops::handle_mkdir(&req, &target)
        .await
        .expect_err("handler must reject when the parent is a symlink");
    assert_eq!(
        err.code,
        FileErrorCode::PolicyDenied as i32,
        "expected PolicyDenied (R10 safe-open rejection); got code={} message={:?}",
        err.code,
        err.message,
    );
    assert!(
        !attacker_root.join("new_dir").exists(),
        "mkdir must not have escaped to the attacker's target",
    );
    assert_eq!(
        fs::read(&sentinel).unwrap(),
        b"do_not_touch",
        "pre-existing data inside the attacker's target must be untouched",
    );
}

#[cfg(unix)]
#[tokio::test]
async fn move_with_symlinked_destination_parent_does_not_destroy_source() {
    // Move is the **most data-loss-sensitive** of the R10 ops because
    // rename atomically destroys the source. If the destination's
    // parent has been swapped for a symlink, the legacy path-based
    // rename would either succeed at the attacker's target (data
    // exfil) or fail mid-flight in a way that left the source gone
    // but the destination missing. Our renameat-via-safe-dirfd
    // refactor must reject **before** the rename runs, so the source
    // survives intact.
    let allowed = TempDir::new().unwrap();
    let allowed_root = allowed.path().canonicalize().unwrap();
    let source = allowed_root.join("payload.txt");
    fs::write(&source, b"important_data").unwrap();

    let attacker = TempDir::new().unwrap();
    let attacker_root = attacker.path().canonicalize().unwrap();
    let dest_parent_link = allowed_root.join("dest_parent_link");
    std::os::unix::fs::symlink(&attacker_root, &dest_parent_link).unwrap();
    let destination = dest_parent_link.join("moved.txt");

    let req = FileMove {
        source: source.to_string_lossy().into_owned(),
        destination: destination.to_string_lossy().into_owned(),
        overwrite: false,
    };
    let err = ahandd::file_manager::fs_ops::handle_move(&req, &source, &destination)
        .await
        .expect_err("handler must reject when destination's parent is a symlink");
    assert_eq!(
        err.code,
        FileErrorCode::PolicyDenied as i32,
        "expected PolicyDenied; got code={} message={:?}",
        err.code,
        err.message,
    );

    assert!(source.exists(), "source must survive a rejected move");
    assert_eq!(
        fs::read(&source).unwrap(),
        b"important_data",
        "source content must be byte-for-byte preserved",
    );
    assert!(
        !attacker_root.join("moved.txt").exists(),
        "rename must not have escaped to the attacker's target",
    );
}

#[cfg(unix)]
#[tokio::test]
async fn copy_single_file_with_symlinked_destination_parent_returns_policy_denied() {
    // Mirror of the Move case for Copy. A path-based `tokio::fs::copy`
    // would write the source bytes at the attacker's target. Our
    // openat-based dest open with NOFOLLOW + safe parent walk rejects
    // before any write occurs.
    let allowed = TempDir::new().unwrap();
    let allowed_root = allowed.path().canonicalize().unwrap();
    let source = allowed_root.join("readable.txt");
    fs::write(&source, b"secret").unwrap();

    let attacker = TempDir::new().unwrap();
    let attacker_root = attacker.path().canonicalize().unwrap();
    let dest_parent_link = allowed_root.join("dest_link");
    std::os::unix::fs::symlink(&attacker_root, &dest_parent_link).unwrap();
    let destination = dest_parent_link.join("copy.txt");

    let req = FileCopy {
        source: source.to_string_lossy().into_owned(),
        destination: destination.to_string_lossy().into_owned(),
        recursive: false,
        overwrite: false,
    };
    let err = ahandd::file_manager::fs_ops::handle_copy(&req, &source, &destination)
        .await
        .expect_err("handler must reject when destination's parent is a symlink");
    assert_eq!(
        err.code,
        FileErrorCode::PolicyDenied as i32,
        "expected PolicyDenied; got code={} message={:?}",
        err.code,
        err.message,
    );
    assert_eq!(fs::read(&source).unwrap(), b"secret");
    assert!(!attacker_root.join("copy.txt").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn create_symlink_with_symlinked_parent_returns_policy_denied() {
    // CreateSymlink lands a new symlink at link_path. If link_path's
    // parent is itself a symlink the attacker injected, a naive
    // implementation would follow it and place the link in the
    // attacker's directory. Our safe-open rejects before symlinkat runs.
    let allowed = TempDir::new().unwrap();
    let allowed_root = allowed.path().canonicalize().unwrap();
    let attacker = TempDir::new().unwrap();
    let attacker_root = attacker.path().canonicalize().unwrap();

    let parent_link = allowed_root.join("parent_link");
    std::os::unix::fs::symlink(&attacker_root, &parent_link).unwrap();
    let link_path = parent_link.join("new_symlink");

    let req = FileCreateSymlink {
        target: "/etc/hosts".into(),
        link_path: link_path.to_string_lossy().into_owned(),
    };
    let err = ahandd::file_manager::fs_ops::handle_create_symlink(&req, &link_path)
        .await
        .expect_err("handler must reject when link parent is a symlink");
    assert_eq!(
        err.code,
        FileErrorCode::PolicyDenied as i32,
        "expected PolicyDenied; got code={} message={:?}",
        err.code,
        err.message,
    );
    assert!(
        !attacker_root.join("new_symlink").exists(),
        "no symlink must have been planted in the attacker's dir",
    );
}

#[cfg(unix)]
#[tokio::test]
async fn chmod_with_symlinked_parent_does_not_modify_target_permissions() {
    use std::os::unix::fs::PermissionsExt;
    // Chmod via dirfd: we want the leaf chmod to never reach the
    // attacker's target. Pre-create a victim file with a known mode
    // and verify it stays exactly that mode after the handler rejects.
    let allowed = TempDir::new().unwrap();
    let allowed_root = allowed.path().canonicalize().unwrap();
    let attacker = TempDir::new().unwrap();
    let attacker_root = attacker.path().canonicalize().unwrap();

    let victim = attacker_root.join("victim.txt");
    fs::write(&victim, b"x").unwrap();
    fs::set_permissions(&victim, fs::Permissions::from_mode(0o600)).unwrap();
    let original_mode = fs::metadata(&victim).unwrap().permissions().mode() & 0o777;

    let parent_link = allowed_root.join("parent_link");
    std::os::unix::fs::symlink(&attacker_root, &parent_link).unwrap();
    let leaf = parent_link.join("victim.txt");

    let req = FileChmod {
        path: leaf.to_string_lossy().into_owned(),
        recursive: false,
        no_follow_symlink: false,
        permission: Some(file_chmod::Permission::Unix(UnixPermission {
            mode: Some(0o777),
            owner: None,
            group: None,
        })),
    };
    let err = ahandd::file_manager::fs_ops::handle_chmod(&req, &leaf)
        .await
        .expect_err("handler must reject when leaf's parent is a symlink");
    assert_eq!(
        err.code,
        FileErrorCode::PolicyDenied as i32,
        "expected PolicyDenied; got code={} message={:?}",
        err.code,
        err.message,
    );

    let final_mode = fs::metadata(&victim).unwrap().permissions().mode() & 0o777;
    assert_eq!(
        final_mode, original_mode,
        "victim.txt mode must be unchanged: original={:o}, after rejected chmod={:o}",
        original_mode, final_mode,
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn linux_openat2_path_returns_policy_denied_for_symlinked_ancestor() {
    // Linux-specific shape: on 5.6+ kernels the `openat2` fast path
    // returns ELOOP atomically; on older kernels we fall back to
    // chain-open which also returns ELOOP. Either way the surfaced
    // FileError must be PolicyDenied — never NotFound, never Io —
    // because operators must read this error as "policy refused to
    // traverse a symlink", not "your filesystem is missing files".
    let dir = TempDir::new().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let real = root.join("real_dir");
    fs::create_dir(&real).unwrap();
    let link = root.join("link_dir");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let target = link.join("inner");

    let req = FileMkdir {
        path: target.to_string_lossy().into_owned(),
        mode: None,
        recursive: false,
    };
    let err = ahandd::file_manager::fs_ops::handle_mkdir(&req, &target)
        .await
        .expect_err("Linux openat2(RESOLVE_NO_SYMLINKS) must reject the symlinked parent");
    assert_eq!(
        err.code,
        FileErrorCode::PolicyDenied as i32,
        "Linux ELOOP path must surface as PolicyDenied; got code={} message={:?}",
        err.code,
        err.message,
    );
    assert!(
        !real.join("inner").exists(),
        "no inner dir must have been created at the symlink target",
    );
}
