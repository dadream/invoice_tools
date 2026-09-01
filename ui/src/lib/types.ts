export type BatchStatus = 'draft' | 'submitted' | 'approved' | 'completed' | 'rejected'

export interface Batch {
  id: number
  name: string
  month: string
  status: BatchStatus
  total_amount: string
  invoice_count: number
  created_at: string
  updated_at: string
  submitted_at: string | null
  approved_at: string | null
  completed_at: string | null
  rejected_at: string | null
}

export interface InvoiceGroupMember {
  invoice_id: number
  invoice_number: string
  input_index: number
  match_reason: string
}

export interface InvoiceGroup {
  id: number
  group_index: number
  kind: 'business_trip' | 'local_month' | 'excluded' | 'needs_review' | 'manual'
  title: string
  start_date: string
  end_date: string
  confidence: number
  requires_review: boolean
  evidence_json: string
  members: InvoiceGroupMember[]
}

export interface BatchGrouping {
  batch_id: number
  rule_version: string
  home_cities_json: string
  overall_confidence: number
  ambiguities_json: string
  created_at: string
  groups: InvoiceGroup[]
}

export interface ReviewAction {
  id: number
  batch_id: number
  action_type: string
  summary: string
  created_at: string
  undone_at: string | null
}

export interface BatchReviewSnapshot {
  id: number
  batch_id: number
  version: number
  content_sha256: string
  invoice_count: number
  total_amount: string
  created_at: string
  invalidated_at: string | null
}

export interface DeliveryTask {
  id: number
  batch_id: number
  review_snapshot_id: number
  kind: 'excel' | 'concur'
  status: 'pending' | 'running' | 'succeeded' | 'failed'
  output_path: string | null
  last_error: string | null
  created_at: string
  updated_at: string
  completed_at: string | null
}

export interface ExpenseLocation {
  city_name: string | null
  city_code: string | null
  province_name: string | null
  province_code: string | null
  country_code: string | null
}

export interface ExpenseTaxDetail {
  amount: string
  rate: string | null
  source: string
}

export type ExpenseInclusionStatus = 'included' | 'duplicate_suspect' | 'excluded'
export type PaymentMethod = 'unknown' | 'personal_card' | 'corporate_card' | 'cash' | 'other'
export type DocumentRole = 'main_invoice' | 'itinerary' | 'detail' | 'supporting' | 'duplicate_copy'

export interface InvoiceDocument {
  id: number
  batch_id: number
  expense_item_id: number
  source_invoice_id: number | null
  source_pending_document_id: number | null
  role: DocumentRole
  file_path: string
  original_name: string
  mime_type: string | null
  sha256: string | null
  created_at: string
}

export interface PendingInvoiceDocument {
  id: number
  batch_id: number
  proposed_role: Exclude<DocumentRole, 'main_invoice' | 'duplicate_copy'>
  file_path: string
  original_name: string
  mime_type: string | null
  sha256: string | null
  detection_reason: string
  status: 'pending' | 'attached' | 'ignored'
  assigned_expense_item_id: number | null
  created_at: string
  updated_at: string
}

export type EmailImportMessageStatus =
  | 'imported'
  | 'needs_attachment_review'
  | 'manual_download'
  | 'needs_confirmation'
  | 'not_invoice'
  | 'failed'

export type EmailImportResolutionStatus = 'open' | 'resolved' | 'ignored'

export interface EmailImportAttachment {
  id: number
  message_id: number
  content_sha256: string | null
  original_name: string
  container_name: string | null
  mime_type: string | null
  byte_len: number
  status: 'invoice' | 'supporting' | 'duplicate' | 'not_invoice' | 'unsupported' | 'failed'
  role_hint: 'invoice' | 'itinerary' | 'detail' | 'supporting' | 'unknown'
  reason: string
  reported_invoice_id: number | null
  pending_document_id: number | null
  manual_import: boolean
  created_at: string
  updated_at: string
}

export interface EmailImportMessage {
  id: number
  batch_id: number
  pipeline_id: string
  mailbox_folder: string
  uid: number
  message_id_sha256: string | null
  sender: string
  subject: string
  received_at: string | null
  status: EmailImportMessageStatus
  resolution_status: EmailImportResolutionStatus
  error_category: string | null
  created_at: string
  updated_at: string
  resolved_at: string | null
  attachments: EmailImportAttachment[]
}

export type EmailCollectionTaskStatus =
  | 'created'
  | 'collecting'
  | 'review'
  | 'completed'
  | 'failed'
  | 'interrupted'

