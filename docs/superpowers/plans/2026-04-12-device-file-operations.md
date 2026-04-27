# Device File Operations Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers-extended-cc:subagent-driven-development (recommended) or superpowers-extended-cc:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement file/folder CRUD operations (read text/binary/image, write, edit, delete, chmod, stat, list, glob, mkdir, copy, move, symlink) executed by the daemon on local filesystem, commanded by cloud agents and dashboard operators.

**Architecture:** New `FileRequest`/`FileResponse` proto messages (envelope fields 31/32) with oneof operations. Daemon-side `FileManager` handles execution with policy/session integration. Hub forwards messages and provides REST endpoints + S3 pre-signed URL flow for large files.

**Tech Stack:** Rust (prost, tokio, image crate), Protobuf, TypeScript (ts-proto), axum (hub HTTP), S3 (aws-sdk-s3)

## File Structure

### New Files

| Path | Responsibility |
|------|---------------|
| `proto/ahand/v1/file_ops.proto` | All file operation message definitions |
| `crates/ahandd/src/file_manager/mod.rs` | FileManager struct, dispatch, re-exports |
| `crates/ahandd/src/file_manager/text_read.rs` | Text reading with triple-limit pagination |
| `crates/ahandd/src/file_manager/binary_read.rs` | Binary + image reading |
| `crates/ahandd/src/file_manager/write_ops.rs` | Write, edit, append operations |
| `crates/ahandd/src/file_manager/fs_ops.rs` | Delete, copy, move, mkdir, symlink, chmod, stat |
| `crates/ahandd/src/file_manager/policy.rs` | File-specific policy (path allow/deny, traversal detection) |
| `crates/ahand-hub/src/http/files.rs` | Hub REST endpoints for file operations |
| `crates/ahand-hub/src/s3.rs` | S3 client wrapper for pre-signed URLs |
| `crates/ahandd/tests/file_ops.rs` | Integration tests for file operations |

### Modified Files

| Path | Change |
|------|--------|
| `proto/ahand/v1/envelope.proto` | Add fields 31, 32 to payload oneof |
| `crates/ahand-protocol/build.rs` | Add `file_ops.proto` to compilation |
| `crates/ahandd/Cargo.toml` | Add `image`, `glob`, `trash` crates |
| `crates/ahandd/src/main.rs` | Initialize FileManager, pass to client |
| `crates/ahandd/src/config.rs` | Add `FilePolicy` config section |
| `crates/ahandd/src/ahand_client.rs` | Route FileRequest to handler |
| `crates/ahand-hub/src/http/mod.rs` | Add file operation routes |
| `crates/ahand-hub/src/ws/device_gateway.rs` | Handle FileResponse from device |
| `crates/ahand-hub/Cargo.toml` | Add `aws-sdk-s3` for pre-signed URLs |
| `crates/ahand-hub/src/config.rs` | Add S3/file transfer config |

---

### Task 0: Proto Definitions & Compilation

**Goal:** Define all file operation messages in protobuf and verify they compile to Rust + TypeScript types.

**Files:**
- Create: `proto/ahand/v1/file_ops.proto`
- Modify: `proto/ahand/v1/envelope.proto:16-38` (payload oneof)
- Modify: `crates/ahand-protocol/build.rs`

**Acceptance Criteria:**
- [ ] `file_ops.proto` contains all message types from spec
- [ ] `envelope.proto` has `file_request = 31` and `file_response = 32`
- [ ] `cargo build -p ahand-protocol` succeeds
- [ ] TypeScript types generate without error (`npm run generate` in packages/proto-ts)
- [ ] Generated Rust code has `FileRequest`, `FileResponse`, all nested types

**Verify:** `cargo build -p ahand-protocol 2>&1 | tail -5` → "Finished"

**Steps:**

- [ ] **Step 1: Create `file_ops.proto`**

Create `proto/ahand/v1/file_ops.proto` with the full spec content. This file contains all FileRequest/FileResponse messages, enums, and nested types as defined in the design spec at `docs/superpowers/specs/2026-04-12-device-file-operations-design.md`.

The proto file must include:
- `FileRequest` with oneof operation (13 variants)
- `FileResponse` with oneof result (14 variants including error)
- All operation messages: `FileReadText`, `FileReadBinary`, `FileReadImage`, `FileWrite`, `FileEdit`, `FileDelete`, `FileChmod`, `FileStat`, `FileList`, `FileGlob`, `FileMkdir`, `FileCopy`, `FileMove`, `FileCreateSymlink`
- All result messages
- Helper messages: `LineCol`, `FilePosition`, `PositionInfo`, `TextLine`, `FileEntry`, `UnixPermission`, `WindowsAcl`, `AclEntry`, `FullWrite`, `FileAppend`, `StringReplace`, `LineRangeReplace`, `ByteRangeReplace`, `FileTransferUrl`
- Enums: `StopReason`, `ImageFormat`, `WriteAction`, `DeleteMode`, `FileType`, `FileErrorCode`, `AclEntryType`

- [ ] **Step 2: Add to envelope.proto**

In `proto/ahand/v1/envelope.proto`, add inside `oneof payload`:
```protobuf
    FileRequest  file_request  = 31;
    FileResponse file_response = 32;
```

Add `import "ahand/v1/file_ops.proto";` at the top.

- [ ] **Step 3: Update build.rs**

In `crates/ahand-protocol/build.rs`, add the new proto to compilation:
```rust
prost_build::compile_protos(
    &[
        "../../proto/ahand/v1/envelope.proto",
        "../../proto/ahand/v1/browser.proto",
        "../../proto/ahand/v1/file_ops.proto",
    ],
    &["../../proto"],
)?;
```

Add: `println!("cargo:rerun-if-changed=../../proto/ahand/v1/file_ops.proto");`

- [ ] **Step 4: Verify Rust compilation**

Run: `cargo build -p ahand-protocol`
Expected: Build succeeds, generated types available.

- [ ] **Step 5: Verify TypeScript generation**

Run: `cd packages/proto-ts && npm run generate`
Expected: TypeScript types generated for all file operation messages.

- [ ] **Step 6: Commit**

```bash
git add proto/ahand/v1/file_ops.proto proto/ahand/v1/envelope.proto crates/ahand-protocol/build.rs
git commit -m "feat(protocol): add file operations proto definitions"
```

---

### Task 1: Daemon FileManager Skeleton, Config & Routing

**Goal:** Create the FileManager module structure, add `[file_policy]` config section, and wire up FileRequest routing in the daemon's WebSocket handler.

**Files:**
- Create: `crates/ahandd/src/file_manager.rs` (module root, re-exports)
- Create: `crates/ahandd/src/file_manager/policy.rs` (file-specific policy)
- Modify: `crates/ahandd/src/config.rs` (add `FilePolicyConfig`)
- Modify: `crates/ahandd/src/main.rs` (initialize FileManager)
- Modify: `crates/ahandd/src/ahand_client.rs` (route FileRequest)
- Modify: `crates/ahandd/Cargo.toml` (add dependencies)

