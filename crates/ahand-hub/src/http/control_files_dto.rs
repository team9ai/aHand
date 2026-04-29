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
//! Most daemon-side failures (file not found, I/O error, encoding
//! mismatch, ...) come back as `success: false` with an `error` field
//! and HTTP 200 — callers commonly want to handle these gracefully
//! (e.g. probing for an optional config) rather than catching a
//! thrown error.
//!
//! The exception is `policy_denied`: the hub elevates it to a
//! hub-level HTTP 403 with body `{error: {code: "POLICY_DENIED", ...}}`
//! so the SDK can branch on `err.code === "policy_denied"` without
//! inspecting the response envelope. A path the daemon's policy
//! refuses isn't fixable by retrying — it deserves the typed-error
//! treatment, alongside auth and ownership failures.
//!
//! Hub-level failures (auth, ownership, offline, timeout, rate limit)
//! return appropriate 4xx/5xx HTTP statuses with the standard
//! `{error: {code, message}}` envelope.

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
        "read_image" => {
            Operation::ReadImage(parse!(ReadImageParams).try_into().map_err(|msg: String| {
                DtoError::InvalidParams {
                    op: "read_image".into(),
                    message: msg,
                }
            })?)
        }
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
        "delete" => Operation::Delete(parse!(DeleteParams).try_into().map_err(|msg: String| {
            DtoError::InvalidParams {
                op: "delete".into(),
                message: msg,
            }
        })?),
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
impl TryFrom<ReadImageParams> for proto::FileReadImage {
    type Error = String;
    fn try_from(p: ReadImageParams) -> Result<Self, Self::Error> {
        let output_format = match p.output_format.as_deref() {
            None | Some("") | Some("original") => proto::ImageFormat::Original as i32,
            Some(s) => match s.to_ascii_lowercase().as_str() {
                "jpeg" | "jpg" => proto::ImageFormat::Jpeg as i32,
                "png" => proto::ImageFormat::Png as i32,
                "webp" => proto::ImageFormat::Webp as i32,
                // Reject unknown values explicitly so a user typo
                // (`tiff`, `gif`) doesn't silently fall back to
                // Original — that would mask the bug at the hub and
                // hand the daemon something it didn't ask for.
                other => {
                    return Err(format!(
                        "output_format '{other}' is not one of: original, jpeg, png, webp"
                    ));
                }
            },
        };
        Ok(Self {
            path: p.path,
            max_width: p.max_width,
            max_height: p.max_height,
            max_bytes: p.max_bytes,
            quality: p.quality,
            output_format: Some(output_format),
            no_follow_symlink: p.no_follow_symlink,
        })
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
impl TryFrom<DeleteParams> for proto::FileDelete {
    type Error = String;
    fn try_from(p: DeleteParams) -> Result<Self, Self::Error> {
        let mode = match p.mode.as_deref() {
            None | Some("") | Some("trash") => proto::DeleteMode::Trash as i32,
            Some(s) => match s.to_ascii_lowercase().as_str() {
                "trash" => proto::DeleteMode::Trash as i32,
                "permanent" => proto::DeleteMode::Permanent as i32,
                // Reject unknown values explicitly — a typo like
                // "permaent" must not silently degrade to "trash"
                // (which has very different filesystem semantics
                // than "permanent" and could surprise the caller).
                other => {
                    return Err(format!("mode '{other}' is not one of: trash, permanent"));
                }
            },
        };
        Ok(Self {
            path: p.path,
            recursive: p.recursive,
            mode,
            no_follow_symlink: p.no_follow_symlink,
        })
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
impl TryFrom<WindowsAclJson> for proto::WindowsAcl {
    type Error = String;
    fn try_from(p: WindowsAclJson) -> Result<Self, Self::Error> {
        let entries: Result<Vec<_>, _> = p.entries.into_iter().map(TryInto::try_into).collect();
        Ok(Self { entries: entries? })
    }
}
impl TryFrom<AclEntryJson> for proto::AclEntry {
    type Error = String;
    fn try_from(p: AclEntryJson) -> Result<Self, Self::Error> {
        let entry_type = match p.entry_type.as_deref() {
            None | Some("") | Some("allow") => proto::AclEntryType::Allow as i32,
            Some(s) => match s.to_ascii_lowercase().as_str() {
                "allow" => proto::AclEntryType::Allow as i32,
                "deny" => proto::AclEntryType::Deny as i32,
                // Reject unknown values explicitly — a typo on a
                // chmod ACL entry must surface, since silently
                // defaulting to "allow" would be a security
                // footgun (the caller may have meant "deny").
                other => {
                    return Err(format!("entry_type '{other}' is not one of: allow, deny"));
                }
            },
        };
        Ok(Self {
            principal: p.principal,
            access_mask: p.access_mask,
            entry_type,
        })
    }
}
impl TryFrom<ChmodParams> for proto::FileChmod {
    type Error = String;
    fn try_from(p: ChmodParams) -> Result<Self, Self::Error> {
        use proto::file_chmod::Permission;
        let permission = match p.permission {
            ChmodPermission::Unix { unix } => Permission::Unix(unix.into()),
            ChmodPermission::Windows { windows } => Permission::Windows(windows.try_into()?),
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

// ──────────────────────────────────────────────────────────────────────
// Unit tests — fast, no-network coverage of the JSON ⇄ proto mapping.
//
// Integration tests in `tests/control_files.rs` cover the HTTP plumbing
// (auth, ownership, error statuses). These unit tests focus on the
// data-shape mapping: for every one of the 14 ops, that the JSON input
// becomes the right proto variant with the right fields, and that every
// proto result variant renders back to the expected JSON shape.
// ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── Request direction: build_request_operation (14 ops) ───────────

    #[test]
    fn build_stat_maps_path_and_no_follow() {
        let op = build_request_operation(
            "stat",
            json!({ "path": "/tmp/x", "no_follow_symlink": true }),
        )
        .unwrap();
        match op {
            proto::file_request::Operation::Stat(s) => {
                assert_eq!(s.path, "/tmp/x");
                assert!(s.no_follow_symlink);
            }
            other => panic!("expected Stat, got {other:?}"),
        }
    }

    #[test]
    fn build_list_maps_all_fields() {
        let op = build_request_operation(
            "list",
            json!({
                "path": "/tmp",
                "max_results": 50,
                "offset": 10,
                "include_hidden": true,
            }),
        )
        .unwrap();
        match op {
            proto::file_request::Operation::List(l) => {
                assert_eq!(l.path, "/tmp");
                assert_eq!(l.max_results, Some(50));
                assert_eq!(l.offset, Some(10));
                assert!(l.include_hidden);
            }
            other => panic!("expected List, got {other:?}"),
        }
    }

    #[test]
    fn build_glob_maps_all_fields() {
        let op = build_request_operation(
            "glob",
            json!({
                "pattern": "**/*.rs",
                "base_path": "/repo",
                "max_results": 100,
            }),
        )
        .unwrap();
        match op {
            proto::file_request::Operation::Glob(g) => {
                assert_eq!(g.pattern, "**/*.rs");
                assert_eq!(g.base_path.as_deref(), Some("/repo"));
                assert_eq!(g.max_results, Some(100));
            }
            other => panic!("expected Glob, got {other:?}"),
        }
    }

    #[test]
    fn build_read_text_with_start_line() {
        let op = build_request_operation(
            "read_text",
            json!({
                "path": "/r.txt",
                "start": { "start_line": 10 },
                "max_lines": 50,
                "line_numbers": true,
            }),
        )
        .unwrap();
        match op {
            proto::file_request::Operation::ReadText(r) => {
                assert_eq!(r.path, "/r.txt");
                assert_eq!(r.start, Some(proto::file_read_text::Start::StartLine(10)));
                assert_eq!(r.max_lines, Some(50));
                assert!(r.line_numbers);
            }
            other => panic!("expected ReadText, got {other:?}"),
        }
    }

    #[test]
    fn build_read_text_with_start_byte() {
        let op = build_request_operation(
            "read_text",
            json!({ "path": "/r.txt", "start": { "start_byte": 1024 } }),
        )
        .unwrap();
        match op {
            proto::file_request::Operation::ReadText(r) => {
                assert_eq!(r.start, Some(proto::file_read_text::Start::StartByte(1024)));
            }
            _ => panic!("expected ReadText"),
        }
    }

    #[test]
    fn build_read_text_with_start_line_col() {
        let op = build_request_operation(
            "read_text",
            json!({
                "path": "/r.txt",
                "start": { "start_line_col": { "line": 5, "col": 3 } },
            }),
        )
        .unwrap();
        match op {
            proto::file_request::Operation::ReadText(r) => match r.start {
                Some(proto::file_read_text::Start::StartLineCol(lc)) => {
                    assert_eq!(lc.line, 5);
                    assert_eq!(lc.col, 3);
                }
                other => panic!("expected StartLineCol, got {other:?}"),
            },
            _ => panic!("expected ReadText"),
        }
    }

    #[test]
    fn build_read_text_with_target_end_positions() {
        // Cover all three FilePosition variants in one sweep.
        for (name, js, check) in [
            (
                "line",
                json!({ "line": 42 }),
                Box::new(|p: &proto::FilePosition| {
                    matches!(p.position, Some(proto::file_position::Position::Line(42)))
                }) as Box<dyn Fn(&proto::FilePosition) -> bool>,
            ),
            (
                "byte_offset",
                json!({ "byte_offset": 999 }),
                Box::new(|p| {
                    matches!(
                        p.position,
                        Some(proto::file_position::Position::ByteOffset(999))
                    )
                }),
            ),
            (
                "line_col",
                json!({ "line_col": { "line": 3, "col": 7 } }),
                Box::new(|p| {
                    matches!(
                        &p.position,
                        Some(proto::file_position::Position::LineCol(lc))
                        if lc.line == 3 && lc.col == 7
                    )
                }),
            ),
        ] {
            let op =
                build_request_operation("read_text", json!({ "path": "/x", "target_end": js }))
                    .unwrap_or_else(|e| panic!("{name}: {e}"));
            let proto::file_request::Operation::ReadText(r) = op else {
                panic!("{name}: expected ReadText");
            };
            let pos = r
                .target_end
                .unwrap_or_else(|| panic!("{name}: no target_end"));
            assert!(check(&pos), "{name} variant check failed: {pos:?}");
        }
    }

    #[test]
    fn build_read_binary_maps_all_fields() {
        let op = build_request_operation(
            "read_binary",
            json!({
                "path": "/bin.dat",
                "byte_offset": 100,
                "byte_length": 500,
                "max_bytes": 1_000_000,
                "no_follow_symlink": true,
            }),
        )
        .unwrap();
        match op {
            proto::file_request::Operation::ReadBinary(r) => {
                assert_eq!(r.path, "/bin.dat");
                assert_eq!(r.byte_offset, 100);
                assert_eq!(r.byte_length, 500);
                assert_eq!(r.max_bytes, Some(1_000_000));
                assert!(r.no_follow_symlink);
            }
            _ => panic!("expected ReadBinary"),
        }
    }

    #[test]
    fn build_read_image_accepts_all_output_formats() {
        for (input, expected) in [
            ("original", proto::ImageFormat::Original),
            ("jpeg", proto::ImageFormat::Jpeg),
            ("jpg", proto::ImageFormat::Jpeg),
            ("png", proto::ImageFormat::Png),
            ("webp", proto::ImageFormat::Webp),
            ("JPEG", proto::ImageFormat::Jpeg),
            ("PNG", proto::ImageFormat::Png),
        ] {
            let op = build_request_operation(
                "read_image",
                json!({ "path": "/i.png", "output_format": input }),
            )
            .unwrap_or_else(|e| panic!("{input}: {e}"));
            match op {
                proto::file_request::Operation::ReadImage(r) => {
                    assert_eq!(r.output_format, Some(expected as i32), "input={input}");
                }
                _ => panic!("expected ReadImage"),
            }
        }
    }

    #[test]
    fn build_read_image_omitted_and_empty_format_default_to_original() {
        for params in [
            json!({ "path": "/i.png" }),
            json!({ "path": "/i.png", "output_format": "" }),
            json!({ "path": "/i.png", "output_format": null }),
        ] {
            let op = build_request_operation("read_image", params).unwrap();
            let proto::file_request::Operation::ReadImage(r) = op else {
                panic!("expected ReadImage");
            };
            assert_eq!(r.output_format, Some(proto::ImageFormat::Original as i32));
        }
    }

    #[test]
    fn build_read_image_rejects_unknown_format() {
        let err = build_request_operation(
            "read_image",
            json!({ "path": "/i", "output_format": "tiff" }),
        )
        .unwrap_err();
        match err {
            DtoError::InvalidParams { op, message } => {
                assert_eq!(op, "read_image");
                assert!(message.contains("tiff"), "message={message}");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn build_write_full_write_content() {
        let op = build_request_operation(
            "write",
            json!({
                "path": "/out.txt",
                "create_parents": true,
                "full_write": { "content": "hello" },
                "encoding": "utf-8",
                "no_follow_symlink": true,
            }),
        )
        .unwrap();
        let proto::file_request::Operation::Write(w) = op else {
            panic!("expected Write");
        };
        assert_eq!(w.path, "/out.txt");
        assert!(w.create_parents);
        assert_eq!(w.encoding.as_deref(), Some("utf-8"));
        assert!(w.no_follow_symlink);
        match w.method {
            Some(proto::file_write::Method::FullWrite(fw)) => {
                assert!(matches!(
                    fw.source,
                    Some(proto::full_write::Source::Content(ref b)) if b == b"hello"
                ));
            }
            other => panic!("expected FullWrite, got {other:?}"),
        }
    }

    #[test]
    fn build_write_full_write_s3_object_key() {
        let op = build_request_operation(
            "write",
            json!({
                "path": "/out.txt",
                "full_write": { "s3_object_key": "bucket/key.bin" },
            }),
        )
        .unwrap();
        let proto::file_request::Operation::Write(w) = op else {
            panic!("expected Write");
        };
        assert!(matches!(
            w.method,
            Some(proto::file_write::Method::FullWrite(ref fw))
            if matches!(&fw.source, Some(proto::full_write::Source::S3ObjectKey(k)) if k == "bucket/key.bin")
        ));
    }

    #[test]
    fn build_write_full_write_rejects_both_content_and_s3() {
        let err = build_request_operation(
            "write",
            json!({
                "path": "/x",
                "full_write": { "content": "a", "s3_object_key": "k" },
            }),
        )
        .unwrap_err();
        let DtoError::InvalidParams { op, message } = err else {
            panic!("expected InvalidParams");
        };
        assert_eq!(op, "write");
        assert!(
            message.contains("content") && message.contains("s3_object_key"),
            "message={message}"
        );
    }

    #[test]
    fn build_write_full_write_rejects_neither_content_nor_s3() {
        let err = build_request_operation("write", json!({ "path": "/x", "full_write": {} }))
            .unwrap_err();
        assert!(matches!(err, DtoError::InvalidParams { op, .. } if op == "write"));
    }

    #[test]
    fn build_write_append_method() {
        let op = build_request_operation(
            "write",
            json!({ "path": "/o", "append": { "content": "more" } }),
        )
        .unwrap();
        let proto::file_request::Operation::Write(w) = op else {
            panic!("expected Write");
        };
        match w.method {
            Some(proto::file_write::Method::Append(a)) => assert_eq!(a.content, b"more"),
            _ => panic!("expected Append"),
        }
    }

    #[test]
    fn build_write_string_replace_method() {
        let op = build_request_operation(
            "write",
            json!({
                "path": "/o",
                "string_replace": {
                    "old_string": "foo",
                    "new_string": "bar",
                    "replace_all": true,
                },
            }),
        )
        .unwrap();
        let proto::file_request::Operation::Write(w) = op else {
            panic!("expected Write");
        };
        match w.method {
            Some(proto::file_write::Method::StringReplace(sr)) => {
                assert_eq!(sr.old_string, "foo");
                assert_eq!(sr.new_string, "bar");
                assert!(sr.replace_all);
            }
            _ => panic!("expected StringReplace"),
        }
    }

    #[test]
    fn build_write_line_range_replace_method() {
        let op = build_request_operation(
            "write",
            json!({
                "path": "/o",
                "line_range_replace": {
                    "start_line": 5,
                    "end_line": 10,
                    "new_content": "replaced",
                },
            }),
        )
        .unwrap();
        let proto::file_request::Operation::Write(w) = op else {
            panic!("expected Write");
        };
        match w.method {
            Some(proto::file_write::Method::LineRangeReplace(r)) => {
                assert_eq!(r.start_line, 5);
                assert_eq!(r.end_line, 10);
                assert_eq!(r.new_content, "replaced");
            }
            _ => panic!("expected LineRangeReplace"),
        }
    }

    #[test]
    fn build_write_byte_range_replace_method_decodes_base64() {
        use base64::Engine as _;
        let payload = b"hello\x00\xff".to_vec();
        let b64 = base64::engine::general_purpose::STANDARD.encode(&payload);
        let op = build_request_operation(
            "write",
            json!({
                "path": "/o",
                "byte_range_replace": {
                    "byte_offset": 0,
                    "byte_length": 7,
                    "new_content_b64": b64,
                },
            }),
        )
        .unwrap();
        let proto::file_request::Operation::Write(w) = op else {
            panic!("expected Write");
        };
        match w.method {
            Some(proto::file_write::Method::ByteRangeReplace(r)) => {
                assert_eq!(r.byte_offset, 0);
                assert_eq!(r.byte_length, 7);
                assert_eq!(r.new_content, payload);
            }
            _ => panic!("expected ByteRangeReplace"),
        }
    }

    #[test]
    fn build_write_byte_range_rejects_invalid_base64() {
        let err = build_request_operation(
            "write",
            json!({
                "path": "/o",
                "byte_range_replace": {
                    "byte_offset": 0,
                    "byte_length": 1,
                    "new_content_b64": "not base64!!!",
                },
            }),
        )
        .unwrap_err();
        let DtoError::InvalidParams { message, .. } = err else {
            panic!("expected InvalidParams");
        };
        assert!(message.contains("base64"), "message={message}");
    }

    #[test]
    fn build_edit_all_three_methods() {
        // StringReplace
        let op = build_request_operation(
            "edit",
            json!({
                "path": "/e",
                "string_replace": { "old_string": "x", "new_string": "y" },
            }),
        )
        .unwrap();
        let proto::file_request::Operation::Edit(e) = op else {
            panic!("expected Edit");
        };
        assert!(matches!(
            e.method,
            Some(proto::file_edit::Method::StringReplace(_))
        ));

        // LineRangeReplace
        let op = build_request_operation(
            "edit",
            json!({
                "path": "/e",
                "line_range_replace": {
                    "start_line": 1,
                    "end_line": 2,
                    "new_content": "x",
                },
            }),
        )
        .unwrap();
        let proto::file_request::Operation::Edit(e) = op else {
            panic!("expected Edit");
        };
        assert!(matches!(
            e.method,
            Some(proto::file_edit::Method::LineRangeReplace(_))
        ));

        // ByteRangeReplace
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD.encode(b"z");
        let op = build_request_operation(
            "edit",
            json!({
                "path": "/e",
                "byte_range_replace": {
                    "byte_offset": 0,
                    "byte_length": 1,
                    "new_content_b64": b64,
                },
            }),
        )
        .unwrap();
        let proto::file_request::Operation::Edit(e) = op else {
            panic!("expected Edit");
        };
        assert!(matches!(
            e.method,
            Some(proto::file_edit::Method::ByteRangeReplace(_))
        ));
    }

    #[test]
    fn build_edit_byte_range_rejects_invalid_base64() {
        let err = build_request_operation(
            "edit",
            json!({
                "path": "/e",
                "byte_range_replace": {
                    "byte_offset": 0,
                    "byte_length": 1,
                    "new_content_b64": "@@@",
                },
            }),
        )
        .unwrap_err();
        assert!(matches!(err, DtoError::InvalidParams { op, .. } if op == "edit"));
    }

    #[test]
    fn build_delete_accepts_trash_and_permanent() {
        for (mode, expected) in [
            ("trash", proto::DeleteMode::Trash),
            ("permanent", proto::DeleteMode::Permanent),
            ("TRASH", proto::DeleteMode::Trash),
            ("PERMANENT", proto::DeleteMode::Permanent),
        ] {
            let op = build_request_operation(
                "delete",
                json!({ "path": "/d", "mode": mode, "recursive": true }),
            )
            .unwrap();
            let proto::file_request::Operation::Delete(d) = op else {
                panic!("expected Delete");
            };
            assert_eq!(d.mode, expected as i32, "mode input={mode}");
            assert!(d.recursive);
        }
    }

    #[test]
    fn build_delete_omitted_mode_defaults_to_trash() {
        let op = build_request_operation("delete", json!({ "path": "/d" })).unwrap();
        let proto::file_request::Operation::Delete(d) = op else {
            panic!("expected Delete");
        };
        assert_eq!(d.mode, proto::DeleteMode::Trash as i32);
    }

    #[test]
    fn build_delete_rejects_unknown_mode() {
        let err = build_request_operation("delete", json!({ "path": "/d", "mode": "permaent" }))
            .unwrap_err();
        let DtoError::InvalidParams { op, message } = err else {
            panic!("expected InvalidParams");
        };
        assert_eq!(op, "delete");
        assert!(message.contains("permaent"), "message={message}");
    }

    #[test]
    fn build_chmod_unix_permission() {
        let op = build_request_operation(
            "chmod",
            json!({
                "path": "/f",
                "recursive": true,
                "unix": { "mode": 0o644, "owner": "alice", "group": "devs" },
            }),
        )
        .unwrap();
        let proto::file_request::Operation::Chmod(c) = op else {
            panic!("expected Chmod");
        };
        assert_eq!(c.path, "/f");
        assert!(c.recursive);
        match c.permission {
            Some(proto::file_chmod::Permission::Unix(u)) => {
                assert_eq!(u.mode, Some(0o644));
                assert_eq!(u.owner.as_deref(), Some("alice"));
                assert_eq!(u.group.as_deref(), Some("devs"));
            }
            _ => panic!("expected Unix permission"),
        }
    }

    #[test]
    fn build_chmod_windows_acl_allow_and_deny() {
        let op = build_request_operation(
            "chmod",
            json!({
                "path": "/f",
                "windows": {
                    "entries": [
                        { "principal": "SYSTEM", "access_mask": 0x1F01FF, "entry_type": "allow" },
                        { "principal": "Guests", "access_mask": 0x120089, "entry_type": "deny" },
                        { "principal": "Users", "access_mask": 0x120089 },
                    ],
                },
            }),
        )
        .unwrap();
        let proto::file_request::Operation::Chmod(c) = op else {
            panic!("expected Chmod");
        };
        match c.permission {
            Some(proto::file_chmod::Permission::Windows(acl)) => {
                assert_eq!(acl.entries.len(), 3);
                assert_eq!(acl.entries[0].entry_type, proto::AclEntryType::Allow as i32);
                assert_eq!(acl.entries[1].entry_type, proto::AclEntryType::Deny as i32);
                // Omitted entry_type defaults to Allow.
                assert_eq!(acl.entries[2].entry_type, proto::AclEntryType::Allow as i32);
            }
            _ => panic!("expected Windows ACL"),
        }
    }

    #[test]
    fn build_chmod_windows_acl_rejects_unknown_entry_type() {
        let err = build_request_operation(
            "chmod",
            json!({
                "path": "/f",
                "windows": {
                    "entries": [
                        { "principal": "X", "access_mask": 1, "entry_type": "maybe" },
                    ],
                },
            }),
        )
        .unwrap_err();
        let DtoError::InvalidParams { op, message } = err else {
            panic!("expected InvalidParams");
        };
        assert_eq!(op, "chmod");
        assert!(message.contains("maybe"), "message={message}");
    }

    #[test]
    fn build_mkdir_maps_all_fields() {
        let op = build_request_operation(
            "mkdir",
            json!({ "path": "/new", "recursive": true, "mode": 0o755 }),
        )
        .unwrap();
        let proto::file_request::Operation::Mkdir(m) = op else {
            panic!("expected Mkdir");
        };
        assert_eq!(m.path, "/new");
        assert!(m.recursive);
        assert_eq!(m.mode, Some(0o755));
    }

    #[test]
    fn build_copy_maps_all_fields() {
        let op = build_request_operation(
            "copy",
            json!({
                "source": "/a",
                "destination": "/b",
                "recursive": true,
                "overwrite": true,
            }),
        )
        .unwrap();
        let proto::file_request::Operation::Copy(c) = op else {
            panic!("expected Copy");
        };
        assert_eq!(c.source, "/a");
        assert_eq!(c.destination, "/b");
        assert!(c.recursive);
        assert!(c.overwrite);
    }

    #[test]
    fn build_move_maps_all_fields() {
        let op = build_request_operation(
            "move",
            json!({ "source": "/s", "destination": "/d", "overwrite": true }),
        )
        .unwrap();
        let proto::file_request::Operation::Move(m) = op else {
            panic!("expected Move");
        };
        assert_eq!(m.source, "/s");
        assert_eq!(m.destination, "/d");
        assert!(m.overwrite);
    }

    #[test]
    fn build_create_symlink_maps_all_fields() {
        let op = build_request_operation(
            "create_symlink",
            json!({ "target": "/real", "link_path": "/link" }),
        )
        .unwrap();
        let proto::file_request::Operation::CreateSymlink(s) = op else {
            panic!("expected CreateSymlink");
        };
        assert_eq!(s.target, "/real");
        assert_eq!(s.link_path, "/link");
    }

    #[test]
    fn build_unknown_operation_returns_error() {
        let err = build_request_operation("xyzzy", json!({})).unwrap_err();
        match err {
            DtoError::UnknownOperation(name) => assert_eq!(name, "xyzzy"),
            _ => panic!("expected UnknownOperation"),
        }
    }

    #[test]
    fn build_missing_required_field_returns_invalid_params() {
        let err = build_request_operation("stat", json!({})).unwrap_err();
        assert!(matches!(err, DtoError::InvalidParams { op, .. } if op == "stat"));
    }

    // ── Response direction: build_response_envelope (all result variants) ──

    fn make_response(result: proto::file_response::Result) -> proto::FileResponse {
        proto::FileResponse {
            request_id: "rid-1".into(),
            result: Some(result),
        }
    }

    #[test]
    fn envelope_empty_response_surfaces_as_error() {
        let env = build_response_envelope(
            proto::FileResponse {
                request_id: "rid".into(),
                result: None,
            },
            "stat",
            5,
        );
        assert!(!env.success);
        assert_eq!(env.operation, "stat");
        let e = env.error.expect("error must be set");
        assert_eq!(e.code, "unspecified");
    }

    #[test]
    fn envelope_daemon_error_maps_all_error_codes() {
        for (proto_code, wire) in [
            (proto::FileErrorCode::Unspecified, "unspecified"),
            (proto::FileErrorCode::NotFound, "not_found"),
            (proto::FileErrorCode::PermissionDenied, "permission_denied"),
            (proto::FileErrorCode::AlreadyExists, "already_exists"),
            (proto::FileErrorCode::NotADirectory, "not_a_directory"),
            (proto::FileErrorCode::IsADirectory, "is_a_directory"),
            (proto::FileErrorCode::NotEmpty, "not_empty"),
            (proto::FileErrorCode::TooLarge, "too_large"),
            (proto::FileErrorCode::InvalidPath, "invalid_path"),
            (proto::FileErrorCode::Io, "io"),
            (proto::FileErrorCode::Encoding, "encoding"),
            (proto::FileErrorCode::MultipleMatches, "multiple_matches"),
            (proto::FileErrorCode::PolicyDenied, "policy_denied"),
        ] {
            let env = build_response_envelope(
                make_response(proto::file_response::Result::Error(proto::FileError {
                    code: proto_code as i32,
                    message: "msg".into(),
                    path: "/p".into(),
                })),
                "stat",
                1,
            );
            assert!(!env.success);
            assert_eq!(
                env.error.as_ref().unwrap().code,
                wire,
                "proto={proto_code:?}"
            );
        }
    }

    #[test]
    fn envelope_stat_result_has_expected_keys_and_file_type() {
        for (ft, wire) in [
            (proto::FileType::File, "file"),
            (proto::FileType::Directory, "directory"),
            (proto::FileType::Symlink, "symlink"),
            (proto::FileType::Other, "other"),
        ] {
            let env = build_response_envelope(
                make_response(proto::file_response::Result::Stat(proto::FileStatResult {
                    path: "/p".into(),
                    file_type: ft as i32,
                    size: 1,
                    modified_ms: 0,
                    created_ms: 0,
                    accessed_ms: 0,
                    unix_permission: Some(proto::UnixPermission {
                        mode: Some(0o644),
                        owner: Some("o".into()),
                        group: Some("g".into()),
                    }),
                    windows_acl: Some(proto::WindowsAcl {
                        entries: vec![proto::AclEntry {
                            principal: "P".into(),
                            access_mask: 1,
                            entry_type: proto::AclEntryType::Allow as i32,
                        }],
                    }),
                    symlink_target: Some("/target".into()),
                })),
                "stat",
                1,
            );
            assert!(env.success);
            let r = env.result.unwrap();
            assert_eq!(r["file_type"], wire, "for {ft:?}");
            assert_eq!(r["unix_permission"]["mode"], 0o644);
            assert_eq!(r["windows_acl"]["entries"][0]["entry_type"], "allow");
            assert_eq!(r["symlink_target"], "/target");
        }
    }

    #[test]
    fn envelope_list_result() {
        let env = build_response_envelope(
            make_response(proto::file_response::Result::List(proto::FileListResult {
                entries: vec![proto::FileEntry {
                    name: "a".into(),
                    file_type: proto::FileType::File as i32,
                    size: 10,
                    modified_ms: 0,
                    symlink_target: None,
                }],
                total_count: 1,
                has_more: true,
            })),
            "list",
            1,
        );
        let r = env.result.unwrap();
        assert_eq!(r["entries"][0]["name"], "a");
        assert_eq!(r["total_count"], 1);
        assert_eq!(r["has_more"], true);
    }

    #[test]
    fn envelope_glob_result() {
        let env = build_response_envelope(
            make_response(proto::file_response::Result::Glob(proto::FileGlobResult {
                entries: vec![],
                total_matches: 0,
                has_more: false,
            })),
            "glob",
            1,
        );
        assert_eq!(env.result.unwrap()["total_matches"], 0);
    }

    #[test]
    fn envelope_read_text_covers_all_stop_reasons() {
        for (sr, wire) in [
            (proto::StopReason::Unspecified, "unspecified"),
            (proto::StopReason::MaxLines, "max_lines"),
            (proto::StopReason::MaxBytes, "max_bytes"),
            (proto::StopReason::TargetEnd, "target_end"),
            (proto::StopReason::FileEnd, "file_end"),
            (proto::StopReason::Error, "error"),
        ] {
            let env = build_response_envelope(
                make_response(proto::file_response::Result::ReadText(
                    proto::FileReadTextResult {
                        lines: vec![proto::TextLine {
                            content: "x".into(),
                            line_number: 1,
                            truncated: false,
                            remaining_bytes: 0,
                        }],
                        stop_reason: sr as i32,
                        start_pos: Some(proto::PositionInfo {
                            line: 1,
                            byte_in_file: 0,
                            byte_in_line: 0,
                        }),
                        end_pos: Some(proto::PositionInfo {
                            line: 1,
                            byte_in_file: 1,
                            byte_in_line: 1,
                        }),
                        remaining_bytes: 0,
                        total_file_bytes: 1,
                        total_lines: 1,
                        detected_encoding: "utf-8".into(),
                    },
                )),
                "read_text",
                1,
            );
            assert_eq!(env.result.unwrap()["stop_reason"], wire, "for {sr:?}");
        }
    }

    #[test]
    fn envelope_read_binary_result_encodes_content_to_base64() {
        use base64::Engine as _;
        let env = build_response_envelope(
            make_response(proto::file_response::Result::ReadBinary(
                proto::FileReadBinaryResult {
                    content: b"\x00\x01\x02".to_vec(),
                    byte_offset: 0,
                    bytes_read: 3,
                    total_file_bytes: 3,
                    remaining_bytes: 0,
                    download_url: Some("https://s3/...".into()),
                    download_url_expires_ms: Some(1000),
                },
            )),
            "read_binary",
            1,
        );
        let r = env.result.unwrap();
        let b64 = r["content_b64"].as_str().unwrap();
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .unwrap();
        assert_eq!(decoded, b"\x00\x01\x02");
        assert_eq!(r["bytes_read"], 3);
        assert_eq!(r["download_url"], "https://s3/...");
    }

    #[test]
    fn envelope_read_image_result_covers_all_image_formats() {
        use base64::Engine as _;
        for (fmt, wire) in [
            (proto::ImageFormat::Original, "original"),
            (proto::ImageFormat::Jpeg, "jpeg"),
            (proto::ImageFormat::Png, "png"),
            (proto::ImageFormat::Webp, "webp"),
        ] {
            let env = build_response_envelope(
                make_response(proto::file_response::Result::ReadImage(
                    proto::FileReadImageResult {
                        content: b"abc".to_vec(),
                        format: fmt as i32,
                        width: 10,
                        height: 20,
                        original_bytes: 100,
                        output_bytes: 50,
                        download_url: None,
                        download_url_expires_ms: None,
                    },
                )),
                "read_image",
                1,
            );
            let r = env.result.unwrap();
            assert_eq!(r["format"], wire, "for {fmt:?}");
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(r["content_b64"].as_str().unwrap())
                .unwrap();
            assert_eq!(decoded, b"abc");
        }
    }

    #[test]
    fn envelope_write_result_covers_all_actions() {
        for (action, wire) in [
            (proto::WriteAction::Created, "created"),
            (proto::WriteAction::Overwritten, "overwritten"),
            (proto::WriteAction::Appended, "appended"),
            (proto::WriteAction::Edited, "edited"),
        ] {
            let env = build_response_envelope(
                make_response(proto::file_response::Result::Write(
                    proto::FileWriteResult {
                        path: "/o".into(),
                        action: action as i32,
                        bytes_written: 10,
                        final_size: 10,
                        replacements_made: Some(1),
                    },
                )),
                "write",
                1,
            );
            assert_eq!(env.result.unwrap()["action"], wire, "for {action:?}");
        }
    }

    #[test]
    fn envelope_edit_result() {
        let env = build_response_envelope(
            make_response(proto::file_response::Result::Edit(proto::FileEditResult {
                path: "/e".into(),
                final_size: 99,
                replacements_made: Some(3),
                match_error: Some("no match".into()),
            })),
            "edit",
            1,
        );
        let r = env.result.unwrap();
        assert_eq!(r["final_size"], 99);
        assert_eq!(r["replacements_made"], 3);
        assert_eq!(r["match_error"], "no match");
    }

    #[test]
    fn envelope_delete_result_covers_both_modes() {
        for (mode, wire) in [
            (proto::DeleteMode::Trash, "trash"),
            (proto::DeleteMode::Permanent, "permanent"),
        ] {
            let env = build_response_envelope(
                make_response(proto::file_response::Result::Delete(
                    proto::FileDeleteResult {
                        path: "/d".into(),
                        mode: mode as i32,
                        items_deleted: 2,
                        trash_path: Some("/.trash/d".into()),
                    },
                )),
                "delete",
                1,
            );
            assert_eq!(env.result.unwrap()["mode"], wire, "for {mode:?}");
        }
    }

    #[test]
    fn envelope_chmod_result() {
        let env = build_response_envelope(
            make_response(proto::file_response::Result::Chmod(
                proto::FileChmodResult {
                    path: "/f".into(),
                    items_modified: 5,
                },
            )),
            "chmod",
            1,
        );
        let r = env.result.unwrap();
        assert_eq!(r["items_modified"], 5);
    }

    #[test]
    fn envelope_mkdir_result() {
        let env = build_response_envelope(
            make_response(proto::file_response::Result::Mkdir(
                proto::FileMkdirResult {
                    path: "/new".into(),
                    already_existed: true,
                },
            )),
            "mkdir",
            1,
        );
        assert_eq!(env.result.unwrap()["already_existed"], true);
    }

    #[test]
    fn envelope_copy_result() {
        let env = build_response_envelope(
            make_response(proto::file_response::Result::Copy(proto::FileCopyResult {
                source: "/a".into(),
                destination: "/b".into(),
                items_copied: 7,
            })),
            "copy",
            1,
        );
        assert_eq!(env.result.unwrap()["items_copied"], 7);
    }

    #[test]
    fn envelope_move_result() {
        let env = build_response_envelope(
            make_response(proto::file_response::Result::MoveResult(
                proto::FileMoveResult {
                    source: "/s".into(),
                    destination: "/d".into(),
                },
            )),
            "move",
            1,
        );
        let r = env.result.unwrap();
        assert_eq!(r["source"], "/s");
        assert_eq!(r["destination"], "/d");
    }

    #[test]
    fn envelope_create_symlink_result() {
        let env = build_response_envelope(
            make_response(proto::file_response::Result::CreateSymlink(
                proto::FileCreateSymlinkResult {
                    link_path: "/l".into(),
                    target: "/t".into(),
                },
            )),
            "create_symlink",
            1,
        );
        let r = env.result.unwrap();
        assert_eq!(r["link_path"], "/l");
        assert_eq!(r["target"], "/t");
    }
}
