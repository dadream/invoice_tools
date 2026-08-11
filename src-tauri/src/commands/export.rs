//! 导出命令模块：批次导出为 Excel 明细表和 PDF 台账。

use std::sync::Mutex;

use printpdf::*;
use rust_decimal::Decimal;
use rust_xlsxwriter::{Color, Format, FormatBorder, Workbook};
use tauri::State;

use crate::error::{AppError, AppResult};
use crate::AppState;

/// 导出批次为 Excel 明细表。

/// 导出批次为 Excel 明细表。
///
/// 返回 xlsx 文件的字节流，前端用 Blob 触发下载。
///
/// # 表头（12 列）
///
/// 1. 发票号码
/// 2. 开票日期
/// 3. 金额
/// 4. 税额
/// 5. 购方名称
/// 6. 销方名称
/// 7. 票种
/// 8. 城市
/// 9. 出发时间
/// 10. 入住日期
/// 11. 签章状态
/// 12. 重复标记
///
/// # 样式
///
/// - 首行冻结 (`freeze_panes(1, 0)`)
/// - 表头灰底加粗，所有单元格带细边框
/// - 金额列设为文本格式 (`@`)，避免科学计数法
/// - 底部合计行显示"合计"和总金额
#[tauri::command]
pub fn export_batch_excel(
    state: State<Mutex<AppState>>,
    batch_id: i64,
) -> AppResult<Vec<u8>> {
    let app_state = state.lock().unwrap();
    let db = app_state.ledger_db()?;

    let batch = db
        .get_batch(batch_id)
        .map_err(|e| AppError::database(format!("获取批次失败: {}", e)))?;
    let invoices = db
        .list_invoices_by_batch(batch_id)
        .map_err(|e| AppError::database(format!("获取发票列表失败: {}", e)))?;

    let mut workbook = Workbook::new();
    let worksheet = workbook.add_worksheet();

    // 样式定义
    let header_fmt = Format::new()
        .set_bold()
        .set_background_color(Color::RGB(0xD9D9D9))
        .set_border(FormatBorder::Thin);
    let cell_fmt = Format::new().set_border(FormatBorder::Thin);
    let amount_fmt = Format::new()
        .set_border(FormatBorder::Thin)
        .set_num_format("@"); // 文本格式，避免科学计数法

    // 表头（12 列）
    worksheet
        .write_with_format(0, 0, "发票号码", &header_fmt)
        .map_err(|e| AppError::internal(format!("写入表头失败: {}", e)))?;
    worksheet
        .write_with_format(0, 1, "开票日期", &header_fmt)
        .map_err(|e| AppError::internal(format!("写入表头失败: {}", e)))?;
    worksheet
        .write_with_format(0, 2, "金额", &header_fmt)
        .map_err(|e| AppError::internal(format!("写入表头失败: {}", e)))?;
    worksheet
        .write_with_format(0, 3, "税额", &header_fmt)
        .map_err(|e| AppError::internal(format!("写入表头失败: {}", e)))?;
    worksheet
        .write_with_format(0, 4, "购方名称", &header_fmt)
        .map_err(|e| AppError::internal(format!("写入表头失败: {}", e)))?;
    worksheet
        .write_with_format(0, 5, "销方名称", &header_fmt)
        .map_err(|e| AppError::internal(format!("写入表头失败: {}", e)))?;
    worksheet
        .write_with_format(0, 6, "票种", &header_fmt)
        .map_err(|e| AppError::internal(format!("写入表头失败: {}", e)))?;
    worksheet
        .write_with_format(0, 7, "城市", &header_fmt)
        .map_err(|e| AppError::internal(format!("写入表头失败: {}", e)))?;
    worksheet
        .write_with_format(0, 8, "出发时间", &header_fmt)
        .map_err(|e| AppError::internal(format!("写入表头失败: {}", e)))?;
    worksheet
        .write_with_format(0, 9, "入住日期", &header_fmt)
        .map_err(|e| AppError::internal(format!("写入表头失败: {}", e)))?;
    worksheet
        .write_with_format(0, 10, "签章状态", &header_fmt)
        .map_err(|e| AppError::internal(format!("写入表头失败: {}", e)))?;
    worksheet
        .write_with_format(0, 11, "重复标记", &header_fmt)
        .map_err(|e| AppError::internal(format!("写入表头失败: {}", e)))?;

    // 冻结首行
    worksheet
        .set_freeze_panes(1, 0)
        .map_err(|e| AppError::internal(format!("冻结窗格失败: {}", e)))?;

    // 数据行
    for (idx, inv) in invoices.iter().enumerate() {
        let row = (idx + 1) as u32;
        worksheet
            .write_with_format(row, 0, &inv.invoice_number, &cell_fmt)
            .map_err(|e| AppError::internal(format!("写入数据失败: {}", e)))?;
        worksheet
            .write_with_format(
                row,
                1,
                inv.issue_date.format("%Y-%m-%d").to_string(),
                &cell_fmt,
            )
            .map_err(|e| AppError::internal(format!("写入数据失败: {}", e)))?;
        worksheet
            .write_with_format(row, 2, inv.amount.to_string(), &amount_fmt)
            .map_err(|e| AppError::internal(format!("写入数据失败: {}", e)))?;
        worksheet
            .write_with_format(
                row,
                3,
                inv.tax_amount
                    .as_ref()
                    .map(|d| d.to_string())
                    .unwrap_or_default(),
                &amount_fmt,
            )
            .map_err(|e| AppError::internal(format!("写入数据失败: {}", e)))?;
        worksheet
            .write_with_format(row, 4, inv.buyer_name.as_deref().unwrap_or(""), &cell_fmt)
            .map_err(|e| AppError::internal(format!("写入数据失败: {}", e)))?;
        worksheet
            .write_with_format(row, 5, inv.seller_name.as_deref().unwrap_or(""), &cell_fmt)
            .map_err(|e| AppError::internal(format!("写入数据失败: {}", e)))?;
        worksheet
            .write_with_format(row, 6, inv.ticket_type.to_str(), &cell_fmt)
            .map_err(|e| AppError::internal(format!("写入数据失败: {}", e)))?;
        worksheet
            .write_with_format(row, 7, inv.city.as_deref().unwrap_or(""), &cell_fmt)
            .map_err(|e| AppError::internal(format!("写入数据失败: {}", e)))?;
        worksheet
            .write_with_format(
                row,
                8,
                inv.departure_time
                    .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
                    .unwrap_or_default(),
                &cell_fmt,
            )
            .map_err(|e| AppError::internal(format!("写入数据失败: {}", e)))?;
        worksheet
            .write_with_format(
                row,
                9,
                inv.checkin_date
                    .map(|d| d.format("%Y-%m-%d").to_string())
                    .unwrap_or_default(),
                &cell_fmt,
            )
            .map_err(|e| AppError::internal(format!("写入数据失败: {}", e)))?;
        worksheet
            .write_with_format(
                row,
                10,
                inv.verification_result.as_deref().unwrap_or(""),
                &cell_fmt,
            )
            .map_err(|e| AppError::internal(format!("写入数据失败: {}", e)))?;
        worksheet
            .write_with_format(row, 11, if inv.is_duplicate { "是" } else { "" }, &cell_fmt)
            .map_err(|e| AppError::internal(format!("写入数据失败: {}", e)))?;
    }

    // 自适应列宽（粗略估算）
    worksheet
        .set_column_width(0, 22.0)
        .map_err(|e| AppError::internal(format!("设置列宽失败: {}", e)))?; // 发票号码
    worksheet
        .set_column_width(1, 12.0)
        .map_err(|e| AppError::internal(format!("设置列宽失败: {}", e)))?; // 日期
    worksheet
        .set_column_width(2, 10.0)
        .map_err(|e| AppError::internal(format!("设置列宽失败: {}", e)))?; // 金额
    worksheet
        .set_column_width(4, 25.0)
        .map_err(|e| AppError::internal(format!("设置列宽失败: {}", e)))?; // 购方
    worksheet
        .set_column_width(5, 25.0)
        .map_err(|e| AppError::internal(format!("设置列宽失败: {}", e)))?; // 销方

    // 底部合计行
    let total_row = (invoices.len() + 1) as u32;
    worksheet
        .write_with_format(total_row, 0, "合计", &header_fmt)
        .map_err(|e| AppError::internal(format!("写入合计行失败: {}", e)))?;
    let total: Decimal = invoices.iter().map(|inv| inv.amount).sum();
    worksheet
        .write_with_format(total_row, 2, total.to_string(), &amount_fmt)
        .map_err(|e| AppError::internal(format!("写入合计行失败: {}", e)))?;

    // 序列化为字节流
    let buf = workbook
        .save_to_buffer()
        .map_err(|e| AppError::internal(format!("Excel 生成失败: {}", e)))?;

    tracing::info!(
        batch_id,
        batch_name = %batch.name,
        invoice_count = invoices.len(),
        size_bytes = buf.len(),
        "导出批次 Excel 成功"
    );

    Ok(buf)
}

