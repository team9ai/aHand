# Device File Operations Design

Date: 2026-04-12

## Overview

Design for local file/folder operations executed by the daemon (ahandd) on behalf of cloud agents and human operators via the hub dashboard. Operations include reading (text, binary, image), writing/editing, deleting, permission management, and directory traversal.

## Architecture Decision

**Dedicated proto messages** (not reusing JobRequest) because:
- Binary efficiency: no base64 overhead for `bytes` fields
- Type safety: structured parameters instead of `string[] args`
- Clean security model: dedicated file policy handler
- Follows existing precedent (BrowserRequest/BrowserResponse pattern)

## Protocol Structure

Two new payload fields in `Envelope.payload` oneof:

```protobuf
FileRequest  file_request  = 31;
FileResponse file_response = 32;
```

### FileRequest / FileResponse

```protobuf
message FileRequest {
  string request_id = 1;

  oneof operation {
    FileReadText read_text = 10;
    FileReadBinary read_binary = 11;
    FileReadImage read_image = 12;
    FileWrite write = 13;
    FileEdit edit = 14;
    FileDelete delete = 15;
    FileChmod chmod = 16;
    FileStat stat = 17;
    FileList list = 18;
    FileGlob glob = 19;
    FileMkdir mkdir = 20;
    FileCopy copy = 21;
    FileMove move = 22;
    FileCreateSymlink create_symlink = 23;
  }
}

message FileResponse {
  string request_id = 1;

  oneof result {
    FileError error = 2;
    FileReadTextResult read_text = 10;
    FileReadBinaryResult read_binary = 11;
    FileReadImageResult read_image = 12;
    FileWriteResult write = 13;
    FileEditResult edit = 14;
    FileDeleteResult delete = 15;
    FileChmodResult chmod = 16;
    FileStatResult stat = 17;
    FileListResult list = 18;
    FileGlobResult glob = 19;
    FileMkdirResult mkdir = 20;
    FileCopyResult copy = 21;
    FileMoveResult move_result = 22;  // renamed from spec's "move" — `move` is a Rust keyword
    FileCreateSymlinkResult create_symlink = 23;
  }
}
```

> **Note on Heartbeat field number.** The implementation also added a
> `Heartbeat heartbeat = 33;` arm to `Envelope.payload` (in a separate stream
> of work that landed alongside file ops). Heartbeat sits at 33 specifically
> so it doesn't clash with FileRequest=31 / FileResponse=32 — see
> `proto/ahand/v1/envelope.proto` for the canonical field numbers.

## Text File Reading (FileReadText)

### Request

```protobuf
message FileReadText {
  string path = 1;

  // Start position (pick one; none = file start)
  oneof start {
    uint64 start_line = 10;        // 1-based line number
    uint64 start_byte = 11;        // absolute byte offset
    LineCol start_line_col = 12;   // line + byte within line
  }

  // Length limits (triple constraint, first reached stops; all have defaults)
  optional uint32 max_lines = 20;      // default 200
  optional uint64 max_bytes = 21;      // default 64KB
  optional FilePosition target_end = 22; // optional target end position

  // Per-line truncation
  optional uint32 max_line_width = 30; // default 500 bytes, 0 = no truncation

  // Options
  optional string encoding = 40;   // empty = auto-detect
  bool line_numbers = 41;          // include line numbers in response
  bool no_follow_symlink = 42;
}

message LineCol {
  uint64 line = 1;   // 1-based
  uint64 col = 2;    // 0-based byte offset within line
}

message FilePosition {
  oneof position {
    uint64 line = 1;
    uint64 byte_offset = 2;
    LineCol line_col = 3;
  }
}
```

### Response

```protobuf
message FileReadTextResult {
  repeated TextLine lines = 1;
  StopReason stop_reason = 2;
  PositionInfo start_pos = 3;
  PositionInfo end_pos = 4;
  uint64 remaining_bytes = 5;     // bytes remaining after stop point
  uint64 total_file_bytes = 6;
  uint64 total_lines = 7;         // 0 = unknown (skipped for large files)
  string detected_encoding = 8;
}

message TextLine {
  string content = 1;
  uint64 line_number = 2;         // 1-based
  bool truncated = 3;             // truncated by max_line_width
  uint32 remaining_bytes = 4;     // bytes remaining in this line after truncation
}

message PositionInfo {
  uint64 line = 1;                // 1-based
  uint64 byte_in_file = 2;       // absolute byte position in file
  uint64 byte_in_line = 3;       // byte position within line
}

enum StopReason {
  STOP_REASON_UNSPECIFIED = 0;
  STOP_REASON_MAX_LINES = 1;
  STOP_REASON_MAX_BYTES = 2;
  STOP_REASON_TARGET_END = 3;
  STOP_REASON_FILE_END = 4;
  STOP_REASON_ERROR = 5;
}
```

