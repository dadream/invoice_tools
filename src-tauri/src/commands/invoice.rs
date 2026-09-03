//! 发票命令模块：解析本地发票文件 → 查重 → 写入批次。
//!
//! PII 约束：日志与错误信息只允许出现文件扩展名、解析级别、错误类别与字段**名**，
//! 绝不出现发票号、金额、姓名、邮箱或完整文件路径。`ParseError` 的 Display
//! 带 `path` 与 `raw`（原始字段值），因此一律经 `parse_error_message` 降级为类别文案。

use std::path::Path;
use std::sync::Mutex;

use serde::Serialize;
use tauri::State;

use invoice_parse::manifest::TagHints;
use invoice_parse::model::{ParseError, ParseLevel, ParsedInvoice, TicketType as ParseTicketType};
use invoice_store::models::{BatchStatus, ReportedInvoice, TicketType as StoreTicketType};

use crate::error::{AppError, AppResult};
use crate::AppState;

const DATE_FMT: &str = "%Y-%m-%d";
const DATETIME_FMT: &str = "%Y-%m-%d %H:%M:%S";

/// 解析结果 DTO —— 尚未入库，供前端确认。
/// 金额与日期全部转字符串，跨 IPC 不使用 f64。
#[derive(Debug, Clone, Serialize)]
pub struct ParsedInvoiceDto {
    pub invoice_number: String,
    pub issue_date: String,
    pub total_amount: String,
    pub tax_amount: Option<String>,
    pub tax_rate: Option<String>,
    pub buyer_name: Option<String>,
    pub seller_name: Option<String>,
    /// rail/flight/hotel/city_transport/meal/courier_logistics/other
    pub ticket_type: String,
    /// L0/L1/L2/L4
    pub parse_level: String,
    pub confidence: f32,
    pub city: Option<String>,
    pub departure_time: Option<String>,
    pub checkin_date: Option<String>,
    pub source_path: String,
    /// 兼容旧版 IPC；MVP 不执行验签，固定为空字符串。
    pub verification_result: String,
}

impl From<ParsedInvoice> for ParsedInvoiceDto {
    fn from(p: ParsedInvoice) -> Self {
        Self {
            invoice_number: p.invoice_number,
            issue_date: p.issue_date.format(DATE_FMT).to_string(),
            total_amount: p.total_amount.to_string(),
            tax_amount: p.tax_amount.map(|d| d.to_string()),
            tax_rate: p.tax_rate.map(|d| d.to_string()),
            buyer_name: p.buyer_name,
            seller_name: p.seller_name,
            ticket_type: to_store_ticket_type(p.ticket_type).to_str().to_string(),
            parse_level: parse_level_to_string(p.parse_level).to_string(),
            confidence: p.confidence,
            city: p.city,
            departure_time: p
                .departure_time
                .map(|dt| dt.format(DATETIME_FMT).to_string()),
            checkin_date: p.checkin_date.map(|d| d.format(DATE_FMT).to_string()),
            source_path: p.source_path.display().to_string(),
            verification_result: String::new(),
        }
    }
}

/// 已入库发票 DTO。字段名与 `ReportedInvoice` 对齐（金额列名是 `amount`）。
#[derive(Debug, Clone, Serialize)]
pub struct InvoiceDto {
    pub id: i64,
    pub batch_id: i64,
    pub invoice_number: String,
    pub issue_date: String,
    pub amount: String,
    pub tax_amount: Option<String>,
    pub buyer_name: Option<String>,
    pub seller_name: Option<String>,
    pub ticket_type: String,
    pub city: Option<String>,
    pub departure_time: Option<String>,
    pub checkin_date: Option<String>,
    pub file_path: String,
    pub created_at: String,
    pub verification_result: Option<String>,
    pub is_duplicate: bool,
    pub duplicate_reason: Option<String>,
    pub is_excluded: bool,
}

impl From<ReportedInvoice> for InvoiceDto {
    fn from(r: ReportedInvoice) -> Self {
        Self {
            id: r.id,
            batch_id: r.batch_id,
            invoice_number: r.invoice_number,
            issue_date: r.issue_date.format(DATE_FMT).to_string(),
            amount: r.amount.to_string(),
            tax_amount: r.tax_amount.map(|d| d.to_string()),
            buyer_name: r.buyer_name,
            seller_name: r.seller_name,
            ticket_type: r.ticket_type.to_str().to_string(),
            city: r.city,
            departure_time: r
                .departure_time
                .map(|dt| dt.format(DATETIME_FMT).to_string()),
            checkin_date: r.checkin_date.map(|d| d.format(DATE_FMT).to_string()),
            file_path: r.file_path,
            created_at: r.created_at.format(DATETIME_FMT).to_string(),
            verification_result: r.verification_result,
            is_duplicate: r.is_duplicate,
            duplicate_reason: r.duplicate_reason,
            is_excluded: false,
        }
    }
}

