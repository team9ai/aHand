//! JSON ⇄ protobuf conversions for the control-plane file endpoint.
//!
//! `POST /api/control/files` speaks JSON on the wire (matching the
//! browser endpoint's symmetry decision) but the daemon-facing
//! envelope is still protobuf — so this module is the bridge.
//!
//! The wire shape is intentionally a thin re-spelling of the proto
//! schema (snake_case field names, same nullability semantics) so the
//! SDK's typed surface and the daemon's typed surface stay in lock
//! step. All 14 file operations are covered.
//!
//! ## Wire format
//!
//! ### Request body
//! ```json
//! {
//!   "device_id": "dev-1",
//!   "operation": "stat" | "list" | ... | "create_symlink",
//!   "params": { ...op-specific fields... },
//!   "timeout_ms": 30000,
//!   "correlation_id": "c-1"
//! }
//! ```
//!
//! ### Response body
//! ```json
//! {
//!   "request_id": "uuid",
//!   "operation": "stat" | ...,
//!   "success": true,
//!   "result": { ...op-specific result... },     // only when success
//!   "error":  { "code": "not_found", "message": "...", "path": "..." },
//!   "duration_ms": 12
//! }
//! ```
//!
//! ## Error semantics
//!
//! Daemon-side failures (file not found, permission denied, policy
//! denied, ...) come back as `success: false` with an `error` field
//! and HTTP 200. Hub-level failures (auth, ownership, offline,
//! timeout, rate limit) return appropriate 4xx/5xx HTTP statuses with
//! the standard `{error: {code, message}}` envelope. This matches the
//! browser endpoint.

use ahand_protocol as proto;
use serde::{Deserialize, Serialize};

// ──────────────────────────────────────────────────────────────────────
// Top-level request / response
// ──────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ControlFilesRequest {
    pub device_id: String,
    pub operation: String,
    #[serde(default)]
    pub params: serde_json::Value,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub correlation_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ControlFilesResponse {
    pub request_id: String,
    pub operation: String,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<FileErrorJson>,
    pub duration_ms: u64,
}

#[derive(Debug, Serialize)]
pub struct FileErrorJson {
    pub code: String,
    pub message: String,
    pub path: String,
}

#[derive(Debug, thiserror::Error)]
pub enum DtoError {
    #[error("unknown operation '{0}'")]
    UnknownOperation(String),
    #[error("invalid params for operation '{op}': {message}")]
    InvalidParams { op: String, message: String },
}

// ──────────────────────────────────────────────────────────────────────
// JSON → proto: build a `FileRequest::operation` from `(operation, params)`
// ──────────────────────────────────────────────────────────────────────

/// Map a wire `(operation, params)` pair into the proto `FileRequest`'s
/// `oneof operation`. Errors are returned with the offending op name so
/// the handler can render a 400.
pub fn build_request_operation(
    op: &str,
    params: serde_json::Value,
) -> Result<proto::file_request::Operation, DtoError> {
    use proto::file_request::Operation;

    let parse_err = |op: &str, e: serde_json::Error| DtoError::InvalidParams {
        op: op.to_string(),
        message: e.to_string(),
    };
    macro_rules! parse {
        ($ty:ty) => {
            serde_json::from_value::<$ty>(params).map_err(|e| parse_err(op, e))?
        };
    }

    Ok(match op {
        "stat" => Operation::Stat(parse!(StatParams).into()),
        "list" => Operation::List(parse!(ListParams).into()),
        "glob" => Operation::Glob(parse!(GlobParams).into()),
        "read_text" => Operation::ReadText(parse!(ReadTextParams).into()),
        "read_binary" => Operation::ReadBinary(parse!(ReadBinaryParams).into()),
        "read_image" => Operation::ReadImage(parse!(ReadImageParams).into()),
        "write" => Operation::Write(parse!(WriteParams).try_into().map_err(|msg: String| {
            DtoError::InvalidParams {
                op: "write".into(),
                message: msg,
            }
        })?),
        "edit" => Operation::Edit(parse!(EditParams).try_into().map_err(|msg: String| {
            DtoError::InvalidParams {
                op: "edit".into(),
                message: msg,
            }
        })?),
        "delete" => Operation::Delete(parse!(DeleteParams).into()),
        "chmod" => Operation::Chmod(parse!(ChmodParams).try_into().map_err(|msg: String| {
            DtoError::InvalidParams {
                op: "chmod".into(),
                message: msg,
            }
        })?),
        "mkdir" => Operation::Mkdir(parse!(MkdirParams).into()),
        "copy" => Operation::Copy(parse!(CopyParams).into()),
        "move" => Operation::Move(parse!(MoveParams).into()),
        "create_symlink" => Operation::CreateSymlink(parse!(CreateSymlinkParams).into()),
        other => return Err(DtoError::UnknownOperation(other.into())),
    })
}

