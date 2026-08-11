//! 流水线命令模块：端到端流程串联（采集 → 解析 → 去重 → 归组 → 审核 → 导出）
//!
//! 采用事件驱动架构，通过 Tauri events 实时推送进度给前端。

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};

use invoice_collect::config::{DateRange, ImapConfig};
use invoice_collect::{dedupe, extract, imap_client};
use invoice_grouping::{group_invoices, types::*};
use invoice_parse::manifest::TagHints;
use invoice_parse::model::{ParsedInvoice, TicketType as ParseTicketType};
use invoice_store::models::ReportedInvoice;

use crate::commands::invoice::{builtin_hints, to_store_ticket_type};
use crate::error::{AppError, AppResult};
use crate::AppState;

/// 流水线配置
#[derive(Debug, Clone, Deserialize)]
pub struct PipelineConfig {
    pub email: String,
    pub password: String,
    pub batch_name: String,
    pub month: String, // "2026-07"
    pub date_range: DateRangeDto,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DateRangeDto {
    pub start: String, // "2026-07-01"
    pub end: String,   // "2026-07-31"
}

/// 阶段进度事件
#[derive(Debug, Clone, Serialize)]
pub struct StageProgress {
    pub stage: String, // "collect" | "parse" | "dedupe" | "group" | "review" | "export"
    pub progress: f32, // 0.0 - 1.0
    pub current: Option<usize>,
    pub total: Option<usize>,
    pub message: String,
}

/// 错误事件
#[derive(Debug, Clone, Serialize)]
pub struct PipelineError {
    pub stage: String,
    pub message: String,
}

/// 完成事件
#[derive(Debug, Clone, Serialize)]
pub struct PipelineComplete {
    pub batch_id: i64,
    pub invoice_count: usize,
    pub total_amount: String,
    pub excel_path: Option<String>,
}

/// 启动流水线（异步，立即返回 pipeline_id）
#[tauri::command]
pub async fn start_pipeline(
    app: AppHandle,
    _state: State<'_, Mutex<AppState>>,
    config: PipelineConfig,
) -> AppResult<String> {
    let pipeline_id = uuid::Uuid::new_v4().to_string();

    tracing::info!(
        pipeline_id = %pipeline_id,
        email = %config.email,
        batch_name = %config.batch_name,
        "流水线启动"
    );

    // 克隆必要的数据，传给异步任务
    let pid = pipeline_id.clone();
    let app_handle = app.clone();

    // 直接在 spawn 中使用闭包捕获 app 和 config
    tauri::async_runtime::spawn(async move {
        if let Err(e) = run_pipeline_impl(app_handle, pid, config).await {
            tracing::error!("流水线执行失败: {}", e);
        }
    });

    Ok(pipeline_id)
}

/// 流水线核心执行逻辑（从 app 中获取 state）
async fn run_pipeline_impl(
    app: AppHandle,
    pipeline_id: String,
    config: PipelineConfig,
) -> AppResult<()> {
    // 从 app 中获取 state
    let state = app.state::<Mutex<AppState>>();
    // Stage 1: collect
    emit_progress(&app, &pipeline_id, "collect", 0.0, 0, None, "连接邮箱服务器...");
    let files = match collect_invoices(&app, &pipeline_id, &config).await {
        Ok(f) => f,
        Err(e) => {
            emit_error(&app, &pipeline_id, "collect", &e.to_string());
            return Err(e);
        }
    };

    if files.is_empty() {
        emit_error(&app, &pipeline_id, "collect", "未找到任何发票附件");
        return Err(AppError::validation("未找到任何发票附件"));
    }

    // Stage 2: parse
    emit_progress(&app, &pipeline_id, "parse", 0.0, 0, Some(files.len()), "开始解析发票...");
    let parsed = match parse_invoices(&app, &pipeline_id, &files).await {
        Ok(p) => p,
        Err(e) => {
            emit_error(&app, &pipeline_id, "parse", &e.to_string());
            return Err(e);
        }
    };

    if parsed.is_empty() {
        emit_error(&app, &pipeline_id, "parse", "所有发票解析失败");
        return Err(AppError::parse("所有发票解析失败"));
    }

    // Stage 3: dedupe
    emit_progress(&app, &pipeline_id, "dedupe", 0.0, 0, Some(parsed.len()), "检查重复发票...");
    let deduplicated = match dedupe_invoices(&app, &pipeline_id, &state, parsed).await {
        Ok(d) => d,
        Err(e) => {
            emit_error(&app, &pipeline_id, "dedupe", &e.to_string());
            return Err(e);
        }
    };

    // Stage 4: group
    emit_progress(&app, &pipeline_id, "group", 0.0, 0, None, "归组行程...");
    let _grouped = match group_invoices_stage(&app, &pipeline_id, &deduplicated).await {
        Ok(g) => g,
        Err(e) => {
            emit_error(&app, &pipeline_id, "group", &e.to_string());
            return Err(e);
        }
    };

    // Stage 5: review (G2 占位：自动接受)
    emit_progress(&app, &pipeline_id, "review", 0.0, 0, None, "审核归组结果（自动通过）...");
    emit_progress(&app, &pipeline_id, "review", 1.0, 0, None, "审核完成");

    // Stage 6: store & export
    emit_progress(&app, &pipeline_id, "export", 0.0, 0, None, "保存到数据库...");
    let batch_id = match store_batch(&app, &pipeline_id, &state, &config, &deduplicated).await {
        Ok(id) => id,
        Err(e) => {
            emit_error(&app, &pipeline_id, "export", &e.to_string());
            return Err(e);
        }
    };

    emit_progress(&app, &pipeline_id, "export", 0.5, 0, None, "生成 Excel 报表...");
    let excel_path = match export_excel(&app, &pipeline_id, &state, batch_id).await {
        Ok(path) => path,
        Err(e) => {
            emit_error(&app, &pipeline_id, "export", &e.to_string());
            return Err(e);
        }
    };

    // 计算总金额
    let total_amount: rust_decimal::Decimal = deduplicated.iter()
        .map(|inv| inv.total_amount)
        .sum();

    emit_complete(&app, &pipeline_id, PipelineComplete {
        batch_id,
        invoice_count: deduplicated.len(),
        total_amount: total_amount.to_string(),
        excel_path: Some(excel_path),
    });

    tracing::info!(
        pipeline_id = %pipeline_id,
        batch_id,
        invoice_count = deduplicated.len(),
        "流水线完成"
    );

    Ok(())
}

/// Stage 1: 采集发票附件
async fn collect_invoices(
    app: &AppHandle,
    pipeline_id: &str,
    config: &PipelineConfig,
) -> AppResult<Vec<PathBuf>> {
    // 解析日期范围
    let date_range = DateRange::parse(&config.date_range.start, &config.date_range.end)
        .map_err(|e| AppError::validation(format!("日期范围格式错误: {}", e)))?;

    // 构建 IMAP 配置
    let domain = config.email.split('@').nth(1).unwrap_or("");
    let host = match domain.to_lowercase().as_str() {
        "qq.com" | "vip.qq.com" | "foxmail.com" => "imap.qq.com",
        "163.com" => "imap.163.com",
        "126.com" => "imap.126.com",
        "gmail.com" => "imap.gmail.com",
        "outlook.com" | "hotmail.com" => "outlook.office365.com",
        _ => "imap.qq.com", // 默认
    };

    let imap_config = ImapConfig {
        host: host.to_string(),
        port: 993,
        username: config.email.clone(),
        password: config.password.clone(),
    };

    emit_progress(app, pipeline_id, "collect", 0.1, 0, None, "连接 IMAP 服务器...");

    let mut session = imap_client::Session::connect(&imap_config)
        .map_err(|e| AppError::network(format!("IMAP 连接失败: {}", e)))?;

    emit_progress(app, pipeline_id, "collect", 0.3, 0, None, "搜索发票邮件...");

    let uids = session.search_range("INBOX", &date_range)
        .map_err(|e| AppError::network(format!("搜索邮件失败: {}", e)))?;

    if uids.is_empty() {
        return Ok(Vec::new());
    }

    emit_progress(app, pipeline_id, "collect", 0.5, 0, Some(uids.len()),
        &format!("找到 {} 封邮件，开始下载附件...", uids.len()));

    // 创建临时目录
    let temp_dir = get_temp_dir()?;
    std::fs::create_dir_all(&temp_dir)?;

    let mut files = Vec::new();
    let mut deduper = dedupe::Deduper::new();

    for (idx, uid) in uids.iter().enumerate() {
        emit_progress(app, pipeline_id, "collect",
            0.5 + 0.5 * (idx as f32 / uids.len() as f32),
            idx, Some(uids.len()),
            &format!("处理邮件 {}/{}", idx + 1, uids.len()));

        let raw = match session.fetch_raw(*uid) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(uid, "获取邮件失败: {}", e);
                continue;
            }
        };

