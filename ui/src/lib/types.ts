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

export interface CreateBatchInput {
  name: string
  month: string
}

export const STATUS_LABELS: Record<BatchStatus, string> = {
  draft: '草稿',
  submitted: '已提交',
  approved: '已审批',
  completed: '已完成',
  rejected: '已驳回',
}

export const STATUS_COLORS: Record<BatchStatus, string> = {
  draft: '#999',
  submitted: '#0070f3',
  approved: '#0a7',
  completed: '#666',
  rejected: '#c33',
}

// 可用的状态转换
export const ALLOWED_TRANSITIONS: Record<BatchStatus, BatchStatus[]> = {
  draft: ['submitted', 'rejected'],
  submitted: ['approved', 'rejected'],
  approved: ['completed', 'rejected'],
  completed: [],
  rejected: [],
}

export function formatAmount(amount: string): string {
  const num = parseFloat(amount)
  if (isNaN(num)) return '¥0.00'
  return '¥' + num.toFixed(2).replace(/\B(?=(\d{3})+(?!\d))/g, ',')
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

export type TicketType = 'rail' | 'flight' | 'hotel' | 'city_transport' | 'meal' | 'other'

export type ParseLevel = 'L0' | 'L1' | 'L2' | 'L4'

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
}

export interface DuplicateCheck {
  is_duplicate: boolean
  existing_batch_id: number | null
  existing_batch_name: string | null
}

/** 下拉框顺序；词表与后端 `StoreTicketType::to_str()` 一致。 */
export const TICKET_TYPES: readonly TicketType[] = [
  'rail',
  'flight',
  'hotel',
  'city_transport',
  'meal',
  'other',
]

export const TICKET_TYPE_LABELS: Record<TicketType, string> = {
  rail: '火车票',
  flight: '机票',
  hotel: '酒店',
  city_transport: '市内交通',
  meal: '餐饮',
  other: '其他',
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