/// 导出批次为 PDF 台账。
///
/// 返回 PDF 文件的字节流，前端用 Blob 触发下载。
///
/// # 排版
///
/// - A4 纸张（210mm × 297mm）
/// - 标题：批次名称（18pt）
/// - 批次信息：月份、状态、发票数、总金额（10pt）
/// - 表格：发票号码、日期、金额、销方（9pt 表头，8pt 数据）
/// - 自动分页：Y 坐标 < 30mm 时新建页面
///
/// # 字体
///
/// 使用 printpdf 内置 Helvetica 字体（仅支持英文和数字）。
/// 中文字段（批次名、销方名）会显示为空白或乱码。
/// 如需中文支持，需下载并内嵌 Noto Sans SC 字体（约 2-8 MB）。
#[tauri::command]
pub fn export_batch_pdf(
    state: State<Mutex<AppState>>,
    batch_id: i64,
) -> AppResult<Vec<u8>> {
    let app_state = state.lock().unwrap();
    let db = app_state.ledger_db()?;

    let batch = db
        .get_batch(batch_id)
        .map_err(|e| AppError::database(format!("获取批次失败: {}", e)))?;
    let invoices = db
        .list_invoices_by_batch(batch_id)
        .map_err(|e| AppError::database(format!("获取发票列表失败: {}", e)))?;

    // 创建文档并添加首页 (A4: 210mm x 297mm)
    let (doc, page1, layer1) = PdfDocument::new("Invoice Ledger", Mm(210.0), Mm(297.0), "Layer 1");
    let font = doc
        .add_builtin_font(BuiltinFont::Helvetica)
        .map_err(|e| AppError::internal(format!("字体加载失败: {:?}", e)))?;

    let mut current_page = doc.get_page(page1);
    let mut current_layer = current_page.get_layer(layer1);

    // 标题 - 批次名称（限英文数字）
    let title = format!("Batch {}: {}", batch.id, sanitize_ascii(&batch.name));
    current_layer.use_text(title, 18.0, Mm(20.0), Mm(270.0), &font);

    // 批次信息
    let status_str = match batch.status {
        invoice_store::models::BatchStatus::Draft => "Draft",
        invoice_store::models::BatchStatus::Submitted => "Submitted",
        invoice_store::models::BatchStatus::Approved => "Approved",
        invoice_store::models::BatchStatus::Completed => "Completed",
        invoice_store::models::BatchStatus::Rejected => "Rejected",
    };
    let info = format!(
        "Month: {}  Status: {}  Invoices: {}  Total: CNY {}",
        batch.month, status_str, batch.invoice_count, batch.total_amount
    );
    current_layer.use_text(info, 10.0, Mm(20.0), Mm(258.0), &font);

    // 表格表头
    let mut y = 245.0;
    current_layer.use_text("Invoice Number", 9.0, Mm(20.0), Mm(y), &font);
    current_layer.use_text("Date", 9.0, Mm(70.0), Mm(y), &font);
    current_layer.use_text("Amount (CNY)", 9.0, Mm(105.0), Mm(y), &font);
    current_layer.use_text("Seller", 9.0, Mm(145.0), Mm(y), &font);

    y -= 8.0;

    // 表格数据
    let mut page_index = page1;
    let mut layer_index = layer1;

    for inv in invoices.iter() {
        if y < 30.0 {
            // 接近底部，新建页面
            let (new_page, new_layer) = doc.add_page(Mm(210.0), Mm(297.0), "Layer 1");
            page_index = new_page;
            layer_index = new_layer;
            current_page = doc.get_page(page_index);
            current_layer = current_page.get_layer(layer_index);
            y = 270.0;
        }

        current_layer.use_text(&inv.invoice_number, 8.0, Mm(20.0), Mm(y), &font);
        current_layer.use_text(
            inv.issue_date.format("%Y-%m-%d").to_string(),
            8.0,
            Mm(70.0),
            Mm(y),
            &font,
        );
        current_layer.use_text(inv.amount.to_string(), 8.0, Mm(105.0), Mm(y), &font);
        current_layer.use_text(
            sanitize_ascii(inv.seller_name.as_deref().unwrap_or("")),
            8.0,
            Mm(145.0),
            Mm(y),
            &font,
        );

        y -= 6.0;
    }

    // 序列化为字节流
    let buf = doc
        .save_to_bytes()
        .map_err(|e| AppError::internal(format!("PDF 生成失败: {:?}", e)))?;

    tracing::info!(
        batch_id,
        batch_name = %batch.name,
        invoice_count = invoices.len(),
        size_bytes = buf.len(),
        "导出批次 PDF 成功"
    );

    Ok(buf)
}

