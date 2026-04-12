//! Binary and image reading handlers.
//!
//! `handle_read_binary` performs a simple byte-range read. `handle_read_image`
//! decodes the image on the daemon, optionally resizes it to fit the caller's
//! dimension limits, and iteratively compresses (for lossy formats) to meet an
//! optional `max_bytes` budget.

use std::io::{Cursor, SeekFrom};
use std::path::Path;

use ahand_protocol::{
    FileError, FileErrorCode, FileReadBinary, FileReadBinaryResult, FileReadImage,
    FileReadImageResult, ImageFormat,
};
use image::{codecs::jpeg::JpegEncoder, codecs::webp::WebPEncoder, DynamicImage, ImageFormat as ImgFmt, ImageReader};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use super::file_error;
use super::fs_ops::io_to_file_error;

const DEFAULT_BINARY_MAX: u64 = 1_048_576; // 1 MB
const DEFAULT_IMAGE_QUALITY: u8 = 85;

pub async fn handle_read_binary(
    req: &FileReadBinary,
    resolved: &Path,
    max_read_bytes: u64,
) -> Result<FileReadBinaryResult, FileError> {
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
    // Clamp against the policy-level read budget so callers can never
    // bypass `max_read_bytes` by requesting a larger per-call max_bytes.
    let max = req.max_bytes.unwrap_or(DEFAULT_BINARY_MAX).min(max_read_bytes);

    let byte_offset = req.byte_offset;
    if byte_offset >= total_file_bytes {
        return Ok(FileReadBinaryResult {
            content: Vec::new(),
            byte_offset,
            bytes_read: 0,
            total_file_bytes,
            remaining_bytes: 0,
            download_url: None,
            download_url_expires_ms: None,
        });
    }

    let available = total_file_bytes - byte_offset;
    let requested = if req.byte_length == 0 {
        available
    } else {
        req.byte_length.min(available)
    };
    let length = requested.min(max);

    let mut file = tokio::fs::File::open(resolved)
        .await
        .map_err(|e| io_to_file_error(e, resolved))?;
    file.seek(SeekFrom::Start(byte_offset))
        .await
        .map_err(|e| io_to_file_error(e, resolved))?;

    let mut buf = vec![0u8; length as usize];
    let mut read = 0;
    while read < buf.len() {
        let n = file
            .read(&mut buf[read..])
            .await
            .map_err(|e| io_to_file_error(e, resolved))?;
        if n == 0 {
            break;
        }
        read += n;
    }
    buf.truncate(read);

    let bytes_read = read as u64;
    let remaining_bytes = total_file_bytes.saturating_sub(byte_offset + bytes_read);

    Ok(FileReadBinaryResult {
        content: buf,
        byte_offset,
        bytes_read,
        total_file_bytes,
        remaining_bytes,
        download_url: None,
        download_url_expires_ms: None,
    })
}

pub async fn handle_read_image(
    req: &FileReadImage,
    resolved: &Path,
    max_read_bytes: u64,
) -> Result<FileReadImageResult, FileError> {
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

    let raw = tokio::fs::read(resolved)
        .await
        .map_err(|e| io_to_file_error(e, resolved))?;
    let original_bytes = raw.len() as u64;

    // Decode the image in a blocking task — image crate operations are CPU bound.
    let requested_format = ImageFormat::try_from(req.output_format.unwrap_or(0))
        .unwrap_or(ImageFormat::Original);
    let requested_quality = req.quality.map(|q| q.clamp(1, 100) as u8);
    let max_width = req.max_width;
    let max_height = req.max_height;
    let max_bytes = req.max_bytes;

    // Raw passthrough: no resize, no recompress, no reformat requested.
    // Return the original file bytes byte-for-byte and only read the header
    // for dimensions. This avoids the decode→encode round trip entirely.
    let is_passthrough = max_width.is_none()
        && max_height.is_none()
        && max_bytes.is_none()
        && requested_quality.is_none()
        && matches!(requested_format, ImageFormat::Original);

    let req_path = req.path.clone();
    let processed = tokio::task::spawn_blocking(move || -> Result<FileReadImageResult, FileError> {
        // Peek the format and dimensions without decoding pixels, so we can
        // reject decompression bombs before allocating the full pixel buffer.
        let header_reader = ImageReader::new(Cursor::new(&raw))
            .with_guessed_format()
            .map_err(|e| {
                file_error(
                    FileErrorCode::Unspecified,
                    "",
                    format!("failed to read image: {e}"),
                )
            })?;
        let source_format = header_reader.format();
        let (header_width, header_height) = header_reader.into_dimensions().map_err(|e| {
            file_error(
                FileErrorCode::Unspecified,
                "",
                format!("failed to read image dimensions: {e}"),
            )
        })?;

        // Decompression bomb guard: reject images whose decoded RGBA pixel
        // buffer would exceed the policy's read budget.
        let decoded_bytes = (header_width as u64)
            .saturating_mul(header_height as u64)
            .saturating_mul(4);
        if decoded_bytes > max_read_bytes {
            return Err(file_error(
                FileErrorCode::TooLarge,
                &req_path,
                format!(
                    "image dimensions {}x{} would exceed max_read_bytes when decoded",
                    header_width, header_height
                ),
            ));
        }

        if is_passthrough {
            let format_proto = source_format
                .map(output_format_to_proto)
                .unwrap_or(ImageFormat::Original);
            let size = raw.len() as u64;
            return Ok(FileReadImageResult {
                content: raw,
                format: format_proto as i32,
                width: header_width,
                height: header_height,
                original_bytes,
                output_bytes: size,
                download_url: None,
                download_url_expires_ms: None,
            });
        }

        // Non-passthrough: fully decode and go through the resize/encode pipeline.
        let reader = ImageReader::new(Cursor::new(&raw))
            .with_guessed_format()
            .map_err(|e| {
                file_error(
                    FileErrorCode::Unspecified,
                    "",
                    format!("failed to read image: {e}"),
                )
            })?;
        let img = reader.decode().map_err(|e| {
            file_error(
                FileErrorCode::Unspecified,
                "",
                format!("failed to decode image: {e}"),
            )
        })?;

        let resized = apply_resize(img, max_width, max_height);
        let (width, height) = (resized.width(), resized.height());

        let output_format =
            resolve_output_format(requested_format, source_format);
        let quality = requested_quality.unwrap_or(DEFAULT_IMAGE_QUALITY);
        let encoded = encode_image(&resized, output_format, quality, max_bytes)?;
        Ok(FileReadImageResult {
            content: encoded.bytes,
            format: output_format_to_proto(output_format) as i32,
            width,
            height,
            original_bytes,
            output_bytes: encoded.size,
            download_url: None,
            download_url_expires_ms: None,
        })
    })
    .await
    .map_err(|e| {
        file_error(
            FileErrorCode::Unspecified,
            "",
            format!("image worker join error: {e}"),
        )
    })??;

    Ok(processed)
}

