//! Text file reading with triple-limit pagination.
//!
//! Implements the `FileReadText` operation: reads lines from an arbitrary
//! start position (line number, byte offset, or line+col), stops when the
//! first of `max_lines`, `max_bytes`, or `target_end` is reached, and reports
//! precise positional metadata plus per-line truncation.
//!
//! Offset / position policy (Round 1 fixes T7 + T8):
//!
//! - **All positions reported to the caller are in raw on-disk bytes.**
//!   `PositionInfo.byte_in_file`, `byte_in_line`, `remaining_bytes`, and the
//!   resolution of `start_byte` / `target_end.byte_offset` are computed on
//!   the raw file buffer before decoding. This preserves the proto contract
//!   for non-UTF-8 inputs where the decoded UTF-8 length differs from the
//!   on-disk byte length.
//! - **Pagination stops exactly at the byte limit, not the next line
//!   boundary.** `max_bytes` and `target_end` can now stop partway through
//!   the current line; the emitted `TextLine.content` is truncated to match
//!   and `remaining_bytes` on that line reports the bytes not consumed.
//! - **Line-start indexing is on raw bytes**, with one well-tested caveat:
//!   we only look at `b'\n'` bytes which are guaranteed to be single-byte
//!   in every encoding `encoding_rs` supports, so the line offsets are
//!   valid regardless of encoding.
//! - **Line bodies are decoded per-line** using `encoding_rs`, then
//!   truncated in UTF-8-safe chunks for the output string.

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
    max_read_bytes: u64,
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

    // Enforce the policy-level max_read_bytes budget before loading the file.
    if total_file_bytes > max_read_bytes {
        return Err(file_error(
            FileErrorCode::TooLarge,
            &req.path,
            format!(
                "file size {} exceeds max_read_bytes ({})",
                total_file_bytes, max_read_bytes
            ),
        ));
    }

    let raw = tokio::fs::read(resolved)
        .await
        .map_err(|e| io_to_file_error(e, resolved))?;

    // Determine which encoding we'll use for decoding individual line slices.
    let encoding = resolve_encoding(&raw, req.encoding.as_deref(), &req.path)?;
    let detected_encoding = encoding.name().to_string();

    // Line offsets computed on RAW bytes. `\n` (0x0A) is a single-byte in
    // every encoding_rs-supported charset so this is safe.
    let line_offsets = compute_line_offsets_raw(&raw);
    let total_lines = line_offsets.len() as u64;

    let (start_byte, start_line_idx) = resolve_start_raw(&req.start, &line_offsets, raw.len())?;
    let max_lines = req.max_lines.unwrap_or(DEFAULT_MAX_LINES) as usize;
    // Clamp against the policy-level read budget so callers can never
    // bypass `max_read_bytes` by requesting a larger per-call max_bytes.
    let max_bytes = req.max_bytes.unwrap_or(DEFAULT_MAX_BYTES).min(max_read_bytes);
    let max_line_width = req.max_line_width.unwrap_or(DEFAULT_MAX_LINE_WIDTH);
    let target_end_byte = req
        .target_end
        .as_ref()
        .and_then(|t| resolve_position_raw(t, &line_offsets, raw.len()).ok());

    // Start position info — all in RAW on-disk bytes.
    let start_line_byte_raw = line_offsets
        .get(start_line_idx)
        .copied()
        .unwrap_or(raw.len());
    let start_pos = PositionInfo {
        line: start_line_idx as u64 + 1,
        byte_in_file: start_byte as u64,
        byte_in_line: (start_byte - start_line_byte_raw) as u64,
    };

    // Iterate lines starting at start_line_idx.
    let mut lines: Vec<TextLine> = Vec::new();
    let mut bytes_accumulated: u64 = 0;
    let mut stop_reason = StopReason::FileEnd;
    let mut end_byte = start_byte;
    let mut end_line_idx = start_line_idx;

    for idx in start_line_idx..line_offsets.len() {
        // max_bytes budget check comes first so `max_bytes=0` returns zero
        // lines with stop_reason=MaxBytes (rather than an empty TextLine).
        if max_bytes == 0 || bytes_accumulated >= max_bytes {
            stop_reason = StopReason::MaxBytes;
            break;
        }
        if lines.len() >= max_lines {
            stop_reason = StopReason::MaxLines;
            break;
        }

        let line_start = line_offsets[idx];
        let line_end = line_offsets.get(idx + 1).copied().unwrap_or(raw.len());
        // On the very first iteration we may start partway into the line.
        let effective_start = if idx == start_line_idx {
            start_byte.max(line_start)
        } else {
            line_start
        };

        // Compute a per-line stop offset based on max_bytes and target_end.
        // `max_bytes` is a global budget across all lines; we stop exactly
        // at the byte where the budget runs out, even if that's mid-line.
        // `target_end` is inclusive up to but not including the target byte.
        let line_bytes_remaining = (line_end - effective_start) as u64;
        let budget_remaining = max_bytes.saturating_sub(bytes_accumulated);
        let max_bytes_cut = effective_start + budget_remaining.min(line_bytes_remaining) as usize;
        let target_end_cut = target_end_byte
            .map(|t| t.max(effective_start).min(line_end))
            .unwrap_or(line_end);
        // `consume_end_raw` is how far we advance in the raw buffer for
        // this line — the earliest of line_end / max_bytes_cut /
        // target_end_cut.
        let consume_end_raw = max_bytes_cut.min(target_end_cut);

        // Did we stop before consuming the whole line?
        let cut_short_by_max_bytes = max_bytes_cut < line_end;
        let cut_short_by_target = target_end_cut < line_end;
        let cut_short = cut_short_by_max_bytes || cut_short_by_target;

        // If the target lands exactly on this line's start, stop WITHOUT
        // emitting an empty `TextLine`. That's what the existing
        // `read_text_respects_target_end_line` test pins as "exclusive"
        // semantics: target_end=line_N returns lines 1..N-1.
        if consume_end_raw == effective_start && cut_short_by_target {
            stop_reason = StopReason::TargetEnd;
            break;
        }

        // `display_end_raw` is how many raw bytes we will decode into the
        // output `content` string. When we consume the entire line and the
        // file has a trailing newline (LF or CRLF), strip it off the
        // displayed content but keep it counted in the consumed range so
        // `end_byte` / `bytes_accumulated` correctly advance past it.
        let mut display_end_raw = consume_end_raw;
        // Only strip a trailing newline when there's at least one byte in
        // the consumed range. Guarding on `display_end_raw > effective_start`
        // avoids an underflow/reversed slice when the effective start has
        // been clamped forward (e.g. a LineCol.col that landed exactly on
        // the start-of-next-line boundary).
        if !cut_short && display_end_raw > effective_start {
            if raw.get(display_end_raw - 1) == Some(&b'\n') {
                display_end_raw -= 1;
                if display_end_raw > effective_start
                    && raw.get(display_end_raw - 1) == Some(&b'\r')
                {
                    display_end_raw -= 1;
                }
            }
        }

        // Apply per-line truncation (max_line_width, measured in raw bytes).
        // `truncate_line` cuts the raw slice first, then decodes, so
        // `remaining_bytes` is always counted in raw on-disk bytes — a must
        // for non-UTF-8 encodings (GBK, Shift-JIS, etc.) where the decoded
        // UTF-8 byte length differs from the raw byte length. The trailing
        // newline, if any, is already excluded by `display_end_raw`, so
        // `remaining_bytes` never includes it.
        let raw_line = &raw[effective_start..display_end_raw];
        let (content, truncated, remaining_bytes) =
            truncate_line(raw_line, encoding, max_line_width);

        lines.push(TextLine {
            content,
            line_number: idx as u64 + 1,
            truncated,
            remaining_bytes,
        });

        // Advance accumulators in RAW on-disk bytes.
        let consumed_raw = (consume_end_raw - effective_start) as u64;
        bytes_accumulated += consumed_raw;
        end_byte = consume_end_raw;
        end_line_idx = idx;

        // Triple-limit stop checks.
        if cut_short_by_target {
            stop_reason = StopReason::TargetEnd;
            break;
        }
        if cut_short_by_max_bytes {
            stop_reason = StopReason::MaxBytes;
            break;
        }
    }

    // If we consumed through the last line without hitting a mid-line stop,
    // and we still hit max_lines, reflect that.
    if lines.len() >= max_lines && stop_reason == StopReason::FileEnd {
        stop_reason = StopReason::MaxLines;
    }

    // End position info — also in RAW bytes.
    let end_line_start_raw = line_offsets
        .get(end_line_idx)
        .copied()
        .unwrap_or(raw.len());
    let end_pos = PositionInfo {
        line: end_line_idx as u64 + 1,
        byte_in_file: end_byte as u64,
        byte_in_line: end_byte.saturating_sub(end_line_start_raw) as u64,
    };

    let remaining_bytes = (raw.len() as u64).saturating_sub(end_byte as u64);

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

