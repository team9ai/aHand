//! Integration tests for daemon file operations.
//!
//! These tests drive the `FileManager::handle` entry point directly (no hub or
//! WebSocket involved), with a permissive policy scoped to a per-test temp dir.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ahand_protocol::{
    file_chmod, file_edit, file_position, file_read_text, file_request, file_response, file_write,
    full_write, ByteRangeReplace, DeleteMode, FileAppend, FileChmod, FileCopy,
    FileCreateSymlink, FileDelete, FileEdit, FileErrorCode, FileGlob, FileList, FileMkdir,
    FileMove, FilePosition, FileReadBinary, FileReadImage, FileReadText, FileRequest, FileStat,
    FileType, FileWrite, FullWrite, ImageFormat, LineCol, LineRangeReplace, StopReason,
    StringReplace, UnixPermission, WriteAction,
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

fn expect_read_text(
    resp: ahand_protocol::FileResponse,
) -> ahand_protocol::FileReadTextResult {
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

fn expect_read_binary(
    resp: ahand_protocol::FileResponse,
) -> ahand_protocol::FileReadBinaryResult {
    match resp.result {
        Some(file_response::Result::ReadBinary(r)) => r,
        other => panic!("expected read_binary result, got {other:?}"),
    }
}

fn expect_read_image(
    resp: ahand_protocol::FileResponse,
) -> ahand_protocol::FileReadImageResult {
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

fn expect_symlink(
    resp: ahand_protocol::FileResponse,
) -> ahand_protocol::FileCreateSymlinkResult {
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
    assert!(result.detected_encoding.to_ascii_lowercase().contains("gbk"));
}

#[tokio::test]
async fn read_text_nonexistent_file_returns_error() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let missing = root.join("missing.txt");

    let resp = mgr.handle(&wrap_read_text(read_text_request(&missing))).await;
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
    let img: ImageBuffer<Rgb<u8>, Vec<u8>> =
        ImageBuffer::from_fn(width, height, |x, y| {
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

    let resp = mgr.handle(&write_request_full(&file, &vec![0u8; 100], false)).await;
    let err = expect_error(resp);
    assert_eq!(err.code, FileErrorCode::TooLarge as i32);
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
    assert_eq!(
        fs::read_to_string(dst_dir.join("sub/b.txt")).unwrap(),
        "b"
    );
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
    assert_eq!(fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
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
        fs::metadata(sub.join("a.txt")).unwrap().permissions().mode() & 0o777,
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
