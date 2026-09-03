import type { ExpenseItem, Invoice } from './types'

export type ExpenseReviewIssueCode = 'duplicate' | 'category' | 'date'

type ReviewableInvoice = Pick<Invoice, 'is_duplicate' | 'is_excluded'>
type ReviewableExpense = Pick<
  ExpenseItem,
  'inclusion_status' | 'category_confirmed' | 'transaction_date_confirmed'
> & Partial<Pick<ExpenseItem, 'counterparty_name'>>

/**
 * Returns only the actionable issues owned by the stable expense review step.
 * Missing Concur-mapping fields such as counterparty_name are intentionally
 * non-blocking here and are validated when the user prepares a delivery.
 */
export function blockingExpenseReviewIssues(
  invoice: ReviewableInvoice,
  expense: ReviewableExpense | null,
): ExpenseReviewIssueCode[] {
  const included = expense
    ? expense.inclusion_status === 'included'
    : !invoice.is_excluded && !invoice.is_duplicate
  if (!included) return []

  const issues: ExpenseReviewIssueCode[] = []
  if (invoice.is_duplicate && !invoice.is_excluded) issues.push('duplicate')
  if (!expense?.category_confirmed) issues.push('category')
  if (!expense?.transaction_date_confirmed) issues.push('date')
  return issues
}
