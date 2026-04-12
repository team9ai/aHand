//! Write and edit operations.
//!
//! FileWrite supports: full write (from inline bytes or S3 key placeholder),
//! append, string-replace, line-range-replace, and byte-range-replace.
//! FileEdit is the same as FileWrite *minus* full-write/append and requires
//! the target file to already exist.

use std::path::Path;

use ahand_protocol::{
    file_edit, file_write, full_write, ByteRangeReplace, FileAppend, FileEdit, FileEditResult,
    FileError, FileErrorCode, FileWrite, FileWriteResult, FullWrite, LineRangeReplace,
    StringReplace, WriteAction,
};

use super::file_error;
use super::fs_ops::io_to_file_error;

pub async fn handle_write(
    req: &FileWrite,
    resolved: &Path,
    max_write_bytes: u64,
) -> Result<FileWriteResult, FileError> {
    ensure_encoding_supported(req.encoding.as_deref(), &req.path)?;

    let Some(method) = &req.method else {
        return Err(file_error(
            FileErrorCode::Unspecified,
            &req.path,
            "no write method specified",
        ));
    };

    // create_parents applies only to methods that produce a file at `path`.
    if req.create_parents {
        if let Some(parent) = resolved.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| io_to_file_error(e, parent))?;
        }
    }

    match method {
        file_write::Method::FullWrite(fw) => {
            handle_full_write(&req.path, resolved, fw, max_write_bytes).await
        }
        file_write::Method::Append(app) => {
            handle_append(&req.path, resolved, app, max_write_bytes).await
        }
        file_write::Method::StringReplace(sr) => {
            handle_string_replace_write(&req.path, resolved, sr, max_write_bytes).await
        }
        file_write::Method::LineRangeReplace(lr) => {
            handle_line_range_replace_write(&req.path, resolved, lr, max_write_bytes).await
        }
        file_write::Method::ByteRangeReplace(br) => {
            handle_byte_range_replace_write(&req.path, resolved, br, max_write_bytes).await
        }
    }
}

pub async fn handle_edit(
    req: &FileEdit,
    resolved: &Path,
    max_write_bytes: u64,
) -> Result<FileEditResult, FileError> {
    ensure_encoding_supported(req.encoding.as_deref(), &req.path)?;

    // Require existing file for edit.
    if !tokio::fs::try_exists(resolved).await.unwrap_or(false) {
        return Err(file_error(FileErrorCode::NotFound, &req.path, "file not found"));
    }
    let metadata = tokio::fs::metadata(resolved)
        .await
        .map_err(|e| io_to_file_error(e, resolved))?;
    if metadata.is_dir() {
        return Err(file_error(
            FileErrorCode::IsADirectory,
            &req.path,
            "target is a directory",
        ));
    }

    let Some(method) = &req.method else {
        return Err(file_error(
            FileErrorCode::Unspecified,
            &req.path,
            "no edit method specified",
        ));
    };

    match method {
        file_edit::Method::StringReplace(sr) => {
            handle_string_replace_edit(&req.path, resolved, sr, max_write_bytes).await
        }
        file_edit::Method::LineRangeReplace(lr) => {
            handle_line_range_replace_edit(&req.path, resolved, lr, max_write_bytes).await
        }
        file_edit::Method::ByteRangeReplace(br) => {
            handle_byte_range_replace_edit(&req.path, resolved, br, max_write_bytes).await
        }
    }
}

// ── Write methods ──────────────────────────────────────────────────────────

