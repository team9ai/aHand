//! Text file reading with triple-limit pagination.
//!
//! Implements the `FileReadText` operation: reads lines from an arbitrary start
//! position (line number, byte offset, or line+col), stops when the first of
//! `max_lines`, `max_bytes`, or `target_end` is reached, and reports precise
//! positional metadata plus per-line truncation.
//!
//! Encoding handling:
//! - UTF-8 input is the fast path (no decode needed).
//! - Non-UTF-8: uses `chardetng` for detection, `encoding_rs` for decoding,
//!   and normalises all output to UTF-8 strings.

use std::path::Path;

use ahand_protocol::{
    file_position, FileError, FileErrorCode, FilePosition, FileReadText, FileReadTextResult,
    PositionInfo, StopReason, TextLine,
};

use super::file_error;
use super::fs_ops::io_to_file_error;

const DEFAULT_MAX_LINES: u32 = 200;
const DEFAULT_MAX_BYTES: u64 = 64 * 1024;
const DEFAULT_MAX_LINE_WIDTH: u32 = 500;

/// Handle a FileReadText request.
pub async fn handle_read_text(
    req: &FileReadText,
    resolved: &Path,
) -> Result<FileReadTextResult, FileError> {
    // Check file existence & get size.
    let metadata = if req.no_follow_symlink {
        tokio::fs::symlink_metadata(resolved).await
    } else {
        tokio::fs::metadata(resolved).await
    }
    .map_err(|e| io_to_file_error(e, resolved))?;

    if !metadata.is_file() {
        return Err(file_error(
            FileErrorCode::IsADirectory,
            &req.path,
            "path is not a regular file",
        ));
    }

    let total_file_bytes = metadata.len();

    // Read the whole file (bounded by max_bytes anyway, and large binary files
    // would not be text). We use async read to stay inside tokio.
    let raw = tokio::fs::read(resolved)
        .await
        .map_err(|e| io_to_file_error(e, resolved))?;

    // Decode to UTF-8 string.
    let (decoded, detected_encoding) = decode_bytes(&raw, req.encoding.as_deref())?;

    // Pre-compute line start byte offsets (into the DECODED buffer).
    let line_offsets = compute_line_offsets(decoded.as_bytes());
    let total_lines = line_offsets.len() as u64;

    // Resolve the start byte offset.
    let (start_byte, start_line_idx) = resolve_start(&req.start, &line_offsets, &decoded)?;

    // Resolve max_lines / max_bytes / target_end / max_line_width.
    let max_lines = req.max_lines.unwrap_or(DEFAULT_MAX_LINES) as usize;
    let max_bytes = req.max_bytes.unwrap_or(DEFAULT_MAX_BYTES);
    let max_line_width = req.max_line_width.unwrap_or(DEFAULT_MAX_LINE_WIDTH);
    let target_end_byte = req
        .target_end
        .as_ref()
        .and_then(|t| resolve_position(t, &line_offsets, &decoded).ok());

    // Start position info (byte offsets into decoded buffer).
    let start_line_byte = line_offsets
        .get(start_line_idx)
        .copied()
        .unwrap_or(decoded.len());
    let start_pos = PositionInfo {
        line: start_line_idx as u64 + 1,
        byte_in_file: start_byte as u64,
        byte_in_line: (start_byte - start_line_byte) as u64,
    };

    // Iterate lines starting at start_line_idx.
    let mut lines: Vec<TextLine> = Vec::new();
    let mut bytes_accumulated: u64 = 0;
    let mut stop_reason = StopReason::FileEnd;
    let mut end_byte = start_byte;
    let mut end_line_idx = start_line_idx;

    let bytes = decoded.as_bytes();

    for idx in start_line_idx..line_offsets.len() {
        if lines.len() >= max_lines {
            stop_reason = StopReason::MaxLines;
            break;
        }

        let line_start = line_offsets[idx];
        let line_end = line_offsets
            .get(idx + 1)
            .copied()
            .unwrap_or(bytes.len());
        // On the very first iteration we may start partway into the line.
        let effective_start = if idx == start_line_idx {
            start_byte.max(line_start)
        } else {
            line_start
        };

        // Check max_bytes BEFORE consuming the line.
        let line_len = (line_end - effective_start) as u64;
        if bytes_accumulated + line_len > max_bytes && !lines.is_empty() {
            stop_reason = StopReason::MaxBytes;
            break;
        }

        // Check target_end: if this line's start byte is past the target, stop.
        if let Some(target) = target_end_byte {
            if effective_start >= target {
                stop_reason = StopReason::TargetEnd;
                break;
            }
        }

        // Read the line content (strip trailing newline if present).
        let mut content_bytes = &bytes[effective_start..line_end];
        if content_bytes.ends_with(b"\n") {
            content_bytes = &content_bytes[..content_bytes.len() - 1];
            if content_bytes.ends_with(b"\r") {
                content_bytes = &content_bytes[..content_bytes.len() - 1];
            }
        }

        // Apply per-line truncation.
        let (content, truncated, remaining_bytes) =
            truncate_line(content_bytes, max_line_width);

        // If the line body alone exceeds the remaining byte budget we must stop
        // after writing this line (or even before it for oversized lines).
        let line_byte_count = content_bytes.len() as u64;
        if bytes_accumulated + line_byte_count > max_bytes && lines.is_empty() {
            // On the very first line, we still honour max_bytes by truncating.
            let allowed = max_bytes.saturating_sub(bytes_accumulated) as usize;
            let safe = safe_utf8_prefix(content_bytes, allowed);
            let content = String::from_utf8_lossy(safe).into_owned();
            lines.push(TextLine {
                content,
                line_number: idx as u64 + 1,
                truncated: true,
                remaining_bytes: (content_bytes.len() - safe.len()) as u32,
            });
            bytes_accumulated += safe.len() as u64;
            end_byte = effective_start + safe.len();
            end_line_idx = idx;
            stop_reason = StopReason::MaxBytes;
            break;
        }

        bytes_accumulated += line_len;
        end_byte = line_end;
        end_line_idx = idx;

        lines.push(TextLine {
            content,
            line_number: idx as u64 + 1,
            truncated,
            remaining_bytes,
        });
    }

    // If we consumed through the last line, stop_reason stays FileEnd. If we
    // hit max_lines check above with idx < len, stop_reason is already set.
    if lines.len() >= max_lines && stop_reason == StopReason::FileEnd {
        stop_reason = StopReason::MaxLines;
    }

    // end position info
    let end_line_start = line_offsets
        .get(end_line_idx)
        .copied()
        .unwrap_or(decoded.len());
    let end_pos = PositionInfo {
        line: end_line_idx as u64 + 1,
        byte_in_file: end_byte as u64,
        byte_in_line: end_byte.saturating_sub(end_line_start) as u64,
    };

    let remaining_bytes = (decoded.len() as u64).saturating_sub(end_byte as u64);

    Ok(FileReadTextResult {
        lines,
        stop_reason: stop_reason as i32,
        start_pos: Some(start_pos),
        end_pos: Some(end_pos),
        remaining_bytes,
        total_file_bytes,
        total_lines,
        detected_encoding,
    })
}

