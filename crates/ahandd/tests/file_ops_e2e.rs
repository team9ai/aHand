//! End-to-end proto round-trip tests for every file operation.
//!
//! These tests verify the full pipeline from encoded proto bytes on the wire
//! through `FileManager::handle` and back out as encoded `FileResponse`
//! bytes. The goal is to catch anything that would break over a real
//! WebSocket even if the handler unit tests (tests/file_ops.rs) pass.

use std::fs;
use std::path::{Path, PathBuf};

use ahand_protocol::{
    file_chmod, file_edit, file_position, file_request, file_response, file_write, full_write,
    ByteRangeReplace, DeleteMode, FileAppend, FileChmod, FileCopy, FileCreateSymlink, FileDelete,
    FileEdit, FileErrorCode, FileGlob, FileList, FileMkdir, FileMove, FilePosition, FileReadBinary,
    FileReadImage, FileReadText, FileRequest, FileResponse, FileStat, FileType, FileWrite,
    FullWrite, ImageFormat, LineRangeReplace, StringReplace, UnixPermission,
};
use ahandd::config::FilePolicyConfig;
use ahandd::file_manager::FileManager;
use prost::Message;
use tempfile::TempDir;

fn test_manager(tmp: &TempDir) -> (FileManager, PathBuf) {
    let root = tmp.path().canonicalize().unwrap();
    let pattern = format!("{}/**", root.to_string_lossy().trim_end_matches('/'));
    let self_pat = root.to_string_lossy().into_owned();
    let mgr = FileManager::new(&FilePolicyConfig {
        enabled: true,
        path_allowlist: vec![pattern, self_pat],
        path_denylist: vec![],
        max_read_bytes: 100_000_000,
        max_write_bytes: 100_000_000,
        dangerous_paths: vec![],
    });
    (mgr, root)
}

/// Encode request → decode → handle → encode response → decode. Returns the
/// parsed FileResponse.
async fn roundtrip(mgr: &FileManager, req: FileRequest) -> FileResponse {
    let encoded_req = req.encode_to_vec();
    let decoded_req = FileRequest::decode(encoded_req.as_slice()).expect("proto request decode");
    let resp = mgr.handle(&decoded_req).await;
    let encoded_resp = resp.encode_to_vec();
    FileResponse::decode(encoded_resp.as_slice()).expect("proto response decode")
}

fn path_str(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

// ── Every operation gets exactly one happy-path round trip ────────────────

#[tokio::test]
async fn e2e_stat() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let file = root.join("stat.txt");
    fs::write(&file, "hi").unwrap();

    let req = FileRequest {
        request_id: "stat".into(),
        operation: Some(file_request::Operation::Stat(FileStat {
            path: path_str(&file),
            no_follow_symlink: false,
        })),
    };
    let resp = roundtrip(&mgr, req).await;
    let Some(file_response::Result::Stat(result)) = resp.result else {
        panic!("expected stat result, got {:?}", resp.result);
    };
    assert_eq!(result.file_type, FileType::File as i32);
    assert_eq!(result.size, 2);
}

#[tokio::test]
async fn e2e_list() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    fs::write(root.join("a"), "a").unwrap();
    fs::write(root.join("b"), "b").unwrap();

    let req = FileRequest {
        request_id: "list".into(),
        operation: Some(file_request::Operation::List(FileList {
            path: path_str(&root),
            max_results: None,
            offset: None,
            include_hidden: false,
        })),
    };
    let resp = roundtrip(&mgr, req).await;
    let Some(file_response::Result::List(result)) = resp.result else {
        panic!("expected list result");
    };
    assert_eq!(result.total_count, 2);
}

#[tokio::test]
async fn e2e_glob() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    fs::write(root.join("one.rs"), "x").unwrap();
    fs::write(root.join("two.rs"), "x").unwrap();

    let req = FileRequest {
        request_id: "glob".into(),
        operation: Some(file_request::Operation::Glob(FileGlob {
            pattern: "*.rs".into(),
            base_path: Some(path_str(&root)),
            max_results: None,
        })),
    };
    let resp = roundtrip(&mgr, req).await;
    let Some(file_response::Result::Glob(result)) = resp.result else {
        panic!("expected glob result");
    };
    assert_eq!(result.total_matches, 2);
}

#[tokio::test]
async fn e2e_mkdir() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let new_dir = root.join("new");

    let req = FileRequest {
        request_id: "mkdir".into(),
        operation: Some(file_request::Operation::Mkdir(FileMkdir {
            path: path_str(&new_dir),
            recursive: false,
            mode: None,
        })),
    };
    let resp = roundtrip(&mgr, req).await;
    let Some(file_response::Result::Mkdir(result)) = resp.result else {
        panic!("expected mkdir result");
    };
    assert!(!result.already_existed);
    assert!(new_dir.is_dir());
}

