//! Integration tests for daemon file operations.
//!
//! These tests drive the `FileManager::handle` entry point directly (no hub or
//! WebSocket involved), with a permissive policy scoped to a per-test temp dir.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ahand_protocol::{
    file_request, file_response, FileErrorCode, FileGlob, FileList, FileMkdir, FileRequest,
    FileStat, FileType,
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