/// 查重结果 DTO
#[derive(Debug, Clone, Serialize)]
pub struct DuplicateCheckDto {
    pub is_duplicate: bool,
    pub match_type: Option<String>, // "exact" / "fuzzy" / null
    pub existing_invoices: Vec<InvoiceSummaryDto>,
}

/// 发票摘要 DTO（用于查重结果）
#[derive(Debug, Clone, Serialize)]
pub struct InvoiceSummaryDto {
    pub id: i64,
    pub batch_id: i64,
    pub batch_name: String,
    pub invoice_number: String,
    pub amount: String,
    pub issue_date: String,
}

pub(crate) fn parse_level_to_string(level: ParseLevel) -> &'static str {
    match level {
        ParseLevel::L0 => "L0",
        ParseLevel::L1 => "L1",
        ParseLevel::L2 => "L2",
        ParseLevel::L4 => "L4",
    }
}

/// 解析侧票种 → 存储侧票种。两者是**不同类型**的独立枚举，必须显式映射。
pub(crate) fn to_store_ticket_type(t: ParseTicketType) -> StoreTicketType {
    match t {
        ParseTicketType::Rail => StoreTicketType::Rail,
        ParseTicketType::Flight => StoreTicketType::Flight,
        ParseTicketType::Hotel => StoreTicketType::Hotel,
        ParseTicketType::CityTransport => StoreTicketType::CityTransport,
        ParseTicketType::Meal => StoreTicketType::Meal,
        ParseTicketType::CourierLogistics => StoreTicketType::CourierLogistics,
        ParseTicketType::Other => StoreTicketType::Other,
    }
}

/// 前端字符串 → 解析侧票种；未知值回落 `Other`。
/// 词表与 `StoreTicketType::to_str()` 保持一致，前后端只认这一套。
fn ticket_type_from_str(s: &str) -> ParseTicketType {
    match s {
        "rail" => ParseTicketType::Rail,
        "flight" => ParseTicketType::Flight,
        "hotel" => ParseTicketType::Hotel,
        "city_transport" => ParseTicketType::CityTransport,
        "meal" => ParseTicketType::Meal,
        "courier_logistics" => ParseTicketType::CourierLogistics,
        _ => ParseTicketType::Other,
    }
}

/// 把 `ParseError` 降级为只含类别的文案。
///
/// 不能直接用 `ParseError` 的 Display：`Io`/`MalformedFormat`/`MissingField`
/// 会带完整文件路径，`UnparseableValue` 会带原始字段值（可能是发票号或金额）。
/// 这里只保留错误类别与字段**名**，字段值与路径一律丢弃。
fn parse_error_message(err: &ParseError) -> String {
    match err {
        ParseError::Io { source, .. } => format!("读取文件失败（{}）", source.kind()),
        ParseError::MalformedFormat { format, .. } => format!("文件不是有效的 {} 格式", format),
        ParseError::MissingField { field, .. } => format!("找不到必需字段 {}", field),
        ParseError::UnparseableValue {
            field,
            expected_type,
            ..
        } => {
            format!("字段 {} 的值无法解析为 {}", field, expected_type)
        }
    }
}

/// 应用内置 hints。
///
/// `fixtures/manifest.toml` 没有全局 `[hints]` 段（hints 是逐样本的
/// `[sample.xml_tag_hints]`），且 `fixtures/` 是开发夹具、不随应用分发，
/// 因此这里内置一份从全部样本聚合出的标签并集。
pub(crate) fn builtin_hints() -> TagHints {
    TagHints {
        invoice_number: vec!["InvoiceNumber".into(), "EIid".into()],
        issue_date: vec!["IssueTime".into(), "RequestTime".into(), "IssueDate".into()],
        total_amount: vec!["TotalTax-includedAmount".into(), "TotalAmount".into()],
        tax_amount: vec!["TotalTaxAm".into(), "TaxAmount".into()],
        tax_rate: vec!["TaxRate".into()],
        buyer_name: vec!["BuyerName".into()],
        seller_name: vec!["SellerName".into()],
    }
}

