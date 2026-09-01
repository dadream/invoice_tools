//! 导出命令模块：批次导出为 Excel 明细表和 PDF 台账。

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use invoice_store::models::BatchStatus;
use printpdf::*;
use rust_decimal::Decimal;
use rust_xlsxwriter::{Color, Format, FormatBorder, Workbook};
use serde::Serialize;
use tauri::State;
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::AppState;

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
pub fn export_batch_excel(state: State<Mutex<AppState>>, batch_id: i64) -> AppResult<Vec<u8>> {
    let app_state = state.lock().unwrap();
    let db = app_state.ledger_db()?;

    let batch = db
        .get_batch(batch_id)
        .map_err(|e| AppError::database(format!("获取批次失败: {}", e)))?;
    let (_, expenses) = db
        .get_active_snapshot_expenses(batch_id)
        .map_err(|e| AppError::validation(format!("请先完成审核并生成有效版本：{e}")))?;
    let (_, invoices) = db
        .get_active_snapshot_invoices(batch_id)
        .map_err(|e| AppError::validation(format!("请先完成审核并生成有效版本：{e}")))?;
    let task = db
        .start_delivery_task(batch_id, "excel")
        .map_err(|e| AppError::database(format!("创建 Excel 交付任务失败: {e}")))?;

    match build_expense_excel_bytes(&batch, &expenses, &invoices) {
        Ok(bytes) => {
            db.finish_delivery_task(task.id, None, None)
                .map_err(|e| AppError::database(format!("记录 Excel 交付结果失败: {e}")))?;
            Ok(bytes)
        }
        Err(error) => {
            let message = error.to_string();
            let _ = db.finish_delivery_task(task.id, None, Some(&message));
            Err(error)
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ExcelExportResult {
    pub path: String,
    pub bytes: u64,
}

/// 将冻结审核版本写入用户明确选择的 `.xlsx` 路径。
///
/// 先在同目录写入临时文件并同步，再替换目标文件，避免生成成功提示对应半个文件。
#[tauri::command]
pub fn export_batch_excel_to_path(
    state: State<Mutex<AppState>>,
    batch_id: i64,
    destination_path: String,
) -> AppResult<ExcelExportResult> {
    let destination = validate_excel_destination(&destination_path)?;
    let app_state = state
        .lock()
        .map_err(|_| AppError::internal("应用状态锁不可用"))?;
    let db = app_state.ledger_db()?;
    let batch = db
        .get_batch(batch_id)
        .map_err(|error| AppError::database(format!("获取批次失败: {error}")))?;
    let (_, expenses) = db
        .get_active_snapshot_expenses(batch_id)
        .map_err(|error| AppError::validation(format!("请先完成审核并生成有效版本：{error}")))?;
    let (_, invoices) = db
        .get_active_snapshot_invoices(batch_id)
        .map_err(|error| AppError::validation(format!("请先完成审核并生成有效版本：{error}")))?;
    let task = db
        .start_delivery_task(batch_id, "excel")
        .map_err(|error| AppError::database(format!("创建 Excel 交付任务失败: {error}")))?;

    let result = build_expense_excel_bytes(&batch, &expenses, &invoices)
        .and_then(|bytes| write_excel_atomically(&destination, &bytes).map(|()| bytes.len()));
    match result {
        Ok(bytes) => {
            let output_path = destination.to_string_lossy().into_owned();
            db.finish_delivery_task(task.id, Some(&output_path), None)
                .map_err(|error| AppError::database(format!("记录 Excel 交付结果失败: {error}")))?;
            Ok(ExcelExportResult {
                path: output_path,
                bytes: u64::try_from(bytes)
                    .map_err(|_| AppError::internal("Excel 文件大小超出支持范围"))?,
            })
        }
        Err(error) => {
            let message = error.to_string();
            let _ = db.finish_delivery_task(task.id, None, Some(&message));
            Err(error)
        }
    }
}

fn validate_excel_destination(raw: &str) -> AppResult<PathBuf> {
    let path = PathBuf::from(raw.trim());
    if !path.is_absolute()
        || !path
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|value| value.eq_ignore_ascii_case("xlsx"))
    {
        return Err(AppError::validation("请选择绝对路径并使用 .xlsx 扩展名"));
    }
    let parent = path
        .parent()
        .ok_or_else(|| AppError::validation("Excel 保存目录无效"))?;
    let parent_metadata = fs::symlink_metadata(parent)
        .map_err(|error| AppError::io(format!("读取 Excel 保存目录失败（{}）", error.kind())))?;
    if !parent_metadata.is_dir() || parent_metadata.file_type().is_symlink() {
        return Err(AppError::validation("Excel 保存目录必须是本地普通文件夹"));
    }
    if path.exists() {
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| AppError::io(format!("读取目标 Excel 失败（{}）", error.kind())))?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(AppError::validation("目标 Excel 必须是普通文件"));
        }
    }
    Ok(path)
}