export interface EmailCollectionTask {
  id: number
  name: string
  account_email: string
  mailbox_folder: string
  date_start: string
  date_end: string
  status: EmailCollectionTaskStatus
  review_status: 'open' | 'completed'
  pipeline_id: string | null
  last_error_category: string | null
  scanned_message_count: number
  candidate_file_count: number
  actionable_message_count: number
  created_at: string
  updated_at: string
  completed_at: string | null
}

export interface CollectedEmailAttachment {
  id: number
  message_id: number
  content_sha256: string | null
  original_name: string
  container_name: string | null
  mime_type: string | null
  byte_len: number
  status: 'candidate' | 'supporting_candidate' | 'filtered' | 'unsupported' | 'failed'
  role_hint: 'invoice' | 'itinerary' | 'detail' | 'supporting' | 'unknown'
  reason: string
  stored_path: string | null
  manual_import: boolean
  user_excluded: boolean
  user_excluded_at: string | null
  used_batch_ids: number[]
  used_batch_names: string[]
  created_at: string
  updated_at: string
}

export interface CollectedEmailMessage {
  id: number
  task_id: number
  mailbox_folder: string
  uid: number
  message_id_sha256: string | null
  sender: string
  subject: string
  received_at: string | null
  status: 'has_candidates' | 'materials_only' | 'manual_download' | 'needs_confirmation' | 'not_relevant' | 'failed'
  resolution_status: 'open' | 'resolved' | 'ignored'
  error_category: string | null
  created_at: string
  updated_at: string
  resolved_at: string | null
  attachments: CollectedEmailAttachment[]
}

export interface CollectedEmailReviewLink {
  id: number
  label: string
  host: string
  scheme: 'http' | 'https'
}

export interface CollectedEmailReviewDetail {
  available: boolean
  senderName: string | null
  senderAddress: string | null
  bodyText: string
  bodyTruncated: boolean
  analyzedAt: string | null
  links: CollectedEmailReviewLink[]
}

export interface BatchCollectionImport {
  id: number
  batch_id: number
  task_id: number
  task_name: string
  status: 'pending' | 'processing' | 'completed' | 'failed' | 'legacy'
  pipeline_id: string | null
  item_count: number
  created_at: string
  updated_at: string
}

export interface ExpenseItem {
  id: number
  batch_id: number
  primary_invoice_id: number
  model_version: number
  category_code: ExpenseCategory
  category_source: string
  category_confirmed: boolean
  transaction_date: string
  transaction_date_source: string
  transaction_date_confirmed: boolean
  description: string
  counterparty_name: string
  location: ExpenseLocation
  payment_method: PaymentMethod
  gross_amount: string
  currency_code: string
  tax_details: ExpenseTaxDetail[]
  trip_group_id: number | null
  inclusion_status: ExpenseInclusionStatus
  provenance_json: string
  documents: InvoiceDocument[]
  created_at: string
  updated_at: string
}

export interface ConcurMappingProfile {
  id: number
  name: string
  company_label: string
  version: number
  status: 'active' | 'archived'
  adapter_kind: 'ui_assisted' | 'api'
  field_rules_json: string
  expense_type_map_json: string
  location_map_json: string
  payment_type_map_json: string
  vat_rate_map_json: string
  required_fields_json: string
  custom_fields_json: string
  created_at: string
  updated_at: string
}

export interface ConcurUploadSession {
  id: number
  batch_id: number
  review_snapshot_id: number
  mapping_profile_id: number
  mapping_profile_version: number
  report_name: string
  report_date: string
  comment: string
  status: 'preflight' | 'ready' | 'running' | 'partial' | 'draft_created' | 'needs_verification' | 'failed'
  idempotency_key: string
  external_report_id: string | null
  upload_overrides_json: string
  mapped_payload_json: string
  gaps_json: string
  last_error: string | null
  created_at: string
  updated_at: string
}

export interface ConcurMappingGap {
  scope: 'mapping_profile' | 'expense_fact' | 'target_override' | 'attachment' | string
  expense_item_id: number | null
  field_key: string
  message: string
  resolution: string
}

export interface MappedExpensePayload {
  expense_item_id: number
  target_fields_json: string
  attachment_document_ids: number[]
}

export interface ConcurUploadPreflight {
  session: ConcurUploadSession
  expenses: MappedExpensePayload[]
  gaps: ConcurMappingGap[]
  ready: boolean
}

export interface ConcurUploadAttachmentState {
  id: number
  document_id: number
  status: string
  idempotency_key: string
  external_attachment_id: string | null
  attempt_count: number
  last_error: string | null
  last_verified_at: string | null
  updated_at: string
}