// ── Helpers ────────────────────────────────────────────────────────────────

/// Decode raw bytes to a UTF-8 `String`, honoring an explicit encoding
/// parameter if provided, otherwise auto-detecting via BOM / chardetng.
fn decode_bytes(
    raw: &[u8],
    encoding_hint: Option<&str>,
) -> Result<(String, String), FileError> {
    if let Some(name) = encoding_hint {
        if name.is_empty() || name.eq_ignore_ascii_case("utf-8") || name.eq_ignore_ascii_case("utf8") {
            return Ok((
                String::from_utf8_lossy(raw).into_owned(),
                "UTF-8".to_string(),
            ));
        }
        let encoding = encoding_rs::Encoding::for_label(name.as_bytes()).ok_or_else(|| {
            file_error(
                FileErrorCode::Encoding,
                "",
                format!("unknown encoding: {name}"),
            )
        })?;
        let (decoded, _, _) = encoding.decode(raw);
        return Ok((decoded.into_owned(), encoding.name().to_string()));
    }

    // Auto-detect. Fast path: UTF-8 validation first — most files are UTF-8.
    if std::str::from_utf8(raw).is_ok() {
        return Ok((
            String::from_utf8_lossy(raw).into_owned(),
            "UTF-8".to_string(),
        ));
    }

    // Non-UTF-8: use chardetng.
    let mut detector = chardetng::EncodingDetector::new();
    detector.feed(raw, true);
    let encoding = detector.guess(None, true);
    let (decoded, _, _) = encoding.decode(raw);
    Ok((decoded.into_owned(), encoding.name().to_string()))
}