### Design Notes

- `total_lines` may be 0 for large files (avoids scanning entire file just to count lines)
- `TextLine` per-line structure enables dashboard rendering + agent line-number references
- Line numbers are 1-based (natural for both humans and AI)
- Triple limit (max_lines, max_bytes, target_end): whichever is reached first stops reading

## Binary File Reading (FileReadBinary)

```protobuf
message FileReadBinary {
  string path = 1;
  uint64 byte_offset = 2;         // start position, default 0
  uint64 byte_length = 3;         // 0 = read to EOF (capped by max_bytes)
  optional uint64 max_bytes = 4;  // single transfer max, default 1MB
  bool no_follow_symlink = 5;
}

message FileReadBinaryResult {
  bytes content = 1;
  uint64 byte_offset = 2;        // actual start position
  uint64 bytes_read = 3;
  uint64 total_file_bytes = 4;
  uint64 remaining_bytes = 5;

  // Large file S3 fallback
  optional string download_url = 10;
  optional uint64 download_url_expires_ms = 11;
}
```

## Image Reading (FileReadImage)

```protobuf
message FileReadImage {
  string path = 1;

  // Compression parameters (all optional; omit = raw transfer)
  optional uint32 max_width = 10;
  optional uint32 max_height = 11;
  optional uint64 max_bytes = 12;       // max compressed size
  optional uint32 quality = 13;         // 1-100, JPEG/WebP quality
  optional ImageFormat output_format = 14;

  bool no_follow_symlink = 20;
}

enum ImageFormat {
  IMAGE_FORMAT_ORIGINAL = 0;
  IMAGE_FORMAT_JPEG = 1;
  IMAGE_FORMAT_PNG = 2;
  IMAGE_FORMAT_WEBP = 3;
}

message FileReadImageResult {
  bytes content = 1;
  ImageFormat format = 2;          // actual output format
  uint32 width = 3;
  uint32 height = 4;
  uint64 original_bytes = 5;
  uint64 output_bytes = 6;

  // Large file S3 fallback
  optional string download_url = 10;
  optional uint64 download_url_expires_ms = 11;
}
```

### Design Notes

- Binary and image are separate operations: image has compression/resize semantics
- Both support S3 fallback (large files return `download_url` instead of `content`)
- Image compression happens on daemon locally before transfer (saves bandwidth)
- `quality` only affects JPEG/WebP; ignored for PNG (lossless)

## Write & Edit Operations

> **v1 encoding limitation.** Writes and edits accept `encoding=""`
> (auto, treated as UTF-8) or `"utf-8"` / `"utf8"` (case-insensitive).
> Any other value is rejected with `FILE_ERROR_CODE_ENCODING`.
> Full encoding conversion on write — equivalent to the read-side
> chardetng/encoding_rs path — is intentionally future work; v1
> contracts assume callers serialize their content as UTF-8.

### FileWrite (create or overwrite files)

```protobuf
message FileWrite {
  string path = 1;
  bool create_parents = 2;        // auto-create intermediate directories

  oneof method {
    FullWrite full_write = 10;
    FileAppend append = 11;
    StringReplace string_replace = 12;
    LineRangeReplace line_range_replace = 13;
    ByteRangeReplace byte_range_replace = 14;
  }

  optional string encoding = 30;  // default UTF-8
  bool no_follow_symlink = 31;
}

message FullWrite {
  oneof source {
    bytes content = 1;            // small file: direct content
    string s3_object_key = 2;    // large file: confirm S3 upload complete
  }
  // Hub-injected on the way to the daemon when source is
  // s3_object_key. The daemon does a plain HTTP GET against this URL
  // and writes the response body to disk; no AWS SDK on the daemon.
  // Clients leave these fields empty; the hub fills them in.
  optional string s3_download_url = 10;
  optional uint64 s3_download_url_expires_ms = 11;
}

message FileAppend {
  bytes content = 1;
}

message StringReplace {
  string old_string = 1;
  string new_string = 2;
  bool replace_all = 3;          // default false
}

message LineRangeReplace {
  uint64 start_line = 1;         // 1-based, inclusive
  uint64 end_line = 2;           // 1-based, inclusive
  string new_content = 3;
}

message ByteRangeReplace {
  uint64 byte_offset = 1;
  uint64 byte_length = 2;        // bytes to delete
  bytes new_content = 3;          // bytes to insert (can differ in length)
}
```