fn write_excel_atomically(destination: &Path, bytes: &[u8]) -> AppResult<()> {
    let parent = destination
        .parent()
        .ok_or_else(|| AppError::validation("Excel 保存目录无效"))?;
    let file_name = destination
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| AppError::validation("Excel 文件名无效"))?;
    let nonce = Uuid::new_v4();
    let staged = parent.join(format!(".{file_name}.{nonce}.tmp"));
    let backup = parent.join(format!(".{file_name}.{nonce}.bak"));
    let write_result = (|| -> AppResult<()> {
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&staged)
            .map_err(|error| {
                AppError::io(format!("创建 Excel 临时文件失败（{}）", error.kind()))
            })?;
        file.write_all(bytes)
            .map_err(|error| AppError::io(format!("写入 Excel 失败（{}）", error.kind())))?;
        file.sync_all()
            .map_err(|error| AppError::io(format!("同步 Excel 失败（{}）", error.kind())))?;
        drop(file);

        if destination.exists() {
            fs::rename(destination, &backup).map_err(|error| {
                AppError::io(format!("准备替换旧 Excel 失败（{}）", error.kind()))
            })?;
        }
        if let Err(error) = fs::rename(&staged, destination) {
            if backup.exists() {
                let _ = fs::rename(&backup, destination);
            }
            return Err(AppError::io(format!("保存 Excel 失败（{}）", error.kind())));
        }
        if backup.exists() {
            fs::remove_file(&backup)
                .map_err(|error| AppError::io(format!("清理旧 Excel 失败（{}）", error.kind())))?;
        }
        Ok(())
    })();
    if staged.exists() {
        let _ = fs::remove_file(&staged);
    }
    write_result
}

