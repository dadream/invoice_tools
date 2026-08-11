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