// ──────────────────────────────────────────────────────────────────────
// JSON params structs (one per op)
// ──────────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct StatParams {
    pub path: String,
    #[serde(default)]
    pub no_follow_symlink: bool,
}
impl From<StatParams> for proto::FileStat {
    fn from(p: StatParams) -> Self {
        Self {
            path: p.path,
            no_follow_symlink: p.no_follow_symlink,
        }
    }
}

#[derive(Deserialize)]
pub struct ListParams {
    pub path: String,
    #[serde(default)]
    pub max_results: Option<u32>,
    #[serde(default)]
    pub offset: Option<u32>,
    #[serde(default)]
    pub include_hidden: bool,
}
impl From<ListParams> for proto::FileList {
    fn from(p: ListParams) -> Self {
        Self {
            path: p.path,
            max_results: p.max_results,
            offset: p.offset,
            include_hidden: p.include_hidden,
        }
    }
}

#[derive(Deserialize)]
pub struct GlobParams {
    pub pattern: String,
    #[serde(default)]
    pub base_path: Option<String>,
    #[serde(default)]
    pub max_results: Option<u32>,
}
impl From<GlobParams> for proto::FileGlob {
    fn from(p: GlobParams) -> Self {
        Self {
            pattern: p.pattern,
            base_path: p.base_path,
            max_results: p.max_results,
        }
    }
}

#[derive(Deserialize)]
pub struct ReadTextParams {
    pub path: String,
    #[serde(default)]
    pub start: Option<TextStart>,
    #[serde(default)]
    pub max_lines: Option<u32>,
    #[serde(default)]
    pub max_bytes: Option<u64>,
    #[serde(default)]
    pub target_end: Option<FilePositionJson>,
    #[serde(default)]
    pub max_line_width: Option<u32>,
    #[serde(default)]
    pub encoding: Option<String>,
    #[serde(default)]
    pub line_numbers: bool,
    #[serde(default)]
    pub no_follow_symlink: bool,
}
#[derive(Deserialize)]
#[serde(untagged)]
pub enum TextStart {
    Line { start_line: u64 },
    Byte { start_byte: u64 },
    LineCol { start_line_col: LineColJson },
}
#[derive(Deserialize, Clone)]
pub struct LineColJson {
    pub line: u64,
    #[serde(default)]
    pub col: u64,
}
impl From<LineColJson> for proto::LineCol {
    fn from(p: LineColJson) -> Self {
        Self {
            line: p.line,
            col: p.col,
        }
    }
}
#[derive(Deserialize)]
#[serde(untagged)]
pub enum FilePositionJson {
    Line { line: u64 },
    Byte { byte_offset: u64 },
    LineCol { line_col: LineColJson },
}
impl From<FilePositionJson> for proto::FilePosition {
    fn from(p: FilePositionJson) -> Self {
        use proto::file_position::Position;
        proto::FilePosition {
            position: Some(match p {
                FilePositionJson::Line { line } => Position::Line(line),
                FilePositionJson::Byte { byte_offset } => Position::ByteOffset(byte_offset),
                FilePositionJson::LineCol { line_col } => Position::LineCol(line_col.into()),
            }),
        }
    }
}
impl From<ReadTextParams> for proto::FileReadText {
    fn from(p: ReadTextParams) -> Self {
        use proto::file_read_text::Start;
        Self {
            path: p.path,
            start: p.start.map(|s| match s {
                TextStart::Line { start_line } => Start::StartLine(start_line),
                TextStart::Byte { start_byte } => Start::StartByte(start_byte),
                TextStart::LineCol { start_line_col } => Start::StartLineCol(start_line_col.into()),
            }),
            max_lines: p.max_lines,
            max_bytes: p.max_bytes,
            target_end: p.target_end.map(Into::into),
            max_line_width: p.max_line_width,
            encoding: p.encoding,
            line_numbers: p.line_numbers,
            no_follow_symlink: p.no_follow_symlink,
        }
    }
}