async fn handle_full_write(
    req_path: &str,
    resolved: &Path,
    fw: &FullWrite,
    max_write_bytes: u64,
) -> Result<FileWriteResult, FileError> {
    let bytes: Vec<u8> = match &fw.source {
        Some(full_write::Source::Content(c)) => c.clone(),
        Some(full_write::Source::S3ObjectKey(_)) => {
            return Err(file_error(
                FileErrorCode::Unspecified,
                req_path,
                "S3 large-file uploads are not wired yet",
            ));
        }
        None => {
            return Err(file_error(
                FileErrorCode::Unspecified,
                req_path,
                "full_write has no source",
            ));
        }
    };

    enforce_size_limit(bytes.len() as u64, max_write_bytes, req_path)?;

    let existed = tokio::fs::try_exists(resolved).await.unwrap_or(false);
    tokio::fs::write(resolved, &bytes)
        .await
        .map_err(|e| io_to_file_error(e, resolved))?;

    Ok(FileWriteResult {
        path: resolved.to_string_lossy().into_owned(),
        action: if existed {
            WriteAction::Overwritten as i32
        } else {
            WriteAction::Created as i32
        },
        bytes_written: bytes.len() as u64,
        final_size: bytes.len() as u64,
        replacements_made: None,
    })
}

async fn handle_append(
    req_path: &str,
    resolved: &Path,
    app: &FileAppend,
    max_write_bytes: u64,
) -> Result<FileWriteResult, FileError> {
    // Read current size to enforce total limit (existing + new).
    let existing_size = match tokio::fs::metadata(resolved).await {
        Ok(m) => m.len(),
        Err(_) => 0,
    };
    enforce_size_limit(existing_size + app.content.len() as u64, max_write_bytes, req_path)?;

    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(resolved)
        .await
        .map_err(|e| io_to_file_error(e, resolved))?;
    tokio::io::AsyncWriteExt::write_all(&mut file, &app.content)
        .await
        .map_err(|e| io_to_file_error(e, resolved))?;
    tokio::io::AsyncWriteExt::flush(&mut file)
        .await
        .map_err(|e| io_to_file_error(e, resolved))?;

    let final_size = tokio::fs::metadata(resolved)
        .await
        .map_err(|e| io_to_file_error(e, resolved))?
        .len();

    Ok(FileWriteResult {
        path: resolved.to_string_lossy().into_owned(),
        action: WriteAction::Appended as i32,
        bytes_written: app.content.len() as u64,
        final_size,
        replacements_made: None,
    })
}

async fn handle_string_replace_write(
    req_path: &str,
    resolved: &Path,
    sr: &StringReplace,
    max_write_bytes: u64,
) -> Result<FileWriteResult, FileError> {
    let (bytes, count) = apply_string_replace(req_path, resolved, sr).await?;
    enforce_size_limit(bytes.len() as u64, max_write_bytes, req_path)?;
    tokio::fs::write(resolved, &bytes)
        .await
        .map_err(|e| io_to_file_error(e, resolved))?;
    Ok(FileWriteResult {
        path: resolved.to_string_lossy().into_owned(),
        action: WriteAction::Edited as i32,
        bytes_written: bytes.len() as u64,
        final_size: bytes.len() as u64,
        replacements_made: Some(count),
    })
}

async fn handle_line_range_replace_write(
    req_path: &str,
    resolved: &Path,
    lr: &LineRangeReplace,
    max_write_bytes: u64,
) -> Result<FileWriteResult, FileError> {
    let bytes = apply_line_range_replace(req_path, resolved, lr).await?;
    enforce_size_limit(bytes.len() as u64, max_write_bytes, req_path)?;
    tokio::fs::write(resolved, &bytes)
        .await
        .map_err(|e| io_to_file_error(e, resolved))?;
    Ok(FileWriteResult {
        path: resolved.to_string_lossy().into_owned(),
        action: WriteAction::Edited as i32,
        bytes_written: bytes.len() as u64,
        final_size: bytes.len() as u64,
        replacements_made: None,
    })
}

async fn handle_byte_range_replace_write(
    req_path: &str,
    resolved: &Path,
    br: &ByteRangeReplace,
    max_write_bytes: u64,
) -> Result<FileWriteResult, FileError> {
    let bytes = apply_byte_range_replace(req_path, resolved, br).await?;
    enforce_size_limit(bytes.len() as u64, max_write_bytes, req_path)?;
    tokio::fs::write(resolved, &bytes)
        .await
        .map_err(|e| io_to_file_error(e, resolved))?;
    Ok(FileWriteResult {
        path: resolved.to_string_lossy().into_owned(),
        action: WriteAction::Edited as i32,
        bytes_written: bytes.len() as u64,
        final_size: bytes.len() as u64,
        replacements_made: None,
    })
}