**Acceptance Criteria:**
- [ ] `FilePolicyConfig` parses from TOML with path allowlist/denylist
- [ ] `FileManager::new(config)` constructs successfully
- [ ] FileRequest routing in `ahand_client.rs` dispatches to `handle_file_request()`
- [ ] Policy checks reject paths outside allowlist
- [ ] Policy checks detect path traversal (`../`)
- [ ] Symlink target validation works
- [ ] `cargo build -p ahandd` succeeds
- [ ] Unit tests pass for policy checker

**Verify:** `cargo test -p ahandd -- file_manager` → all tests pass

**Steps:**

- [ ] **Step 1: Add dependencies to Cargo.toml**

In `crates/ahandd/Cargo.toml`, add:
```toml
image = { version = "0.25", default-features = false, features = ["jpeg", "png", "webp"] }
glob = "0.3"
trash = "5"
```

- [ ] **Step 2: Write policy tests**

Create `crates/ahandd/tests/file_policy_tests.rs`:
```rust
use ahandd::file_manager::policy::{FilePolicyChecker, FilePolicyConfig};

#[test]
fn test_path_within_allowlist() {
    let config = FilePolicyConfig {
        enabled: true,
        path_allowlist: vec!["/home/user/**".into()],
        path_denylist: vec![],
        max_read_bytes: 100_000_000,
        max_write_bytes: 100_000_000,
        dangerous_paths: vec![],
    };
    let checker = FilePolicyChecker::new(&config);
    assert!(checker.check_path("/home/user/foo.txt", false).is_ok());
}

#[test]
fn test_path_outside_allowlist() {
    let config = FilePolicyConfig {
        enabled: true,
        path_allowlist: vec!["/home/user/**".into()],
        path_denylist: vec![],
        max_read_bytes: 100_000_000,
        max_write_bytes: 100_000_000,
        dangerous_paths: vec![],
    };
    let checker = FilePolicyChecker::new(&config);
    assert!(checker.check_path("/etc/passwd", false).is_err());
}

#[test]
fn test_path_traversal_detected() {
    let config = FilePolicyConfig {
        enabled: true,
        path_allowlist: vec!["/home/user/**".into()],
        path_denylist: vec![],
        max_read_bytes: 100_000_000,
        max_write_bytes: 100_000_000,
        dangerous_paths: vec![],
    };
    let checker = FilePolicyChecker::new(&config);
    assert!(checker.check_path("/home/user/../../etc/passwd", false).is_err());
}

#[test]
fn test_denylist_overrides_allowlist() {
    let config = FilePolicyConfig {
        enabled: true,
        path_allowlist: vec!["/home/user/**".into()],
        path_denylist: vec!["/home/user/.ssh/**".into()],
        max_read_bytes: 100_000_000,
        max_write_bytes: 100_000_000,
        dangerous_paths: vec![],
    };
    let checker = FilePolicyChecker::new(&config);
    assert!(checker.check_path("/home/user/.ssh/id_rsa", false).is_err());
}

#[test]
fn test_dangerous_path_requires_strict() {
    let config = FilePolicyConfig {
        enabled: true,
        path_allowlist: vec!["/home/user/**".into()],
        path_denylist: vec![],
        max_read_bytes: 100_000_000,
        max_write_bytes: 100_000_000,
        dangerous_paths: vec!["/home/user/.bashrc".into()],
    };
    let checker = FilePolicyChecker::new(&config);
    let result = checker.check_path("/home/user/.bashrc", false);
    assert!(result.unwrap().needs_approval);
}
```

- [ ] **Step 3: Implement FilePolicyConfig in config.rs**

Add to `crates/ahandd/src/config.rs`:
```rust
/// File operation policy configuration.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct FilePolicyConfig {
    /// Enable file operations (default: false).
    #[serde(default)]
    pub enabled: bool,

    /// Allowed path patterns (glob syntax). Empty = deny all.
    #[serde(default)]
    pub path_allowlist: Vec<String>,

    /// Denied path patterns (checked before allowlist).
    #[serde(default)]
    pub path_denylist: Vec<String>,

    /// Maximum bytes for a single read operation.
    #[serde(default = "default_max_read_bytes")]
    pub max_read_bytes: u64,

    /// Maximum bytes for a single write operation.
    #[serde(default = "default_max_write_bytes")]
    pub max_write_bytes: u64,

    /// Paths that require STRICT approval regardless of session mode.
    #[serde(default)]
    pub dangerous_paths: Vec<String>,
}

fn default_max_read_bytes() -> u64 { 104_857_600 } // 100MB
fn default_max_write_bytes() -> u64 { 104_857_600 }

impl Default for FilePolicyConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            path_allowlist: vec![],
            path_denylist: vec![],
            max_read_bytes: default_max_read_bytes(),
            max_write_bytes: default_max_write_bytes(),
            dangerous_paths: vec![],
        }
    }
}
```

Add to `Config` struct: `pub file_policy: Option<FilePolicyConfig>,`

- [ ] **Step 4: Implement FilePolicyChecker**

Create `crates/ahandd/src/file_manager/policy.rs` with:
- `FilePolicyChecker::new(config)` constructor
- `check_path(path, is_write) -> Result<PolicyResult, FileError>` method
- Path canonicalization (resolve symlinks, normalize)
- Traversal detection (reject paths containing `..` after normalization)
- Glob-based allowlist/denylist matching
- Dangerous path detection (returns `needs_approval: true`)
- Symlink target validation (resolved target must also pass checks)

- [ ] **Step 5: Create FileManager module directory**

Create `crates/ahandd/src/file_manager/mod.rs`:
```rust
pub mod policy;

use std::sync::Arc;
use ahand_protocol::*;
use crate::config::FilePolicyConfig;
use policy::FilePolicyChecker;

pub struct FileManager {
    policy: FilePolicyChecker,
    config: FilePolicyConfig,
}

impl FileManager {
    pub fn new(config: &FilePolicyConfig) -> Self {
        Self {
            policy: FilePolicyChecker::new(config),
            config: config.clone(),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    pub async fn handle(&self, req: &FileRequest) -> FileResponse {
        // Dispatch based on operation oneof
        // Each arm calls into submodule (text_read, binary_read, etc.)
        todo!("implement in subsequent tasks")
    }
}
```

- [ ] **Step 6: Wire up routing in ahand_client.rs**

Add to the payload match in `ahand_client.rs`:
```rust
Some(envelope::Payload::FileRequest(req)) => {
    handle_file_request(device_id, caller_uid, &req, &tx, session_mgr, file_mgr)
        .await;
}
```

