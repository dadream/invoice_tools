//! 数据库模型定义
//!
//! 包含账号、凭证、批次、发票等核心数据模型

use chrono::{NaiveDate, NaiveDateTime};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// 邮箱账号
///
/// 存储在 accounts.db 的 accounts 表
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    /// 账号 ID（主键）
    pub id: i64,

    /// 邮箱地址
    pub email: String,

    /// IMAP 服务器地址
    pub imap_server: String,

    /// IMAP 端口
    pub imap_port: u16,

    /// 是否启用（用于临时禁用账号）
    pub enabled: bool,

    /// 创建时间
    pub created_at: NaiveDateTime,

    /// 更新时间
    pub updated_at: NaiveDateTime,
}

/// 邮箱凭证（加密存储）
///
/// 存储在 accounts.db 的 credentials 表
#[derive(Debug, Clone)]
pub struct Credential {
    /// 凭证 ID（主键）
    pub id: i64,

    /// 关联的账号 ID（外键）
    pub account_id: i64,

    /// 加密后的密码（格式：[nonce || ciphertext || tag]）
    pub encrypted_password: Vec<u8>,

    /// 创建时间
    pub created_at: NaiveDateTime,

    /// 更新时间
    pub updated_at: NaiveDateTime,
}

/// 报销批次
///
/// 存储在 ledger.db 的 batches 表
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Batch {
    /// 批次 ID（主键）
    pub id: i64,

    /// 批次名称（如 "2026年7月出差"）
    pub name: String,

    /// 批次月份（用于归组，格式 YYYY-MM）
    pub month: String,

    /// 批次状态
    pub status: BatchStatus,

    /// 批次总金额
    pub total_amount: Decimal,

    /// 发票张数
    pub invoice_count: i32,

    /// 创建时间
    pub created_at: NaiveDateTime,

    /// 更新时间
    pub updated_at: NaiveDateTime,

    /// 提交时间（状态变为 Submitted 时）
    pub submitted_at: Option<NaiveDateTime>,

    /// 批准时间（状态变为 Approved 时）
    pub approved_at: Option<NaiveDateTime>,

    /// 完成时间（状态变为 Completed 时）
    pub completed_at: Option<NaiveDateTime>,

    /// 驳回时间（状态变为 Rejected 时）
    pub rejected_at: Option<NaiveDateTime>,
}

/// 一次完成审核后冻结的批次数据版本。
///
/// 导出与 Concur 交付必须引用该实体，不能再次读取可变的草稿数据。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchReviewSnapshot {
    pub id: i64,
    pub batch_id: i64,
    pub version: i32,
    pub content_sha256: String,
    pub invoice_count: i32,
    pub total_amount: Decimal,
    pub created_at: String,
    pub invalidated_at: Option<String>,
}

/// 基于审核快照执行的一次交付任务。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeliveryTask {
    pub id: i64,
    pub batch_id: i64,
    pub review_snapshot_id: i64,
    pub kind: String,
    pub status: String,
    pub output_path: Option<String>,
    pub last_error: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub completed_at: Option<String>,
}

/// 与任何目标报销系统解耦的稳定本地费用项。
///
/// `primary_invoice_id` 只用于关联当前兼容的发票事实表；Concur 字段 ID、
/// 选项 ID、租户必填性和企业自定义字段不得写入该实体。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpenseItem {
    pub id: i64,
    pub batch_id: i64,
    pub primary_invoice_id: i64,
    pub model_version: i32,
    pub category_code: String,
    /// parser.classification / manual_review / unclassified
    #[serde(default = "legacy_category_source")]
    pub category_source: String,
    /// 系统建议只有在高置信规则命中或用户明确确认后才为 true。
    #[serde(default = "legacy_category_confirmed")]
    pub category_confirmed: bool,
    pub transaction_date: NaiveDate,
    pub transaction_date_source: String,
    pub transaction_date_confirmed: bool,
    pub description: String,
    pub counterparty_name: String,
    pub location: ExpenseLocation,
    pub payment_method: String,
    pub gross_amount: Decimal,
    pub currency_code: String,
    pub tax_details: Vec<ExpenseTaxDetail>,
    pub trip_group_id: Option<i64>,
    /// included / duplicate_suspect / excluded
    pub inclusion_status: String,
    pub provenance_json: String,
    pub documents: Vec<InvoiceDocument>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExpenseLocation {
    pub city_name: Option<String>,
    pub city_code: Option<String>,
    pub province_name: Option<String>,
    pub province_code: Option<String>,
    pub country_code: Option<String>,
}