// ── Edit methods (mirror write methods but return FileEditResult) ────────

async fn handle_string_replace_edit(
    req_path: &str,
    resolved: &Path,
    sr: &StringReplace,
    max_write_bytes: u64,
) -> Result<FileEditResult, FileError> {
    let existing = tokio::fs::read_to_string(resolved)
        .await
        .map_err(|e| io_to_file_error(e, resolved))?;
    let matches = existing.matches(&sr.old_string).count() as u32;
    if matches == 0 {
        return Ok(FileEditResult {
            path: resolved.to_string_lossy().into_owned(),
            final_size: existing.len() as u64,
            replacements_made: Some(0),
            match_error: Some(format!("old_string not found in {req_path}")),
        });
    }
    if matches > 1 && !sr.replace_all {
        return Ok(FileEditResult {
            path: resolved.to_string_lossy().into_owned(),
            final_size: existing.len() as u64,
            replacements_made: Some(0),
            match_error: Some(format!("multiple matches found ({matches})")),
        });
    }
    let updated = if sr.replace_all {
        existing.replace(&sr.old_string, &sr.new_string)
    } else {
        existing.replacen(&sr.old_string, &sr.new_string, 1)
    };
    enforce_size_limit(updated.len() as u64, max_write_bytes, req_path)?;
    tokio::fs::write(resolved, updated.as_bytes())
        .await
        .map_err(|e| io_to_file_error(e, resolved))?;
    Ok(FileEditResult {
        path: resolved.to_string_lossy().into_owned(),
        final_size: updated.len() as u64,
        replacements_made: Some(matches),
        match_error: None,
    })
}

async fn handle_line_range_replace_edit(
    req_path: &str,
    resolved: &Path,
    lr: &LineRangeReplace,
    max_write_bytes: u64,
) -> Result<FileEditResult, FileError> {
    let bytes = apply_line_range_replace(req_path, resolved, lr).await?;
    enforce_size_limit(bytes.len() as u64, max_write_bytes, req_path)?;
    tokio::fs::write(resolved, &bytes)
        .await
        .map_err(|e| io_to_file_error(e, resolved))?;
    Ok(FileEditResult {
        path: resolved.to_string_lossy().into_owned(),
        final_size: bytes.len() as u64,
        replacements_made: None,
        match_error: None,
    })
}

async fn handle_byte_range_replace_edit(
    req_path: &str,
    resolved: &Path,
    br: &ByteRangeReplace,
    max_write_bytes: u64,
) -> Result<FileEditResult, FileError> {
    let bytes = apply_byte_range_replace(req_path, resolved, br).await?;
    enforce_size_limit(bytes.len() as u64, max_write_bytes, req_path)?;
    tokio::fs::write(resolved, &bytes)
        .await
        .map_err(|e| io_to_file_error(e, resolved))?;
    Ok(FileEditResult {
        path: resolved.to_string_lossy().into_owned(),
        final_size: bytes.len() as u64,
        replacements_made: None,
        match_error: None,
    })
}

// ── Shared transformation helpers ─────────────────────────────────────────

async fn apply_string_replace(
    req_path: &str,
    resolved: &Path,
    sr: &StringReplace,
) -> Result<(Vec<u8>, u32), FileError> {
    let existing = tokio::fs::read_to_string(resolved)
        .await
        .map_err(|e| io_to_file_error(e, resolved))?;
    let matches = existing.matches(&sr.old_string).count() as u32;
    if matches == 0 {
        return Err(file_error(
            FileErrorCode::NotFound,
            req_path,
            "old_string not found",
        ));
    }
    if matches > 1 && !sr.replace_all {
        return Err(file_error(
            FileErrorCode::MultipleMatches,
            req_path,
            format!("multiple matches found ({matches})"),
        ));
    }
    let updated = if sr.replace_all {
        existing.replace(&sr.old_string, &sr.new_string)
    } else {
        existing.replacen(&sr.old_string, &sr.new_string, 1)
    };
    Ok((updated.into_bytes(), matches))
}

