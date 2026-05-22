//! PDF reading handler.
//!
//! Uses pure Rust PDF parsing/rendering through `pdf_oxide` so the daemon does
//! not depend on external Poppler commands such as `pdfinfo` or `pdftoppm`.

use std::path::Path;

use ahand_protocol::{
    FileError, FileErrorCode, FileReadPdf, FileReadPdfMode, FileReadPdfResult, ImageFormat,
    PdfMetadata, PdfPageImage, PdfPageRange, PdfPageText,
};
use pdf_oxide::document::PdfDocument;
use pdf_oxide::rendering::{self, ImageFormat as PdfImageFormat, RenderOptions};

use super::file_error;
use super::fs_ops::io_to_file_error;

const RAW_MODEL_MAX_BYTES: u64 = 20 * 1024 * 1024;
const RAW_MODEL_MAX_PAGES: u32 = 100;
const AUTO_DEFAULT_PAGES: u32 = 5;
const IMGS_DEFAULT_PAGES: u32 = 5;
const IMGS_MAX_PAGES: u32 = 20;
const TEXT_MAX_PAGES: u32 = 100;
const RENDER_DPI: u32 = 100;

pub async fn handle_read_pdf(
    req: &FileReadPdf,
    resolved: &Path,
    max_read_bytes: u64,
) -> Result<FileReadPdfResult, FileError> {
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

    let bytes = tokio::fs::read(resolved)
        .await
        .map_err(|e| io_to_file_error(e, resolved))?;
    if !bytes.starts_with(b"%PDF-") {
        return Err(file_error(
            FileErrorCode::InvalidPath,
            &req.path,
            "file is not a valid PDF (missing %PDF- header)",
        ));
    }

    let mode = FileReadPdfMode::try_from(req.mode).unwrap_or(FileReadPdfMode::Auto);
    let path = req.path.clone();
    let requested_range = req.page_range.clone();

    tokio::task::spawn_blocking(move || {
        read_pdf_blocking(path, bytes, total_file_bytes, mode, requested_range)
    })
    .await
    .map_err(|e| file_error(FileErrorCode::Unspecified, &req.path, e.to_string()))?
}

fn read_pdf_blocking(
    path: String,
    bytes: Vec<u8>,
    total_file_bytes: u64,
    mode: FileReadPdfMode,
    requested_range: Option<PdfPageRange>,
) -> Result<FileReadPdfResult, FileError> {
    let doc = PdfDocument::from_bytes(bytes.clone())
        .map_err(|e| file_error(FileErrorCode::Unspecified, &path, e.to_string()))?;
    let total_pages = u32::try_from(
        doc.page_count()
            .map_err(|e| file_error(FileErrorCode::Unspecified, &path, e.to_string()))?,
    )
    .map_err(|_| file_error(FileErrorCode::TooLarge, &path, "PDF page count exceeds u32"))?;
    let metadata = PdfMetadata {
        path: path.clone(),
        total_file_bytes,
        total_pages,
    };

    match mode {
        FileReadPdfMode::Metadata => {
            reject_range_for_mode(&path, mode, requested_range.as_ref())?;
            Ok(FileReadPdfResult {
                mode: mode as i32,
                metadata: Some(metadata),
                page_range: None,
                raw_content: Vec::new(),
                images: Vec::new(),
                text_pages: Vec::new(),
            })
        }
        FileReadPdfMode::Raw => {
            reject_range_for_mode(&path, mode, requested_range.as_ref())?;
            if total_file_bytes > RAW_MODEL_MAX_BYTES {
                return Err(file_error(
                    FileErrorCode::TooLarge,
                    &path,
                    format!(
                        "PDF raw mode requires files <= {} bytes",
                        RAW_MODEL_MAX_BYTES
                    ),
                ));
            }
            if total_pages > RAW_MODEL_MAX_PAGES {
                return Err(file_error(
                    FileErrorCode::TooLarge,
                    &path,
                    format!("PDF raw mode requires <= {} pages", RAW_MODEL_MAX_PAGES),
                ));
            }
            Ok(FileReadPdfResult {
                mode: mode as i32,
                metadata: Some(metadata),
                page_range: None,
                raw_content: bytes,
                images: Vec::new(),
                text_pages: Vec::new(),
            })
        }
        FileReadPdfMode::Imgs => {
            let range = resolve_range(
                &path,
                requested_range,
                total_pages,
                IMGS_DEFAULT_PAGES,
                IMGS_MAX_PAGES,
            )?;
            let images = render_page_images(&doc, &path, &range)?;
            Ok(FileReadPdfResult {
                mode: mode as i32,
                metadata: Some(metadata),
                page_range: Some(range),
                raw_content: Vec::new(),
                images,
                text_pages: Vec::new(),
            })
        }
        FileReadPdfMode::Text => {
            let range = resolve_range(
                &path,
                requested_range,
                total_pages,
                TEXT_MAX_PAGES,
                TEXT_MAX_PAGES,
            )?;
            let text_pages = extract_page_text(&doc, &path, &range)?;
            Ok(FileReadPdfResult {
                mode: mode as i32,
                metadata: Some(metadata),
                page_range: Some(range),
                raw_content: Vec::new(),
                images: Vec::new(),
                text_pages,
            })
        }
        FileReadPdfMode::Auto => {
            let range = resolve_range(
                &path,
                requested_range,
                total_pages,
                AUTO_DEFAULT_PAGES,
                IMGS_MAX_PAGES,
            )?;
            let images = render_page_images(&doc, &path, &range)?;
            let text_pages = extract_page_text(&doc, &path, &range)?;
            Ok(FileReadPdfResult {
                mode: mode as i32,
                metadata: Some(metadata),
                page_range: Some(range),
                raw_content: Vec::new(),
                images,
                text_pages,
            })
        }
    }
}

