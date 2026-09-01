use crate::model::{ParsedInvoice, TicketType};
use crate::ocr::OfflineOcrError;
use crate::pdf::SupportingDocumentFacts;
use std::path::Path;

const MAX_PDF_BYTES: u64 = 25 * 1024 * 1024;
const MAX_PDF_PAGES: u32 = 5;
const MAX_RENDERED_PAGE_BYTES: u64 = 25 * 1024 * 1024;
const MAX_RENDERED_PAGE_SIDE: f32 = 2_000.0;

#[derive(Debug, thiserror::Error)]
pub enum ScannedPdfError {
    #[error("扫描 PDF 文件不存在、不是普通文件或使用了不安全的链接")]
    InvalidFile,
    #[error("扫描 PDF 文件超过 25 MB 限制")]
    FileTooLarge,
    #[error("扫描 PDF 页数必须为 1–5 页")]
    PageCount,
    #[error("扫描 PDF 无法由 Windows 打开或渲染；请确认文件未损坏且未加密")]
    Render,
    #[error("扫描 PDF 页面超过 25 MB 渲染限制")]
    RenderedPageTooLarge,
    #[error("扫描 PDF 的前 5 页未识别出完整发票字段")]
    NoInvoice,
    #[error("扫描 PDF 处理线程异常；请重启应用后重试")]
    WorkerFailure,
    #[error(transparent)]
    Ocr(#[from] OfflineOcrError),
    #[error("扫描 PDF OCR 仅支持 Windows")]
    UnsupportedPlatform,
}

pub fn parse_scanned_invoice_pdf(
    pdf_path: &Path,
    asset_dir: &Path,
    ticket_type: TicketType,
) -> Result<ParsedInvoice, ScannedPdfError> {
    let metadata = std::fs::symlink_metadata(pdf_path).map_err(|_| ScannedPdfError::InvalidFile)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(ScannedPdfError::InvalidFile);
    }
    if metadata.len() > MAX_PDF_BYTES {
        return Err(ScannedPdfError::FileTooLarge);
    }

    parse_scanned_invoice_pdf_platform(pdf_path, asset_dir, ticket_type)
}

/// 读取 PDF 页数，供只读原件查看器提供稳定分页。该操作不渲染、也不修改文件。
pub fn pdf_page_count(pdf_path: &Path) -> Result<u32, ScannedPdfError> {
    let metadata = std::fs::symlink_metadata(pdf_path).map_err(|_| ScannedPdfError::InvalidFile)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(ScannedPdfError::InvalidFile);
    }
    if metadata.len() > MAX_PDF_BYTES {
        return Err(ScannedPdfError::FileTooLarge);
    }
    let document = lopdf::Document::load(pdf_path).map_err(|_| ScannedPdfError::Render)?;
    u32::try_from(document.get_pages().len()).map_err(|_| ScannedPdfError::PageCount)
}

/// 使用 Windows PDF 引擎把指定页渲染为 PNG。页码从 0 开始；返回值只用于
/// 本地只读预览，避免 WebView PDF 插件在不同机器上表现不一致。
pub fn render_pdf_preview_page(
    pdf_path: &Path,
    page_index: u32,
    max_side: u32,
) -> Result<Vec<u8>, ScannedPdfError> {
    let metadata = std::fs::symlink_metadata(pdf_path).map_err(|_| ScannedPdfError::InvalidFile)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(ScannedPdfError::InvalidFile);
    }
    if metadata.len() > MAX_PDF_BYTES {
        return Err(ScannedPdfError::FileTooLarge);
    }
    if !(600..=3_000).contains(&max_side) {
        return Err(ScannedPdfError::Render);
    }
    render_pdf_preview_page_platform(pdf_path, page_index, max_side)
}

/// 对无文本层的配套材料逐页 OCR，并复用确定性的行程单/结账单事实提取器。
/// 该函数由隔离 OCR worker 调用；主应用进程不直接加载 OCR 运行时。
pub fn extract_scanned_supporting_document_facts(
    pdf_path: &Path,
    asset_dir: &Path,
) -> Result<Option<SupportingDocumentFacts>, ScannedPdfError> {
    let page_count = pdf_page_count(pdf_path)?;
    if page_count == 0 || page_count > MAX_PDF_PAGES {
        return Err(ScannedPdfError::PageCount);
    }
    let mut text = String::new();
    for page_index in 0..page_count {
        let png = render_pdf_preview_page(pdf_path, page_index, MAX_RENDERED_PAGE_SIDE as u32)?;
        let boxes = crate::ocr::recognize_offline_bytes(&png, asset_dir)?;
        for text_box in boxes {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&text_box.text);
        }
    }
    Ok(crate::pdf::extract_supporting_document_facts(&text))
}