fn legacy_category_source() -> String {
    "legacy.ticket_type".to_string()
}

fn legacy_category_confirmed() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpenseTaxDetail {
    pub amount: Decimal,
    pub rate: Option<Decimal>,
    pub source: String,
}

/// 挂载到同一费用项的发票、行程单、消费明细及其他配套材料。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvoiceDocument {
    pub id: i64,
    pub batch_id: i64,
    pub expense_item_id: i64,
    pub source_invoice_id: Option<i64>,
    #[serde(default)]
    pub source_pending_document_id: Option<i64>,
    /// main_invoice / itinerary / detail / supporting / duplicate_copy
    pub role: String,
    pub file_path: String,
    pub original_name: String,
    pub mime_type: Option<String>,
    pub sha256: Option<String>,
    pub created_at: String,
}

/// 已导入但尚不能安全判断归属的材料。它不形成费用，也不计入批次金额。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingInvoiceDocument {
    pub id: i64,
    pub batch_id: i64,
    /// itinerary / detail / supporting
    pub proposed_role: String,
    pub file_path: String,
    pub original_name: String,
    pub mime_type: Option<String>,
    pub sha256: Option<String>,
    pub detection_reason: String,
    /// pending / attached / ignored
    pub status: String,
    pub assigned_expense_item_id: Option<i64>,
    pub created_at: String,
    pub updated_at: String,
}

/// 流水线原子写入批次时使用的待挂载材料输入。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewPendingInvoiceDocument {
    pub proposed_role: String,
    pub file_path: String,
    pub original_name: String,
    pub mime_type: Option<String>,
    pub sha256: Option<String>,
    pub detection_reason: String,
    /// 仅当解析阶段用材料金额与类型得到唯一主发票时填写；存储层在同一事务中
    /// 将材料挂载到该输入发票对应的费用。没有唯一结果必须为 None。
    pub auto_assign_invoice_index: Option<usize>,
}

/// 邮件来源台账中的一个逻辑附件。ZIP 内文件按独立逻辑附件记录，
/// `container_name` 保留它与原 ZIP 的关系。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailImportAttachment {
    pub id: i64,
    pub message_id: i64,
    pub content_sha256: Option<String>,
    pub original_name: String,
    pub container_name: Option<String>,
    pub mime_type: Option<String>,
    pub byte_len: i64,
    /// invoice / supporting / duplicate / not_invoice / unsupported / failed
    pub status: String,
    /// invoice / itinerary / detail / supporting / unknown
    pub role_hint: String,
    pub reason: String,
    pub reported_invoice_id: Option<i64>,
    pub pending_document_id: Option<i64>,
    pub manual_import: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// 批次内一封邮件的可审核处理结果。正文和正文链接不进入该结构。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailImportMessage {
    pub id: i64,
    pub batch_id: i64,
    pub pipeline_id: String,
    pub mailbox_folder: String,
    pub uid: i64,
    pub message_id_sha256: Option<String>,
    pub sender: String,
    pub subject: String,
    pub received_at: Option<String>,
    /// imported / needs_attachment_review / manual_download / needs_confirmation /
    /// not_invoice / failed
    pub status: String,
    /// open / resolved / ignored
    pub resolution_status: String,
    pub error_category: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub resolved_at: Option<String>,
    pub attachments: Vec<EmailImportAttachment>,
}

/// 流水线最终事务写入邮件台账时使用的附件输入。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewEmailImportAttachment {
    pub content_sha256: Option<String>,
    pub original_name: String,
    pub container_name: Option<String>,
    pub mime_type: Option<String>,
    pub byte_len: i64,
    pub status: String,
    pub role_hint: String,
    pub reason: String,
    pub is_content_duplicate: bool,
    pub invoice_input_index: Option<usize>,
    pub pending_document_index: Option<usize>,
    pub manual_import: bool,
}

/// 新邮箱邮件或对既有邮件执行“下载后本地导入”的事务输入。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewEmailImportMessage {
    pub existing_message_id: Option<i64>,
    pub mailbox_folder: String,
    pub uid: i64,
    pub message_id_sha256: Option<String>,
    pub sender: String,
    pub subject: String,
    pub received_at: Option<String>,
    pub initial_status: String,
    pub error_category: Option<String>,
    pub attachments: Vec<NewEmailImportAttachment>,
}