fn run_pdf_parse_attempt<F>(attempt: &'static str, parser: F) -> Option<ParsedInvoice>
where
    F: FnOnce() -> Result<ParsedInvoice, ParseError>,
{
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(parser)) {
        Ok(Ok(parsed)) => Some(parsed),
        Ok(Err(_)) => {
            tracing::debug!(attempt, "PDF 解析路径未命中，继续降级");
            None
        }
        Err(_) => {
            // 不输出 panic payload；第三方解析库可能把文件内容片段放进 payload。
            tracing::warn!(attempt, "PDF 解析路径发生 panic，已隔离并继续降级");
            None
        }
    }
}

pub(crate) fn parse_pdf_with_fallbacks(
    path: &Path,
    bytes: &[u8],
    hints: &TagHints,
    ticket_type: ParseTicketType,
) -> AppResult<ParsedInvoice> {
    if bytes.is_empty() || bytes.len() > invoice_parse::pdf_embedded::MAX_PDF_BYTES {
        return Err(AppError::validation("PDF 文件为空或超过 25 MiB 大小上限"));
    }
    if let Some(parsed) = run_pdf_parse_attempt("embedded_structured_xml", || {
        invoice_parse::pdf_embedded::parse_embedded_rail_invoice(bytes, path)
    }) {
        return Ok(parsed);
    }
    if let Some(parsed) = run_pdf_parse_attempt("travel_ticket_text", || {
        let text = invoice_parse::pdf::extract_text(bytes, path)?;
        invoice_parse::pdf::parse_detected_travel_invoice_text(&text, path)
    }) {
        return Ok(parsed);
    }
    if let Some(parsed) = run_pdf_parse_attempt("positioned_text", || {
        invoice_parse::pdf_text::parse_vat_invoice_from_boxes(bytes, path)
    }) {
        return Ok(parsed);
    }
    if let Some(parsed) = run_pdf_parse_attempt("flat_text", || {
        invoice_parse::pdf::parse_invoice_pdf(bytes, path, hints, ticket_type)
    }) {
        return Ok(parsed);
    }
    let is_ride_hailing_itinerary = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        invoice_parse::pdf::extract_text(bytes, path)
    }))
    .ok()
    .and_then(Result::ok)
    .is_some_and(|text| invoice_parse::pdf::is_unambiguous_ride_hailing_itinerary(&text));
    if is_ride_hailing_itinerary {
        tracing::debug!(
            category = "ride_hailing_itinerary",
            "PDF 是报销辅助材料，跳过无收益的 OCR"
        );
        return Err(AppError::parse(
            "检测到网约车行程单辅助材料；请保留供人工核对，不作为发票原件解析",
        ));
    }
    let asset_dir = crate::paths::ocr_assets_dir()
        .map_err(|_| AppError::parse("离线 OCR 组件路径不可用；请重新解压完整便携包"))?;
    crate::ocr_worker::parse_with_worker(path, &asset_dir, ticket_type)
        .map_err(|_| AppError::parse("PDF 文本解析和离线 OCR 均失败，请人工复核原件"))
}

/// 按扩展名分派解析。`parse_invoice` 与 `add_invoice_to_batch` 共用，
/// 后者不信任前端回传的字段，一律重新解析。
pub(crate) fn do_parse(path: &str, ticket_type: Option<&str>) -> AppResult<ParsedInvoice> {
    let p = Path::new(path);
    if !p.is_file() {
        return Err(AppError::validation("文件不存在或不是普通文件"));
    }

    let ext = p
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();

    let hints = builtin_hints();
    let tt = ticket_type
        .map(ticket_type_from_str)
        .unwrap_or(ParseTicketType::Other);

    // 先确认扩展名受支持，再进入解析（不支持的类型属于 validation，不是 parse 失败）
    if !matches!(
        ext.as_str(),
        "xml" | "ofd" | "pdf" | "png" | "jpg" | "jpeg" | "webp" | "bmp"
    ) {
        return Err(AppError::validation(format!("不支持的文件类型: .{}", ext)));
    }

    if matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "webp" | "bmp") {
        let asset_dir = crate::paths::ocr_assets_dir()
            .map_err(|_| AppError::parse("离线 OCR 组件路径不可用；请重新解压完整便携包"))?;
        let parsed = crate::ocr_worker::parse_with_worker(p, &asset_dir, tt)?;
        tracing::info!(
            ext = %ext,
            parse_level = parse_level_to_string(parsed.parse_level),
            confidence = parsed.confidence,
            "图片发票 OCR 解析成功"
        );
        return Ok(parsed);
    }

    // 只带 io kind，不拼接路径，避免把用户目录结构写进日志
    let bytes =
        std::fs::read(p).map_err(|e| AppError::io(format!("读取文件失败（{}）", e.kind())))?;

    if ext == "pdf" {
        let parsed = parse_pdf_with_fallbacks(p, &bytes, &hints, tt)?;
        tracing::info!(
            ext = %ext,
            parse_level = parse_level_to_string(parsed.parse_level),
            confidence = parsed.confidence,
            "PDF 发票解析成功"
        );
        return Ok(parsed);
    }

    // panic containment：`pdf_text::extract_text_boxes` 自带 catch_unwind，但
    // flat-text 回落路径 `pdf::extract_text` 没有——pdf-extract 对某些字体编码
    // 直接 assert!，会 panic 穿透并杀掉进程。这里在命令层统一兜住，
    // 保证任何格式都不会让 panic 逃逸到 IPC 边界。
    let dispatch = std::panic::AssertUnwindSafe(|| match ext.as_str() {
        "xml" => invoice_parse::xml::parse_invoice_xml(&bytes, p, &hints, tt),
        "ofd" => invoice_parse::ofd::parse_invoice_ofd(&bytes, p, &hints, tt),
        "pdf" => unreachable!("PDF 已在独立逐级隔离路径处理"),
        _ => unreachable!("扩展名已在上面校验"),
    });

    let result = std::panic::catch_unwind(dispatch).map_err(|_| {
        // 不外传 panic payload：可能含解析库拼进来的文件内容片段
        tracing::warn!(ext = %ext, "解析库 panic，已拦截");
        AppError::parse("解析失败: 文件结构异常，解析库无法处理")
    })?;

    let parsed = result.map_err(|e| {
        tracing::warn!(ext = %ext, "发票解析失败");
        AppError::parse(format!("解析失败: {}", parse_error_message(&e)))
    })?;

    tracing::info!(
        ext = %ext,
        parse_level = parse_level_to_string(parsed.parse_level),
        confidence = parsed.confidence,
        "发票解析成功"
    );

    Ok(parsed)
}