### FileEdit (modify existing files only)

```protobuf
message FileEdit {
  string path = 1;

  oneof method {
    StringReplace string_replace = 10;
    LineRangeReplace line_range_replace = 11;
    ByteRangeReplace byte_range_replace = 12;
  }

  optional string encoding = 30;
  bool no_follow_symlink = 31;
}
```

### Responses

```protobuf
message FileWriteResult {
  string path = 1;
  WriteAction action = 2;
  uint64 bytes_written = 3;
  uint64 final_size = 4;
  optional uint32 replacements_made = 10;
}

message FileEditResult {
  string path = 1;
  uint64 final_size = 2;
  optional uint32 replacements_made = 10;
  optional string match_error = 20;  // "old_string not found" or "multiple matches found (3)"
}

enum WriteAction {
  WRITE_ACTION_CREATED = 0;
  WRITE_ACTION_OVERWRITTEN = 1;
  WRITE_ACTION_APPENDED = 2;
  WRITE_ACTION_EDITED = 3;
}
```

### Design Notes

- `FileWrite` vs `FileEdit` separation: Write can create files + `create_parents`; Edit only modifies existing
- `StringReplace` with multiple matches but `replace_all=false` returns `match_error` with count
- Large file upload: hub replies with `upload_url` first, daemon pulls from S3 after confirmation
- `ByteRangeReplace.new_content` length can differ from `byte_length` (supports insert/shrink)

## Delete, Permissions, Stat

### FileDelete

```protobuf
message FileDelete {
  string path = 1;
  bool recursive = 2;             // required true for non-empty directories
  DeleteMode mode = 3;
  bool no_follow_symlink = 4;     // true = delete symlink itself
}

enum DeleteMode {
  DELETE_MODE_TRASH = 0;          // default: move to system trash
  DELETE_MODE_PERMANENT = 1;
}

message FileDeleteResult {
  string path = 1;
  DeleteMode mode = 2;
  uint32 items_deleted = 3;       // count for recursive deletes
  optional string trash_path = 4; // location in trash (TRASH mode)
}
```

### FileChmod

```protobuf
message FileChmod {
  string path = 1;
  bool recursive = 2;
  bool no_follow_symlink = 3;

  oneof permission {
    UnixPermission unix = 10;
    WindowsAcl windows = 11;
  }
}

message UnixPermission {
  optional uint32 mode = 1;       // e.g. 0o755
  optional string owner = 2;
  optional string group = 3;
}

message WindowsAcl {
  repeated AclEntry entries = 1;
}

message AclEntry {
  string principal = 1;           // user/group name
  uint32 access_mask = 2;        // Windows ACCESS_MASK
  AclEntryType entry_type = 3;   // renamed from spec's "type" — Rust keyword
}

enum AclEntryType {
  // proto3 best practice prefixes the enum type name onto every value
  // so sibling enums in the same package can't collide.
  ACL_ENTRY_TYPE_ALLOW = 0;
  ACL_ENTRY_TYPE_DENY = 1;
}

message FileChmodResult {
  string path = 1;
  uint32 items_modified = 2;
}
```

### FileStat

```protobuf
message FileStat {
  string path = 1;
  bool no_follow_symlink = 2;
}

message FileStatResult {
  string path = 1;
  FileType file_type = 2;
  uint64 size = 3;
  uint64 modified_ms = 4;
  uint64 created_ms = 5;
  uint64 accessed_ms = 6;
  optional UnixPermission unix_permission = 10;
  optional WindowsAcl windows_acl = 11;
  optional string symlink_target = 20;
}

enum FileType {
  FILE_TYPE_FILE = 0;
  FILE_TYPE_DIRECTORY = 1;
  FILE_TYPE_SYMLINK = 2;
  FILE_TYPE_OTHER = 3;
}
```