Implement `handle_file_request()` following the `handle_browser_request` pattern:
1. Check `file_mgr.is_enabled()`
2. Check session mode via `session_mgr`
3. Call `file_mgr.handle(&req)`
4. Send response via `tx.send(Envelope { payload: FileResponse })`

- [ ] **Step 7: Initialize FileManager in main.rs**

In daemon startup, after config parsing:
```rust
let file_mgr = Arc::new(FileManager::new(
    config.file_policy.as_ref().unwrap_or(&FilePolicyConfig::default())
));
```

Pass `file_mgr` to the WebSocket client connection handler.

- [ ] **Step 8: Verify build and tests**

Run: `cargo build -p ahandd && cargo test -p ahandd -- file`
Expected: Builds and policy tests pass.

- [ ] **Step 9: Commit**

```bash
git add crates/ahandd/src/file_manager.rs crates/ahandd/src/file_manager/ crates/ahandd/tests/file_policy_tests.rs
git add crates/ahandd/src/config.rs crates/ahandd/src/main.rs crates/ahandd/src/ahand_client.rs crates/ahandd/Cargo.toml
git commit -m "feat(daemon): add FileManager skeleton with policy and routing"
```

---

### Task 2: FileStat, FileList, FileGlob, FileMkdir

**Goal:** Implement the simplest file operations first — metadata queries and directory creation. These establish the pattern for all other operations.

**Files:**
- Create: `crates/ahandd/src/file_manager/fs_ops.rs`
- Modify: `crates/ahandd/src/file_manager.rs` (dispatch stat/list/glob/mkdir)
- Test: `crates/ahandd/tests/file_ops.rs`

**Acceptance Criteria:**
- [ ] `FileStat` returns correct type, size, mtime, permissions, symlink target
- [ ] `FileList` paginates with offset/max_results, respects include_hidden
- [ ] `FileGlob` matches patterns and returns sorted by mtime
- [ ] `FileMkdir` creates directories (recursive and non-recursive)
- [ ] All operations respect file policy (path checks)
- [ ] Error cases: not found, permission denied, not a directory

**Verify:** `cargo test -p ahandd -- fs_ops` → all pass

**Steps:**

- [ ] **Step 1: Write integration tests**

Create `crates/ahandd/tests/file_ops.rs` with tests using a temp directory:
```rust
use tempfile::TempDir;

#[tokio::test]
async fn test_stat_file() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("test.txt");
    std::fs::write(&file, "hello").unwrap();
    
    let mgr = create_test_file_manager(dir.path());
    let req = file_stat_request(file.to_str().unwrap());
    let resp = mgr.handle(&req).await;
    
    let stat = extract_stat_result(&resp);
    assert_eq!(stat.file_type, FileType::File as i32);
    assert_eq!(stat.size, 5);
}

#[tokio::test]
async fn test_stat_directory() { /* ... */ }

#[tokio::test]
async fn test_stat_symlink_follow() { /* ... */ }

#[tokio::test]
async fn test_stat_symlink_no_follow() { /* ... */ }

#[tokio::test]
async fn test_stat_not_found() { /* ... */ }

#[tokio::test]
async fn test_list_directory() { /* ... */ }

#[tokio::test]
async fn test_list_pagination() { /* ... */ }

#[tokio::test]
async fn test_list_hidden_files() { /* ... */ }

#[tokio::test]
async fn test_glob_pattern() { /* ... */ }

#[tokio::test]
async fn test_glob_recursive() { /* ... */ }

#[tokio::test]
async fn test_mkdir_basic() { /* ... */ }

#[tokio::test]
async fn test_mkdir_recursive() { /* ... */ }

#[tokio::test]
async fn test_mkdir_already_exists() { /* ... */ }
```

- [ ] **Step 2: Implement fs_ops.rs**

Create `crates/ahandd/src/file_manager/fs_ops.rs`:
- `pub async fn handle_stat(req: &FileStat) -> Result<FileStatResult, FileError>`
  - Use `tokio::fs::symlink_metadata` (no_follow) or `tokio::fs::metadata` (follow)
  - Map `std::fs::FileType` to proto `FileType`
  - Read Unix permissions via `std::os::unix::fs::PermissionsExt`
  - Read symlink target via `tokio::fs::read_link`
- `pub async fn handle_list(req: &FileList) -> Result<FileListResult, FileError>`
  - Use `tokio::fs::read_dir`
  - Filter hidden files (name starts with `.`) unless `include_hidden`
  - Sort by mtime descending
  - Apply offset + max_results pagination
  - Return total_count and has_more
- `pub async fn handle_glob(req: &FileGlob) -> Result<FileGlobResult, FileError>`
  - Use `glob::glob()` with base_path prepended
  - Collect matches up to max_results
  - Stat each match to build FileEntry
  - Sort by mtime descending
- `pub async fn handle_mkdir(req: &FileMkdir) -> Result<FileMkdirResult, FileError>`
  - Check if already exists → return `already_existed: true`
  - Use `tokio::fs::create_dir` or `create_dir_all` based on recursive
  - Set Unix mode if specified via `std::fs::set_permissions`

- [ ] **Step 3: Wire up dispatch in FileManager**

In `file_manager.rs`, update the `handle()` method to match the operation oneof:
```rust
pub async fn handle(&self, req: &FileRequest) -> FileResponse {
    let request_id = req.request_id.clone();
    match &req.operation {
        Some(Operation::Stat(stat_req)) => {
            self.policy.check_path(&stat_req.path, false)?;
            // ... call fs_ops::handle_stat
        }
        Some(Operation::List(list_req)) => { /* ... */ }
        Some(Operation::Glob(glob_req)) => { /* ... */ }
        Some(Operation::Mkdir(mkdir_req)) => { /* ... */ }
        _ => todo!("other operations in subsequent tasks"),
    }
}
```

- [ ] **Step 4: Run tests and fix**

Run: `cargo test -p ahandd -- fs_ops`
Expected: All tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/ahandd/src/file_manager/fs_ops.rs crates/ahandd/src/file_manager.rs crates/ahandd/tests/file_ops.rs
git commit -m "feat(daemon): implement FileStat, FileList, FileGlob, FileMkdir"
```

---

### Task 3: FileReadText — Triple-Limit Text Pagination

**Goal:** Implement the most complex read operation: text file reading with triple-limit pagination (max_lines, max_bytes, target_end), per-line truncation, and precise position reporting.

**Files:**
- Create: `crates/ahandd/src/file_manager/text_read.rs`
- Modify: `crates/ahandd/src/file_manager.rs` (dispatch read_text)
- Test: `crates/ahandd/tests/file_ops.rs` (add text read tests)

**Acceptance Criteria:**
- [ ] Start position by line number, byte offset, or line+col works
- [ ] Stops at max_lines limit and reports `STOP_REASON_MAX_LINES`
- [ ] Stops at max_bytes limit and reports `STOP_REASON_MAX_BYTES`
- [ ] Stops at target_end and reports `STOP_REASON_TARGET_END`
- [ ] Reports `STOP_REASON_FILE_END` when EOF reached
- [ ] Per-line truncation with `max_line_width` marks truncated lines
- [ ] `remaining_bytes` on truncated lines reports exact remaining
- [ ] `PositionInfo` accurate for start and end (line, byte_in_file, byte_in_line)
- [ ] `remaining_bytes` after stop point is correct
- [ ] Encoding auto-detection works (UTF-8, GBK, Shift-JIS)
- [ ] Forced encoding via parameter works

**Verify:** `cargo test -p ahandd -- text_read` → all pass

**Steps:**

- [ ] **Step 1: Write text read tests**

Add to test file — test with known file contents:
```rust
#[tokio::test]
async fn test_read_text_basic() {
    // File: "line1\nline2\nline3\n"
    // Read all → 3 lines, FILE_END
}