/// 解析本地发票文件，返回结构化字段供前端确认（不入库）
#[tauri::command]
pub fn parse_invoice(path: String, ticket_type: Option<String>) -> AppResult<ParsedInvoiceDto> {
    let parsed = do_parse(&path, ticket_type.as_deref())?;
    // MVP 只负责解析并保留原件，不对 OFD/XML 签章或发票真伪作判断。
    Ok(ParsedInvoiceDto::from(parsed))
}

/// 按多字段组合查重：发票号精确匹配 或 (金额+日期+票种) 模糊匹配
#[tauri::command]
pub fn check_duplicate(
    invoice_number: String,
    amount: String,
    issue_date: String,
    ticket_type: String,
    state: State<Mutex<AppState>>,
) -> AppResult<DuplicateCheckDto> {
    use chrono::NaiveDate;
    use rust_decimal::Decimal;
    use std::str::FromStr;

    let app_state = state
        .lock()
        .map_err(|e| AppError::internal(format!("状态锁错误: {}", e)))?;
    let db = app_state.ledger_db()?;

    // 解析参数
    let amount_decimal =
        Decimal::from_str(&amount).map_err(|_| AppError::validation("金额格式无效"))?;
    let issue_date_parsed = NaiveDate::parse_from_str(&issue_date, DATE_FMT)
        .map_err(|_| AppError::validation("日期格式无效"))?;

    // 调用数据库查重
    let duplicates = db
        .find_potential_duplicates(
            &invoice_number,
            &amount_decimal,
            &issue_date_parsed,
            &ticket_type,
            None,
        )
        .map_err(|e| AppError::database(format!("查重失败: {}", e)))?;

    if duplicates.is_empty() {
        return Ok(DuplicateCheckDto {
            is_duplicate: false,
            match_type: None,
            existing_invoices: vec![],
        });
    }

    // 判断匹配类型：命中发票号 → exact，否则 → fuzzy
    let has_exact_match = duplicates
        .iter()
        .any(|inv| inv.invoice_number == invoice_number);
    let match_type = if has_exact_match { "exact" } else { "fuzzy" };

    // 构造摘要 DTO，查询每个发票的批次名称
    let mut summaries = Vec::new();
    for inv in duplicates {
        let batch_name = db
            .get_batch(inv.batch_id)
            .ok()
            .map(|b| b.name)
            .unwrap_or_else(|| "未知批次".to_string());

        summaries.push(InvoiceSummaryDto {
            id: inv.id,
            batch_id: inv.batch_id,
            batch_name,
            invoice_number: inv.invoice_number,
            amount: inv.amount.to_string(),
            issue_date: inv.issue_date.format(DATE_FMT).to_string(),
        });
    }

    tracing::info!(match_type = match_type, count = summaries.len(), "查重命中");

    Ok(DuplicateCheckDto {
        is_duplicate: true,
        match_type: Some(match_type.to_string()),
        existing_invoices: summaries,
    })
}