/// 按冻结的稳定 `ExpenseItem` 输出 Excel；Concur 映射与目标字段不会进入该文件。
pub fn build_expense_excel_bytes(
    batch: &invoice_store::models::Batch,
    expenses: &[invoice_store::models::ExpenseItem],
    invoices: &[invoice_store::models::ReportedInvoice],
) -> AppResult<Vec<u8>> {
    let mut workbook = Workbook::new();
    let header = Format::new()
        .set_bold()
        .set_background_color(Color::RGB(0xD9E7DF))
        .set_border(FormatBorder::Thin);
    let cell = Format::new().set_border(FormatBorder::Thin);
    let amount = Format::new()
        .set_border(FormatBorder::Thin)
        .set_num_format("@");
    let invoices_by_id = invoices
        .iter()
        .map(|invoice| (invoice.id, invoice))
        .collect::<HashMap<_, _>>();

    {
        let worksheet = workbook.add_worksheet();
        worksheet
            .set_name("费用清单")
            .map_err(|e| AppError::internal(format!("设置工作表名称失败: {e}")))?;
        let headers = [
            "费用ID",
            "发票号码",
            "开票日期",
            "费用分类",
            "实际发生日期",
            "日期已确认",
            "费用说明",
            "交易对方",
            "城市",
            "省份",
            "付款方式",
            "实际金额",
            "币种",
            "票面税额",
            "票面税率",
            "行程组ID",
            "材料数",
        ];
        for (column, value) in headers.iter().enumerate() {
            worksheet
                .write_with_format(0, column as u16, *value, &header)
                .map_err(|e| AppError::internal(format!("写入费用表头失败: {e}")))?;
        }
        worksheet
            .set_freeze_panes(1, 0)
            .map_err(|e| AppError::internal(format!("冻结窗格失败: {e}")))?;
        for (index, expense) in expenses.iter().enumerate() {
            let row = (index + 1) as u32;
            let invoice = invoices_by_id.get(&expense.primary_invoice_id).copied();
            let tax_amount = expense
                .tax_details
                .iter()
                .fold(Decimal::ZERO, |sum, tax| sum + tax.amount);
            let tax_rates = expense
                .tax_details
                .iter()
                .filter_map(|tax| tax.rate.map(|rate| rate.normalize().to_string()))
                .collect::<Vec<_>>()
                .join(" / ");
            let values = [
                expense.id.to_string(),
                safe_spreadsheet_text(
                    invoice
                        .map(|value| value.invoice_number.as_str())
                        .unwrap_or(""),
                ),
                invoice
                    .map(|value| value.issue_date.format("%Y-%m-%d").to_string())
                    .unwrap_or_default(),
                expense.category_code.clone(),
                expense.transaction_date.format("%Y-%m-%d").to_string(),
                if expense.transaction_date_confirmed {
                    "是"
                } else {
                    "否"
                }
                .to_string(),
                safe_spreadsheet_text(&expense.description),
                safe_spreadsheet_text(&expense.counterparty_name),
                safe_spreadsheet_text(expense.location.city_name.as_deref().unwrap_or("")),
                safe_spreadsheet_text(expense.location.province_name.as_deref().unwrap_or("")),
                expense.payment_method.clone(),
                expense.gross_amount.to_string(),
                expense.currency_code.clone(),
                tax_amount.to_string(),
                tax_rates,
                expense
                    .trip_group_id
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
                expense.documents.len().to_string(),
            ];
            for (column, value) in values.iter().enumerate() {
                let format = if matches!(column, 11 | 13) {
                    &amount
                } else {
                    &cell
                };
                worksheet
                    .write_with_format(row, column as u16, value, format)
                    .map_err(|e| AppError::internal(format!("写入费用数据失败: {e}")))?;
            }
        }
        let total_row = (expenses.len() + 1) as u32;
        let total = expenses
            .iter()
            .fold(Decimal::ZERO, |sum, expense| sum + expense.gross_amount);
        worksheet
            .write_with_format(total_row, 0, "合计", &header)
            .map_err(|e| AppError::internal(format!("写入费用合计失败: {e}")))?;
        worksheet
            .write_with_format(total_row, 11, total.to_string(), &amount)
            .map_err(|e| AppError::internal(format!("写入费用合计失败: {e}")))?;
        for (column, width) in [
            (0, 10.0),
            (1, 24.0),
            (2, 14.0),
            (3, 16.0),
            (4, 14.0),
            (5, 12.0),
            (6, 30.0),
            (7, 28.0),
            (8, 14.0),
            (9, 14.0),
            (10, 16.0),
            (11, 14.0),
            (12, 9.0),
            (13, 12.0),
            (14, 14.0),
            (15, 12.0),
            (16, 10.0),
        ] {
            worksheet
                .set_column_width(column, width)
                .map_err(|e| AppError::internal(format!("设置费用列宽失败: {e}")))?;
        }
    }

    {
        let worksheet = workbook.add_worksheet();
        worksheet
            .set_name("材料清单")
            .map_err(|e| AppError::internal(format!("设置材料工作表失败: {e}")))?;
        for (column, value) in ["费用ID", "发票号码", "材料角色", "文件名", "SHA-256"]
            .iter()
            .enumerate()
        {
            worksheet
                .write_with_format(0, column as u16, *value, &header)
                .map_err(|e| AppError::internal(format!("写入材料表头失败: {e}")))?;
        }
        let mut row = 1u32;
        for expense in expenses {
            let invoice_number = invoices_by_id
                .get(&expense.primary_invoice_id)
                .map(|invoice| safe_spreadsheet_text(&invoice.invoice_number))
                .unwrap_or_default();
            for document in &expense.documents {
                for (column, value) in [
                    expense.id.to_string(),
                    invoice_number.clone(),
                    document.role.clone(),
                    safe_spreadsheet_text(&document.original_name),
                    document.sha256.clone().unwrap_or_default(),
                ]
                .iter()
                .enumerate()
                {
                    worksheet
                        .write_with_format(row, column as u16, value, &cell)
                        .map_err(|e| AppError::internal(format!("写入材料数据失败: {e}")))?;
                }
                row += 1;
            }
        }
        worksheet
            .set_freeze_panes(1, 0)
            .map_err(|e| AppError::internal(format!("冻结材料窗格失败: {e}")))?;
        for (column, width) in [(0, 10.0), (1, 24.0), (2, 16.0), (3, 32.0), (4, 66.0)] {
            worksheet
                .set_column_width(column, width)
                .map_err(|e| AppError::internal(format!("设置材料列宽失败: {e}")))?;
        }
    }

    let bytes = workbook
        .save_to_buffer()
        .map_err(|e| AppError::internal(format!("Excel 生成失败: {e}")))?;
    tracing::info!(
        batch_id = batch.id,
        expense_count = expenses.len(),
        size_bytes = bytes.len(),
        "导出稳定费用项 Excel 成功"
    );
    Ok(bytes)
}