#[cfg(target_os = "windows")]
fn render_pdf_preview_page_platform(
    pdf_path: &Path,
    page_index: u32,
    max_side: u32,
) -> Result<Vec<u8>, ScannedPdfError> {
    let pdf_path = dunce::canonicalize(pdf_path).map_err(|_| ScannedPdfError::InvalidFile)?;
    std::thread::Builder::new()
        .name("invoice-pdf-preview".to_string())
        .spawn(move || render_preview_page_on_windows_worker(&pdf_path, page_index, max_side))
        .map_err(|_| ScannedPdfError::WorkerFailure)?
        .join()
        .map_err(|_| ScannedPdfError::WorkerFailure)?
}

#[cfg(not(target_os = "windows"))]
fn render_pdf_preview_page_platform(
    _pdf_path: &Path,
    _page_index: u32,
    _max_side: u32,
) -> Result<Vec<u8>, ScannedPdfError> {
    Err(ScannedPdfError::UnsupportedPlatform)
}

#[cfg(target_os = "windows")]
fn render_preview_page_on_windows_worker(
    pdf_path: &Path,
    page_index: u32,
    max_side: u32,
) -> Result<Vec<u8>, ScannedPdfError> {
    use windows::{
        core::HSTRING,
        Data::Pdf::{PdfDocument, PdfPageRenderOptions},
        Storage::{
            StorageFile,
            Streams::{DataReader, InMemoryRandomAccessStream},
        },
        Win32::System::WinRT::{RoInitialize, RoUninitialize, RO_INIT_MULTITHREADED},
    };

    struct WinRtGuard;
    impl Drop for WinRtGuard {
        fn drop(&mut self) {
            unsafe { RoUninitialize() };
        }
    }

    unsafe { RoInitialize(RO_INIT_MULTITHREADED) }.map_err(|_| ScannedPdfError::Render)?;
    let _winrt = WinRtGuard;
    let file = StorageFile::GetFileFromPathAsync(&HSTRING::from(pdf_path.as_os_str()))
        .and_then(|operation| operation.get())
        .map_err(|_| ScannedPdfError::Render)?;
    let document = PdfDocument::LoadFromFileAsync(&file)
        .and_then(|operation| operation.get())
        .map_err(|_| ScannedPdfError::Render)?;
    let page_count = document.PageCount().map_err(|_| ScannedPdfError::Render)?;
    if page_count == 0 || page_index >= page_count {
        return Err(ScannedPdfError::PageCount);
    }
    let page = document
        .GetPage(page_index)
        .map_err(|_| ScannedPdfError::Render)?;
    let page_size = page.Size().map_err(|_| ScannedPdfError::Render)?;
    if !page_size.Width.is_finite()
        || !page_size.Height.is_finite()
        || page_size.Width <= 0.0
        || page_size.Height <= 0.0
    {
        return Err(ScannedPdfError::Render);
    }
    let max_side = max_side as f32;
    let scale = max_side / page_size.Width.max(page_size.Height);
    let width = (page_size.Width * scale).round().clamp(1.0, max_side) as u32;
    let height = (page_size.Height * scale).round().clamp(1.0, max_side) as u32;
    let options = PdfPageRenderOptions::new().map_err(|_| ScannedPdfError::Render)?;
    options
        .SetDestinationWidth(width)
        .and_then(|_| options.SetDestinationHeight(height))
        .and_then(|_| options.SetIsIgnoringHighContrast(true))
        .map_err(|_| ScannedPdfError::Render)?;
    let stream = InMemoryRandomAccessStream::new().map_err(|_| ScannedPdfError::Render)?;
    page.RenderWithOptionsToStreamAsync(&stream, &options)
        .and_then(|operation| operation.get())
        .map_err(|_| ScannedPdfError::Render)?;
    let rendered_size = stream.Size().map_err(|_| ScannedPdfError::Render)?;
    if rendered_size == 0
        || rendered_size > MAX_RENDERED_PAGE_BYTES
        || rendered_size > u32::MAX as u64
    {
        return Err(ScannedPdfError::RenderedPageTooLarge);
    }
    let input = stream
        .GetInputStreamAt(0)
        .map_err(|_| ScannedPdfError::Render)?;
    let reader = DataReader::CreateDataReader(&input).map_err(|_| ScannedPdfError::Render)?;
    let loaded = reader
        .LoadAsync(rendered_size as u32)
        .and_then(|operation| operation.get())
        .map_err(|_| ScannedPdfError::Render)?;
    if loaded != rendered_size as u32 {
        return Err(ScannedPdfError::Render);
    }
    let mut png = vec![0_u8; loaded as usize];
    reader
        .ReadBytes(&mut png)
        .map_err(|_| ScannedPdfError::Render)?;
    reader.Close().map_err(|_| ScannedPdfError::Render)?;
    stream.Close().map_err(|_| ScannedPdfError::Render)?;
    page.Close().map_err(|_| ScannedPdfError::Render)?;
    Ok(png)
}

