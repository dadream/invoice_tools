import { describe, expect, it } from 'vitest'
import { adjacentReviewInvoiceId, normalizeReviewQueue } from './reviewQueue'

describe('review queue', () => {
  it('keeps the filtered order and never adds invoices outside that queue', () => {
    expect(normalizeReviewQueue([12, 8, 5], [5, 8, 12, 99], 8)).toEqual([12, 8, 5])
  })

  it('keeps the opened invoice reachable when a caller did not provide a queue', () => {
    expect(normalizeReviewQueue([], [5, 8, 12], 8)).toEqual([8])
  })

  it('does not wrap from the end of a filtered queue to its beginning', () => {
    const queue = [12, 8, 5]
    expect(adjacentReviewInvoiceId(queue, 8, 1)).toBe(5)
    expect(adjacentReviewInvoiceId(queue, 5, 1)).toBeNull()
    expect(adjacentReviewInvoiceId(queue, 12, -1)).toBeNull()
  })
})