#[derive(Deserialize)]
pub struct ReadBinaryParams {
    pub path: String,
    #[serde(default)]
    pub byte_offset: u64,
    #[serde(default)]
    pub byte_length: u64,
    #[serde(default)]
    pub max_bytes: Option<u64>,
    #[serde(default)]
    pub no_follow_symlink: bool,
}
impl From<ReadBinaryParams> for proto::FileReadBinary {
    fn from(p: ReadBinaryParams) -> Self {
        Self {
            path: p.path,
            byte_offset: p.byte_offset,
            byte_length: p.byte_length,
            max_bytes: p.max_bytes,
            no_follow_symlink: p.no_follow_symlink,
        }
    }
}

#[derive(Deserialize)]
pub struct ReadImageParams {
    pub path: String,
    #[serde(default)]
    pub max_width: Option<u32>,
    #[serde(default)]
    pub max_height: Option<u32>,
    #[serde(default)]
    pub max_bytes: Option<u64>,
    #[serde(default)]
    pub quality: Option<u32>,
    #[serde(default)]
    pub output_format: Option<String>,
    #[serde(default)]
    pub no_follow_symlink: bool,
}
impl From<ReadImageParams> for proto::FileReadImage {
    fn from(p: ReadImageParams) -> Self {
        Self {
            path: p.path,
            max_width: p.max_width,
            max_height: p.max_height,
            max_bytes: p.max_bytes,
            quality: p.quality,
            output_format: Some(image_format_to_i32(p.output_format.as_deref())),
            no_follow_symlink: p.no_follow_symlink,
        }
    }
}
fn image_format_to_i32(s: Option<&str>) -> i32 {
    match s.unwrap_or("").to_ascii_lowercase().as_str() {
        "" | "original" => proto::ImageFormat::Original as i32,
        "jpeg" | "jpg" => proto::ImageFormat::Jpeg as i32,
        "png" => proto::ImageFormat::Png as i32,
        "webp" => proto::ImageFormat::Webp as i32,
        // Unknown values fall back to the proto default — daemon will
        // surface a clearer error than the hub could.
        _ => proto::ImageFormat::Original as i32,
    }
}