/// 生成批次 Excel 字节流（与 [`export_batch_excel`] 共用的核心逻辑）。
///
/// 独立于 Tauri `State` 之外，便于流水线（H1）在已持有数据时直接复用，
/// 无需二次查库或重复排版代码。
pub fn build_excel_bytes(
    batch: &invoice_store::models::Batch,
    invoices: &[invoice_store::models::ReportedInvoice],
) -> AppResult<Vec<u8>> {
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
        batch_id = batch.id,
        batch_name = %batch.name,
        invoice_count = invoices.len(),
        size_bytes = buf.len(),
        "导出批次 Excel 成功"
    );

    Ok(buf)
}

/// 导出审核后的 UTF-8 CSV。返回值带 BOM，便于 Windows Excel 直接识别中文。
#[tauri::command]
pub fn export_batch_csv(state: State<Mutex<AppState>>, batch_id: i64) -> AppResult<Vec<u8>> {
    let app_state = state.lock().unwrap();
    let db = app_state.ledger_db()?;
    let batch = db
        .get_batch(batch_id)
        .map_err(|e| AppError::database(format!("获取批次失败: {e}")))?;
    let (_, invoices) = db
        .get_active_snapshot_invoices(batch_id)
        .map_err(|e| AppError::validation(format!("请先完成审核并生成有效版本：{e}")))?;
    Ok(build_csv_bytes(&batch, &invoices))
}

pub fn build_csv_bytes(
    batch: &invoice_store::models::Batch,
    invoices: &[invoice_store::models::ReportedInvoice],
) -> Vec<u8> {
    let mut output = String::new();
    push_csv_row(
        &mut output,
        &[
            "发票号码",
            "开票日期",
            "金额",
            "税额",
            "购方名称",
            "销方名称",
            "票种",
            "城市",
            "出发时间",
            "入住日期",
            "签章状态",
            "重复标记",
        ]
        .map(str::to_string),
    );
    for invoice in invoices {
        push_csv_row(
            &mut output,
            &[
                invoice.invoice_number.clone(),
                invoice.issue_date.format("%Y-%m-%d").to_string(),
                invoice.amount.to_string(),
                invoice
                    .tax_amount
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_default(),
                safe_spreadsheet_text(invoice.buyer_name.as_deref().unwrap_or("")),
                safe_spreadsheet_text(invoice.seller_name.as_deref().unwrap_or("")),
                invoice.ticket_type.to_str().to_string(),
                safe_spreadsheet_text(invoice.city.as_deref().unwrap_or("")),
                invoice
                    .departure_time
                    .map(|value| value.format("%Y-%m-%d %H:%M").to_string())
                    .unwrap_or_default(),
                invoice
                    .checkin_date
                    .map(|value| value.format("%Y-%m-%d").to_string())
                    .unwrap_or_default(),
                invoice.verification_result.clone().unwrap_or_default(),
                if invoice.is_duplicate { "是" } else { "" }.to_string(),
            ],
        );
    }

    let mut bytes = vec![0xEF, 0xBB, 0xBF];
    bytes.extend_from_slice(output.as_bytes());
    tracing::info!(
        batch_id = batch.id,
        invoice_count = invoices.len(),
        size_bytes = bytes.len(),
        "导出批次 CSV 成功"
    );
    bytes
}

