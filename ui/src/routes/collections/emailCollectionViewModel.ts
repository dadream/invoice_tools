import type { CollectedEmailMessage } from '../../lib/types'

export type MessageGroup = 'needs_action' | 'pending' | 'reviewed'
export type SortDirection = 'desc' | 'asc'

export function messageGroup(message: CollectedEmailMessage): MessageGroup {
  if (message.resolution_status !== 'open') return 'reviewed'
  return ['materials_only', 'manual_download', 'needs_confirmation', 'failed'].includes(message.status)
    ? 'needs_action'
    : 'pending'
}

export function groupLabel(group: MessageGroup): string {
  return group === 'needs_action' ? '需要用户处理' : group === 'pending' ? '待审核' : '已审核'
}

/** Display only the calendar date; never apply a timezone conversion to server text. */
export function receivedDate(value: string | null): string {
  if (!value) return '日期未知'
  const match = value.match(/\d{4}-\d{2}-\d{2}/)
  return match?.[0] ?? value.slice(0, 10)
}

export function sortCollectedMessages(
  source: CollectedEmailMessage[],
  direction: SortDirection,
): CollectedEmailMessage[] {
  return [...source].sort((left, right) => {
    const a = left.received_at ? receivedDate(left.received_at) : ''
    const b = right.received_at ? receivedDate(right.received_at) : ''
    if (!a && !b) return right.id - left.id
    if (!a) return 1
    if (!b) return -1
    const dateOrder = a.localeCompare(b)
    return dateOrder === 0 ? right.id - left.id : direction === 'desc' ? -dateOrder : dateOrder
  })
}

/** Keep review navigation inside the group from which the user entered. */
export function messagesForReviewGroup(
  source: CollectedEmailMessage[],
  group: MessageGroup,
  direction: SortDirection,
  currentMessageId?: number | null,
): CollectedEmailMessage[] {
  return sortCollectedMessages(
    source.filter((message) => messageGroup(message) === group || message.id === currentMessageId),
    direction,
  )
}

export function collectionProcessResult(message: CollectedEmailMessage): string {
  if (message.resolution_status === 'ignored') return '用户确认无关'
  const saved = message.attachments.filter((attachment) => attachment.stored_path).length
  if (message.resolution_status === 'resolved') return saved ? `材料已确认（${saved}）` : '用户已完成处理'
  if (message.status === 'has_candidates') return `已保存 ${saved} 个候选材料`
  if (message.status === 'materials_only') return `已保存 ${saved} 个配套材料`
  if (message.status === 'manual_download') return '邮件含下载链接，需用户取得文件'
  if (message.status === 'needs_confirmation') return '疑似开票通知，需用户确认'
  if (message.status === 'failed') return '邮件或附件读取失败'
  return '系统判断与报销无关'
}
