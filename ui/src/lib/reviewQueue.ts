export interface ReviewQueueContext {
  invoiceIds: number[]
  label: string
}

export function normalizeReviewQueue(
  invoiceIds: number[],
  availableInvoiceIds: number[],
  initialInvoiceId: number,
): number[] {
  const available = new Set(availableInvoiceIds)
  const seen = new Set<number>()
  const normalized = invoiceIds.filter((invoiceId) => {
    if (!available.has(invoiceId) || seen.has(invoiceId)) return false
    seen.add(invoiceId)
    return true
  })
  if (available.has(initialInvoiceId) && !seen.has(initialInvoiceId)) normalized.unshift(initialInvoiceId)
  return normalized
}

export function adjacentReviewInvoiceId(
  invoiceIds: number[],
  currentInvoiceId: number,
  direction: -1 | 1,
): number | null {
  const index = invoiceIds.indexOf(currentInvoiceId)
  if (index < 0) return null
  return invoiceIds[index + direction] ?? null
}