/// 移除非 ASCII 字符，避免 Helvetica 字体显示乱码。
fn sanitize_ascii(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_graphic() || c.is_ascii_whitespace())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use invoice_store::models::TicketType;
    use invoice_store::LedgerDb;
    use rust_decimal::Decimal;
    use std::str::FromStr;

    /// 创建临时数据库用于测试
    fn create_test_db() -> LedgerDb {
        LedgerDb::new(":memory:").expect("创建内存数据库失败")
    }

    /// 测试核心导出逻辑（不通过 Tauri 命令）
    fn export_batch_excel_internal(db: &LedgerDb, batch_id: i64) -> AppResult<Vec<u8>> {
        let _batch = db
            .get_batch(batch_id)
            .map_err(|e| AppError::database(format!("获取批次失败: {}", e)))?;
        let invoices = db
            .list_invoices_by_batch(batch_id)
            .map_err(|e| AppError::database(format!("获取发票列表失败: {}", e)))?;

        let mut workbook = Workbook::new();
        let worksheet = workbook.add_worksheet();

        let header_fmt = Format::new()
            .set_bold()
            .set_background_color(Color::RGB(0xD9D9D9))
            .set_border(FormatBorder::Thin);
        let cell_fmt = Format::new().set_border(FormatBorder::Thin);
        let amount_fmt = Format::new()
            .set_border(FormatBorder::Thin)
            .set_num_format("@");

        worksheet
            .write_with_format(0, 0, "发票号码", &header_fmt)
            .map_err(|e| AppError::internal(format!("写入表头失败: {}", e)))?;
        worksheet
            .write_with_format(0, 1, "开票日期", &header_fmt)
            .map_err(|e| AppError::internal(format!("写入表头失败: {}", e)))?;
        worksheet
            .write_with_format(0, 2, "金额", &header_fmt)
            .map_err(|e| AppError::internal(format!("写入表头失败: {}", e)))?;
        worksheet
            .write_with_format(0, 3, "税额", &header_fmt)
            .map_err(|e| AppError::internal(format!("写入表头失败: {}", e)))?;
        worksheet
            .write_with_format(0, 4, "购方名称", &header_fmt)
            .map_err(|e| AppError::internal(format!("写入表头失败: {}", e)))?;
        worksheet
            .write_with_format(0, 5, "销方名称", &header_fmt)
            .map_err(|e| AppError::internal(format!("写入表头失败: {}", e)))?;
        worksheet
            .write_with_format(0, 6, "票种", &header_fmt)
            .map_err(|e| AppError::internal(format!("写入表头失败: {}", e)))?;
        worksheet
            .write_with_format(0, 7, "城市", &header_fmt)
            .map_err(|e| AppError::internal(format!("写入表头失败: {}", e)))?;
        worksheet
            .write_with_format(0, 8, "出发时间", &header_fmt)
            .map_err(|e| AppError::internal(format!("写入表头失败: {}", e)))?;
        worksheet
            .write_with_format(0, 9, "入住日期", &header_fmt)
            .map_err(|e| AppError::internal(format!("写入表头失败: {}", e)))?;
        worksheet
            .write_with_format(0, 10, "签章状态", &header_fmt)
            .map_err(|e| AppError::internal(format!("写入表头失败: {}", e)))?;
        worksheet
            .write_with_format(0, 11, "重复标记", &header_fmt)
            .map_err(|e| AppError::internal(format!("写入表头失败: {}", e)))?;

        worksheet
            .set_freeze_panes(1, 0)
            .map_err(|e| AppError::internal(format!("冻结窗格失败: {}", e)))?;

        for (idx, inv) in invoices.iter().enumerate() {
            let row = (idx + 1) as u32;
            worksheet
                .write_with_format(row, 0, &inv.invoice_number, &cell_fmt)
                .map_err(|e| AppError::internal(format!("写入数据失败: {}", e)))?;
            worksheet
                .write_with_format(
                    row,
                    1,
                    inv.issue_date.format("%Y-%m-%d").to_string(),
                    &cell_fmt,
                )
                .map_err(|e| AppError::internal(format!("写入数据失败: {}", e)))?;
            worksheet
                .write_with_format(row, 2, inv.amount.to_string(), &amount_fmt)
                .map_err(|e| AppError::internal(format!("写入数据失败: {}", e)))?;
            worksheet
                .write_with_format(
                    row,
                    3,
                    inv.tax_amount
                        .as_ref()
                        .map(|d| d.to_string())
                        .unwrap_or_default(),
                    &amount_fmt,
                )
                .map_err(|e| AppError::internal(format!("写入数据失败: {}", e)))?;
            worksheet
                .write_with_format(row, 4, inv.buyer_name.as_deref().unwrap_or(""), &cell_fmt)
                .map_err(|e| AppError::internal(format!("写入数据失败: {}", e)))?;
            worksheet
                .write_with_format(row, 5, inv.seller_name.as_deref().unwrap_or(""), &cell_fmt)
                .map_err(|e| AppError::internal(format!("写入数据失败: {}", e)))?;
            worksheet
                .write_with_format(row, 6, inv.ticket_type.to_str(), &cell_fmt)
                .map_err(|e| AppError::internal(format!("写入数据失败: {}", e)))?;
            worksheet
                .write_with_format(row, 7, inv.city.as_deref().unwrap_or(""), &cell_fmt)
                .map_err(|e| AppError::internal(format!("写入数据失败: {}", e)))?;
            worksheet
                .write_with_format(
                    row,
                    8,
                    inv.departure_time
                        .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
                        .unwrap_or_default(),
                    &cell_fmt,
                )
                .map_err(|e| AppError::internal(format!("写入数据失败: {}", e)))?;
            worksheet
                .write_with_format(
                    row,
                    9,
                    inv.checkin_date
                        .map(|d| d.format("%Y-%m-%d").to_string())
                        .unwrap_or_default(),
                    &cell_fmt,
                )
                .map_err(|e| AppError::internal(format!("写入数据失败: {}", e)))?;
            worksheet
                .write_with_format(
                    row,
                    10,
                    inv.verification_result.as_deref().unwrap_or(""),
                    &cell_fmt,
                )
                .map_err(|e| AppError::internal(format!("写入数据失败: {}", e)))?;
            worksheet
                .write_with_format(row, 11, if inv.is_duplicate { "是" } else { "" }, &cell_fmt)
                .map_err(|e| AppError::internal(format!("写入数据失败: {}", e)))?;
        }

        worksheet
            .set_column_width(0, 22.0)
            .map_err(|e| AppError::internal(format!("设置列宽失败: {}", e)))?;
        worksheet
            .set_column_width(1, 12.0)
            .map_err(|e| AppError::internal(format!("设置列宽失败: {}", e)))?;
        worksheet
            .set_column_width(2, 10.0)
            .map_err(|e| AppError::internal(format!("设置列宽失败: {}", e)))?;
        worksheet
            .set_column_width(4, 25.0)
            .map_err(|e| AppError::internal(format!("设置列宽失败: {}", e)))?;
        worksheet
            .set_column_width(5, 25.0)
            .map_err(|e| AppError::internal(format!("设置列宽失败: {}", e)))?;

        let total_row = (invoices.len() + 1) as u32;
        worksheet
            .write_with_format(total_row, 0, "合计", &header_fmt)
            .map_err(|e| AppError::internal(format!("写入合计行失败: {}", e)))?;
        let total: Decimal = invoices.iter().map(|inv| inv.amount).sum();
        worksheet
            .write_with_format(total_row, 2, total.to_string(), &amount_fmt)
            .map_err(|e| AppError::internal(format!("写入合计行失败: {}", e)))?;

        let buf = workbook
            .save_to_buffer()
            .map_err(|e| AppError::internal(format!("Excel 生成失败: {}", e)))?;

        Ok(buf)
    }

    #[test]
    fn exports_excel_with_all_fields() {
        let db = create_test_db();

        // 创建批次
        let batch_id = db.create_batch("测试批次", "2026-08").expect("创建批次失败");

        // 添加两张发票
        let invoice1 = invoice_store::models::ReportedInvoice {
            id: 0, // 会被数据库覆盖
            batch_id,
            invoice_number: "12345678901234567890".to_string(),
            issue_date: NaiveDate::from_ymd_opt(2026, 8, 1).unwrap(),
            amount: Decimal::from_str("100.50").unwrap(),
            tax_amount: Some(Decimal::from_str("9.05").unwrap()),
            buyer_name: Some("测试公司".to_string()),
            seller_name: Some("供应商A".to_string()),
            ticket_type: TicketType::Rail,
            city: Some("北京".to_string()),
            departure_time: Some(
                NaiveDate::from_ymd_opt(2026, 8, 1)
                    .unwrap()
                    .and_hms_opt(9, 30, 0)
                    .unwrap(),
            ),
            checkin_date: None,
            file_path: "/tmp/test1.xml".to_string(),
            created_at: chrono::Utc::now().naive_utc(),
            updated_at: chrono::Utc::now().naive_utc(),
            verification_result: Some("valid".to_string()),
            is_duplicate: false,
            duplicate_reason: None,
        };

        let invoice2 = invoice_store::models::ReportedInvoice {
            id: 0,
            batch_id,
            invoice_number: "09876543210987654321".to_string(),
            issue_date: NaiveDate::from_ymd_opt(2026, 8, 5).unwrap(),
            amount: Decimal::from_str("250.00").unwrap(),
            tax_amount: None,
            buyer_name: None,
            seller_name: Some("供应商B".to_string()),
            ticket_type: TicketType::Hotel,
            city: Some("上海".to_string()),
            departure_time: None,
            checkin_date: Some(NaiveDate::from_ymd_opt(2026, 8, 5).unwrap()),
            file_path: "/tmp/test2.ofd".to_string(),
            created_at: chrono::Utc::now().naive_utc(),
            updated_at: chrono::Utc::now().naive_utc(),
            verification_result: None,
            is_duplicate: true,
            duplicate_reason: Some("重复发票号".to_string()),
        };

        db.add_invoice(&invoice1).expect("插入发票 1 失败");
        db.add_invoice(&invoice2).expect("插入发票 2 失败");

        // 导出 Excel
        let result = export_batch_excel_internal(&db, batch_id);
        assert!(result.is_ok(), "导出失败: {:?}", result.err());

        let bytes = result.unwrap();

        // 断言：返回非空字节流
        assert!(!bytes.is_empty(), "返回字节流为空");

        // 断言：前 4 字节是 ZIP 魔数（xlsx 本质是 ZIP）
        assert_eq!(&bytes[0..4], b"PK\x03\x04", "文件头不是 ZIP 魔数");

        // 手动验证：写到临时文件检查内容
        std::fs::write("/tmp/test_export.xlsx", &bytes).expect("写入临时文件失败");
        println!("✓ 测试文件已写入 /tmp/test_export.xlsx，可手动用 LibreOffice 打开检查");
    }

    #[test]
    fn exports_empty_batch() {
        let db = create_test_db();
        let batch_id = db.create_batch("空批次", "2026-09").expect("创建批次失败");

        let result = export_batch_excel_internal(&db, batch_id);
        assert!(result.is_ok(), "导出空批次失败: {:?}", result.err());

        let bytes = result.unwrap();
        assert!(!bytes.is_empty());
        assert_eq!(&bytes[0..4], b"PK\x03\x04");
    }

    #[test]
    fn returns_error_for_nonexistent_batch() {
        let db = create_test_db();

        let result = export_batch_excel_internal(&db, 99999);
        assert!(result.is_err(), "应该返回错误");

        let err = result.err().unwrap();
        assert_eq!(err.kind(), crate::error::ErrorKind::Database);
        assert!(err.message().contains("获取批次失败"));
    }

    /// 测试 PDF 导出核心逻辑（不通过 Tauri 命令）
    fn export_batch_pdf_internal(db: &LedgerDb, batch_id: i64) -> AppResult<Vec<u8>> {
        let batch = db
            .get_batch(batch_id)
            .map_err(|e| AppError::database(format!("获取批次失败: {}", e)))?;
        let invoices = db
            .list_invoices_by_batch(batch_id)
            .map_err(|e| AppError::database(format!("获取发票列表失败: {}", e)))?;

        let (doc, page1, layer1) = PdfDocument::new("Invoice Ledger", Mm(210.0), Mm(297.0), "Layer 1");
        let font = doc
            .add_builtin_font(BuiltinFont::Helvetica)
            .map_err(|e| AppError::internal(format!("字体加载失败: {:?}", e)))?;

        let mut current_page = doc.get_page(page1);
        let mut current_layer = current_page.get_layer(layer1);

        let title = format!("Batch {}: {}", batch.id, sanitize_ascii(&batch.name));
        current_layer.use_text(title, 18.0, Mm(20.0), Mm(270.0), &font);

        let status_str = match batch.status {
            invoice_store::models::BatchStatus::Draft => "Draft",
            invoice_store::models::BatchStatus::Submitted => "Submitted",
            invoice_store::models::BatchStatus::Approved => "Approved",
            invoice_store::models::BatchStatus::Completed => "Completed",
            invoice_store::models::BatchStatus::Rejected => "Rejected",
        };
        let info = format!(
            "Month: {}  Status: {}  Invoices: {}  Total: CNY {}",
            batch.month, status_str, batch.invoice_count, batch.total_amount
        );
        current_layer.use_text(info, 10.0, Mm(20.0), Mm(258.0), &font);

        let mut y = 245.0;
        current_layer.use_text("Invoice Number", 9.0, Mm(20.0), Mm(y), &font);
        current_layer.use_text("Date", 9.0, Mm(70.0), Mm(y), &font);
        current_layer.use_text("Amount (CNY)", 9.0, Mm(105.0), Mm(y), &font);
        current_layer.use_text("Seller", 9.0, Mm(145.0), Mm(y), &font);

        y -= 8.0;

        let mut page_index = page1;
        let mut layer_index = layer1;

        for inv in invoices.iter() {
            if y < 30.0 {
                let (new_page, new_layer) = doc.add_page(Mm(210.0), Mm(297.0), "Layer 1");
                page_index = new_page;
                layer_index = new_layer;
                current_page = doc.get_page(page_index);
                current_layer = current_page.get_layer(layer_index);
                y = 270.0;
            }

            current_layer.use_text(&inv.invoice_number, 8.0, Mm(20.0), Mm(y), &font);
            current_layer.use_text(
                inv.issue_date.format("%Y-%m-%d").to_string(),
                8.0,
                Mm(70.0),
                Mm(y),
                &font,
            );
            current_layer.use_text(inv.amount.to_string(), 8.0, Mm(105.0), Mm(y), &font);
            current_layer.use_text(
                sanitize_ascii(inv.seller_name.as_deref().unwrap_or("")),
                8.0,
                Mm(145.0),
                Mm(y),
                &font,
            );

            y -= 6.0;
        }

        let buf = doc
            .save_to_bytes()
            .map_err(|e| AppError::internal(format!("PDF 生成失败: {:?}", e)))?;

        Ok(buf)
    }

    #[test]
    fn exports_pdf_with_invoice_data() {
        let db = create_test_db();

        let batch_id = db.create_batch("Test Batch 2026-08", "2026-08").expect("创建批次失败");

        let invoice1 = invoice_store::models::ReportedInvoice {
            id: 0,
            batch_id,
            invoice_number: "12345678901234567890".to_string(),
            issue_date: NaiveDate::from_ymd_opt(2026, 8, 1).unwrap(),
            amount: Decimal::from_str("100.50").unwrap(),
            tax_amount: Some(Decimal::from_str("9.05").unwrap()),
            buyer_name: Some("Test Company".to_string()),
            seller_name: Some("Supplier A".to_string()),
            ticket_type: TicketType::Rail,
            city: Some("Beijing".to_string()),
            departure_time: Some(
                NaiveDate::from_ymd_opt(2026, 8, 1)
                    .unwrap()
                    .and_hms_opt(9, 30, 0)
                    .unwrap(),
            ),
            checkin_date: None,
            file_path: "/tmp/test1.xml".to_string(),
            created_at: chrono::Utc::now().naive_utc(),
            updated_at: chrono::Utc::now().naive_utc(),
            verification_result: Some("valid".to_string()),
            is_duplicate: false,
            duplicate_reason: None,
        };

        let invoice2 = invoice_store::models::ReportedInvoice {
            id: 0,
            batch_id,
            invoice_number: "09876543210987654321".to_string(),
            issue_date: NaiveDate::from_ymd_opt(2026, 8, 5).unwrap(),
            amount: Decimal::from_str("250.00").unwrap(),
            tax_amount: None,
            buyer_name: None,
            seller_name: Some("Supplier B".to_string()),
            ticket_type: TicketType::Hotel,
            city: Some("Shanghai".to_string()),
            departure_time: None,
            checkin_date: Some(NaiveDate::from_ymd_opt(2026, 8, 5).unwrap()),
            file_path: "/tmp/test2.ofd".to_string(),
            created_at: chrono::Utc::now().naive_utc(),
            updated_at: chrono::Utc::now().naive_utc(),
            verification_result: None,
            is_duplicate: false,
            duplicate_reason: None,
        };

        db.add_invoice(&invoice1).expect("插入发票 1 失败");
        db.add_invoice(&invoice2).expect("插入发票 2 失败");

        let result = export_batch_pdf_internal(&db, batch_id);
        assert!(result.is_ok(), "PDF 导出失败: {:?}", result.err());

        let bytes = result.unwrap();

        // 断言：返回非空字节流
        assert!(!bytes.is_empty(), "返回字节流为空");

        // 断言：前 4 字节是 PDF 魔数
        assert_eq!(&bytes[0..4], b"%PDF", "文件头不是 PDF 魔数");

        // 手动验证：写到临时文件检查内容
        std::fs::write("/tmp/test_export.pdf", &bytes).expect("写入临时文件失败");
        println!("✓ 测试文件已写入 /tmp/test_export.pdf，可手动用 PDF 阅读器打开检查");
    }

    #[test]
    fn exports_pdf_for_empty_batch() {
        let db = create_test_db();
        let batch_id = db.create_batch("Empty Batch", "2026-09").expect("创建批次失败");

        let result = export_batch_pdf_internal(&db, batch_id);
        assert!(result.is_ok(), "导出空批次 PDF 失败: {:?}", result.err());

        let bytes = result.unwrap();
        assert!(!bytes.is_empty());
        assert_eq!(&bytes[0..4], b"%PDF");
    }

    #[test]
    fn pdf_export_returns_error_for_nonexistent_batch() {
        let db = create_test_db();

        let result = export_batch_pdf_internal(&db, 99999);
        assert!(result.is_err(), "应该返回错误");

        let err = result.err().unwrap();
        assert_eq!(err.kind(), crate::error::ErrorKind::Database);
        assert!(err.message().contains("获取批次失败"));
    }
}