## Directory Operations

### FileList

```protobuf
message FileList {
  string path = 1;
  optional uint32 max_results = 2;  // default 1000
  optional uint32 offset = 3;       // pagination offset
  bool include_hidden = 4;
}

message FileListResult {
  repeated FileEntry entries = 1;
  uint32 total_count = 2;
  bool has_more = 3;
}

message FileEntry {
  string name = 1;
  FileType file_type = 2;
  uint64 size = 3;
  uint64 modified_ms = 4;
  optional string symlink_target = 5;
}
```

### FileGlob

```protobuf
message FileGlob {
  string pattern = 1;               // e.g. "**/*.rs"
  optional string base_path = 2;    // search root, default cwd
  optional uint32 max_results = 3;  // default 1000
}

message FileGlobResult {
  repeated FileEntry entries = 1;
  uint32 total_matches = 2;
  bool has_more = 3;
}
```

### FileMkdir

```protobuf
message FileMkdir {
  string path = 1;
  bool recursive = 2;              // default true (mkdir -p)
  optional uint32 mode = 3;       // Unix permission, default 0o755
}

message FileMkdirResult {
  string path = 1;
  bool already_existed = 2;
}
```

### FileCopy / FileMove

```protobuf
message FileCopy {
  string source = 1;
  string destination = 2;
  bool recursive = 3;
  bool overwrite = 4;             // default false
}

message FileMove {
  string source = 1;
  string destination = 2;
  bool overwrite = 3;
}

message FileCopyResult {
  string source = 1;
  string destination = 2;
  uint32 items_copied = 3;
}

message FileMoveResult {
  string source = 1;
  string destination = 2;
}
```

### FileCreateSymlink

```protobuf
message FileCreateSymlink {
  string target = 1;              // what the symlink points to
  string link_path = 2;          // the symlink itself
}

message FileCreateSymlinkResult {
  string link_path = 1;
  string target = 2;
}
```

## Error Handling

```protobuf
message FileError {
  FileErrorCode code = 1;
  string message = 2;
  string path = 3;
}

enum FileErrorCode {
  // proto3 best practice prefixes the enum type name onto every value;
  // matching the implementation in proto/ahand/v1/file_ops.proto.
  FILE_ERROR_CODE_UNSPECIFIED = 0;
  FILE_ERROR_CODE_NOT_FOUND = 1;
  FILE_ERROR_CODE_PERMISSION_DENIED = 2;
  FILE_ERROR_CODE_ALREADY_EXISTS = 3;
  FILE_ERROR_CODE_NOT_A_DIRECTORY = 4;
  FILE_ERROR_CODE_IS_A_DIRECTORY = 5;
  FILE_ERROR_CODE_NOT_EMPTY = 6;         // non-recursive delete on non-empty dir
  FILE_ERROR_CODE_TOO_LARGE = 7;         // exceeds transfer limit
  FILE_ERROR_CODE_INVALID_PATH = 8;      // path traversal or security issue
  FILE_ERROR_CODE_IO = 9;
  FILE_ERROR_CODE_ENCODING = 10;         // encoding detection/conversion failed
  FILE_ERROR_CODE_MULTIPLE_MATCHES = 11; // StringReplace found multiple matches
  FILE_ERROR_CODE_POLICY_DENIED = 12;    // blocked by file policy
}
```

## Large File S3 Transfer

> **v2 status.** Wired end-to-end. The hub mediates S3 access on both
> sides so daemons stay S3-unaware (no AWS SDK, no credentials).
> Implementation lives in `crates/ahand-hub/src/http/files.rs`
> (`upload_url`, `maybe_swap_large_read_response`,
> `maybe_inject_full_write_download_url`) and the daemon's
> `handle_full_write` in `crates/ahandd/src/file_manager/write_ops.rs`
> which fetches via plain HTTP GET when the hub injects
> `s3_download_url`. The Round 1 design — daemon negotiates upload URL
> from the hub via a back-channel — was simplified out: the hub is
> already the only S3-aware participant, so it just performs the
> upload itself on the read path.

### Threshold

Hub-side configuration: `file_transfer_threshold_bytes` (default 1MB). Daemon does not need to know the threshold — the hub decides the transfer path.

### Read Flow (large file)