/// Compute the byte offset of each line start in the buffer.
///
/// Line 0 starts at byte 0. Line N+1 starts at the byte immediately after a
/// `\n` in the buffer. A final line with no trailing newline is still counted.
/// An empty buffer has zero lines (empty vector).
fn compute_line_offsets(bytes: &[u8]) -> Vec<usize> {
    if bytes.is_empty() {
        return Vec::new();
    }
    let mut offsets = vec![0usize];
    for (i, b) in bytes.iter().enumerate() {
        if *b == b'\n' && i + 1 < bytes.len() {
            offsets.push(i + 1);
        }
    }
    offsets
}

/// Resolve the request's start oneof to a `(byte_offset, line_index)` pair in
/// the decoded buffer. Returns `line_index = offsets.len()` when the start is
/// past EOF.
fn resolve_start(
    start: &Option<file_read_text_mod::Start>,
    offsets: &[usize],
    decoded: &str,
) -> Result<(usize, usize), FileError> {
    let Some(start) = start else {
        return Ok((0, 0));
    };
    match start {
        file_read_text_mod::Start::StartLine(line) => {
            // 1-based; clamp to available lines.
            let idx = (line.saturating_sub(1) as usize).min(offsets.len());
            let byte = offsets.get(idx).copied().unwrap_or(decoded.len());
            Ok((byte, idx))
        }
        file_read_text_mod::Start::StartByte(byte) => {
            let byte = (*byte as usize).min(decoded.len());
            let idx = line_index_for_byte(offsets, byte);
            Ok((byte, idx))
        }
        file_read_text_mod::Start::StartLineCol(lc) => {
            let line_idx = (lc.line.saturating_sub(1) as usize).min(offsets.len());
            let line_start = offsets.get(line_idx).copied().unwrap_or(decoded.len());
            let byte = (line_start + lc.col as usize).min(decoded.len());
            Ok((byte, line_idx))
        }
    }
}

/// Resolve a FilePosition target to a byte offset in the decoded buffer.
fn resolve_position(
    pos: &FilePosition,
    offsets: &[usize],
    decoded: &str,
) -> Result<usize, FileError> {
    match &pos.position {
        Some(file_position::Position::Line(line)) => {
            let idx = (line.saturating_sub(1) as usize).min(offsets.len());
            Ok(offsets.get(idx).copied().unwrap_or(decoded.len()))
        }
        Some(file_position::Position::ByteOffset(b)) => {
            Ok((*b as usize).min(decoded.len()))
        }
        Some(file_position::Position::LineCol(lc)) => {
            let line_idx = (lc.line.saturating_sub(1) as usize).min(offsets.len());
            let line_start = offsets.get(line_idx).copied().unwrap_or(decoded.len());
            Ok((line_start + lc.col as usize).min(decoded.len()))
        }
        None => Ok(decoded.len()),
    }
}

fn line_index_for_byte(offsets: &[usize], byte: usize) -> usize {
    // Binary search, returning the largest line_start <= byte.
    match offsets.binary_search(&byte) {
        Ok(idx) => idx,
        Err(idx) => idx.saturating_sub(1),
    }
}

/// Truncate a line to `max_line_width` bytes (UTF-8 safe). Returns
/// `(content, truncated, remaining_bytes)`.
fn truncate_line(line: &[u8], max_width: u32) -> (String, bool, u32) {
    if max_width == 0 || line.len() <= max_width as usize {
        return (String::from_utf8_lossy(line).into_owned(), false, 0);
    }
    let cut = safe_utf8_prefix(line, max_width as usize);
    let remaining = (line.len() - cut.len()) as u32;
    (
        String::from_utf8_lossy(cut).into_owned(),
        true,
        remaining,
    )
}

/// Truncate a UTF-8 byte slice at `max_bytes`, but back up to a UTF-8 char
/// boundary so we never split a multi-byte code point.
fn safe_utf8_prefix(bytes: &[u8], max_bytes: usize) -> &[u8] {
    if bytes.len() <= max_bytes {
        return bytes;
    }
    let mut end = max_bytes;
    // Back up to a UTF-8 char boundary. Valid UTF-8 start bytes satisfy
    // (byte & 0xC0) != 0x80.
    while end > 0 && (bytes[end] & 0xC0) == 0x80 {
        end -= 1;
    }
    &bytes[..end]
}

// Shortcut so function signatures above can use the generated oneof path
// without fully qualifying it everywhere.
use ahand_protocol::file_read_text as file_read_text_mod;