#[tokio::test]
async fn test_read_text_max_lines() {
    // File: 100 lines
    // max_lines=5 → 5 lines, MAX_LINES
}

#[tokio::test]
async fn test_read_text_max_bytes() {
    // File: 100 lines of 50 bytes each
    // max_bytes=120 → stops mid-file, MAX_BYTES
}

#[tokio::test]
async fn test_read_text_target_end() {
    // File: 10 lines
    // target_end = line 5 → 5 lines, TARGET_END
}

#[tokio::test]
async fn test_read_text_start_line() {
    // Start at line 3 → first result line is line 3
}

#[tokio::test]
async fn test_read_text_start_byte() {
    // Start at byte offset → correct position
}

#[tokio::test]
async fn test_read_text_start_line_col() {
    // Start at line 2, col 5 → partial first line
}

#[tokio::test]
async fn test_read_text_line_truncation() {
    // Line with 1000 chars, max_line_width=100
    // → truncated=true, remaining_bytes=900
}

#[tokio::test]
async fn test_read_text_position_info() {
    // Verify start_pos and end_pos are byte-accurate
}

#[tokio::test]
async fn test_read_text_remaining_bytes() {
    // After reading 3 of 10 lines, remaining_bytes = bytes of lines 4-10
}

#[tokio::test]
async fn test_read_text_empty_file() {
    // Empty file → 0 lines, FILE_END
}

#[tokio::test]
async fn test_read_text_triple_limit_bytes_first() {
    // Set all three limits: bytes triggers first → MAX_BYTES
}
```

- [ ] **Step 2: Implement text_read.rs core**

Create `crates/ahandd/src/file_manager/text_read.rs`:

Core algorithm:
1. Open file, seek to start position (resolve line/byte/line_col to byte offset)
2. Read line by line using `BufReader`
3. For each line:
   - Check max_lines counter
   - Check accumulated bytes counter (against max_bytes)
   - Check if position exceeds target_end
   - If any limit reached: stop, record stop reason
   - Apply max_line_width truncation if needed
   - Build `TextLine` with line_number, content, truncated, remaining_bytes
4. After loop: calculate remaining_bytes (file_size - current_position)
5. Build `PositionInfo` for start and end

Key implementation details:
- Use `tokio::io::BufReader` with `AsyncBufReadExt::read_line()`
- Track byte position manually (sum of bytes read including newlines)
- For start_line: scan forward counting newlines until target line
- For start_byte: seek directly to byte offset
- For start_line_col: scan to line, then offset within line
- Encoding detection: use `encoding_rs` crate (add to Cargo.toml)
- If encoding specified: use it to decode; otherwise auto-detect from BOM or content

- [ ] **Step 3: Add encoding_rs dependency**

In `crates/ahandd/Cargo.toml`:
```toml
encoding_rs = "0.8"
chardetng = "0.1"
```

- [ ] **Step 4: Wire up dispatch and run tests**

Add `read_text` arm to FileManager dispatch. Run tests.

Run: `cargo test -p ahandd -- text_read`
Expected: All pass.

- [ ] **Step 5: Commit**

```bash
git add crates/ahandd/src/file_manager/text_read.rs crates/ahandd/tests/file_ops.rs crates/ahandd/Cargo.toml
git commit -m "feat(daemon): implement FileReadText with triple-limit pagination"
```

---

### Task 4: FileReadBinary & FileReadImage

**Goal:** Implement binary byte-range reading and image reading with on-device compression/resize.

**Files:**
- Create: `crates/ahandd/src/file_manager/binary_read.rs`
- Modify: `crates/ahandd/src/file_manager.rs` (dispatch)
- Test: `crates/ahandd/tests/file_ops.rs` (add binary/image tests)

**Acceptance Criteria:**
- [ ] Binary read returns exact byte range requested
- [ ] Binary read respects max_bytes limit
- [ ] Binary read reports remaining_bytes correctly
- [ ] Image read returns original image when no compression params
- [ ] Image resize (max_width, max_height) preserves aspect ratio
- [ ] Image quality parameter works for JPEG/WebP
- [ ] Image format conversion (PNG→WebP, etc.) works
- [ ] Image max_bytes iteratively reduces quality to fit
- [ ] Non-image files return appropriate error

**Verify:** `cargo test -p ahandd -- binary_read` → all pass

**Steps:**

- [ ] **Step 1: Write tests**

```rust
#[tokio::test]
async fn test_read_binary_full() {
    // 100 byte file, read all → 100 bytes, FILE_END
}

#[tokio::test]
async fn test_read_binary_range() {
    // 100 byte file, offset=20, length=30 → bytes 20..50
}

#[tokio::test]
async fn test_read_binary_max_bytes() {
    // 10MB file, max_bytes=1024 → first 1024 bytes, remaining = file_size - 1024
}

#[tokio::test]
async fn test_read_binary_past_eof() {
    // offset beyond file size → empty, remaining=0
}

#[tokio::test]
async fn test_read_image_passthrough() {
    // Read PNG with no compression → original bytes
}

#[tokio::test]
async fn test_read_image_resize() {
    // 1000x800 PNG, max_width=500 → 500x400 output
}

#[tokio::test]
async fn test_read_image_quality() {
    // JPEG with quality=50 → smaller than quality=100
}

#[tokio::test]
async fn test_read_image_format_convert() {
    // PNG → WebP conversion
}

#[tokio::test]
async fn test_read_image_max_bytes() {
    // Large image, max_bytes=50000 → iteratively compress until fits
}