        let email = match extract::extract_email(&raw) {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!(uid, "解析邮件失败: {}", e);
                continue;
            }
        };

        // 分类和去重
        for att in email.attachments {
            // 解压 ZIP（如果需要）
            let expanded = extract::extract_zip_if_needed(&att);

            for item in expanded {
                // 去重
                if !deduper.is_new(None, &item.data) {
                    continue;
                }

                // 分类：检查文件名是否包含发票关键词
                let is_invoice = item.filename.to_lowercase().contains("发票")
                    || item.filename.to_lowercase().contains("invoice")
                    || item.filename.to_lowercase().contains("行程单")
                    || item.filename.ends_with(".xml")
                    || item.filename.ends_with(".ofd");

                if is_invoice {
                    // 保存文件
                    let sanitized_name = sanitize_filename(&item.filename);
                    let file_path = temp_dir.join(&sanitized_name);
                    std::fs::write(&file_path, &item.data)?;
                    files.push(file_path);
                }
            }
        }
    }

    Ok(files)
}

/// Stage 2: 解析发票
async fn parse_invoices(
    app: &AppHandle,
    pipeline_id: &str,
    files: &[PathBuf],
) -> AppResult<Vec<ParsedInvoice>> {
    let mut parsed = Vec::new();
    let hints = builtin_hints();

    for (idx, file) in files.iter().enumerate() {
        emit_progress(app, pipeline_id, "parse",
            idx as f32 / files.len() as f32,
            idx, Some(files.len()),
            &format!("解析 {}/{}", idx + 1, files.len()));

        match parse_single_invoice(file, &hints) {
            Ok(invoice) => parsed.push(invoice),
            Err(e) => {
                tracing::warn!(path = %file.display(), "解析失败: {}", e);
                // 单个文件失败不终止流水线
            }
        }
    }

    Ok(parsed)
}