#[cfg(target_os = "windows")]
fn parse_scanned_invoice_pdf_platform(
    pdf_path: &Path,
    asset_dir: &Path,
    ticket_type: TicketType,
) -> Result<ParsedInvoice, ScannedPdfError> {
    let pdf_path = dunce::canonicalize(pdf_path).map_err(|_| ScannedPdfError::InvalidFile)?;
    let asset_dir = asset_dir.to_path_buf();
    std::thread::Builder::new()
        .name("invoice-pdf-ocr".to_string())
        .spawn(move || parse_on_windows_worker(&pdf_path, &asset_dir, ticket_type))
        .map_err(|_| ScannedPdfError::WorkerFailure)?
        .join()
        .map_err(|_| ScannedPdfError::WorkerFailure)?
}

#[cfg(not(target_os = "windows"))]
fn parse_scanned_invoice_pdf_platform(
    _pdf_path: &Path,
    _asset_dir: &Path,
    _ticket_type: TicketType,
) -> Result<ParsedInvoice, ScannedPdfError> {
    Err(ScannedPdfError::UnsupportedPlatform)
}

#[cfg(target_os = "windows")]
fn parse_on_windows_worker(
    pdf_path: &Path,
    asset_dir: &Path,
    ticket_type: TicketType,
) -> Result<ParsedInvoice, ScannedPdfError> {
    use windows::{
        core::HSTRING,
        Data::Pdf::{PdfDocument, PdfPageRenderOptions},
        Storage::{
            StorageFile,
            Streams::{DataReader, InMemoryRandomAccessStream},
        },
        Win32::System::WinRT::{RoInitialize, RoUninitialize, RO_INIT_MULTITHREADED},
    };

    struct WinRtGuard;
    impl Drop for WinRtGuard {
        fn drop(&mut self) {
            unsafe { RoUninitialize() };
        }
    }

    unsafe { RoInitialize(RO_INIT_MULTITHREADED) }.map_err(|_| ScannedPdfError::Render)?;
    let _winrt = WinRtGuard;
    let file = StorageFile::GetFileFromPathAsync(&HSTRING::from(pdf_path.as_os_str()))
        .and_then(|operation| operation.get())
        .map_err(|_| ScannedPdfError::Render)?;
    let document = PdfDocument::LoadFromFileAsync(&file)
        .and_then(|operation| operation.get())
        .map_err(|_| ScannedPdfError::Render)?;
    let page_count = document.PageCount().map_err(|_| ScannedPdfError::Render)?;
    if page_count == 0 || page_count > MAX_PDF_PAGES {
        return Err(ScannedPdfError::PageCount);
    }

    for page_index in 0..page_count {
        let page = document
            .GetPage(page_index)
            .map_err(|_| ScannedPdfError::Render)?;
        let page_size = page.Size().map_err(|_| ScannedPdfError::Render)?;
        if !page_size.Width.is_finite()
            || !page_size.Height.is_finite()
            || page_size.Width <= 0.0
            || page_size.Height <= 0.0
        {
            return Err(ScannedPdfError::Render);
        }
        let scale = MAX_RENDERED_PAGE_SIDE / page_size.Width.max(page_size.Height);
        let width = (page_size.Width * scale)
            .round()
            .clamp(1.0, MAX_RENDERED_PAGE_SIDE) as u32;
        let height = (page_size.Height * scale)
            .round()
            .clamp(1.0, MAX_RENDERED_PAGE_SIDE) as u32;
        let options = PdfPageRenderOptions::new().map_err(|_| ScannedPdfError::Render)?;
        options
            .SetDestinationWidth(width)
            .and_then(|_| options.SetDestinationHeight(height))
            .and_then(|_| options.SetIsIgnoringHighContrast(true))
            .map_err(|_| ScannedPdfError::Render)?;

        let stream = InMemoryRandomAccessStream::new().map_err(|_| ScannedPdfError::Render)?;
        page.RenderWithOptionsToStreamAsync(&stream, &options)
            .and_then(|operation| operation.get())
            .map_err(|_| ScannedPdfError::Render)?;
        let rendered_size = stream.Size().map_err(|_| ScannedPdfError::Render)?;
        if rendered_size == 0
            || rendered_size > MAX_RENDERED_PAGE_BYTES
            || rendered_size > u32::MAX as u64
        {
            return Err(ScannedPdfError::RenderedPageTooLarge);
        }
        let input = stream
            .GetInputStreamAt(0)
            .map_err(|_| ScannedPdfError::Render)?;
        let reader = DataReader::CreateDataReader(&input).map_err(|_| ScannedPdfError::Render)?;
        let loaded = reader
            .LoadAsync(rendered_size as u32)
            .and_then(|operation| operation.get())
            .map_err(|_| ScannedPdfError::Render)?;
        if loaded != rendered_size as u32 {
            return Err(ScannedPdfError::Render);
        }
        let mut png = vec![0_u8; loaded as usize];
        reader
            .ReadBytes(&mut png)
            .map_err(|_| ScannedPdfError::Render)?;
        reader.Close().map_err(|_| ScannedPdfError::Render)?;
        stream.Close().map_err(|_| ScannedPdfError::Render)?;
        page.Close().map_err(|_| ScannedPdfError::Render)?;

        match crate::ocr::parse_invoice_image_bytes(&png, pdf_path, asset_dir) {
            Ok(mut invoice) => {
                invoice.ticket_type = crate::expense_classifier::resolve_ticket_type_hint(
                    invoice.ticket_type,
                    ticket_type,
                );
                return Ok(invoice);
            }
            Err(OfflineOcrError::MissingField { .. } | OfflineOcrError::InvalidField { .. }) => {}
            Err(error) => return Err(ScannedPdfError::Ocr(error)),
        }
    }

    Err(ScannedPdfError::NoInvoice)
}
#[cfg(all(test, target_os = "windows"))]
mod tests {
    use super::*;
    use crate::model::ParseLevel;
    use rust_decimal::Decimal;
    use std::str::FromStr;