#[tokio::test]
async fn test_read_image_not_image() {
    // .txt file → error
}
```

- [ ] **Step 2: Implement binary_read.rs**

Create `crates/ahandd/src/file_manager/binary_read.rs`:

```rust
pub async fn handle_read_binary(req: &FileReadBinary) -> Result<FileReadBinaryResult, FileError> {
    let metadata = tokio::fs::metadata(&req.path).await?;
    let file_size = metadata.len();
    let max = req.max_bytes.unwrap_or(1_048_576); // 1MB default
    
    let offset = req.byte_offset;
    let length = if req.byte_length == 0 {
        (file_size - offset).min(max)
    } else {
        req.byte_length.min(max)
    };
    
    let mut file = tokio::fs::File::open(&req.path).await?;
    file.seek(SeekFrom::Start(offset)).await?;
    let mut buf = vec![0u8; length as usize];
    let bytes_read = file.read_exact(&mut buf).await?;
    
    Ok(FileReadBinaryResult {
        content: buf,
        byte_offset: offset,
        bytes_read: bytes_read as u64,
        total_file_bytes: file_size,
        remaining_bytes: file_size.saturating_sub(offset + bytes_read as u64),
        download_url: None,
        download_url_expires_ms: None,
    })
}

pub async fn handle_read_image(req: &FileReadImage) -> Result<FileReadImageResult, FileError> {
    let raw = tokio::fs::read(&req.path).await?;
    let original_bytes = raw.len() as u64;
    
    // Detect format from content
    let format = image::guess_format(&raw)?;
    
    // Decode image
    let img = image::load_from_memory(&raw)?;
    let (orig_w, orig_h) = img.dimensions();
    
    // Resize if needed
    let img = apply_resize(img, req.max_width, req.max_height);
    let (out_w, out_h) = img.dimensions();
    
    // Encode to target format with quality
    let output_format = resolve_format(req.output_format, format);
    let mut output = encode_image(&img, output_format, req.quality)?;
    
    // If max_bytes specified, iteratively reduce quality
    if let Some(max_bytes) = req.max_bytes {
        output = compress_to_fit(&img, output_format, max_bytes, output)?;
    }
    
    Ok(FileReadImageResult {
        content: output.clone(),
        format: output_format as i32,
        width: out_w,
        height: out_h,
        original_bytes,
        output_bytes: output.len() as u64,
        download_url: None,
        download_url_expires_ms: None,
    })
}
```

Helper functions: `apply_resize`, `resolve_format`, `encode_image`, `compress_to_fit`.

- [ ] **Step 3: Run tests**

Run: `cargo test -p ahandd -- binary_read`
Expected: All pass.

- [ ] **Step 4: Commit**

```bash
git add crates/ahandd/src/file_manager/binary_read.rs crates/ahandd/tests/file_ops.rs
git commit -m "feat(daemon): implement FileReadBinary and FileReadImage"
```

---

### Task 5: FileWrite & FileEdit

**Goal:** Implement all write and edit operations: full write, append, string replace, line range replace, byte range replace.

**Files:**
- Create: `crates/ahandd/src/file_manager/write_ops.rs`
- Modify: `crates/ahandd/src/file_manager.rs` (dispatch write/edit)
- Test: `crates/ahandd/tests/file_ops.rs` (add write/edit tests)

**Acceptance Criteria:**
- [ ] FullWrite creates new file with content
- [ ] FullWrite with `create_parents` creates intermediate directories
- [ ] FullWrite overwrites existing file
- [ ] FileAppend appends to existing file
- [ ] StringReplace replaces first occurrence (replace_all=false)
- [ ] StringReplace replaces all occurrences (replace_all=true)
- [ ] StringReplace returns error when old_string not found
- [ ] StringReplace returns match_error when multiple matches but replace_all=false
- [ ] LineRangeReplace replaces specified line range
- [ ] ByteRangeReplace replaces specified byte range
- [ ] FileEdit rejects non-existent files (unlike FileWrite which creates)
- [ ] Encoding parameter respected for write operations
- [ ] Policy enforces max_write_bytes

**Verify:** `cargo test -p ahandd -- write_ops` → all pass

**Steps:**

- [ ] **Step 1: Write tests**

```rust
#[tokio::test]
async fn test_full_write_create() {
    // Write to non-existent path → creates file, WRITE_ACTION_CREATED
}

#[tokio::test]
async fn test_full_write_create_parents() {
    // Write to nested non-existent path with create_parents=true
}

#[tokio::test]
async fn test_full_write_overwrite() {
    // Write to existing file → WRITE_ACTION_OVERWRITTEN
}

#[tokio::test]
async fn test_append() {
    // Existing file "hello", append " world" → "hello world"
}

#[tokio::test]
async fn test_string_replace_single() {
    // "foo bar foo", replace "foo"->"baz", replace_all=false → "baz bar foo"
}

#[tokio::test]
async fn test_string_replace_all() {
    // "foo bar foo", replace "foo"->"baz", replace_all=true → "baz bar baz"
}

#[tokio::test]
async fn test_string_replace_not_found() {
    // "hello" replace "xyz" → FILE_ERROR_NOT_FOUND-like match_error
}

#[tokio::test]
async fn test_string_replace_multiple_matches_error() {
    // "foo foo foo", replace "foo" with replace_all=false
    // → match_error "multiple matches found (3)"
}

#[tokio::test]
async fn test_line_range_replace() {
    // 5-line file, replace lines 2-3 with "new content"
}

#[tokio::test]
async fn test_byte_range_replace() {
    // Replace bytes 5..10 with "XYZ" (different length)
}

#[tokio::test]
async fn test_edit_nonexistent_file() {
    // FileEdit on missing file → FILE_ERROR_NOT_FOUND
}

#[tokio::test]
async fn test_write_exceeds_max_bytes() {
    // Content larger than max_write_bytes → FILE_ERROR_TOO_LARGE
}
```

- [ ] **Step 2: Implement write_ops.rs**

Create `crates/ahandd/src/file_manager/write_ops.rs`:

```rust
pub async fn handle_write(req: &FileWrite, max_write_bytes: u64) -> Result<FileWriteResult, FileError> {
    match &req.method {
        Some(Method::FullWrite(fw)) => handle_full_write(&req.path, fw, req.create_parents, max_write_bytes).await,
        Some(Method::Append(app)) => handle_append(&req.path, app).await,
        Some(Method::StringReplace(sr)) => handle_string_replace_write(&req.path, sr).await,
        Some(Method::LineRangeReplace(lr)) => handle_line_range_replace(&req.path, lr).await,
        Some(Method::ByteRangeReplace(br)) => handle_byte_range_replace(&req.path, br).await,
        None => Err(file_error(FileErrorCode::Unspecified, "no write method specified")),
    }
}