#[derive(Deserialize)]
pub struct WriteParams {
    pub path: String,
    #[serde(default)]
    pub create_parents: bool,
    #[serde(flatten)]
    pub method: WriteMethod,
    #[serde(default)]
    pub encoding: Option<String>,
    #[serde(default)]
    pub no_follow_symlink: bool,
}
#[derive(Deserialize)]
#[serde(untagged)]
pub enum WriteMethod {
    FullWrite {
        full_write: FullWriteJson,
    },
    Append {
        append: AppendJson,
    },
    StringReplace {
        string_replace: StringReplaceJson,
    },
    LineRangeReplace {
        line_range_replace: LineRangeReplaceJson,
    },
    ByteRangeReplace {
        byte_range_replace: ByteRangeReplaceJson,
    },
}
#[derive(Deserialize)]
pub struct FullWriteJson {
    /// Either `content` (UTF-8 string written as bytes) OR `s3_object_key`.
    /// Exactly one must be set.
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub s3_object_key: Option<String>,
}
#[derive(Deserialize)]
pub struct AppendJson {
    pub content: String,
}
#[derive(Deserialize)]
pub struct StringReplaceJson {
    pub old_string: String,
    pub new_string: String,
    #[serde(default)]
    pub replace_all: bool,
}
#[derive(Deserialize)]
pub struct LineRangeReplaceJson {
    pub start_line: u64,
    pub end_line: u64,
    pub new_content: String,
}
#[derive(Deserialize)]
pub struct ByteRangeReplaceJson {
    pub byte_offset: u64,
    pub byte_length: u64,
    /// Base64-encoded replacement bytes.
    pub new_content_b64: String,
}
impl TryFrom<WriteParams> for proto::FileWrite {
    type Error = String;
    fn try_from(p: WriteParams) -> Result<Self, Self::Error> {
        use proto::file_write::Method;
        let method = match p.method {
            WriteMethod::FullWrite { full_write } => {
                let source = match (full_write.content, full_write.s3_object_key) {
                    (Some(content), None) => {
                        proto::full_write::Source::Content(content.into_bytes())
                    }
                    (None, Some(key)) => proto::full_write::Source::S3ObjectKey(key),
                    (Some(_), Some(_)) => {
                        return Err(
                            "full_write may not set both 'content' and 's3_object_key'".into()
                        );
                    }
                    (None, None) => {
                        return Err(
                            "full_write requires either 'content' or 's3_object_key'".into()
                        );
                    }
                };
                Method::FullWrite(proto::FullWrite {
                    source: Some(source),
                })
            }
            WriteMethod::Append { append } => Method::Append(proto::FileAppend {
                content: append.content.into_bytes(),
            }),
            WriteMethod::StringReplace { string_replace } => {
                Method::StringReplace(string_replace.into())
            }
            WriteMethod::LineRangeReplace { line_range_replace } => {
                Method::LineRangeReplace(line_range_replace.into())
            }
            WriteMethod::ByteRangeReplace { byte_range_replace } => {
                Method::ByteRangeReplace(byte_range_replace.try_into()?)
            }
        };
        Ok(proto::FileWrite {
            path: p.path,
            create_parents: p.create_parents,
            method: Some(method),
            encoding: p.encoding,
            no_follow_symlink: p.no_follow_symlink,
        })
    }
}
impl From<StringReplaceJson> for proto::StringReplace {
    fn from(j: StringReplaceJson) -> Self {
        Self {
            old_string: j.old_string,
            new_string: j.new_string,
            replace_all: j.replace_all,
        }
    }
}
impl From<LineRangeReplaceJson> for proto::LineRangeReplace {
    fn from(j: LineRangeReplaceJson) -> Self {
        Self {
            start_line: j.start_line,
            end_line: j.end_line,
            new_content: j.new_content,
        }
    }
}
impl TryFrom<ByteRangeReplaceJson> for proto::ByteRangeReplace {
    type Error = String;
    fn try_from(j: ByteRangeReplaceJson) -> Result<Self, Self::Error> {
        use base64::Engine as _;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&j.new_content_b64)
            .map_err(|e| format!("invalid base64 in new_content_b64: {e}"))?;
        Ok(Self {
            byte_offset: j.byte_offset,
            byte_length: j.byte_length,
            new_content: bytes,
        })
    }
}

#[derive(Deserialize)]
pub struct EditParams {
    pub path: String,
    #[serde(flatten)]
    pub method: EditMethod,
    #[serde(default)]
    pub encoding: Option<String>,
    #[serde(default)]
    pub no_follow_symlink: bool,
}
#[derive(Deserialize)]
#[serde(untagged)]
pub enum EditMethod {
    StringReplace {
        string_replace: StringReplaceJson,
    },
    LineRangeReplace {
        line_range_replace: LineRangeReplaceJson,
    },
    ByteRangeReplace {
        byte_range_replace: ByteRangeReplaceJson,
    },
}
impl TryFrom<EditParams> for proto::FileEdit {
    type Error = String;
    fn try_from(p: EditParams) -> Result<Self, Self::Error> {
        use proto::file_edit::Method;
        let method = match p.method {
            EditMethod::StringReplace { string_replace } => {
                Method::StringReplace(string_replace.into())
            }
            EditMethod::LineRangeReplace { line_range_replace } => {
                Method::LineRangeReplace(line_range_replace.into())
            }
            EditMethod::ByteRangeReplace { byte_range_replace } => {
                Method::ByteRangeReplace(byte_range_replace.try_into()?)
            }
        };
        Ok(proto::FileEdit {
            path: p.path,
            method: Some(method),
            encoding: p.encoding,
            no_follow_symlink: p.no_follow_symlink,
        })
    }
}

