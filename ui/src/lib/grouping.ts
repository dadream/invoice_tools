import type { ExpenseItem, InvoiceGroup, InvoiceGroupMember } from './types'

export type TransportEvidenceStatus = 'present' | 'missing' | 'company_paid' | 'not_required'
export type TransportDocumentKind = 'sale' | 'refund' | 'change' | 'unknown'

function evidence(group: InvoiceGroup): Record<string, unknown> {
  try {
    const value: unknown = JSON.parse(group.evidence_json)
    return value !== null && typeof value === 'object' && !Array.isArray(value)
      ? value as Record<string, unknown>
      : {}
  } catch {
    return {}
  }
}

export function transportDocumentKind(member: InvoiceGroupMember | undefined): TransportDocumentKind {
  const reason = member?.match_reason ?? ''
  if (reason.includes('交通票性质：退票费')) return 'refund'
  if (reason.includes('交通票性质：改签费')) return 'change'
  if (reason.includes('交通票性质：有效售票')) return 'sale'
  return 'unknown'
}

export function transportDocumentKindForInvoice(group: InvoiceGroup, invoiceId: number): TransportDocumentKind {
  return transportDocumentKind(group.members.find((member) => member.invoice_id === invoiceId))
}

export function groupTransportEvidenceStatus(group: InvoiceGroup, expenses: ExpenseItem[]): TransportEvidenceStatus {
  const stored = evidence(group).transportEvidenceStatus
  if (stored === 'present' || stored === 'missing' || stored === 'company_paid' || stored === 'not_required') return stored
  const hasRouteAnchor = expenses.some((expense) => {
    if (!group.members.some((member) => member.invoice_id === expense.primary_invoice_id)) return false
    const kind = transportDocumentKindForInvoice(group, expense.primary_invoice_id)
    return ((expense.category_code === 'rail' || expense.category_code === 'flight') && kind !== 'refund' && kind !== 'change')
      || expense.documents.some((document) => document.role === 'itinerary')
  })
  return hasRouteAnchor ? 'present' : 'missing'
}

export function displayGroupTitle(group: InvoiceGroup, expenses: ExpenseItem[] = []): string {
  const source = evidence(group).source
  if (source === 'manual_review' || group.kind === 'manual') return group.title
  if (group.kind === 'local_month') {
    const match = group.start_date.match(/^\d{4}-(\d{2})/)
    return match ? `${Number(match[1])} 月市内消费` : '市内消费'
  }
  if (group.kind === 'excluded') return '未计入费用'
  if (group.kind === 'needs_review') return '待确定归属的费用'
  if (group.kind !== 'business_trip') return group.title

  const current = group.title.split('· 含 ')[0].trim()
  if (current.endsWith('出差') && !/^\d{4}-/.test(current)) return current
  const legacyDestination = current.match(/^\d{4}-\d{2}-\d{2}\s+至\s+\d{4}-\d{2}-\d{2}\s+·\s+(.+)$/)?.[1]
  const expenseCities = Array.from(new Set(
    expenses
      .filter((expense) => group.members.some((member) => member.invoice_id === expense.primary_invoice_id))
      .map((expense) => expense.location.city_name)
      .filter((city): city is string => Boolean(city)),
  ))
  const destination = (legacyDestination || expenseCities.at(-1) || '目的地待确认')
    .split(/\s*(?:→|、|,)\s*/)
    .filter(Boolean)
    .join('、')
  return `${destination}出差`
}