pub async fn handle_edit(req: &FileEdit) -> Result<FileEditResult, FileError> {
    // Verify file exists first
    if !tokio::fs::try_exists(&req.path).await.unwrap_or(false) {
        return Err(file_error(FileErrorCode::NotFound, &req.path));
    }
    match &req.method {
        Some(Method::StringReplace(sr)) => handle_string_replace_edit(&req.path, sr).await,
        Some(Method::LineRangeReplace(lr)) => handle_line_range_replace_edit(&req.path, lr).await,
        Some(Method::ByteRangeReplace(br)) => handle_byte_range_replace_edit(&req.path, br).await,
        None => Err(file_error(FileErrorCode::Unspecified, "no edit method specified")),
    }
}
```

Key implementation for StringReplace:
1. Read entire file content as string
2. Count occurrences of `old_string`
3. If 0: return match_error "old_string not found"
4. If >1 and !replace_all: return match_error "multiple matches found (N)"
5. If 1 or replace_all: perform replacement, write back
6. Return replacements_made count

- [ ] **Step 3: Run tests**

Run: `cargo test -p ahandd -- write_ops`
Expected: All pass.

- [ ] **Step 4: Commit**

```bash
git add crates/ahandd/src/file_manager/write_ops.rs crates/ahandd/tests/file_ops.rs
git commit -m "feat(daemon): implement FileWrite and FileEdit operations"
```

---

### Task 6: FileDelete, FileCopy, FileMove, FileCreateSymlink, FileChmod

**Goal:** Implement all remaining mutation operations with proper safety (trash vs permanent, recursive guards, platform-aware permissions).

**Files:**
- Modify: `crates/ahandd/src/file_manager/fs_ops.rs` (add delete/copy/move/symlink/chmod)
- Modify: `crates/ahandd/src/file_manager.rs` (dispatch new operations)
- Test: `crates/ahandd/tests/file_ops.rs` (add mutation tests)

**Acceptance Criteria:**
- [ ] Delete TRASH mode moves to system trash, returns trash_path
- [ ] Delete PERMANENT mode removes file from filesystem
- [ ] Delete recursive removes non-empty directory
- [ ] Delete non-recursive on non-empty directory returns NOT_EMPTY error
- [ ] Copy file works (with overwrite control)
- [ ] Copy recursive copies directory tree
- [ ] Move file works (rename within same fs)
- [ ] Move across filesystems works (copy+delete fallback)
- [ ] CreateSymlink creates valid symlink
- [ ] Chmod sets Unix mode correctly
- [ ] Chmod recursive applies to all children
- [ ] Chmod chown returns PERMISSION_DENIED without root
- [ ] Windows ACL path compiles (even if not tested on macOS)

**Verify:** `cargo test -p ahandd -- fs_mutation` → all pass

**Steps:**

- [ ] **Step 1: Write tests**

```rust
#[tokio::test]
async fn test_delete_trash() {
    // Create file, delete with TRASH → file gone, trash_path returned
}

#[tokio::test]
async fn test_delete_permanent() {
    // Create file, delete PERMANENT → file gone
}

#[tokio::test]
async fn test_delete_recursive_directory() {
    // Dir with files, recursive=true → all deleted, items_deleted correct
}

#[tokio::test]
async fn test_delete_non_recursive_non_empty() {
    // Dir with files, recursive=false → NOT_EMPTY error
}

#[tokio::test]
async fn test_copy_file() {
    // Copy file → destination exists with same content
}

#[tokio::test]
async fn test_copy_no_overwrite() {
    // Copy to existing path, overwrite=false → ALREADY_EXISTS error
}

#[tokio::test]
async fn test_copy_recursive_directory() {
    // Copy dir tree → all files copied
}

#[tokio::test]
async fn test_move_file() {
    // Move file → source gone, destination exists
}

#[tokio::test]
async fn test_move_overwrite() {
    // Move to existing, overwrite=true → replaces
}

#[tokio::test]
async fn test_create_symlink() {
    // Create symlink → symlink points to target
}

#[tokio::test]
async fn test_chmod_mode() {
    // Set mode 0o644 → verify with fs::metadata
}

#[tokio::test]
async fn test_chmod_recursive() {
    // Recursive chmod on directory → all children changed
}
```

- [ ] **Step 2: Implement delete operations**

In `fs_ops.rs`:
```rust
pub async fn handle_delete(req: &FileDelete) -> Result<FileDeleteResult, FileError> {
    let path = Path::new(&req.path);
    let metadata = if req.no_follow_symlink {
        tokio::fs::symlink_metadata(path).await
    } else {
        tokio::fs::metadata(path).await
    }.map_err(|_| file_error(FileErrorCode::NotFound, &req.path))?;
    
    match DeleteMode::try_from(req.mode).unwrap_or(DeleteMode::Trash) {
        DeleteMode::Trash => {
            let trash_path = trash::delete(path)?;
            Ok(FileDeleteResult { mode: DeleteMode::Trash as i32, items_deleted: 1, trash_path: Some(trash_path) })
        }
        DeleteMode::Permanent => {
            if metadata.is_dir() {
                if !req.recursive {
                    // Check if empty
                    let mut entries = tokio::fs::read_dir(path).await?;
                    if entries.next_entry().await?.is_some() {
                        return Err(file_error(FileErrorCode::NotEmpty, &req.path));
                    }
                    tokio::fs::remove_dir(path).await?;
                    Ok(FileDeleteResult { items_deleted: 1, .. })
                } else {
                    let count = count_recursive(path).await;
                    tokio::fs::remove_dir_all(path).await?;
                    Ok(FileDeleteResult { items_deleted: count, .. })
                }
            } else {
                tokio::fs::remove_file(path).await?;
                Ok(FileDeleteResult { items_deleted: 1, .. })
            }
        }
    }
}
```

- [ ] **Step 3: Implement copy, move, symlink**

```rust
pub async fn handle_copy(req: &FileCopy) -> Result<FileCopyResult, FileError> {
    // Check overwrite
    // If recursive: walk tree, copy each file
    // Else: tokio::fs::copy
}

pub async fn handle_move(req: &FileMove) -> Result<FileMoveResult, FileError> {
    // Try rename first (fast, same filesystem)
    // Fallback: copy + delete (cross-filesystem)
}