#[derive(Deserialize)]
pub struct DeleteParams {
    pub path: String,
    #[serde(default)]
    pub recursive: bool,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub no_follow_symlink: bool,
}
impl From<DeleteParams> for proto::FileDelete {
    fn from(p: DeleteParams) -> Self {
        let mode = match p
            .mode
            .as_deref()
            .unwrap_or("trash")
            .to_ascii_lowercase()
            .as_str()
        {
            "permanent" => proto::DeleteMode::Permanent as i32,
            _ => proto::DeleteMode::Trash as i32,
        };
        Self {
            path: p.path,
            recursive: p.recursive,
            mode,
            no_follow_symlink: p.no_follow_symlink,
        }
    }
}

#[derive(Deserialize)]
pub struct ChmodParams {
    pub path: String,
    #[serde(default)]
    pub recursive: bool,
    #[serde(default)]
    pub no_follow_symlink: bool,
    #[serde(flatten)]
    pub permission: ChmodPermission,
}
#[derive(Deserialize)]
#[serde(untagged)]
pub enum ChmodPermission {
    Unix { unix: UnixPermissionJson },
    Windows { windows: WindowsAclJson },
}
#[derive(Deserialize)]
pub struct UnixPermissionJson {
    #[serde(default)]
    pub mode: Option<u32>,
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub group: Option<String>,
}
impl From<UnixPermissionJson> for proto::UnixPermission {
    fn from(p: UnixPermissionJson) -> Self {
        Self {
            mode: p.mode,
            owner: p.owner,
            group: p.group,
        }
    }
}
#[derive(Deserialize)]
pub struct WindowsAclJson {
    pub entries: Vec<AclEntryJson>,
}
#[derive(Deserialize)]
pub struct AclEntryJson {
    pub principal: String,
    pub access_mask: u32,
    /// "allow" | "deny" — defaults to "allow" if missing.
    #[serde(default)]
    pub entry_type: Option<String>,
}
impl From<WindowsAclJson> for proto::WindowsAcl {
    fn from(p: WindowsAclJson) -> Self {
        Self {
            entries: p.entries.into_iter().map(Into::into).collect(),
        }
    }
}
impl From<AclEntryJson> for proto::AclEntry {
    fn from(p: AclEntryJson) -> Self {
        let entry_type = match p
            .entry_type
            .as_deref()
            .unwrap_or("allow")
            .to_ascii_lowercase()
            .as_str()
        {
            "deny" => proto::AclEntryType::Deny as i32,
            _ => proto::AclEntryType::Allow as i32,
        };
        Self {
            principal: p.principal,
            access_mask: p.access_mask,
            entry_type,
        }
    }
}
impl TryFrom<ChmodParams> for proto::FileChmod {
    type Error = String;
    fn try_from(p: ChmodParams) -> Result<Self, Self::Error> {
        use proto::file_chmod::Permission;
        let permission = match p.permission {
            ChmodPermission::Unix { unix } => Permission::Unix(unix.into()),
            ChmodPermission::Windows { windows } => Permission::Windows(windows.into()),
        };
        Ok(Self {
            path: p.path,
            recursive: p.recursive,
            no_follow_symlink: p.no_follow_symlink,
            permission: Some(permission),
        })
    }
}

#[derive(Deserialize)]
pub struct MkdirParams {
    pub path: String,
    #[serde(default)]
    pub recursive: bool,
    #[serde(default)]
    pub mode: Option<u32>,
}
impl From<MkdirParams> for proto::FileMkdir {
    fn from(p: MkdirParams) -> Self {
        Self {
            path: p.path,
            recursive: p.recursive,
            mode: p.mode,
        }
    }
}

