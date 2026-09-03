import { describe, expect, it } from 'vitest'
import { blockingExpenseReviewIssues } from './expenseReview'

describe('expense review issues', () => {
  it('does not treat a missing counterparty as an expense-review blocker', () => {
    expect(blockingExpenseReviewIssues(
      { is_duplicate: false, is_excluded: false },
      {
        inclusion_status: 'included',
        category_confirmed: true,
        transaction_date_confirmed: true,
        counterparty_name: '',
      },
    )).toEqual([])
  })

  it('uses the same actionable type and date issues for counts and row status', () => {
    expect(blockingExpenseReviewIssues(
      { is_duplicate: false, is_excluded: false },
      {
        inclusion_status: 'included',
        category_confirmed: false,
        transaction_date_confirmed: false,
      },
    )).toEqual(['category', 'date'])
  })

  it('does not require review for an expense that is already excluded', () => {
    expect(blockingExpenseReviewIssues(
      { is_duplicate: true, is_excluded: true },
      {
        inclusion_status: 'duplicate_suspect',
        category_confirmed: false,
        transaction_date_confirmed: false,
      },
    )).toEqual([])
  })
})