/// 解析并把发票写入批次。
///
/// 只接 `batch_id`/`path`/`ticket_type`——后端重新解析，不信任前端回传的字段。
#[tauri::command]
pub fn add_invoice_to_batch(
    batch_id: i64,
    path: String,
    ticket_type: Option<String>,
    state: State<Mutex<AppState>>,
) -> AppResult<InvoiceDto> {
    use chrono::Utc;

    let app_state = state
        .lock()
        .map_err(|e| AppError::internal(format!("状态锁错误: {}", e)))?;
    let db = app_state.ledger_db()?;

    // 1. 批次必须存在且为 Draft
    let batch = db
        .get_batch(batch_id)
        .map_err(|e| AppError::database(format!("查询批次失败: {}", e)))?;
    if !matches!(batch.status, BatchStatus::Draft) {
        return Err(AppError::validation("只有草稿状态的批次可以添加发票"));
    }

    // 2. 重新解析
    let parsed = do_parse(&path, ticket_type.as_deref())?;

    // 3. 查重（不阻断，但标记）。MVP 不执行签章或真伪验证。
    let store_ticket_type = to_store_ticket_type(parsed.ticket_type);
    let duplicates = db
        .find_potential_duplicates(
            &parsed.invoice_number,
            &parsed.total_amount,
            &parsed.issue_date,
            store_ticket_type.to_str(),
            None,
        )
        .map_err(|e| AppError::database(format!("查重失败: {}", e)))?;

    let (is_duplicate, duplicate_reason) = if !duplicates.is_empty() {
        let has_exact = duplicates
            .iter()
            .any(|inv| inv.invoice_number == parsed.invoice_number);
        let reason = if has_exact {
            format!("发票号一致（已存在 {} 条记录）", duplicates.len())
        } else {
            format!("金额+日期+票种一致（已存在 {} 条记录）", duplicates.len())
        };
        (true, Some(reason))
    } else {
        (false, None)
    };

    // 5. 构造 ReportedInvoice。id 由自增填充；created_at/updated_at 由 add_invoice 内部写。
    let now = Utc::now().naive_utc();
    let record = ReportedInvoice {
        id: 0,
        batch_id,
        invoice_number: parsed.invoice_number.clone(),
        issue_date: parsed.issue_date,
        amount: parsed.total_amount,
        tax_amount: parsed.tax_amount,
        buyer_name: parsed.buyer_name.clone(),
        seller_name: parsed.seller_name.clone(),
        ticket_type: store_ticket_type,
        city: parsed.city.clone(),
        departure_time: parsed.departure_time,
        checkin_date: parsed.checkin_date,
        // 绝对路径，便于后续重新解析或打开原件
        file_path: std::fs::canonicalize(&path)
            .map(|p| p.display().to_string())
            .unwrap_or(path),
        created_at: now,
        updated_at: now,
        verification_result: None,
        is_duplicate,
        duplicate_reason,
    };

    let invoice_id = db
        .add_invoice(&record)
        .map_err(|e| AppError::database(format!("写入发票失败: {}", e)))?;

    tracing::info!(
        batch_id,
        invoice_id,
        parse_level = parse_level_to_string(parsed.parse_level),
        is_duplicate,
        "发票已加入批次"
    );

    // 6. 回读，让前端拿到 DB 生成的 id 与时间戳
    let stored = db
        .list_invoices_by_batch(batch_id)
        .map_err(|e| AppError::database(format!("回读发票失败: {}", e)))?
        .into_iter()
        .find(|i| i.id == invoice_id)
        .ok_or_else(|| AppError::database("写入后无法回读发票记录"))?;

    Ok(InvoiceDto::from(stored))
}

/// 列出批次下的所有发票
#[tauri::command]
pub fn list_batch_invoices(
    batch_id: i64,
    state: State<Mutex<AppState>>,
) -> AppResult<Vec<InvoiceDto>> {
    let app_state = state
        .lock()
        .map_err(|e| AppError::internal(format!("状态锁错误: {}", e)))?;
    let db = app_state.ledger_db()?;

    let invoices = db
        .list_invoices_by_batch(batch_id)
        .map_err(|e| AppError::database(format!("查询发票失败: {}", e)))?;

    let excluded_ids = db
        .list_excluded_invoice_ids(batch_id)
        .map_err(|e| AppError::database(format!("查询排除状态失败: {e}")))?;

    Ok(invoices
        .into_iter()
        .map(|invoice| {
            let mut dto = InvoiceDto::from(invoice);
            dto.is_excluded = excluded_ids.contains(&dto.id);
            dto
        })
        .collect())
}