async fn apply_line_range_replace(
    req_path: &str,
    resolved: &Path,
    lr: &LineRangeReplace,
) -> Result<Vec<u8>, FileError> {
    if lr.start_line == 0 || lr.end_line == 0 || lr.end_line < lr.start_line {
        return Err(file_error(
            FileErrorCode::Unspecified,
            req_path,
            "invalid line range (start/end must be 1-based and start<=end)",
        ));
    }
    let existing = tokio::fs::read_to_string(resolved)
        .await
        .map_err(|e| io_to_file_error(e, resolved))?;
    let mut lines: Vec<&str> = existing.split_inclusive('\n').collect();
    let start = (lr.start_line as usize) - 1;
    let end = (lr.end_line as usize) - 1;
    if start >= lines.len() {
        return Err(file_error(
            FileErrorCode::Unspecified,
            req_path,
            format!("start_line {} exceeds file length", lr.start_line),
        ));
    }
    let end = end.min(lines.len() - 1);

    // Determine whether the replaced range ended with a newline in the source;
    // if so, ensure the replacement also keeps one to avoid merging with the
    // next line.
    let had_trailing_newline = lines[end].ends_with('\n');
    let mut new_block = lr.new_content.clone();
    if had_trailing_newline && !new_block.ends_with('\n') {
        new_block.push('\n');
    }

    // Splice.
    lines.splice(start..=end, std::iter::once(new_block.as_str()));
    Ok(lines.concat().into_bytes())
}

async fn apply_byte_range_replace(
    req_path: &str,
    resolved: &Path,
    br: &ByteRangeReplace,
) -> Result<Vec<u8>, FileError> {
    let existing = tokio::fs::read(resolved)
        .await
        .map_err(|e| io_to_file_error(e, resolved))?;

    // Detect u64 overflow from the caller first.
    let Some(end_u64) = br.byte_offset.checked_add(br.byte_length) else {
        return Err(file_error(
            FileErrorCode::InvalidPath,
            req_path,
            "byte range overflow (byte_offset + byte_length > u64::MAX)",
        ));
    };

    // Validate against file size (still in u64, before casting to usize).
    let file_len = existing.len() as u64;
    if br.byte_offset > file_len || end_u64 > file_len {
        return Err(file_error(
            FileErrorCode::Unspecified,
            req_path,
            "byte range out of bounds",
        ));
    }

    let start = br.byte_offset as usize;
    let end = end_u64 as usize;
    let mut out = Vec::with_capacity(existing.len() - (end - start) + br.new_content.len());
    out.extend_from_slice(&existing[..start]);
    out.extend_from_slice(&br.new_content);
    out.extend_from_slice(&existing[end..]);
    Ok(out)
}

/// Reject non-UTF-8 encoding parameters. V1 only supports UTF-8 writes;
/// anything else returns FILE_ERROR_CODE_ENCODING with a clear message.
fn ensure_encoding_supported(
    encoding: Option<&str>,
    req_path: &str,
) -> Result<(), FileError> {
    match encoding {
        None => Ok(()),
        Some(e)
            if e.is_empty()
                || e.eq_ignore_ascii_case("utf-8")
                || e.eq_ignore_ascii_case("utf8") =>
        {
            Ok(())
        }
        Some(other) => Err(file_error(
            FileErrorCode::Encoding,
            req_path,
            format!(
                "encoding '{}' is not supported for writes (v1 only supports UTF-8)",
                other
            ),
        )),
    }
}

fn enforce_size_limit(size: u64, max: u64, path: &str) -> Result<(), FileError> {
    if size > max {
        Err(file_error(
            FileErrorCode::TooLarge,
            path,
            format!("content {size} bytes exceeds max_write_bytes ({max})"),
        ))
    } else {
        Ok(())
    }
}