/// 独立于报销批次的邮箱收集任务。收集任务只确认来源材料完整性，
/// 不包含发票字段解析、金额或归组结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailCollectionTask {
    pub id: i64,
    pub name: String,
    pub account_email: String,
    pub mailbox_folder: String,
    pub date_start: String,
    pub date_end: String,
    /// created / collecting / review / completed / failed / interrupted
    pub status: String,
    /// open / completed
    pub review_status: String,
    pub pipeline_id: Option<String>,
    pub last_error_category: Option<String>,
    pub scanned_message_count: i64,
    pub candidate_file_count: i64,
    pub actionable_message_count: i64,
    pub created_at: String,
    pub updated_at: String,
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollectedEmailAttachment {
    pub id: i64,
    pub message_id: i64,
    pub content_sha256: Option<String>,
    pub original_name: String,
    pub container_name: Option<String>,
    pub mime_type: Option<String>,
    pub byte_len: i64,
    /// candidate / supporting_candidate / filtered / unsupported / failed
    pub status: String,
    /// invoice / itinerary / detail / supporting / unknown
    pub role_hint: String,
    pub reason: String,
    pub stored_path: Option<String>,
    pub manual_import: bool,
    /// 用户在来源审核中明确标记为无效；文件仍保留用于追溯，但不会进入批次。
    pub user_excluded: bool,
    pub user_excluded_at: Option<String>,
    pub used_batch_ids: Vec<i64>,
    pub used_batch_names: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollectedEmailMessage {
    pub id: i64,
    pub task_id: i64,
    pub mailbox_folder: String,
    pub uid: i64,
    pub message_id_sha256: Option<String>,
    pub sender: String,
    pub subject: String,
    pub received_at: Option<String>,
    /// has_candidates / materials_only / manual_download / needs_confirmation /
    /// not_relevant / failed
    pub status: String,
    /// open / resolved / ignored
    pub resolution_status: String,
    pub error_category: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub resolved_at: Option<String>,
    pub attachments: Vec<CollectedEmailAttachment>,
}

/// 收集阶段一次性生成并长期保存在本地台账中的安全正文和下载链接。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollectedEmailReviewSnapshot {
    pub message_id: i64,
    pub sender_name: Option<String>,
    pub sender_address: Option<String>,
    pub body_text: String,
    pub body_truncated: bool,
    pub analyzed_at: String,
    pub links: Vec<CollectedEmailLink>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollectedEmailLink {
    pub id: i64,
    pub message_id: i64,
    pub position: i64,
    pub label: String,
    pub host: String,
    /// 仅可信 Rust 后端读取；Tauri DTO 不得把完整地址返回给 WebView。
    pub url: String,
    pub scheme: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewCollectedEmailLink {
    pub label: String,
    pub host: String,
    pub url: String,
    pub scheme: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewCollectedEmailReviewSnapshot {
    pub sender_name: Option<String>,
    pub sender_address: Option<String>,
    pub body_text: String,
    pub body_truncated: bool,
    pub links: Vec<NewCollectedEmailLink>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewCollectedEmailAttachment {
    pub content_sha256: Option<String>,
    pub original_name: String,
    pub container_name: Option<String>,
    pub mime_type: Option<String>,
    pub byte_len: i64,
    pub status: String,
    pub role_hint: String,
    pub reason: String,
    pub stored_path: Option<String>,
    pub manual_import: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewCollectedEmailMessage {
    pub mailbox_folder: String,
    pub uid: i64,
    pub message_id_sha256: Option<String>,
    pub sender: String,
    pub subject: String,
    pub received_at: Option<String>,
    pub status: String,
    pub error_category: Option<String>,
    pub review: Option<NewCollectedEmailReviewSnapshot>,
    pub attachments: Vec<NewCollectedEmailAttachment>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchCollectionImport {
    pub id: i64,
    pub batch_id: i64,
    pub task_id: i64,
    pub task_name: String,
    pub status: String,
    pub pipeline_id: Option<String>,
    pub item_count: i64,
    pub created_at: String,
    pub updated_at: String,
}

/// 本地费用项人工核对输入。字段保持目标系统无关。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpenseItemUpdate {
    pub category_code: String,
    pub category_confirmed: bool,
    pub transaction_date: NaiveDate,
    pub transaction_date_confirmed: bool,
    pub description: String,
    pub counterparty_name: String,
    pub location: ExpenseLocation,
    pub payment_method: String,
    pub gross_amount: Decimal,
    pub currency_code: String,
    pub tax_details: Vec<ExpenseTaxDetail>,
}

/// A parser-derived category correction for an expense not overridden by the user.
/// `confirmed` is true only for an explicit invoice item/service match. Merchant-name fallbacks
/// remain visible suggestions and still require user confirmation. `other` revokes a stale
/// automatic classification when the current parser no longer finds supporting evidence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpenseCategoryDetection {
    pub expense_item_id: i64,
    pub category_code: String,
    pub source: String,
    pub confirmed: bool,
}

/// 一个企业/租户的版本化 Concur 映射档案。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConcurMappingProfile {
    pub id: i64,
    pub name: String,
    pub company_label: String,
    pub version: i32,
    pub status: String,
    pub adapter_kind: String,
    pub field_rules_json: String,
    pub expense_type_map_json: String,
    pub location_map_json: String,
    pub payment_type_map_json: String,
    pub vat_rate_map_json: String,
    pub required_fields_json: String,
    pub custom_fields_json: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConcurMappingProfileInput {
    pub profile_id: Option<i64>,
    pub name: String,
    pub company_label: String,
    pub adapter_kind: String,
    pub field_rules_json: String,
    pub expense_type_map_json: String,
    pub location_map_json: String,
    pub payment_type_map_json: String,
    pub vat_rate_map_json: String,
    pub required_fields_json: String,
    pub custom_fields_json: String,
}

/// 一次冻结审核版本到 Concur 草稿的本地会话。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConcurUploadSession {
    pub id: i64,
    pub batch_id: i64,
    pub review_snapshot_id: i64,
    pub mapping_profile_id: i64,
    pub mapping_profile_version: i32,
    pub report_name: String,
    pub report_date: NaiveDate,
    pub comment: String,
    pub status: String,
    pub idempotency_key: String,
    pub external_report_id: Option<String>,
    pub upload_overrides_json: String,
    pub mapped_payload_json: String,
    pub gaps_json: String,
    pub last_error: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConcurMappingGap {
    pub scope: String,
    pub expense_item_id: Option<i64>,
    pub field_key: String,
    pub message: String,
    pub resolution: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MappedExpensePayload {
    pub expense_item_id: i64,
    pub target_fields_json: String,
    pub attachment_document_ids: Vec<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConcurUploadPreflight {
    pub session: ConcurUploadSession,
    pub expenses: Vec<MappedExpensePayload>,
    pub gaps: Vec<ConcurMappingGap>,
    pub ready: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConcurUploadAttachmentState {
    pub id: i64,
    pub document_id: i64,
    pub status: String,
    pub idempotency_key: String,
    pub external_attachment_id: Option<String>,
    pub attempt_count: i32,
    pub last_error: Option<String>,
    pub last_verified_at: Option<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConcurUploadItemState {
    pub id: i64,
    pub expense_item_id: i64,
    pub status: String,
    pub idempotency_key: String,
    pub mapped_payload_json: String,
    pub external_expense_id: Option<String>,
    pub attempt_count: i32,
    pub last_error: Option<String>,
    pub last_verified_at: Option<String>,
    pub updated_at: String,
    pub attachments: Vec<ConcurUploadAttachmentState>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConcurUploadStatus {
    pub session: ConcurUploadSession,
    pub items: Vec<ConcurUploadItemState>,
}

/// 批次状态
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BatchStatus {
    /// 草稿（正在编辑）
    Draft,

    /// 已提交（等待审核）
    Submitted,

    /// 已批准（等待打款）
    Approved,

    /// 已完成（已打款）
    Completed,

    /// 已拒绝
    Rejected,
}

impl BatchStatus {
    /// 转换为数据库存储的整数
    pub fn to_i32(self) -> i32 {
        match self {
            BatchStatus::Draft => 0,
            BatchStatus::Submitted => 1,
            BatchStatus::Approved => 2,
            BatchStatus::Completed => 3,
            BatchStatus::Rejected => 4,
        }
    }

    /// 从数据库整数转换
    pub fn from_i32(value: i32) -> Option<Self> {
        match value {
            0 => Some(BatchStatus::Draft),
            1 => Some(BatchStatus::Submitted),
            2 => Some(BatchStatus::Approved),
            3 => Some(BatchStatus::Completed),
            4 => Some(BatchStatus::Rejected),
            _ => None,
        }
    }
}

/// 已报销的发票记录
///
/// 存储在 ledger.db 的 reported_invoices 表
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportedInvoice {
    /// 记录 ID（主键）
    pub id: i64,

    /// 关联的批次 ID（外键）
    pub batch_id: i64,

    /// 发票号码（20 位）
    pub invoice_number: String,

    /// 开票日期
    pub issue_date: NaiveDate,

    /// 发票金额
    pub amount: Decimal,

    /// 税额
    pub tax_amount: Option<Decimal>,

    /// 购方名称
    pub buyer_name: Option<String>,

    /// 销方名称
    pub seller_name: Option<String>,

    /// 票据类型
    pub ticket_type: TicketType,

    /// 出发城市（交通票）
    pub city: Option<String>,

    /// 出发时间（交通票）
    pub departure_time: Option<NaiveDateTime>,

    /// 入住日期（酒店）
    pub checkin_date: Option<NaiveDate>,

    /// 发票文件路径
    pub file_path: String,

    /// 创建时间
    pub created_at: NaiveDateTime,

    /// 更新时间
    pub updated_at: NaiveDateTime,

    /// 签章验证结果（"valid"/"invalid"/"not_signed" 或 NULL）
    pub verification_result: Option<String>,

    /// 是否标记为重复
    pub is_duplicate: bool,

    /// 重复原因说明
    pub duplicate_reason: Option<String>,
}

/// 一次批次归组运行的待写入快照。
#[derive(Debug, Clone)]
pub struct NewBatchGrouping {
    pub batch_id: i64,
    pub rule_version: String,
    pub home_cities_json: String,
    pub overall_confidence: f32,
    pub ambiguities_json: String,
    pub groups: Vec<NewInvoiceGroup>,
}

#[derive(Debug, Clone)]
pub struct NewInvoiceGroup {
    pub group_index: usize,
    pub kind: String,
    pub title: String,
    pub start_date: String,
    pub end_date: String,
    pub confidence: f32,
    pub requires_review: bool,
    pub evidence_json: String,
    pub members: Vec<NewInvoiceGroupMember>,
}

#[derive(Debug, Clone)]
pub struct NewInvoiceGroupMember {
    pub invoice_id: i64,
    pub input_index: usize,
    pub match_reason: String,
}

/// 批次审核页读取的归组快照。字段保持为简单值，避免存储层依赖归组算法 crate。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchGrouping {
    pub batch_id: i64,
    pub rule_version: String,
    pub home_cities_json: String,
    pub overall_confidence: f32,
    pub ambiguities_json: String,
    pub created_at: String,
    pub groups: Vec<InvoiceGroup>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvoiceGroup {
    pub id: i64,
    pub group_index: usize,
    pub kind: String,
    pub title: String,
    pub start_date: String,
    pub end_date: String,
    pub confidence: f32,
    pub requires_review: bool,
    pub evidence_json: String,
    pub members: Vec<InvoiceGroupMember>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvoiceGroupMember {
    pub invoice_id: i64,
    pub invoice_number: String,
    pub input_index: usize,
    pub match_reason: String,
}

/// 可恢复流水线在 ledger.db 中的持久状态。config_json 不包含会话授权码。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineRun {
    pub pipeline_id: String,
    pub config_json: String,
    pub source_kind: String,
    pub stage: String,
    pub status: String,
    pub task_dir: String,
    pub batch_id: Option<i64>,
    pub last_error: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// 流水线最终事务使用的按输入下标归组快照。事务插入发票后再解析为数据库主键。
#[derive(Debug, Clone)]
pub struct IndexedBatchGrouping {
    pub rule_version: String,
    pub home_cities_json: String,
    pub overall_confidence: f32,
    pub ambiguities_json: String,
    pub groups: Vec<IndexedInvoiceGroup>,
}

#[derive(Debug, Clone)]
pub struct IndexedInvoiceGroup {
    pub group_index: usize,
    pub kind: String,
    pub title: String,
    pub start_date: String,
    pub end_date: String,
    pub confidence: f32,
    pub requires_review: bool,
    pub evidence_json: String,
    pub members: Vec<IndexedInvoiceGroupMember>,
}

#[derive(Debug, Clone)]
pub struct IndexedInvoiceGroupMember {
    pub input_index: usize,
    pub match_reason: String,
}

/// 人工审核保存的完整可编辑字段。签章结果和原件路径不可在 UI 中修改。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvoiceReviewUpdate {
    pub invoice_number: String,
    pub issue_date: NaiveDate,
    pub amount: Decimal,
    pub tax_amount: Option<Decimal>,
    pub buyer_name: Option<String>,
    pub seller_name: Option<String>,
    pub ticket_type: TicketType,
    pub city: Option<String>,
    pub departure_time: Option<NaiveDateTime>,
    pub checkin_date: Option<NaiveDate>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewAction {
    pub id: i64,
    pub batch_id: i64,
    pub action_type: String,
    pub summary: String,
    pub created_at: String,
    pub undone_at: Option<String>,
}

/// 一个批次的 Concur 收据邮件试发与人工确认状态。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConcurSendSession {
    pub batch_id: i64,
    pub sender_email: String,
    pub recipient_email: String,
    pub trial_invoice_id: i64,
    pub trial_status: String,
    pub confirmed_behavior: Option<String>,
    pub confirmed_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// 单张收据的持久化发送状态。status 为 pending/sending/sent/failed/unknown。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConcurSendItem {
    pub batch_id: i64,
    pub invoice_id: i64,
    pub idempotency_key: String,
    pub attachment_name: String,
    pub attachment_sha256: String,
    pub status: String,
    pub attempt_count: i64,
    pub last_error: Option<String>,
    pub message_id: Option<String>,
    pub sent_at: Option<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewConcurSendItem {
    pub invoice_id: i64,
    pub idempotency_key: String,
    pub attachment_name: String,
    pub attachment_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConcurReserveOutcome {
    Reserved(ConcurSendItem),
    AlreadySent(ConcurSendItem),
    InProgress,
}

/// 票据类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TicketType {
    /// 火车票
    Rail,

    /// 飞机票
    Flight,

    /// 酒店
    Hotel,

    /// 城市交通（出租车、地铁、公交）
    CityTransport,

    /// 餐饮
    Meal,

    /// 快递、配送与物流服务
    CourierLogistics,

    /// 其他
    Other,
}

impl TicketType {
    /// 转换为数据库存储的字符串
    pub fn to_str(self) -> &'static str {
        match self {
            TicketType::Rail => "rail",
            TicketType::Flight => "flight",
            TicketType::Hotel => "hotel",
            TicketType::CityTransport => "city_transport",
            TicketType::Meal => "meal",
            TicketType::CourierLogistics => "courier_logistics",
            TicketType::Other => "other",
        }
    }

    /// 从数据库字符串转换
    pub fn from_db_str(s: &str) -> Option<Self> {
        match s {
            "rail" => Some(TicketType::Rail),
            "flight" => Some(TicketType::Flight),
            "hotel" => Some(TicketType::Hotel),
            "city_transport" => Some(TicketType::CityTransport),
            "meal" => Some(TicketType::Meal),
            "courier_logistics" => Some(TicketType::CourierLogistics),
            "other" => Some(TicketType::Other),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batch_status_conversion() {
        assert_eq!(BatchStatus::Draft.to_i32(), 0);
        assert_eq!(BatchStatus::Submitted.to_i32(), 1);
        assert_eq!(BatchStatus::from_i32(0), Some(BatchStatus::Draft));
        assert_eq!(BatchStatus::from_i32(99), None);
    }

    #[test]
    fn ticket_type_conversion() {
        assert_eq!(TicketType::Rail.to_str(), "rail");
        assert_eq!(TicketType::Flight.to_str(), "flight");
        assert_eq!(TicketType::from_db_str("rail"), Some(TicketType::Rail));
        assert_eq!(TicketType::from_db_str("unknown"), None);
    }

    #[test]
    fn ticket_type_roundtrip() {
        let types = [
            TicketType::Rail,
            TicketType::Flight,
            TicketType::Hotel,
            TicketType::CityTransport,
            TicketType::Meal,
            TicketType::CourierLogistics,
            TicketType::Other,
        ];

        for ticket_type in types {
            let s = ticket_type.to_str();
            let parsed = TicketType::from_db_str(s);
            assert_eq!(parsed, Some(ticket_type));
        }
    }
}