pub async fn handle_create_symlink(req: &FileCreateSymlink) -> Result<FileCreateSymlinkResult, FileError> {
    tokio::fs::symlink(&req.target, &req.link_path).await?;
    Ok(FileCreateSymlinkResult { link_path: req.link_path.clone(), target: req.target.clone() })
}
```

- [ ] **Step 4: Implement chmod**

```rust
pub async fn handle_chmod(req: &FileChmod) -> Result<FileChmodResult, FileError> {
    match &req.permission {
        Some(Permission::Unix(unix)) => {
            if let Some(mode) = unix.mode {
                set_unix_mode(&req.path, mode, req.recursive).await?;
            }
            if unix.owner.is_some() || unix.group.is_some() {
                // chown requires root — check and return error if not root
                if !is_root() {
                    return Err(file_error(FileErrorCode::PermissionDenied, "chown requires root"));
                }
                // Use nix::unistd::chown
            }
        }
        Some(Permission::Windows(_acl)) => {
            // Windows-only implementation behind cfg(windows)
            #[cfg(not(windows))]
            return Err(file_error(FileErrorCode::Unspecified, "Windows ACL not supported on this platform"));
        }
        None => return Err(file_error(FileErrorCode::Unspecified, "no permission specified")),
    }
    Ok(FileChmodResult { path: req.path.clone(), items_modified: 1 })
}
```

- [ ] **Step 5: Run tests**

Run: `cargo test -p ahandd -- fs_mutation`
Expected: All pass.

- [ ] **Step 6: Commit**

```bash
git add crates/ahandd/src/file_manager/fs_ops.rs crates/ahandd/tests/file_ops.rs
git commit -m "feat(daemon): implement Delete, Copy, Move, Symlink, Chmod"
```

---

### Task 7: Hub HTTP Endpoints & Message Forwarding

**Goal:** Add REST API endpoints on the hub for file operations, and handle FileResponse messages from devices in the gateway.

**Files:**
- Create: `crates/ahand-hub/src/http/files.rs`
- Modify: `crates/ahand-hub/src/http/mod.rs` (add routes)
- Modify: `crates/ahand-hub/src/ws/device_gateway.rs` (handle FileResponse)
- Modify: `crates/ahand-hub/src/state.rs` (add pending file request tracking)

**Acceptance Criteria:**
- [ ] `POST /api/devices/{device_id}/files` accepts FileRequest JSON, forwards to device
- [ ] Response is returned when device replies with FileResponse (request_id correlation)
- [ ] Timeout handling: returns 504 if device doesn't respond within timeout
- [ ] Auth: requires dashboard access token
- [ ] FileResponse from device correctly routed back to waiting HTTP handler
- [ ] Hub correctly forwards FileRequest to device via ConnectionRegistry

**Verify:** `cargo build -p ahand-hub` → compiles; manual test with curl

**Steps:**

- [ ] **Step 1: Create files.rs with endpoint**

Create `crates/ahand-hub/src/http/files.rs`:
```rust
use axum::{
    extract::{Path, State},
    Json,
};
use tokio::sync::oneshot;
use std::time::Duration;
use crate::state::AppState;

/// POST /api/devices/{device_id}/files
/// Body: FileRequest as JSON
/// Returns: FileResponse as JSON
pub async fn file_operation(
    State(state): State<AppState>,
    Path(device_id): Path<String>,
    Json(req): Json<serde_json::Value>,  // Accept as raw JSON, encode to proto
) -> Result<Json<serde_json::Value>, ApiError> {
    // 1. Validate auth (require_dashboard_access)
    // 2. Parse request, assign request_id if missing
    // 3. Register pending request with oneshot channel
    // 4. Encode to proto Envelope with FileRequest payload
    // 5. Send to device via ConnectionRegistry
    // 6. Await response with timeout (30s default)
    // 7. Return FileResponse as JSON
}
```

- [ ] **Step 2: Add pending request tracking to state**

In `crates/ahand-hub/src/state.rs`, add:
```rust
use dashmap::DashMap;
use tokio::sync::oneshot;

pub struct PendingFileRequests {
    requests: DashMap<String, oneshot::Sender<FileResponse>>,
}

impl PendingFileRequests {
    pub fn register(&self, request_id: String) -> oneshot::Receiver<FileResponse> {
        let (tx, rx) = oneshot::channel();
        self.requests.insert(request_id, tx);
        rx
    }
    
    pub fn resolve(&self, request_id: &str, response: FileResponse) {
        if let Some((_, tx)) = self.requests.remove(request_id) {
            let _ = tx.send(response);
        }
    }
}
```

- [ ] **Step 3: Handle FileResponse in device_gateway**

In device frame handler, add match arm:
```rust
Some(envelope::Payload::FileResponse(resp)) => {
    state.pending_file_requests.resolve(&resp.request_id, resp);
}
```

- [ ] **Step 4: Add routes to mod.rs**

```rust
.route("/api/devices/{device_id}/files", post(files::file_operation))
```

- [ ] **Step 5: Verify build**

Run: `cargo build -p ahand-hub`
Expected: Compiles successfully.

- [ ] **Step 6: Commit**

```bash
git add crates/ahand-hub/src/http/files.rs crates/ahand-hub/src/http/mod.rs
git add crates/ahand-hub/src/ws/device_gateway.rs crates/ahand-hub/src/state.rs
git commit -m "feat(hub): add file operation HTTP endpoints and response routing"
```

---

### Task 8: S3 Large File Transfer

**Goal:** Implement the pre-signed URL flow for files exceeding the transfer threshold. The hub acts as the S3 intermediary — the daemon never holds S3 credentials.

**Architecture Decision:** For v1, the flow is hub-centric:
- **Reads:** Daemon sends content in FileResponse (capped at configurable max, e.g. 10MB). Hub receives it, uploads to S3 if large, then returns download URL to client. This keeps the daemon simple.
- **Writes:** Client uploads to S3 via pre-signed URL from hub, then sends FileRequest with `s3_object_key`. Hub downloads from S3, includes content in FileRequest to daemon.
- **WS frame size:** Daemon enforces its own `max_read_bytes` from config. Files beyond that are rejected with `FILE_ERROR_TOO_LARGE` until the S3 flow is exercised.

**Files:**
- Modify: `crates/ahand-hub/Cargo.toml` (add aws-sdk-s3)
- Modify: `crates/ahand-hub/src/config.rs` (add S3Config)
- Create: `crates/ahand-hub/src/s3.rs` (S3 client wrapper)
- Modify: `crates/ahand-hub/src/http/files.rs` (large file upload/download URL flow)

**Acceptance Criteria:**
- [ ] Hub config accepts S3 bucket, region, endpoint, threshold_bytes
- [ ] `POST /api/devices/{device_id}/files/upload-url` returns pre-signed upload URL + object_key
- [ ] Hub uploads daemon response content to S3 when it exceeds threshold
- [ ] Hub returns `download_url` in HTTP response for large file reads
- [ ] Hub downloads from S3 and forwards content to daemon for large writes
- [ ] Files below threshold transfer directly (no S3 involvement)
- [ ] S3 disabled gracefully when config not provided (files above threshold return TOO_LARGE)

**Verify:** `cargo build -p ahand-hub` → compiles

**Steps:**

- [ ] **Step 1: Add S3 config to hub**

In `crates/ahand-hub/src/config.rs`:
```rust
#[derive(Debug, Deserialize, Clone)]
pub struct S3Config {
    pub bucket: String,
    pub region: String,
    pub endpoint: Option<String>,          // For MinIO/local testing
    pub file_transfer_threshold_bytes: u64, // default 1MB
    pub url_expiration_secs: u64,          // default 3600
}