/// 从批次中删除发票（仅所属批次为 Draft 时允许）
#[tauri::command]
pub fn delete_invoice(invoice_id: i64, state: State<Mutex<AppState>>) -> AppResult<()> {
    let app_state = state
        .lock()
        .map_err(|e| AppError::internal(format!("状态锁错误: {}", e)))?;
    let db = app_state.ledger_db()?;

    // delete_invoice 本身不校验批次状态，这里先定位所属批次再判状态
    let invoice = db
        .get_invoice(invoice_id)
        .map_err(|e| AppError::database(format!("查询发票失败: {}", e)))?
        .ok_or_else(|| AppError::validation("发票不存在"))?;

    let batch = db
        .get_batch(invoice.batch_id)
        .map_err(|e| AppError::database(format!("查询批次失败: {}", e)))?;
    if !matches!(batch.status, BatchStatus::Draft) {
        return Err(AppError::validation("只有草稿状态的批次可以删除发票"));
    }

    db.delete_invoice(invoice_id)
        .map_err(|e| AppError::database(format!("删除发票失败: {}", e)))?;

    tracing::info!(invoice_id, batch_id = invoice.batch_id, "发票已从批次删除");
    Ok(())
}

/// 清除发票的重复标记（用户确认不是重复后调用）
#[tauri::command]
pub fn clear_duplicate_flag(invoice_id: i64, state: State<Mutex<AppState>>) -> AppResult<()> {
    let app_state = state
        .lock()
        .map_err(|e| AppError::internal(format!("状态锁错误: {}", e)))?;
    let db = app_state.ledger_db()?;

    db.resolve_duplicate_with_audit(invoice_id)
        .map_err(|e| AppError::database(format!("确认非重复失败: {}", e)))?;

    tracing::info!(invoice_id, "已清除发票重复标记");
    Ok(())
}