/// Stage 3: 去重检查
async fn dedupe_invoices(
    app: &AppHandle,
    pipeline_id: &str,
    state: &tauri::State<'_, Mutex<AppState>>,
    invoices: Vec<ParsedInvoice>,
) -> AppResult<Vec<ParsedInvoice>> {
    let app_state = state.lock().unwrap();
    let db = app_state.ledger_db()?;

    let mut deduplicated = Vec::new();

    for (idx, invoice) in invoices.iter().enumerate() {
        emit_progress(app, pipeline_id, "dedupe",
            idx as f32 / invoices.len() as f32,
            idx, Some(invoices.len()),
            &format!("检查 {}/{}", idx + 1, invoices.len()));

        // 多字段查重：发票号精确命中，或（金额 + 日期 + 票种）模糊命中。
        // 票种必须用发票自身的值，硬编码会让模糊匹配比对到错误票种。
        let ticket_type = to_store_ticket_type(invoice.ticket_type).to_str();
        let duplicates = db.find_potential_duplicates(
            &invoice.invoice_number,
            &invoice.total_amount,
            &invoice.issue_date,
            ticket_type,
            None,
        ).map_err(|e| AppError::database(format!("查重失败: {}", e)))?;

        if duplicates.is_empty() {
            deduplicated.push(invoice.clone());
        } else {
            tracing::info!(invoice_number = %invoice.invoice_number, "跳过重复发票");
        }
    }

    Ok(deduplicated)
}