export interface ConcurUploadItemState {
  id: number
  expense_item_id: number
  status: string
  idempotency_key: string
  mapped_payload_json: string
  external_expense_id: string | null
  attempt_count: number
  last_error: string | null
  last_verified_at: string | null
  updated_at: string
  attachments: ConcurUploadAttachmentState[]
}

export interface ConcurUploadStatus {
  session: ConcurUploadSession
  items: ConcurUploadItemState[]
}

export interface ConcurDraftCapability {
  enabled: boolean
  adapter_status: string
  reason: string
  required_confirmations: string[]
}


export interface CreateBatchInput {
  name: string
}

export const STATUS_LABELS: Record<BatchStatus, string> = {
  draft: '审核中',
  submitted: '审核已完成',
  approved: '交付处理中',
  completed: '已交付',
  rejected: '已作废',
}

export const STATUS_COLORS: Record<BatchStatus, string> = {
  draft: '#657068',
  submitted: '#c47a16',
  approved: '#136b52',
  completed: '#354139',
  rejected: '#b33a32',
}

// 可用的状态转换
export const ALLOWED_TRANSITIONS: Record<BatchStatus, BatchStatus[]> = {
  draft: ['submitted', 'rejected'],
  submitted: ['approved', 'rejected'],
  approved: ['completed', 'rejected'],
  completed: [],
  rejected: [],
}

export function formatAmount(amount: string, currencyCode = 'CNY'): string {
  const num = parseFloat(amount)
  const currency = /^[A-Z]{3}$/.test(currencyCode.trim().toUpperCase())
    ? currencyCode.trim().toUpperCase()
    : 'CNY'
  if (isNaN(num)) return `${currency} 0.00`
  try {
    return new Intl.NumberFormat('zh-CN', {
      style: 'currency',
      currency,
      currencyDisplay: 'symbol',
      minimumFractionDigits: 2,
      maximumFractionDigits: 2,
    }).format(num)
  } catch {
    return `${currency} ${num.toFixed(2).replace(/\B(?=(\d{3})+(?!\d))/g, ',')}`
  }
}

export function formatDate(date: string): string {
  return date.replace(' ', ' ')
}

// ---------------------------------------------------------------------------
// 发票（S0.7）
//
// 字段名与后端 DTO 一一对应，均为 snake_case：
// `ParsedInvoiceDto` / `InvoiceDto` / `DuplicateCheckDto`
// （见 src-tauri/src/commands/invoice.rs，三者都没有 serde rename_all）。
// 注意命令**参数名**是另一套规则：`#[tauri::command]` 默认把参数转成
// camelCase，因此 invoke 时传 `batchId`/`ticketType`/`invoiceNumber`/`invoiceId`。
// ---------------------------------------------------------------------------

export type TicketType = 'rail' | 'flight' | 'hotel' | 'city_transport' | 'meal' | 'courier_logistics' | 'other'

/** 软件自身的稳定费用分类；与票据类型和 Concur 费用类型均独立。 */
export type ExpenseCategory = 'rail' | 'flight' | 'hotel' | 'city_transport' | 'meal' | 'courier_logistics' | 'other'

export type ParseLevel = 'L0' | 'L1' | 'L2' | 'L4'

export type VerificationResult = 'valid' | 'invalid' | 'unsupported' | 'not_signed' | 'not_applicable'

/** 解析结果，尚未入库。金额与税率是字符串（后端 Decimal），仅用于展示。 */
export interface ParsedInvoice {
  invoice_number: string
  issue_date: string
  total_amount: string
  tax_amount: string | null
  tax_rate: string | null
  buyer_name: string | null
  seller_name: string | null
  ticket_type: TicketType
  parse_level: ParseLevel
  confidence: number
  city: string | null
  departure_time: string | null
  checkin_date: string | null
  source_path: string
  verification_result: VerificationResult
}

/** 已入库发票。金额列名是 `amount`（与 `ParsedInvoice.total_amount` 不同）。 */
export interface Invoice {
  id: number
  batch_id: number
  invoice_number: string
  issue_date: string
  amount: string
  tax_amount: string | null
  buyer_name: string | null
  seller_name: string | null
  ticket_type: TicketType
  city: string | null
  departure_time: string | null
  checkin_date: string | null
  file_path: string
  created_at: string
  verification_result: VerificationResult | null
  is_duplicate: boolean
  duplicate_reason: string | null
  is_excluded: boolean
}


export interface InvoicePreviewMetadata {
  file_name: string
  extension: string
  mime_type: string | null
  preview_kind: 'image' | 'pdf' | 'ofd' | 'text' | 'unsupported' | 'too_large'
  bytes: number
  page_count: number | null
}