```
Agent/Dashboard ──FileRequest(read_binary)──→ Hub
Hub ──FileRequest──→ Daemon
Daemon: reads file, returns FileResponse with inline content
Hub: receives FileResponse
  if content.len() > threshold:
    1. Upload bytes to S3 (object key: file-ops/{device_id}/read-{uuid}.bin)
    2. Generate pre-signed GET URL
    3. Rewrite result: clear content, set download_url + download_url_expires_ms
  forward to caller
Agent/Dashboard:
  if response.download_url is set:
    fetch directly from S3
  else:
    use response.content inline
```

Read-side keys carry a `read-` segment so operators writing S3 lifecycle rules (e.g. shorter expiry on read-side spillover than on write-side staging objects) can target them without affecting the upload-url path.

Daemons return inline content uniformly; the swap is invisible to them. If `[s3]` is unconfigured the hub forwards the response unchanged — large responses still work but use one big WebSocket frame instead of an out-of-band download.

### Write Flow (large file)

```
Agent/Dashboard ──POST /files/upload-url──→ Hub
Hub:
  if [s3] is unconfigured:
    return 503 + S3_DISABLED
  generate pre-signed PUT URL (object key: file-ops/{device_id}/{uuid}.bin)
  return { object_key, upload_url, expires_at_ms }
Agent/Dashboard:
  PUT bytes directly to upload_url
  send FileRequest(write, FullWrite{ s3_object_key })

Hub: receives FileRequest with FullWrite{s3_object_key}
  if [s3] is unconfigured:
    return 503 + S3_DISABLED  (fail fast, daemon would reject anyway)
  generate pre-signed GET URL for s3_object_key
  inject FullWrite.s3_download_url + s3_download_url_expires_ms
  forward to daemon
Daemon:
  if FullWrite.s3_download_url is missing:
    return FILE_ERROR_CODE_UNSPECIFIED  (hub bug or version mismatch)
  reqwest::get(s3_download_url) → write bytes to local file
  enforce max_write_bytes against Content-Length AND actual length
  reply FileWriteResult { ... }
```

Daemons never hold S3 credentials. The presigned GET URL is a regular HTTP resource — `reqwest` handles it without any AWS-specific code.

### Protocol Support

```protobuf
message FullWrite {
  oneof source {
    bytes content = 1;
    string s3_object_key = 2;
  }
  // Hub-injected before forwarding; populated only when source is
  // s3_object_key. Clients leave these empty.
  optional string s3_download_url = 10;
  optional uint64 s3_download_url_expires_ms = 11;
}

// Read-side fallback fields on FileReadBinaryResult / FileReadImageResult:
//   optional string download_url = 10;
//   optional uint64 download_url_expires_ms = 11;
```

The legacy `FileTransferUrl` message is no longer used by the wire protocol — the JSON body of `POST /files/upload-url` carries the same data:

```json
{ "object_key": "file-ops/<device_id>/<uuid>.bin",
  "upload_url": "https://...",
  "expires_at_ms": 1714400000000 }
```

## Security Model

File operations integrate with aHand's existing security infrastructure:

### Layer 1: Session Mode

| Mode | Behavior |
|------|----------|
| INACTIVE | Reject all file operations |
| STRICT | Every operation requires user approval |
| TRUST | Auto-approve (with inactivity timeout) |
| AUTO_ACCEPT | All auto-approved |

### Layer 2: File Policy

Daemon configuration (`ahandd.toml`):

```toml
[file_policy]
enabled = true
path_allowlist = ["/home/user/**", "/tmp/**"]
path_denylist = ["/etc/**", "~/.ssh/**", "~/.gnupg/**", "/proc/**", "/sys/**"]
max_read_bytes = 104857600     # 100MB
max_write_bytes = 104857600
dangerous_paths = ["~/.bashrc", "~/.zshrc", "~/.gitconfig"]
```

### Layer 3: Path Security Checks

1. All paths resolved to absolute paths before policy check
2. Symlink targets must also be within allowlist (escape prevention)
3. Path traversal detection (`../` sequences)
4. Canonical path comparison after resolution

### Security Rules

1. `DELETE_MODE_PERMANENT` + recursive in TRUST mode escalates to STRICT (requires confirmation)
2. `FileChmod` changing owner requires root; daemon returns `PERMISSION_DENIED` without root
3. Approval requests include operation details (path, operation type, scope) for user judgment
4. `dangerous_paths` always require STRICT approval regardless of session mode
