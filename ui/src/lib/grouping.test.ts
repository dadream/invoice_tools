import { describe, expect, it } from 'vitest'
import { displayGroupTitle, transportDocumentKind, transportRouteForInvoice } from './grouping'
import type { InvoiceGroup, InvoiceGroupMember } from './types'

function group(title: string, kind: InvoiceGroup['kind'] = 'business_trip'): InvoiceGroup {
  return {
    id: 1, group_index: 0, kind, title,
    start_date: '2026-06-04', end_date: '2026-06-05', confidence: 1,
    requires_review: true, evidence_json: '{}', members: [],
  }
}

describe('grouping presentation', () => {
  it('normalizes legacy business-trip titles', () => {
    expect(displayGroupTitle(group('2026-06-04 至 2026-06-05 · 赤峰 → 太原'))).toBe('赤峰、太原出差')
  })

  it('uses one local-month label without the year', () => {
    expect(displayGroupTitle(group('2026 年 6 月市内消费', 'local_month'))).toBe('6 月市内消费')
  })

  it('uses a dedicated courier-month label', () => {
    expect(displayGroupTitle(group('2026 年 6 月快递物流', 'courier_month'))).toBe('6 月快递物流')
  })

  it('reads the exact route for a transport member', () => {
    const value = group('邢台出差')
    value.members = [{ invoice_id: 9, invoice_number: 'x', input_index: 47, match_reason: '' }]
    value.evidence_json = JSON.stringify({
      transportRoutes: [{ inputIndex: 47, route: '北京西→邢台东' }],
    })
    expect(transportRouteForInvoice(value, 9)).toBe('北京西→邢台东')
  })

  it('recognizes refund members from the persisted match reason', () => {
    const member: InvoiceGroupMember = {
      invoice_id: 9, invoice_number: 'x', input_index: 0,
      match_reason: 'deterministic-v2：按类型归组；交通票性质：退票费，不作为路线节点',
    }
    expect(transportDocumentKind(member)).toBe('refund')
  })
})