fn safe_spreadsheet_text(value: &str) -> String {
    if value.trim_start().starts_with(['=', '+', '-', '@']) {
        format!("'{value}")
    } else {
        value.to_string()
    }
}

fn push_csv_row(output: &mut String, fields: &[String]) {
    for (index, field) in fields.iter().enumerate() {
        if index > 0 {
            output.push(',');
        }
        output.push('"');
        output.push_str(&field.replace('"', "\"\""));
        output.push('"');
    }
    output.push_str("\r\n");
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
pub fn export_batch_pdf(state: State<Mutex<AppState>>, batch_id: i64) -> AppResult<Vec<u8>> {
    let app_state = state.lock().unwrap();
    let db = app_state.ledger_db()?;

    let batch = db
        .get_batch(batch_id)
        .map_err(|e| AppError::database(format!("获取批次失败: {}", e)))?;
    let (_, invoices) = db
        .get_active_snapshot_invoices(batch_id)
        .map_err(|e| AppError::validation(format!("请先完成审核并生成有效版本：{e}")))?;

    build_pdf_bytes(&batch, &invoices)
}

pub fn build_pdf_bytes(
    batch: &invoice_store::models::Batch,
    invoices: &[invoice_store::models::ReportedInvoice],
) -> AppResult<Vec<u8>> {
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

    for inv in invoices.iter() {
        if y < 30.0 {
            // 接近底部，新建页面
            let (new_page, new_layer) = doc.add_page(Mm(210.0), Mm(297.0), "Layer 1");
            current_page = doc.get_page(new_page);
            current_layer = current_page.get_layer(new_layer);
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
        batch_id = batch.id,
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

pub(crate) fn ensure_batch_exportable(status: &BatchStatus) -> AppResult<()> {
    if matches!(
        status,
        BatchStatus::Submitted | BatchStatus::Approved | BatchStatus::Completed
    ) {
        Ok(())
    } else {
        Err(AppError::validation(
            "批次尚未完成审核；请先解决阻断项并提交审核结果",
        ))
    }
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
            .list_reimbursable_invoices_by_batch(batch_id)
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
        let batch_id = db
            .create_batch("测试批次", "2026-08")
            .expect("创建批次失败");

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

        // Windows 标准用户不能假设盘符根目录存在可写的 /tmp。
        // 写入唯一临时目录并逐字节回读，既覆盖实际落盘又不留下测试文件。
        let temp_dir = tempfile::tempdir().expect("创建临时目录失败");
        let export_path = temp_dir.path().join("test_export.xlsx");
        std::fs::write(&export_path, &bytes).expect("写入临时文件失败");
        assert_eq!(
            std::fs::read(&export_path).expect("回读临时文件失败"),
            bytes
        );
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

    #[test]
    fn atomic_excel_write_replaces_existing_file_and_cleans_staging_files() {
        let temp_dir = tempfile::tempdir().expect("创建临时目录失败");
        let destination = temp_dir.path().join("审核结果.xlsx");
        std::fs::write(&destination, b"old workbook").expect("写入旧文件失败");

        write_excel_atomically(&destination, b"new workbook").expect("原子替换失败");

        assert_eq!(
            std::fs::read(&destination).expect("读取新文件失败"),
            b"new workbook"
        );
        let remaining = std::fs::read_dir(temp_dir.path())
            .expect("读取临时目录失败")
            .map(|entry| entry.expect("读取目录项失败").file_name())
            .collect::<Vec<_>>();
        assert_eq!(remaining, vec![destination.file_name().unwrap()]);
    }

    /// 测试 PDF 导出核心逻辑（不通过 Tauri 命令）
    fn export_batch_pdf_internal(db: &LedgerDb, batch_id: i64) -> AppResult<Vec<u8>> {
        let batch = db
            .get_batch(batch_id)
            .map_err(|e| AppError::database(format!("获取批次失败: {}", e)))?;
        let invoices = db
            .list_reimbursable_invoices_by_batch(batch_id)
            .map_err(|e| AppError::database(format!("获取发票列表失败: {}", e)))?;

        let (doc, page1, layer1) =
            PdfDocument::new("Invoice Ledger", Mm(210.0), Mm(297.0), "Layer 1");
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

        for inv in invoices.iter() {
            if y < 30.0 {
                let (new_page, new_layer) = doc.add_page(Mm(210.0), Mm(297.0), "Layer 1");
                current_page = doc.get_page(new_page);
                current_layer = current_page.get_layer(new_layer);
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

        let batch_id = db
            .create_batch("Test Batch 2026-08", "2026-08")
            .expect("创建批次失败");

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

        // Windows 标准用户不能假设盘符根目录存在可写的 /tmp。
        // 写入唯一临时目录并逐字节回读，既覆盖实际落盘又不留下测试文件。
        let temp_dir = tempfile::tempdir().expect("创建临时目录失败");
        let export_path = temp_dir.path().join("test_export.pdf");
        std::fs::write(&export_path, &bytes).expect("写入临时文件失败");
        assert_eq!(
            std::fs::read(&export_path).expect("回读临时文件失败"),
            bytes
        );
    }

    #[test]
    fn exports_pdf_for_empty_batch() {
        let db = create_test_db();
        let batch_id = db
            .create_batch("Empty Batch", "2026-09")
            .expect("创建批次失败");

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

    #[test]
    fn csv_is_utf8_bom_escaped_and_formula_safe() {
        let db = create_test_db();
        let batch_id = db.create_batch("CSV 测试", "2026-08").unwrap();
        db.add_invoice(&invoice_store::models::ReportedInvoice {
            id: 0,
            batch_id,
            invoice_number: "12345678901234567890".to_string(),
            issue_date: NaiveDate::from_ymd_opt(2026, 8, 1).unwrap(),
            amount: Decimal::from_str("100.50").unwrap(),
            tax_amount: Some(Decimal::from_str("9.05").unwrap()),
            buyer_name: Some("测试公司".to_string()),
            seller_name: Some("=HYPERLINK(\"https://example.invalid\",\"x,y\")".to_string()),
            ticket_type: TicketType::Rail,
            city: Some("北京".to_string()),
            departure_time: None,
            checkin_date: None,
            file_path: "C:/test/invoice.xml".to_string(),
            created_at: chrono::Utc::now().naive_utc(),
            updated_at: chrono::Utc::now().naive_utc(),
            verification_result: Some("valid".to_string()),
            is_duplicate: false,
            duplicate_reason: None,
        })
        .unwrap();

        let batch = db.get_batch(batch_id).unwrap();
        let invoices = db.list_reimbursable_invoices_by_batch(batch_id).unwrap();
        let bytes = build_csv_bytes(&batch, &invoices);
        assert_eq!(&bytes[..3], &[0xEF, 0xBB, 0xBF]);
        let text = std::str::from_utf8(&bytes[3..]).unwrap();
        assert!(text.starts_with("\"发票号码\",\"开票日期\""));
        assert!(text.contains("\"测试公司\""));
        assert!(text.contains("\"'=HYPERLINK(\"\"https://example.invalid\"\",\"\"x,y\"\")\""));
        assert!(!text.contains("\"=HYPERLINK"));
        assert!(text.ends_with("\r\n"));
    }
    #[test]
    fn export_gate_rejects_unreviewed_or_rejected_batches() {
        assert!(ensure_batch_exportable(&BatchStatus::Draft).is_err());
        assert!(ensure_batch_exportable(&BatchStatus::Rejected).is_err());
        assert!(ensure_batch_exportable(&BatchStatus::Submitted).is_ok());
        assert!(ensure_batch_exportable(&BatchStatus::Approved).is_ok());
        assert!(ensure_batch_exportable(&BatchStatus::Completed).is_ok());
    }
}