#[tokio::test]
async fn e2e_read_text() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let file = root.join("text.txt");
    fs::write(&file, "one\ntwo\n").unwrap();

    let req = FileRequest {
        request_id: "read_text".into(),
        operation: Some(file_request::Operation::ReadText(FileReadText {
            path: path_str(&file),
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
    let resp = roundtrip(&mgr, req).await;
    let Some(file_response::Result::ReadText(result)) = resp.result else {
        panic!("expected read_text result");
    };
    assert_eq!(result.lines.len(), 2);
}

#[tokio::test]
async fn e2e_read_binary() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let file = root.join("bin.dat");
    fs::write(&file, b"0123456789").unwrap();

    let req = FileRequest {
        request_id: "read_binary".into(),
        operation: Some(file_request::Operation::ReadBinary(FileReadBinary {
            path: path_str(&file),
            byte_offset: 2,
            byte_length: 4,
            max_bytes: None,
            no_follow_symlink: false,
        })),
    };
    let resp = roundtrip(&mgr, req).await;
    let Some(file_response::Result::ReadBinary(result)) = resp.result else {
        panic!("expected read_binary result");
    };
    assert_eq!(result.content, b"2345");
}

#[tokio::test]
async fn e2e_read_image() {
    use image::{ImageBuffer, Rgb};
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let file = root.join("img.png");
    let img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_fn(32, 32, |_, _| Rgb([0, 0, 0]));
    img.save_with_format(&file, image::ImageFormat::Png).unwrap();

    let req = FileRequest {
        request_id: "read_image".into(),
        operation: Some(file_request::Operation::ReadImage(FileReadImage {
            path: path_str(&file),
            max_width: Some(16),
            max_height: Some(16),
            max_bytes: None,
            quality: None,
            output_format: Some(ImageFormat::Png as i32),
            no_follow_symlink: false,
        })),
    };
    let resp = roundtrip(&mgr, req).await;
    let Some(file_response::Result::ReadImage(result)) = resp.result else {
        panic!("expected read_image result");
    };
    assert_eq!(result.width, 16);
    assert_eq!(result.height, 16);
}

#[tokio::test]
async fn e2e_write() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let file = root.join("w.txt");

    let req = FileRequest {
        request_id: "write".into(),
        operation: Some(file_request::Operation::Write(FileWrite {
            path: path_str(&file),
            create_parents: false,
            encoding: None,
            no_follow_symlink: false,
            method: Some(file_write::Method::FullWrite(FullWrite {
                source: Some(full_write::Source::Content(b"payload".to_vec())),
                ..Default::default()
            })),
        })),
    };
    let resp = roundtrip(&mgr, req).await;
    let Some(file_response::Result::Write(result)) = resp.result else {
        panic!("expected write result");
    };
    assert_eq!(result.bytes_written, 7);
    assert_eq!(fs::read(&file).unwrap(), b"payload");
}

#[tokio::test]
async fn e2e_edit() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let file = root.join("e.txt");
    fs::write(&file, "hello world").unwrap();

    let req = FileRequest {
        request_id: "edit".into(),
        operation: Some(file_request::Operation::Edit(FileEdit {
            path: path_str(&file),
            encoding: None,
            no_follow_symlink: false,
            method: Some(file_edit::Method::StringReplace(StringReplace {
                old_string: "world".into(),
                new_string: "friend".into(),
                replace_all: false,
            })),
        })),
    };
    let resp = roundtrip(&mgr, req).await;
    let Some(file_response::Result::Edit(result)) = resp.result else {
        panic!("expected edit result");
    };
    assert_eq!(result.replacements_made, Some(1));
    assert_eq!(fs::read_to_string(&file).unwrap(), "hello friend");
}

#[tokio::test]
async fn e2e_delete() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let file = root.join("d.txt");
    fs::write(&file, "x").unwrap();

    let req = FileRequest {
        request_id: "delete".into(),
        operation: Some(file_request::Operation::Delete(FileDelete {
            path: path_str(&file),
            recursive: false,
            mode: DeleteMode::Permanent as i32,
            no_follow_symlink: false,
        })),
    };
    let resp = roundtrip(&mgr, req).await;
    let Some(file_response::Result::Delete(result)) = resp.result else {
        panic!("expected delete result");
    };
    assert_eq!(result.items_deleted, 1);
    assert!(!file.exists());
}

#[tokio::test]
async fn e2e_copy() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let src = root.join("src.txt");
    let dst = root.join("dst.txt");
    fs::write(&src, "content").unwrap();

    let req = FileRequest {
        request_id: "copy".into(),
        operation: Some(file_request::Operation::Copy(FileCopy {
            source: path_str(&src),
            destination: path_str(&dst),
            recursive: false,
            overwrite: false,
        })),
    };
    let resp = roundtrip(&mgr, req).await;
    let Some(file_response::Result::Copy(result)) = resp.result else {
        panic!("expected copy result");
    };
    assert_eq!(result.items_copied, 1);
    assert_eq!(fs::read_to_string(&dst).unwrap(), "content");
}