#[derive(Deserialize)]
pub struct CopyParams {
    pub source: String,
    pub destination: String,
    #[serde(default)]
    pub recursive: bool,
    #[serde(default)]
    pub overwrite: bool,
}
impl From<CopyParams> for proto::FileCopy {
    fn from(p: CopyParams) -> Self {
        Self {
            source: p.source,
            destination: p.destination,
            recursive: p.recursive,
            overwrite: p.overwrite,
        }
    }
}

#[derive(Deserialize)]
pub struct MoveParams {
    pub source: String,
    pub destination: String,
    #[serde(default)]
    pub overwrite: bool,
}
impl From<MoveParams> for proto::FileMove {
    fn from(p: MoveParams) -> Self {
        Self {
            source: p.source,
            destination: p.destination,
            overwrite: p.overwrite,
        }
    }
}

#[derive(Deserialize)]
pub struct CreateSymlinkParams {
    pub target: String,
    pub link_path: String,
}
impl From<CreateSymlinkParams> for proto::FileCreateSymlink {
    fn from(p: CreateSymlinkParams) -> Self {
        Self {
            target: p.target,
            link_path: p.link_path,
        }
    }
}

// ──────────────────────────────────────────────────────────────────────
// Proto → JSON: serialize a `FileResponse` into the wire envelope.
// ──────────────────────────────────────────────────────────────────────

/// Translate a daemon-side `FileResponse` into the JSON envelope.
/// Returns `(operation_name, response_envelope)` so the handler can
/// echo back the operation tag the caller sent (when the daemon's
/// result variant matches the requested op) — falling back to "error"
/// or "unknown" when the variant disagrees.
///
/// `requested_op` is the operation tag the caller sent ("stat", ...).
/// We use it as the response's `operation` field. If the daemon
/// returned an Error result, the caller's op tag is still echoed back
/// so the SDK can correlate request/response by client-side state.
pub fn build_response_envelope(
    response: proto::FileResponse,
    requested_op: &str,
    duration_ms: u64,
) -> ControlFilesResponse {
    use proto::file_response::Result as R;
    let request_id = response.request_id.clone();
    let (success, result, error) = match response.result {
        Some(R::Error(e)) => (
            false,
            None,
            Some(FileErrorJson {
                code: file_error_code_to_str(e.code).to_string(),
                message: e.message,
                path: e.path,
            }),
        ),
        Some(R::ReadText(r)) => (true, Some(read_text_result_to_json(r)), None),
        Some(R::ReadBinary(r)) => (true, Some(read_binary_result_to_json(r)), None),
        Some(R::ReadImage(r)) => (true, Some(read_image_result_to_json(r)), None),
        Some(R::Write(r)) => (true, Some(write_result_to_json(r)), None),
        Some(R::Edit(r)) => (true, Some(edit_result_to_json(r)), None),
        Some(R::Delete(r)) => (true, Some(delete_result_to_json(r)), None),
        Some(R::Chmod(r)) => (
            true,
            Some(serde_json::json!({
                "path": r.path,
                "items_modified": r.items_modified,
            })),
            None,
        ),
        Some(R::Stat(r)) => (true, Some(stat_result_to_json(r)), None),
        Some(R::List(r)) => (true, Some(list_result_to_json(r)), None),
        Some(R::Glob(r)) => (true, Some(glob_result_to_json(r)), None),
        Some(R::Mkdir(r)) => (
            true,
            Some(serde_json::json!({
                "path": r.path,
                "already_existed": r.already_existed,
            })),
            None,
        ),
        Some(R::Copy(r)) => (
            true,
            Some(serde_json::json!({
                "source": r.source,
                "destination": r.destination,
                "items_copied": r.items_copied,
            })),
            None,
        ),
        Some(R::MoveResult(r)) => (
            true,
            Some(serde_json::json!({
                "source": r.source,
                "destination": r.destination,
            })),
            None,
        ),
        Some(R::CreateSymlink(r)) => (
            true,
            Some(serde_json::json!({
                "link_path": r.link_path,
                "target": r.target,
            })),
            None,
        ),
        None => (
            false,
            None,
            Some(FileErrorJson {
                code: "unspecified".into(),
                message: "device returned an empty FileResponse".into(),
                path: String::new(),
            }),
        ),
    };

    ControlFilesResponse {
        request_id,
        operation: requested_op.to_string(),
        success,
        result,
        error,
        duration_ms,
    }
}