impl Default for S3Config {
    fn default() -> Self {
        Self {
            bucket: String::new(),
            region: "us-east-1".into(),
            endpoint: None,
            file_transfer_threshold_bytes: 1_048_576,
            url_expiration_secs: 3600,
        }
    }
}
```

Add `pub s3: Option<S3Config>` to hub's main Config struct.

- [ ] **Step 2: Add aws-sdk-s3 dependency**

In `crates/ahand-hub/Cargo.toml`:
```toml
aws-sdk-s3 = "1"
aws-config = "1"
```

- [ ] **Step 3: Create S3 client wrapper**

Create `crates/ahand-hub/src/s3.rs`:
```rust
use aws_sdk_s3::presigning::PresigningConfig;
use std::time::Duration;

pub struct S3Client {
    client: aws_sdk_s3::Client,
    bucket: String,
    expiration: Duration,
    threshold: u64,
}

impl S3Client {
    pub async fn new(config: &S3Config) -> Self {
        let aws_config = aws_config::from_env()
            .region(aws_sdk_s3::config::Region::new(config.region.clone()))
            .load()
            .await;
        let mut s3_config = aws_sdk_s3::config::Builder::from(&aws_config);
        if let Some(endpoint) = &config.endpoint {
            s3_config = s3_config.endpoint_url(endpoint).force_path_style(true);
        }
        Self {
            client: aws_sdk_s3::Client::from_conf(s3_config.build()),
            bucket: config.bucket.clone(),
            expiration: Duration::from_secs(config.url_expiration_secs),
            threshold: config.file_transfer_threshold_bytes,
        }
    }

    pub fn threshold(&self) -> u64 { self.threshold }

    pub async fn generate_upload_url(&self, key: &str) -> anyhow::Result<(String, u64)> {
        let presign = PresigningConfig::expires_in(self.expiration)?;
        let req = self.client.put_object()
            .bucket(&self.bucket)
            .key(key)
            .presigned(presign)
            .await?;
        let expires_ms = /* current time + expiration as ms */;
        Ok((req.uri().to_string(), expires_ms))
    }

    pub async fn generate_download_url(&self, key: &str) -> anyhow::Result<(String, u64)> {
        let presign = PresigningConfig::expires_in(self.expiration)?;
        let req = self.client.get_object()
            .bucket(&self.bucket)
            .key(key)
            .presigned(presign)
            .await?;
        let expires_ms = /* current time + expiration as ms */;
        Ok((req.uri().to_string(), expires_ms))
    }

    pub async fn upload_bytes(&self, key: &str, data: Vec<u8>) -> anyhow::Result<()> {
        self.client.put_object()
            .bucket(&self.bucket)
            .key(key)
            .body(data.into())
            .send()
            .await?;
        Ok(())
    }

    pub async fn download_bytes(&self, key: &str) -> anyhow::Result<Vec<u8>> {
        let resp = self.client.get_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await?;
        let bytes = resp.body.collect().await?.into_bytes().to_vec();
        Ok(bytes)
    }
}
```

- [ ] **Step 4: Integrate into file endpoint — read flow**

In `files.rs`, after receiving FileResponse from daemon with binary content:
```rust
// Hub receives FileReadBinaryResult with content
if let Some(s3) = &state.s3_client {
    if response_content.len() as u64 > s3.threshold() {
        // Upload daemon's response to S3
        let key = format!("file-ops/{}/{}", device_id, uuid::Uuid::new_v4());
        s3.upload_bytes(&key, response_content).await?;
        // Generate download URL for client
        let (url, expires) = s3.generate_download_url(&key).await?;
        // Return download_url instead of content
        result.content = vec![];
        result.download_url = Some(url);
        result.download_url_expires_ms = Some(expires);
    }
}
```

- [ ] **Step 5: Integrate into file endpoint — write flow**

Add endpoint `POST /api/devices/{device_id}/files/upload-url`:
```rust
pub async fn get_upload_url(
    State(state): State<AppState>,
    Path(device_id): Path<String>,
) -> Result<Json<FileTransferUrl>, ApiError> {
    let s3 = state.s3_client.as_ref().ok_or(ApiError::ServiceUnavailable)?;
    let key = format!("file-ops/{}/{}", device_id, uuid::Uuid::new_v4());
    let (url, expires) = s3.generate_upload_url(&key).await?;
    Ok(Json(FileTransferUrl { url, expires_ms: expires, object_key: key }))
}
```

When client sends FileRequest with `s3_object_key`:
```rust
// Hub downloads from S3, injects content into the request before sending to daemon
if let Some(key) = &full_write.s3_object_key {
    let content = s3.download_bytes(key).await?;
    // Rewrite the FileRequest to use direct content
    full_write.content = content;
    full_write.s3_object_key = None;
}
```

- [ ] **Step 6: Commit**

```bash
git add crates/ahand-hub/src/s3.rs crates/ahand-hub/src/config.rs crates/ahand-hub/Cargo.toml
git add crates/ahand-hub/src/http/files.rs
git commit -m "feat(hub): add S3 pre-signed URL flow for large file transfer"
```

---

### Task 9: End-to-End Integration Testing

**Goal:** Verify the full pipeline works: HTTP request → hub → daemon → filesystem → response. Test with real WebSocket connection in a test harness.

**Files:**
- Create: `crates/ahandd/tests/file_ops_e2e.rs`
- Modify: `crates/ahandd/tests/file_ops.rs` (finalize all unit tests)

**Acceptance Criteria:**
- [ ] Full roundtrip: FileRequest encoded → sent via WebSocket → daemon processes → FileResponse returned
- [ ] All 13 operations work end-to-end
- [ ] Policy rejection flows correctly (returns FileError with POLICY_DENIED)
- [ ] Session mode STRICT triggers approval flow for file ops
- [ ] Error cases return proper FileErrorCode values
- [ ] Large file S3 flow works (mocked S3 or MinIO in CI)

**Verify:** `cargo test -p ahandd -- file_ops` → all pass (unit + integration)

**Steps:**

- [ ] **Step 1: Create E2E test harness**

Set up a test that:
1. Creates a temporary directory with test files
2. Instantiates FileManager with permissive policy (allowlist = temp dir)
3. Sends FileRequest proto messages
4. Verifies FileResponse contents
5. Verifies filesystem state after mutations

- [ ] **Step 2: Test each operation E2E**

Write one test per operation type verifying the full encode → handle → decode cycle.

- [ ] **Step 3: Test policy rejection**

Configure restrictive policy, verify operations on disallowed paths return `FILE_ERROR_POLICY_DENIED`.

- [ ] **Step 4: Test session mode integration**

Verify that STRICT mode for file operations triggers the approval mechanism (synthetic approval in test).

- [ ] **Step 5: Run full test suite**

Run: `cargo test -p ahandd -- file_ops`
Expected: All pass.

- [ ] **Step 6: Commit**

```bash
git add crates/ahandd/tests/
git commit -m "test(daemon): add comprehensive file operations E2E tests"
```

---