export interface OfdPreviewText {
  text: string
  x_mm: number
  y_mm: number
  width_mm: number
  height_mm: number
  font_size_mm: number
}

export interface OfdPreviewPage {
  page: number
  width_mm: number
  height_mm: number
  texts: OfdPreviewText[]
}

export interface PdfPreviewText {
  text: string
  x: number
  y: number
  width: number
  height: number
}

export interface PdfTextPreviewPage {
  page: number
  width: number
  height: number
  texts: PdfPreviewText[]
}

export interface InvoiceSummary {
  id: number
  batch_id: number
  batch_name: string
  invoice_number: string
  amount: string
  issue_date: string
}

export interface DuplicateCheck {
  is_duplicate: boolean
  match_type: 'exact' | 'fuzzy' | null
  existing_invoices: InvoiceSummary[]
}

/** 下拉框顺序；词表与后端 `StoreTicketType::to_str()` 一致。 */
export const TICKET_TYPES: readonly TicketType[] = [
  'rail',
  'flight',
  'hotel',
  'city_transport',
  'meal',
  'courier_logistics',
  'other',
]

export const TICKET_TYPE_LABELS: Record<TicketType, string> = {
  rail: '火车票',
  flight: '机票',
  hotel: '酒店',
  city_transport: '市内交通',
  meal: '餐饮',
  courier_logistics: '快递/物流',
  other: '其他',
}

export const EXPENSE_CATEGORIES: readonly ExpenseCategory[] = [
  'rail',
  'flight',
  'hotel',
  'city_transport',
  'meal',
  'courier_logistics',
  'other',
]

export const EXPENSE_CATEGORY_LABELS: Record<ExpenseCategory, string> = {
  rail: '火车',
  flight: '机票',
  hotel: '住宿',
  city_transport: '市内交通',
  meal: '餐饮',
  courier_logistics: '快递/物流',
  other: '其他',
}

export const DATE_SOURCE_LABELS: Record<string, string> = {
  manual_review: '人工确认',
  departure_time: '出发日期',
  checkin_date: '入住日期',
  service_date: '消费日期',
  invoice_issue_date_candidate: '开票日期候选',
}

export const CATEGORY_SOURCE_LABELS: Record<string, string> = {
  'parser.classification': '票面规则识别',
  'invoice.ticket_type': '票据类型推断',
  'legacy.ticket_type': '历史票据类型',
  manual_review: '人工确认',
  unclassified: '尚无可靠依据',
}

export function expenseCategoryLabel(
  expense: Pick<ExpenseItem, 'category_code' | 'category_confirmed'>,
): string {
  return expense.category_confirmed
    ? EXPENSE_CATEGORY_LABELS[expense.category_code]
    : '待分类'
}

export function transactionDateSourceLabel(source: string): string {
  return DATE_SOURCE_LABELS[source] ?? '系统候选'
}

export function expenseCategorySourceLabel(source: string): string {
  return CATEGORY_SOURCE_LABELS[source] ?? '系统建议'
}

/** 解析级别的可信度说明，供确认页提示用户是否需要核对。 */
export const PARSE_LEVEL_HINTS: Record<ParseLevel, string> = {
  L0: '结构化直读，字段可信',
  L1: '版式解析，建议核对金额',
  L2: 'OCR 识别，请逐项核对',
  L4: '字段冲突，必须人工确认',
}

/** 解析级别对应的提示强度，驱动徽标配色。 */
export const PARSE_LEVEL_SEVERITY: Record<ParseLevel, 'ok' | 'warn' | 'danger'> = {
  L0: 'ok',
  L1: 'ok',
  L2: 'warn',
  L4: 'danger',
}

/** 验签结果标签 */
export const VERIFICATION_LABELS: Record<VerificationResult, string> = {
  valid: '✓ 签章验证通过',
  invalid: '✗ 签章验证失败',
  unsupported: '签章格式暂不支持',
  not_signed: '未发现数字签章',
  not_applicable: '无需验签',
}

/** 验签结果配色 */
export const VERIFICATION_COLORS: Record<VerificationResult, string> = {
  valid: 'green',
  invalid: 'red',
  unsupported: 'amber',
  not_signed: 'gray',
  not_applicable: 'gray',
}

/**
 * 仅用于展示的金额合计。
 *
 * 前端把 Decimal 字符串转 Number 求和会有浮点误差，精确合计以后端为准
 * （批次的 `total_amount` 由数据库侧统计）。
 */
export function sumAmounts(amounts: string[]): string {
  const total = amounts.reduce((acc, raw) => {
    const num = parseFloat(raw)
    return acc + (isNaN(num) ? 0 : num)
  }, 0)
  return total.toFixed(2)
}
