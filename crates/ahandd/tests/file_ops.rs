//! Integration tests for daemon file operations.
//!
//! These tests drive the `FileManager::handle` entry point directly (no hub or
//! WebSocket involved), with a permissive policy scoped to a per-test temp dir.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ahand_protocol::{
    file_position, file_read_text, file_request, file_response, FileErrorCode, FileGlob, FileList,
    FileMkdir, FilePosition, FileReadText, FileRequest, FileStat, FileType, LineCol, StopReason,
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