fn reject_range_for_mode(
    path: &str,
    mode: FileReadPdfMode,
    range: Option<&PdfPageRange>,
) -> Result<(), FileError> {
    if range.is_some() {
        return Err(file_error(
            FileErrorCode::InvalidPath,
            path,
            format!("read_pdf mode {:?} does not accept a page range", mode),
        ));
    }
    Ok(())
}

fn resolve_range(
    path: &str,
    requested: Option<PdfPageRange>,
    total_pages: u32,
    default_pages: u32,
    max_pages: u32,
) -> Result<PdfPageRange, FileError> {
    if total_pages == 0 {
        return Err(file_error(
            FileErrorCode::InvalidPath,
            path,
            "PDF has no pages",
        ));
    }

    let range = requested.unwrap_or(PdfPageRange {
        start_page: 1,
        end_page: total_pages.min(default_pages),
    });
    if range.start_page == 0 {
        return Err(file_error(
            FileErrorCode::InvalidPath,
            path,
            "page range start_page must be >= 1",
        ));
    }
    if range.end_page < range.start_page {
        return Err(file_error(
            FileErrorCode::InvalidPath,
            path,
            "page range end_page must be >= start_page",
        ));
    }
    if range.end_page > total_pages {
        return Err(file_error(
            FileErrorCode::InvalidPath,
            path,
            format!(
                "page range end_page {} exceeds total pages {}",
                range.end_page, total_pages
            ),
        ));
    }
    let count = range.end_page - range.start_page + 1;
    if count > max_pages {
        return Err(file_error(
            FileErrorCode::TooLarge,
            path,
            format!("page range contains {count} pages; maximum is {max_pages}"),
        ));
    }
    Ok(range)
}

fn extract_page_text(
    doc: &PdfDocument,
    path: &str,
    range: &PdfPageRange,
) -> Result<Vec<PdfPageText>, FileError> {
    (range.start_page..=range.end_page)
        .map(|page_number| {
            let content = doc
                .extract_text((page_number - 1) as usize)
                .map_err(|e| file_error(FileErrorCode::Unspecified, path, e.to_string()))?;
            Ok(PdfPageText {
                page_number,
                content,
            })
        })
        .collect()
}

fn render_page_images(
    doc: &PdfDocument,
    path: &str,
    range: &PdfPageRange,
) -> Result<Vec<PdfPageImage>, FileError> {
    let options = RenderOptions::with_dpi(RENDER_DPI);
    (range.start_page..=range.end_page)
        .map(|page_number| {
            let rendered = rendering::render_page(doc, (page_number - 1) as usize, &options)
                .map_err(|e| file_error(FileErrorCode::Unspecified, path, e.to_string()))?;
            let output_bytes = rendered.data.len() as u64;
            let format = match rendered.format {
                PdfImageFormat::Png => ImageFormat::Png,
                PdfImageFormat::Jpeg => ImageFormat::Jpeg,
                PdfImageFormat::RawRgba8 => ImageFormat::Original,
            };
            Ok(PdfPageImage {
                page_number,
                content: rendered.data,
                format: format as i32,
                width: rendered.width,
                height: rendered.height,
                output_bytes,
            })
        })
        .collect()
}