#[tokio::test]
async fn e2e_move() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let src = root.join("src.txt");
    let dst = root.join("dst.txt");
    fs::write(&src, "x").unwrap();

    let req = FileRequest {
        request_id: "move".into(),
        operation: Some(file_request::Operation::Move(FileMove {
            source: path_str(&src),
            destination: path_str(&dst),
            overwrite: false,
        })),
    };
    let resp = roundtrip(&mgr, req).await;
    let Some(file_response::Result::MoveResult(_)) = resp.result else {
        panic!("expected move_result");
    };
    assert!(!src.exists());
    assert!(dst.exists());
}

#[cfg(unix)]
#[tokio::test]
async fn e2e_create_symlink() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let target = root.join("target");
    let link = root.join("link");
    fs::write(&target, "x").unwrap();

    let req = FileRequest {
        request_id: "symlink".into(),
        operation: Some(file_request::Operation::CreateSymlink(FileCreateSymlink {
            target: path_str(&target),
            link_path: path_str(&link),
        })),
    };
    let resp = roundtrip(&mgr, req).await;
    let Some(file_response::Result::CreateSymlink(_)) = resp.result else {
        panic!("expected create_symlink result");
    };
    assert!(fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
}

#[cfg(unix)]
#[tokio::test]
async fn e2e_chmod() {
    use std::os::unix::fs::PermissionsExt;
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let file = root.join("perm.txt");
    fs::write(&file, "x").unwrap();

    let req = FileRequest {
        request_id: "chmod".into(),
        operation: Some(file_request::Operation::Chmod(FileChmod {
            path: path_str(&file),
            recursive: false,
            no_follow_symlink: false,
            permission: Some(file_chmod::Permission::Unix(UnixPermission {
                mode: Some(0o600),
                owner: None,
                group: None,
            })),
        })),
    };
    let resp = roundtrip(&mgr, req).await;
    let Some(file_response::Result::Chmod(_)) = resp.result else {
        panic!("expected chmod result");
    };
    assert_eq!(
        fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

// ── Error & rejection flows ───────────────────────────────────────────────

#[tokio::test]
async fn e2e_policy_rejection_returns_policy_denied_error() {
    let dir = TempDir::new().unwrap();
    let (mgr, _root) = test_manager(&dir);

    let req = FileRequest {
        request_id: "denied".into(),
        operation: Some(file_request::Operation::Stat(FileStat {
            path: "/etc/passwd".into(),
            no_follow_symlink: false,
        })),
    };
    let resp = roundtrip(&mgr, req).await;
    let Some(file_response::Result::Error(err)) = resp.result else {
        panic!("expected error");
    };
    assert_eq!(err.code, FileErrorCode::PolicyDenied as i32);
}

#[tokio::test]
async fn e2e_not_found_returns_not_found_error() {
    let dir = TempDir::new().unwrap();
    let (mgr, root) = test_manager(&dir);
    let missing = root.join("not-there.txt");

    let req = FileRequest {
        request_id: "not-found".into(),
        operation: Some(file_request::Operation::Stat(FileStat {
            path: path_str(&missing),
            no_follow_symlink: false,
        })),
    };
    let resp = roundtrip(&mgr, req).await;
    let Some(file_response::Result::Error(err)) = resp.result else {
        panic!("expected error");
    };
    assert_eq!(err.code, FileErrorCode::NotFound as i32);
}

#[tokio::test]
async fn e2e_disabled_policy_rejects_everything() {
    let mgr = FileManager::new(&FilePolicyConfig::default());

    let req = FileRequest {
        request_id: "disabled".into(),
        operation: Some(file_request::Operation::Stat(FileStat {
            path: "/anything".into(),
            no_follow_symlink: false,
        })),
    };
    let resp = roundtrip(&mgr, req).await;
    let Some(file_response::Result::Error(err)) = resp.result else {
        panic!("expected error");
    };
    assert_eq!(err.code, FileErrorCode::PolicyDenied as i32);
}

// Touched types that are otherwise only used transitively via handler paths.
#[allow(dead_code)]
fn _touch_unused_types() {
    let _ = FileAppend { content: vec![] };
    let _ = LineRangeReplace {
        start_line: 1,
        end_line: 1,
        new_content: String::new(),
    };
    let _ = ByteRangeReplace {
        byte_offset: 0,
        byte_length: 0,
        new_content: vec![],
    };
    let _ = FilePosition {
        position: Some(file_position::Position::Line(1)),
    };
}