/// Pick the `encoding_rs::Encoding` we should use to decode slices from
/// `raw`, honouring an explicit user hint first and falling back to BOM +
/// chardetng auto-detection.
fn resolve_encoding(
    raw: &[u8],
    encoding_hint: Option<&str>,
    req_path: &str,
) -> Result<&'static encoding_rs::Encoding, FileError> {
    if let Some(name) = encoding_hint {
        if !name.is_empty() {
            if name.eq_ignore_ascii_case("utf-8") || name.eq_ignore_ascii_case("utf8") {
                return Ok(encoding_rs::UTF_8);
            }
            return encoding_rs::Encoding::for_label(name.as_bytes()).ok_or_else(|| {
                file_error(
                    FileErrorCode::Encoding,
                    req_path,
                    format!("unknown encoding: {name}"),
                )
            });
        }
        // Empty string → fall through to auto-detect path below.
    }

    // Auto-detect. UTF-8 validation fast path.
    if std::str::from_utf8(raw).is_ok() {
        return Ok(encoding_rs::UTF_8);
    }

    let mut detector = chardetng::EncodingDetector::new();
    detector.feed(raw, true);
    Ok(detector.guess(None, true))
}

/// Compute the byte offset of each line start in the RAW buffer.
///
/// Line 0 starts at byte 0. Line N+1 starts at the byte immediately after a
/// `\n` in the buffer. A final line with no trailing newline is still
/// counted. An empty buffer has zero lines (empty vector).
fn compute_line_offsets_raw(bytes: &[u8]) -> Vec<usize> {
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

/// Resolve the request's start oneof to a `(byte_offset, line_index)` pair
/// in the RAW buffer. Returns `line_index = offsets.len()` when the start
/// is past EOF. All values are measured in raw on-disk bytes.
fn resolve_start_raw(
    start: &Option<file_read_text_mod::Start>,
    offsets: &[usize],
    raw_len: usize,
) -> Result<(usize, usize), FileError> {
    let Some(start) = start else {
        return Ok((0, 0));
    };
    match start {
        file_read_text_mod::Start::StartLine(line) => {
            let idx = (line.saturating_sub(1) as usize).min(offsets.len());
            let byte = offsets.get(idx).copied().unwrap_or(raw_len);
            Ok((byte, idx))
        }
        file_read_text_mod::Start::StartByte(byte) => {
            let byte = (*byte as usize).min(raw_len);
            let idx = line_index_for_byte(offsets, byte);
            Ok((byte, idx))
        }
        file_read_text_mod::Start::StartLineCol(lc) => {
            let line_idx = (lc.line.saturating_sub(1) as usize).min(offsets.len());
            let line_start = offsets.get(line_idx).copied().unwrap_or(raw_len);
            // Clamp `col` against the end of the current line (the start of
            // the next line, or `raw_len` for the last line) so an oversized
            // `col` never spills into the next line. Worst case lands on the
            // newline character, a valid "end of line" position.
            let next_start = offsets.get(line_idx + 1).copied().unwrap_or(raw_len);
            let byte = (line_start + lc.col as usize).min(next_start);
            Ok((byte, line_idx))
        }
    }
}

/// Resolve a FilePosition target to a raw byte offset.
fn resolve_position_raw(
    pos: &FilePosition,
    offsets: &[usize],
    raw_len: usize,
) -> Result<usize, FileError> {
    match &pos.position {
        Some(file_position::Position::Line(line)) => {
            let idx = (line.saturating_sub(1) as usize).min(offsets.len());
            Ok(offsets.get(idx).copied().unwrap_or(raw_len))
        }
        Some(file_position::Position::ByteOffset(b)) => Ok((*b as usize).min(raw_len)),
        Some(file_position::Position::LineCol(lc)) => {
            let line_idx = (lc.line.saturating_sub(1) as usize).min(offsets.len());
            let line_start = offsets.get(line_idx).copied().unwrap_or(raw_len);
            // Clamp `col` against the end of the current line so oversized
            // `col` values never spill into the next line.
            let next_start = offsets.get(line_idx + 1).copied().unwrap_or(raw_len);
            Ok((line_start + lc.col as usize).min(next_start))
        }
        None => Ok(raw_len),
    }
}

fn line_index_for_byte(offsets: &[usize], byte: usize) -> usize {
    match offsets.binary_search(&byte) {
        Ok(idx) => idx,
        Err(idx) => idx.saturating_sub(1),
    }
}

/// Truncate a raw line slice to `max_line_width` raw on-disk bytes, then
/// decode it via `encoding`. Returns `(content, truncated, remaining_bytes)`
/// where `remaining_bytes` is in **raw** on-disk bytes (not UTF-8 bytes).
///
/// This must operate on the raw slice before decoding so the remaining-byte
/// accounting stays consistent for non-UTF-8 encodings (GBK, Shift-JIS, …)
/// where the decoded UTF-8 length differs from the raw length. For UTF-8 we
/// back the cut up to a char boundary so multi-byte code points are never
/// split. For non-UTF-8 encodings we cut at the raw byte boundary directly;
/// in the worst case the encoder's `decode` call replaces a dangling trail
/// byte with U+FFFD, which is acceptable — doing better would require
/// per-encoding state tracking that `encoding_rs` doesn't expose here.
fn truncate_line(
    raw_line: &[u8],
    encoding: &'static encoding_rs::Encoding,
    max_width: u32,
) -> (String, bool, u32) {
    if max_width == 0 || raw_line.len() <= max_width as usize {
        let (decoded, _, _) = encoding.decode(raw_line);
        return (decoded.into_owned(), false, 0);
    }
    let mut cut = max_width as usize;
    if encoding == encoding_rs::UTF_8 {
        // Back up to a UTF-8 char boundary so we never split a multi-byte
        // code point. The continuation-byte test `(b & 0xC0) == 0x80`
        // identifies 10xxxxxx bytes.
        while cut > 0 && (raw_line[cut] & 0xC0) == 0x80 {
            cut -= 1;
        }
    }
    let (decoded, _, _) = encoding.decode(&raw_line[..cut]);
    let remaining = (raw_line.len() - cut) as u32;
    (decoded.into_owned(), true, remaining)
}

use ahand_protocol::file_read_text as file_read_text_mod;