/// Stage 4: 归组行程
async fn group_invoices_stage(
    app: &AppHandle,
    pipeline_id: &str,
    invoices: &[ParsedInvoice],
) -> AppResult<GroupingResult> {
    emit_progress(app, pipeline_id, "group", 0.5, 0, None, "执行归组算法...");

    // 创建简单的 no-op 解决器
    struct SimpleResolver;
    impl AmbiguityResolver for SimpleResolver {
        fn resolve(&self, _ambiguities: &[Ambiguity]) -> Result<Vec<AmbiguityResolution>, anyhow::Error> {
            Ok(Vec::new())
        }
    }

    let config = GroupingConfig {
        home_cities: vec!["北京".to_string()], // TODO: 从用户配置读取
        ambiguity_handler: Box::new(SimpleResolver),
    };

    let result = group_invoices(invoices, &config)
        .map_err(|e| AppError::internal(format!("归组失败: {}", e)))?;

    emit_progress(app, pipeline_id, "group", 1.0, 0, None,
        &format!("归组完成，识别 {} 个行程", result.trips.len()));

    Ok(result)
}

/// Stage 6: 保存批次和发票到数据库
async fn store_batch(
    app: &AppHandle,
    pipeline_id: &str,
    state: &tauri::State<'_, Mutex<AppState>>,
    config: &PipelineConfig,
    invoices: &[ParsedInvoice],
) -> AppResult<i64> {
    let app_state = state.lock().unwrap();
    let db = app_state.ledger_db()?;

    // 创建批次
    let batch_id = db.create_batch(&config.batch_name, &config.month)
        .map_err(|e| AppError::database(format!("创建批次失败: {}", e)))?;

    // 添加发票
    for (idx, invoice) in invoices.iter().enumerate() {
        emit_progress(app, pipeline_id, "export",
            0.1 + 0.4 * (idx as f32 / invoices.len() as f32),
            idx, Some(invoices.len()),
            &format!("保存发票 {}/{}", idx + 1, invoices.len()));

        let reported = ReportedInvoice {
            id: 0, // 自动生成
            batch_id,
            invoice_number: invoice.invoice_number.clone(),
            issue_date: invoice.issue_date,
            amount: invoice.total_amount,
            tax_amount: invoice.tax_amount,
            buyer_name: invoice.buyer_name.clone(),
            seller_name: invoice.seller_name.clone(),
            ticket_type: to_store_ticket_type(invoice.ticket_type),
            city: invoice.city.clone(),
            departure_time: invoice.departure_time,
            checkin_date: invoice.checkin_date,
            file_path: invoice.source_path.display().to_string(),
            created_at: chrono::Utc::now().naive_utc(),
            updated_at: chrono::Utc::now().naive_utc(),
            verification_result: None, // TODO: 签章验证结果
            is_duplicate: false,
            duplicate_reason: None,
        };

        db.add_invoice(&reported)
            .map_err(|e| AppError::database(format!("保存发票失败: {}", e)))?;
    }

    Ok(batch_id)
}