// ── Helpers ────────────────────────────────────────────────────────────────

fn apply_resize(img: DynamicImage, max_w: Option<u32>, max_h: Option<u32>) -> DynamicImage {
    if max_w.is_none() && max_h.is_none() {
        return img;
    }
    let target_w = max_w.unwrap_or(u32::MAX);
    let target_h = max_h.unwrap_or(u32::MAX);
    let (w, h) = (img.width(), img.height());
    if w <= target_w && h <= target_h {
        return img;
    }
    img.resize(target_w, target_h, image::imageops::FilterType::Triangle)
}

/// Resolve the target output format given the request and the detected source
/// format. `ImageFormat::Original` maps to the source format (or PNG if
/// detection failed).
fn resolve_output_format(
    requested: ImageFormat,
    source_format: Option<ImgFmt>,
) -> ImgFmt {
    match requested {
        ImageFormat::Original => source_format.unwrap_or(ImgFmt::Png),
        ImageFormat::Jpeg => ImgFmt::Jpeg,
        ImageFormat::Png => ImgFmt::Png,
        ImageFormat::Webp => ImgFmt::WebP,
    }
}

fn output_format_to_proto(format: ImgFmt) -> ImageFormat {
    match format {
        ImgFmt::Jpeg => ImageFormat::Jpeg,
        ImgFmt::Png => ImageFormat::Png,
        ImgFmt::WebP => ImageFormat::Webp,
        _ => ImageFormat::Original,
    }
}

struct EncodedImage {
    bytes: Vec<u8>,
    size: u64,
}

fn encode_image(
    img: &DynamicImage,
    format: ImgFmt,
    quality: u8,
    max_bytes: Option<u64>,
) -> Result<EncodedImage, FileError> {
    // For lossless formats (PNG), quality is ignored and max_bytes can only be
    // met by resizing, which we don't do here. Return a single encode pass.
    if !format_supports_quality(format) {
        let bytes = encode_single(img, format, quality)?;
        return Ok(EncodedImage {
            size: bytes.len() as u64,
            bytes,
        });
    }

    // Lossy: iteratively lower quality until we fit. Start at requested quality.
    let mut q = quality;
    let mut last = encode_single(img, format, q)?;
    if let Some(budget) = max_bytes {
        while last.len() as u64 > budget && q > 10 {
            q = q.saturating_sub(10);
            last = encode_single(img, format, q)?;
        }
    }
    let size = last.len() as u64;
    Ok(EncodedImage { bytes: last, size })
}

fn format_supports_quality(format: ImgFmt) -> bool {
    matches!(format, ImgFmt::Jpeg | ImgFmt::WebP)
}

fn encode_single(
    img: &DynamicImage,
    format: ImgFmt,
    quality: u8,
) -> Result<Vec<u8>, FileError> {
    let mut out = Vec::new();
    match format {
        ImgFmt::Jpeg => {
            let mut cursor = Cursor::new(&mut out);
            let encoder = JpegEncoder::new_with_quality(&mut cursor, quality);
            img.to_rgb8()
                .write_with_encoder(encoder)
                .map_err(|e| image_encode_error(e))?;
        }
        ImgFmt::Png => {
            img.write_to(&mut Cursor::new(&mut out), ImgFmt::Png)
                .map_err(|e| image_encode_error(e))?;
        }
        ImgFmt::WebP => {
            // The `image` crate WebP encoder is lossless by default; quality
            // is accepted but not applied. Good enough for now.
            let encoder = WebPEncoder::new_lossless(Cursor::new(&mut out));
            img.to_rgba8()
                .write_with_encoder(encoder)
                .map_err(|e| image_encode_error(e))?;
        }
        _ => {
            img.write_to(&mut Cursor::new(&mut out), format)
                .map_err(|e| image_encode_error(e))?;
        }
    }
    Ok(out)
}

fn image_encode_error(e: image::ImageError) -> FileError {
    file_error(
        FileErrorCode::Unspecified,
        "",
        format!("failed to encode image: {e}"),
    )
}