    #[test]
    #[ignore = "由 scripts/verify-windows.ps1 显式执行扫描 PDF OCR 金样"]
    fn scanned_pdf_ocr_reads_synthetic_vat_invoice() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let pdf_path = root.join("fixtures/synthetic/ocr-vat-invoice-scanned.pdf");
        let asset_dir = root.join("src-tauri/assets/ocr");
        let invoice = parse_scanned_invoice_pdf(&pdf_path, &asset_dir, TicketType::Other).unwrap();

        assert_eq!(invoice.invoice_number, "26112000000000000001");
        assert_eq!(invoice.issue_date.to_string(), "2026-06-18");
        assert_eq!(invoice.total_amount, Decimal::from_str("1200.00").unwrap());
        assert_eq!(
            invoice.tax_amount,
            Some(Decimal::from_str("67.92").unwrap())
        );
        assert_eq!(invoice.buyer_name.as_deref(), Some("北京示例科技有限公司"));
        assert_eq!(invoice.seller_name.as_deref(), Some("上海演示商贸有限公司"));
        assert_eq!(invoice.parse_level, ParseLevel::L2);
        assert!(
            invoice.confidence >= 0.85,
            "实际置信度 {}",
            invoice.confidence
        );
    }

    /// 真实原件只通过显式环境变量启用。测试只验证 Windows PDF 引擎返回
    /// 一张非空 PNG，不输出路径、文件名或任何票面内容。
    #[test]
    #[ignore = "requires an explicitly authorized private PDF path"]
    fn private_pdf_preview_renders_png() {
        let path = std::env::var_os("INVOICE_REAL_PDF_PREVIEW_PATH")
            .expect("INVOICE_REAL_PDF_PREVIEW_PATH is required");
        let rendered = render_pdf_preview_page(Path::new(&path), 0, 2_200)
            .expect("authorized private PDF preview must render");

        println!("rendered_bytes={}", rendered.len());
        println!("private_values_logged=false");
        assert!(rendered.len() > 8);
        assert_eq!(&rendered[..8], b"\x89PNG\r\n\x1a\n");
    }
}