/// Stage 6: 导出 Excel
async fn export_excel(
    _app: &AppHandle,
    _pipeline_id: &str,
    state: &tauri::State<'_, Mutex<AppState>>,
    batch_id: i64,
) -> AppResult<String> {
    // 直接使用内部逻辑，不通过 State wrapper
    let app_state = state.lock().unwrap();
    let db = app_state.ledger_db()?;

    let batch = db.get_batch(batch_id)
        .map_err(|e| AppError::database(format!("获取批次失败: {}", e)))?;
    let invoices = db.list_invoices_by_batch(batch_id)
        .map_err(|e| AppError::database(format!("获取发票列表失败: {}", e)))?;

    drop(app_state); // 释放锁，生成过程不持锁

    // 复用 D 模块的真实 Excel 排版逻辑（12 列 + 冻结首行 + 合计行）
    let bytes = crate::commands::export::build_excel_bytes(&batch, &invoices)?;

    let temp_dir = get_temp_dir()?;
    std::fs::create_dir_all(&temp_dir)
        .map_err(|e| AppError::internal(format!("创建导出目录失败: {}", e)))?;
    let excel_path = temp_dir.join(format!("batch_{}.xlsx", batch_id));
    std::fs::write(&excel_path, &bytes)
        .map_err(|e| AppError::internal(format!("写入 Excel 文件失败: {}", e)))?;

    Ok(excel_path.display().to_string())
}

// ==================== 辅助函数 ====================

/// 获取临时目录
fn get_temp_dir() -> AppResult<PathBuf> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".to_string());
    let temp_dir = PathBuf::from(home).join(".invoice-assistant").join("temp");
    Ok(temp_dir)
}

/// 清理文件名（移除非法字符）
fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
            c
        } else {
            '_'
        })
        .collect()
}

/// 解析单个发票文件
fn parse_single_invoice(path: &Path, hints: &TagHints) -> Result<ParsedInvoice, anyhow::Error> {
    let ext = path.extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();

    let bytes = std::fs::read(path)?;
    let path_buf = path.to_path_buf();

    // 使用 catch_unwind 包装，防止解析库 panic
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let ext_ref = ext.as_str();
        match ext_ref.to_lowercase().as_str() {
            "xml" => invoice_parse::xml::parse_invoice_xml(&bytes, path, hints, ParseTicketType::Other),
            "ofd" => invoice_parse::ofd::parse_invoice_ofd(&bytes, path, hints, ParseTicketType::Other),
            "pdf" => {
                // 先尝试 L1 坐标解析，失败再降级到 flat-text
                invoice_parse::pdf_text::parse_vat_invoice_from_boxes(&bytes, path)
                    .or_else(|_| invoice_parse::pdf::parse_invoice_pdf(&bytes, path, hints, ParseTicketType::Other))
            },
            _ => {
                // 不支持的格式，返回错误
                Err(invoice_parse::model::ParseError::MalformedFormat {
                    format: "unknown",
                    path: path_buf.clone(),
                    detail: format!("不支持的文件格式: {}", ext_ref),
                })
            },
        }
    }));

    match result {
        Ok(Ok(invoice)) => Ok(invoice),
        Ok(Err(e)) => Err(anyhow::anyhow!("解析失败: {}", e)),
        Err(_) => Err(anyhow::anyhow!("解析库 panic")),
    }
}

// ==================== 事件发送 ====================

fn emit_progress(
    app: &AppHandle,
    pipeline_id: &str,
    stage: &str,
    progress: f32,
    current: usize,
    total: Option<usize>,
    message: &str,
) {
    let event = StageProgress {
        stage: stage.to_string(),
        progress,
        current: Some(current),
        total,
        message: message.to_string(),
    };

    let event_name = format!("pipeline:progress:{}", pipeline_id);
    if let Err(e) = app.emit(&event_name, event) {
        tracing::error!("发送进度事件失败: {}", e);
    }
}

fn emit_error(app: &AppHandle, pipeline_id: &str, stage: &str, message: &str) {
    let event = PipelineError {
        stage: stage.to_string(),
        message: message.to_string(),
    };

    let event_name = format!("pipeline:error:{}", pipeline_id);
    if let Err(e) = app.emit(&event_name, event) {
        tracing::error!("发送错误事件失败: {}", e);
    }
}

fn emit_complete(app: &AppHandle, pipeline_id: &str, result: PipelineComplete) {
    let event_name = format!("pipeline:complete:{}", pipeline_id);
    if let Err(e) = app.emit(&event_name, result) {
        tracing::error!("发送完成事件失败: {}", e);
    }
}