/// Map proto `FileErrorCode` → wire-stable lowercase string. The SDK
/// branches on these strings so they're effectively part of the API.
fn file_error_code_to_str(code: i32) -> &'static str {
    match proto::FileErrorCode::try_from(code).unwrap_or(proto::FileErrorCode::Unspecified) {
        proto::FileErrorCode::Unspecified => "unspecified",
        proto::FileErrorCode::NotFound => "not_found",
        proto::FileErrorCode::PermissionDenied => "permission_denied",
        proto::FileErrorCode::AlreadyExists => "already_exists",
        proto::FileErrorCode::NotADirectory => "not_a_directory",
        proto::FileErrorCode::IsADirectory => "is_a_directory",
        proto::FileErrorCode::NotEmpty => "not_empty",
        proto::FileErrorCode::TooLarge => "too_large",
        proto::FileErrorCode::InvalidPath => "invalid_path",
        proto::FileErrorCode::Io => "io",
        proto::FileErrorCode::Encoding => "encoding",
        proto::FileErrorCode::MultipleMatches => "multiple_matches",
        proto::FileErrorCode::PolicyDenied => "policy_denied",
    }
}

fn file_type_to_str(ft: i32) -> &'static str {
    match proto::FileType::try_from(ft).unwrap_or(proto::FileType::Other) {
        proto::FileType::File => "file",
        proto::FileType::Directory => "directory",
        proto::FileType::Symlink => "symlink",
        proto::FileType::Other => "other",
    }
}

fn write_action_to_str(a: i32) -> &'static str {
    match proto::WriteAction::try_from(a).unwrap_or(proto::WriteAction::Created) {
        proto::WriteAction::Created => "created",
        proto::WriteAction::Overwritten => "overwritten",
        proto::WriteAction::Appended => "appended",
        proto::WriteAction::Edited => "edited",
    }
}

fn delete_mode_to_str(m: i32) -> &'static str {
    match proto::DeleteMode::try_from(m).unwrap_or(proto::DeleteMode::Trash) {
        proto::DeleteMode::Trash => "trash",
        proto::DeleteMode::Permanent => "permanent",
    }
}

fn image_format_to_str(f: i32) -> &'static str {
    match proto::ImageFormat::try_from(f).unwrap_or(proto::ImageFormat::Original) {
        proto::ImageFormat::Original => "original",
        proto::ImageFormat::Jpeg => "jpeg",
        proto::ImageFormat::Png => "png",
        proto::ImageFormat::Webp => "webp",
    }
}

fn unix_permission_to_json(p: &proto::UnixPermission) -> serde_json::Value {
    serde_json::json!({
        "mode": p.mode,
        "owner": p.owner,
        "group": p.group,
    })
}

fn windows_acl_to_json(p: &proto::WindowsAcl) -> serde_json::Value {
    serde_json::json!({
        "entries": p.entries.iter().map(|e| serde_json::json!({
            "principal": e.principal,
            "access_mask": e.access_mask,
            "entry_type": match proto::AclEntryType::try_from(e.entry_type).unwrap_or(proto::AclEntryType::Allow) {
                proto::AclEntryType::Allow => "allow",
                proto::AclEntryType::Deny => "deny",
            }
        })).collect::<Vec<_>>(),
    })
}

fn stat_result_to_json(r: proto::FileStatResult) -> serde_json::Value {
    serde_json::json!({
        "path": r.path,
        "file_type": file_type_to_str(r.file_type),
        "size": r.size,
        "modified_ms": r.modified_ms,
        "created_ms": r.created_ms,
        "accessed_ms": r.accessed_ms,
        "unix_permission": r.unix_permission.as_ref().map(unix_permission_to_json),
        "windows_acl": r.windows_acl.as_ref().map(windows_acl_to_json),
        "symlink_target": r.symlink_target,
    })
}

fn list_result_to_json(r: proto::FileListResult) -> serde_json::Value {
    serde_json::json!({
        "entries": r.entries.iter().map(file_entry_to_json).collect::<Vec<_>>(),
        "total_count": r.total_count,
        "has_more": r.has_more,
    })
}

