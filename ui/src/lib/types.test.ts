import { describe, expect, it } from 'vitest'
import {
  expenseCategoryLabel,
  formatAmount,
  transactionDateSourceLabel,
} from './types'

describe('expense presentation helpers', () => {
  it('does not present an unconfirmed fallback as a real Other category', () => {
    expect(expenseCategoryLabel({ category_code: 'other', category_confirmed: false })).toBe('待分类')
    expect(expenseCategoryLabel({ category_code: 'other', category_confirmed: true })).toBe('其他')
    expect(expenseCategoryLabel({ category_code: 'meal', category_confirmed: true })).toBe('餐饮')
    expect(expenseCategoryLabel({ category_code: 'courier_logistics', category_confirmed: true })).toBe('快递/物流')
  })

  it('exposes the business meaning of transaction date sources', () => {
    expect(transactionDateSourceLabel('departure_time')).toBe('出发日期')
    expect(transactionDateSourceLabel('invoice_issue_date_candidate')).toBe('开票日期候选')
  })

  it('formats the expense currency instead of always forcing CNY', () => {
    expect(formatAmount('123.45', 'CNY')).toContain('123.45')
    expect(formatAmount('123.45', 'USD')).toContain('123.45')
    expect(formatAmount('123.45', 'USD')).not.toMatch(/^¥/)
  })
})