/// 用户确认候选确实重复；保留原件和重复依据，但明确保持不计入总额。
#[tauri::command]
pub fn confirm_duplicate_flag(invoice_id: i64, state: State<Mutex<AppState>>) -> AppResult<()> {
    let app_state = state
        .lock()
        .map_err(|e| AppError::internal(format!("状态锁错误: {e}")))?;
    app_state
        .ledger_db()?
        .confirm_duplicate_with_audit(invoice_id)
        .map_err(|e| AppError::database(format!("确认重复失败: {e}")))?;
    tracing::info!(invoice_id, "用户确认重复，保持不计入总额");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorKind;

    #[test]
    fn builtin_hints_covers_all_seven_fields() {
        let h = builtin_hints();
        assert!(
            !h.invoice_number.is_empty(),
            "invoice_number hints 不能为空"
        );
        assert!(!h.issue_date.is_empty(), "issue_date hints 不能为空");
        assert!(!h.total_amount.is_empty(), "total_amount hints 不能为空");
        assert!(!h.tax_amount.is_empty(), "tax_amount hints 不能为空");
        assert!(!h.tax_rate.is_empty(), "tax_rate hints 不能为空");
        assert!(!h.buyer_name.is_empty(), "buyer_name hints 不能为空");
        assert!(!h.seller_name.is_empty(), "seller_name hints 不能为空");
    }

    #[test]
    fn builtin_hints_parse_the_packaged_pipeline_synthetic_xml() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("fixtures")
            .join("synthetic")
            .join("vat-invoice.xml");
        let bytes = std::fs::read(&path).expect("应能读取合成 XML 夹具");
        let parsed = invoice_parse::xml::parse_invoice_xml(
            &bytes,
            &path,
            &builtin_hints(),
            ParseTicketType::Other,
        )
        .expect("生产内置 hints 应覆盖发布流水线使用的合成 XML");

        assert_eq!(parsed.invoice_number, "26112000000000000001");
        assert_eq!(parsed.issue_date.format(DATE_FMT).to_string(), "2026-06-18");
        assert_eq!(parsed.total_amount.to_string(), "1200.00");
        assert_eq!(
            parsed.tax_amount.map(|v| v.to_string()).as_deref(),
            Some("67.92")
        );
    }

    #[test]
    fn parse_level_maps_all_four_levels() {
        assert_eq!(parse_level_to_string(ParseLevel::L0), "L0");
        assert_eq!(parse_level_to_string(ParseLevel::L1), "L1");
        assert_eq!(parse_level_to_string(ParseLevel::L2), "L2");
        assert_eq!(parse_level_to_string(ParseLevel::L4), "L4");
    }

    #[test]
    fn store_ticket_type_mapping_covers_six_variants() {
        // 两个枚举独立，这里同时校验映射方向与落库词表
        let cases = [
            (ParseTicketType::Rail, StoreTicketType::Rail, "rail"),
            (ParseTicketType::Flight, StoreTicketType::Flight, "flight"),
            (ParseTicketType::Hotel, StoreTicketType::Hotel, "hotel"),
            (
                ParseTicketType::CityTransport,
                StoreTicketType::CityTransport,
                "city_transport",
            ),
            (ParseTicketType::Meal, StoreTicketType::Meal, "meal"),
            (
                ParseTicketType::CourierLogistics,
                StoreTicketType::CourierLogistics,
                "courier_logistics",
            ),
            (ParseTicketType::Other, StoreTicketType::Other, "other"),
        ];

        for (parse_tt, store_tt, wire) in cases {
            let mapped = to_store_ticket_type(parse_tt);
            assert_eq!(mapped, store_tt);
            assert_eq!(mapped.to_str(), wire);
        }
    }

    #[test]
    fn ticket_type_from_str_roundtrips_wire_vocabulary() {
        for wire in ["rail", "flight", "hotel", "city_transport", "meal", "other"] {
            let parsed = ticket_type_from_str(wire);
            assert_eq!(to_store_ticket_type(parsed).to_str(), wire);
        }
    }

    #[test]
    fn ticket_type_from_str_falls_back_to_other() {
        assert_eq!(ticket_type_from_str("unknown"), ParseTicketType::Other);
        assert_eq!(ticket_type_from_str(""), ParseTicketType::Other);
        // 大小写敏感：Rail 不是词表里的值
        assert_eq!(ticket_type_from_str("Rail"), ParseTicketType::Other);
    }

    #[test]
    fn parse_invoice_rejects_missing_file() {
        let err = parse_invoice("/nonexistent/path/invoice.xml".into(), None).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Validation);
    }

    #[test]
    fn parse_invoice_rejects_directory_path() {
        // 目录不是普通文件，同样走 validation 分支
        let dir = std::env::temp_dir();
        let err = parse_invoice(dir.display().to_string(), None).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Validation);
    }

    #[test]
    fn parse_invoice_rejects_unsupported_extension() {
        let path = std::env::temp_dir().join("s07-invoice-test-sample.txt");
        std::fs::write(&path, b"not an invoice").unwrap();

        let err = parse_invoice(path.display().to_string(), None).unwrap_err();
        let _ = std::fs::remove_file(&path);

        assert_eq!(err.kind(), ErrorKind::Validation);
        assert!(
            err.message().contains(".txt"),
            "应指出扩展名: {}",
            err.message()
        );
    }

    #[test]
    fn parse_invoice_maps_malformed_xml_to_parse_error() {
        let path = std::env::temp_dir().join("s07-invoice-test-malformed.xml");
        // 合法 XML 但没有任何发票字段 → MissingField
        std::fs::write(&path, b"<root><foo>bar</foo></root>").unwrap();

        let err = parse_invoice(path.display().to_string(), Some("rail".into())).unwrap_err();
        let _ = std::fs::remove_file(&path);

        assert_eq!(err.kind(), ErrorKind::Parse);
    }

    #[test]
    fn parse_error_message_omits_path_and_raw_value() {
        // Io/MalformedFormat/MissingField 的 Display 带完整路径，
        // UnparseableValue 的 Display 带原始字段值（可能是发票号或金额）。
        // parse_error_message 必须把两者都剥掉。
        let secret_path =
            std::path::PathBuf::from("/home/someone/发票/张三-12345678901234567890.xml");

        let io = ParseError::Io {
            path: secret_path.clone(),
            source: std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied"),
        };
        let msg = parse_error_message(&io);
        assert!(!msg.contains("someone"), "不得泄露路径: {msg}");
        assert!(!msg.contains("张三"), "不得泄露姓名: {msg}");

        let missing = ParseError::MissingField {
            path: secret_path.clone(),
            field: "invoice_number".into(),
        };
        let msg = parse_error_message(&missing);
        assert!(msg.contains("invoice_number"), "应保留字段名: {msg}");
        assert!(!msg.contains("someone"), "不得泄露路径: {msg}");

        let unparseable = ParseError::UnparseableValue {
            field: "total_amount".into(),
            raw: "553.00".into(),
            expected_type: "decimal",
        };
        let msg = parse_error_message(&unparseable);
        assert!(msg.contains("total_amount"), "应保留字段名: {msg}");
        assert!(!msg.contains("553.00"), "不得泄露字段值: {msg}");

        let malformed = ParseError::MalformedFormat {
            path: secret_path,
            format: "XML",
            detail: "元素未闭合".into(),
        };
        let msg = parse_error_message(&malformed);
        assert!(msg.contains("XML"));
        assert!(!msg.contains("someone"), "不得泄露路径: {msg}");
    }

    /// `pdf_extract` 对某些字体编码直接 `assert!(name == "Identity-H")`。
    /// 坐标与 flat-text 两条文本路径现在分别隔离 panic，任一路径失败后仍应
    /// 继续尝试 OCR；命令层的最外层防护还必须保证第三方库不能杀死进程。
    ///
    /// 夹具不随仓库分发（fixtures/samples 在 .gitignore 中），缺失时跳过。
    #[test]
    fn parse_invoice_contains_pdf_library_panic() {
        let sample = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../fixtures/samples/06-unknown-fbf5dc58.pdf");
        if !sample.is_file() {
            eprintln!("跳过：夹具缺失 06-unknown-fbf5dc58.pdf");
            return;
        }

        // panic 被拦截后必须是 Err 而不是进程崩溃；成功解析也算通过（说明上游已修）
        match parse_invoice(sample.display().to_string(), None) {
            Ok(_) => {}
            Err(e) => {
                assert_eq!(e.kind(), ErrorKind::Parse);
                assert!(
                    !e.message().contains("Identity-H"),
                    "不得外传 panic payload: {}",
                    e.message()
                );
            }
        }
    }

    #[test]
    fn parsed_dto_serializes_amounts_as_strings_in_camel_case() {
        use chrono::NaiveDate;
        use rust_decimal::Decimal;
        use std::str::FromStr;

        let parsed = ParsedInvoice {
            invoice_number: "24312000000012345678".into(),
            issue_date: NaiveDate::from_ymd_opt(2026, 7, 3).unwrap(),
            total_amount: Decimal::from_str("553.00").unwrap(),
            tax_amount: Some(Decimal::from_str("50.73").unwrap()),
            tax_rate: Some(Decimal::from_str("0.09").unwrap()),
            buyer_name: None,
            seller_name: None,
            ticket_type: ParseTicketType::Rail,
            transport_document_kind: Default::default(),
            parse_level: ParseLevel::L0,
            confidence: 1.0,
            city: None,
            travel_route: None,
            departure_time: None,
            checkin_date: None,
            source_path: std::path::PathBuf::from("/tmp/a.xml"),
        };

        let dto = ParsedInvoiceDto::from(parsed);

        let json = serde_json::to_value(dto).unwrap();
        // 金额必须是字符串，不能是 JSON number（否则前端拿到 f64）
        assert_eq!(json["total_amount"], "553.00");
        assert_eq!(json["tax_amount"], "50.73");
        assert_eq!(json["issue_date"], "2026-07-03");
        assert_eq!(json["ticket_type"], "rail");
        assert_eq!(json["parse_level"], "L0");
        assert_eq!(json["verification_result"], "");
        assert!(json["total_amount"].is_string());
        assert!(json["verification_result"].is_string());
    }

    #[test]
    fn invoice_dto_serializes_in_camel_case() {
        use chrono::{NaiveDate, Utc};
        use rust_decimal::Decimal;
        use std::str::FromStr;

        let record = ReportedInvoice {
            id: 7,
            batch_id: 3,
            invoice_number: "11111111111111111111".into(),
            issue_date: NaiveDate::from_ymd_opt(2026, 7, 10).unwrap(),
            amount: Decimal::from_str("100.50").unwrap(),
            tax_amount: None,
            buyer_name: None,
            seller_name: Some("某公司".into()),
            ticket_type: StoreTicketType::Hotel,
            city: None,
            departure_time: None,
            checkin_date: None,
            file_path: "/tmp/b.ofd".into(),
            created_at: Utc::now().naive_utc(),
            updated_at: Utc::now().naive_utc(),
            verification_result: Some("valid".into()),
            is_duplicate: false,
            duplicate_reason: None,
        };

        let json = serde_json::to_value(InvoiceDto::from(record)).unwrap();
        assert_eq!(json["id"], 7);
        assert_eq!(json["batch_id"], 3);
        assert_eq!(json["amount"], "100.50");
        assert_eq!(json["ticket_type"], "hotel");
        assert_eq!(json["file_path"], "/tmp/b.ofd");
        assert!(json["amount"].is_string());
        assert_eq!(json["verification_result"], "valid");
        assert_eq!(json["is_duplicate"], false);
        assert_eq!(json["is_excluded"], false);
        assert!(json["duplicate_reason"].is_null());
    }
}