fn glob_result_to_json(r: proto::FileGlobResult) -> serde_json::Value {
    serde_json::json!({
        "entries": r.entries.iter().map(file_entry_to_json).collect::<Vec<_>>(),
        "total_matches": r.total_matches,
        "has_more": r.has_more,
    })
}

fn file_entry_to_json(e: &proto::FileEntry) -> serde_json::Value {
    serde_json::json!({
        "name": e.name,
        "file_type": file_type_to_str(e.file_type),
        "size": e.size,
        "modified_ms": e.modified_ms,
        "symlink_target": e.symlink_target,
    })
}

fn read_text_result_to_json(r: proto::FileReadTextResult) -> serde_json::Value {
    serde_json::json!({
        "lines": r.lines.iter().map(|l| serde_json::json!({
            "content": l.content,
            "line_number": l.line_number,
            "truncated": l.truncated,
            "remaining_bytes": l.remaining_bytes,
        })).collect::<Vec<_>>(),
        "stop_reason": match proto::StopReason::try_from(r.stop_reason).unwrap_or(proto::StopReason::Unspecified) {
            proto::StopReason::Unspecified => "unspecified",
            proto::StopReason::MaxLines => "max_lines",
            proto::StopReason::MaxBytes => "max_bytes",
            proto::StopReason::TargetEnd => "target_end",
            proto::StopReason::FileEnd => "file_end",
            proto::StopReason::Error => "error",
        },
        "start_pos": r.start_pos.as_ref().map(position_info_to_json),
        "end_pos": r.end_pos.as_ref().map(position_info_to_json),
        "remaining_bytes": r.remaining_bytes,
        "total_file_bytes": r.total_file_bytes,
        "total_lines": r.total_lines,
        "detected_encoding": r.detected_encoding,
    })
}

fn position_info_to_json(p: &proto::PositionInfo) -> serde_json::Value {
    serde_json::json!({
        "line": p.line,
        "byte_in_file": p.byte_in_file,
        "byte_in_line": p.byte_in_line,
    })
}

fn read_binary_result_to_json(r: proto::FileReadBinaryResult) -> serde_json::Value {
    use base64::Engine as _;
    let content_b64 = base64::engine::general_purpose::STANDARD.encode(&r.content);
    serde_json::json!({
        "content_b64": content_b64,
        "byte_offset": r.byte_offset,
        "bytes_read": r.bytes_read,
        "total_file_bytes": r.total_file_bytes,
        "remaining_bytes": r.remaining_bytes,
        "download_url": r.download_url,
        "download_url_expires_ms": r.download_url_expires_ms,
    })
}

fn read_image_result_to_json(r: proto::FileReadImageResult) -> serde_json::Value {
    use base64::Engine as _;
    let content_b64 = base64::engine::general_purpose::STANDARD.encode(&r.content);
    serde_json::json!({
        "content_b64": content_b64,
        "format": image_format_to_str(r.format),
        "width": r.width,
        "height": r.height,
        "original_bytes": r.original_bytes,
        "output_bytes": r.output_bytes,
        "download_url": r.download_url,
        "download_url_expires_ms": r.download_url_expires_ms,
    })
}

fn write_result_to_json(r: proto::FileWriteResult) -> serde_json::Value {
    serde_json::json!({
        "path": r.path,
        "action": write_action_to_str(r.action),
        "bytes_written": r.bytes_written,
        "final_size": r.final_size,
        "replacements_made": r.replacements_made,
    })
}

fn edit_result_to_json(r: proto::FileEditResult) -> serde_json::Value {
    serde_json::json!({
        "path": r.path,
        "final_size": r.final_size,
        "replacements_made": r.replacements_made,
        "match_error": r.match_error,
    })
}

fn delete_result_to_json(r: proto::FileDeleteResult) -> serde_json::Value {
    serde_json::json!({
        "path": r.path,
        "mode": delete_mode_to_str(r.mode),
        "items_deleted": r.items_deleted,
        "trash_path": r.trash_path,
    })
}
