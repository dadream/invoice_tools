<script lang="ts">
  import { untrack } from 'svelte'
  import { open } from '@tauri-apps/plugin-dialog'
  import { describeError, invokeSafe } from '../../lib/ipc'
  import { displayGroupTitle } from '../../lib/grouping'
  import {
    adjacentReviewInvoiceId, normalizeReviewQueue, type ReviewQueueContext,
  } from '../../lib/reviewQueue'
  import type {
    BatchGrouping, DocumentRole, ExpenseCategory, ExpenseItem, Invoice, InvoiceGroup, PaymentMethod, TicketType,
  } from '../../lib/types'
  import {
    EXPENSE_CATEGORIES, EXPENSE_CATEGORY_LABELS, TICKET_TYPES, TICKET_TYPE_LABELS,
    expenseCategoryLabel, expenseCategorySourceLabel, formatAmount, transactionDateSourceLabel,
  } from '../../lib/types'
  import ConfirmDialog from '../../lib/ConfirmDialog.svelte'
  import OriginalPreview from './OriginalPreview.svelte'

  interface Props {
    batchId: number
    initialInvoiceId: number
    reviewQueue: ReviewQueueContext
    invoices: Invoice[]
    expenseItems: ExpenseItem[]
    grouping: BatchGrouping | null
    canEdit: boolean
    onChanged: () => Promise<void>
    onBack: (message?: string) => void
  }

  interface ReviewIssue {
    tone: 'danger' | 'warning' | 'muted' | 'ok'
    label: string
    detail: string
    action?: 'confirm-date' | 'confirm-category'
    actionLabel?: string
  }

  let { batchId, initialInvoiceId, reviewQueue, invoices, expenseItems, grouping, canEdit, onChanged, onBack }: Props = $props()
  let selectedInvoiceId = $state(untrack(() => initialInvoiceId))
  let previewDocumentId = $state<number | null>(null)
  let viewerOpen = $state(false)
  let viewerCollapsed = $state(false)
  let viewerFullscreen = $state(false)
  let working = $state<string | null>(null)
  let actionError = $state<string | null>(null)
  let statusMessage = $state<string | null>(null)
  let loadedSignature = ''
  let previewExpenseId = $state<number | null>(null)
  let attachRole = $state<Exclude<DocumentRole, 'main_invoice'>>('itinerary')
  let duplicateTargetExpenseId = $state('')
  let confirmation = $state<
    | { kind: 'remove-document'; documentId: number }
    | { kind: 'confirm-duplicate' }
    | { kind: 'mark-distinct' }
    | { kind: 'toggle-excluded'; excluding: boolean }
    | null
  >(null)

  let invoiceNumber = $state('')
  let issueDate = $state('')
  let amount = $state('')
  let taxAmount = $state('')
  let buyerName = $state('')
  let sellerName = $state('')
  let ticketType = $state<TicketType>('other')
  let city = $state('')
  let departureTime = $state('')
  let checkinDate = $state('')

  let expenseCategory = $state<ExpenseCategory>('other')
  let expenseCategoryConfirmed = $state(false)
  let transactionDate = $state('')
  let transactionDateConfirmed = $state(false)
  let description = $state('')
  let counterpartyName = $state('')
  let expenseCity = $state('')
  let provinceName = $state('')
  let countryCode = $state('CN')
  let paymentMethod = $state<PaymentMethod>('unknown')
  let grossAmount = $state('')
  let currencyCode = $state('CNY')
  let expenseTaxAmount = $state('')
  let expenseTaxRate = $state('')

  const groups = $derived(grouping?.groups ?? [])
  const queueInvoiceIds = $derived(normalizeReviewQueue(reviewQueue.invoiceIds, invoices.map((invoice) => invoice.id), initialInvoiceId))
  const orderedInvoices = $derived(queueInvoiceIds.map((invoiceId) => invoices.find((invoice) => invoice.id === invoiceId)).filter((invoice): invoice is Invoice => Boolean(invoice)))
  const selectedInvoice = $derived(invoices.find((invoice) => invoice.id === selectedInvoiceId) ?? null)
  const selectedExpense = $derived(expenseForInvoice(selectedInvoiceId))
  const selectedIndex = $derived(orderedInvoices.findIndex((invoice) => invoice.id === selectedInvoiceId))
  const previewDocument = $derived(selectedExpense?.documents.find((document) => document.id === previewDocumentId) ?? null)
  const hasMainInvoiceDocument = $derived(selectedExpense?.documents.some((document) => document.role === 'main_invoice') ?? false)
  const supportingDocuments = $derived(selectedExpense?.documents.filter((document) => document.role !== 'main_invoice') ?? [])
  const usesPaperInvoice = $derived(isPaperInvoiceExpense(selectedExpense))
  const duplicateTargets = $derived(expenseItems.filter((expense) =>
    expense.inclusion_status === 'included' && expense.id !== selectedExpense?.id,
  ))
  const duplicateComparisonExpense = $derived(
    duplicateTargets.find((expense) => String(expense.id) === duplicateTargetExpenseId) ?? null,
  )
  const duplicateComparisonInvoice = $derived(
    invoices.find((invoice) => invoice.id === duplicateComparisonExpense?.primary_invoice_id) ?? null,
  )
  const currentGroup = $derived.by((): InvoiceGroup | null => {
    if (!selectedInvoice) return null
    return groups.find((group) => group.members.some((member) => member.invoice_id === selectedInvoice.id)) ?? null
  })
  const invoiceFormDirty = $derived(Boolean(selectedInvoice && (
    invoiceNumber !== selectedInvoice.invoice_number
      || issueDate !== selectedInvoice.issue_date
      || amount !== selectedInvoice.amount
      || taxAmount !== (selectedInvoice.tax_amount ?? '')
      || buyerName !== (selectedInvoice.buyer_name ?? '')
      || sellerName !== (selectedInvoice.seller_name ?? '')
      || ticketType !== selectedInvoice.ticket_type
      || city !== (selectedInvoice.city ?? '')
      || departureTime !== (selectedInvoice.departure_time ? selectedInvoice.departure_time.replace(' ', 'T').slice(0, 16) : '')
      || checkinDate !== (selectedInvoice.checkin_date ?? '')
  )))
  const expenseFormDirty = $derived(Boolean(selectedExpense && (
    expenseCategory !== selectedExpense.category_code
      || expenseCategoryConfirmed !== selectedExpense.category_confirmed
      || transactionDate !== selectedExpense.transaction_date
      || transactionDateConfirmed !== selectedExpense.transaction_date_confirmed
      || description !== selectedExpense.description
      || counterpartyName !== selectedExpense.counterparty_name
      || expenseCity !== (selectedExpense.location.city_name ?? '')
      || provinceName !== (selectedExpense.location.province_name ?? '')
      || countryCode !== (selectedExpense.location.country_code ?? 'CN')
      || paymentMethod !== selectedExpense.payment_method
      || grossAmount !== selectedExpense.gross_amount
      || currencyCode !== selectedExpense.currency_code
      || expenseTaxAmount !== (selectedExpense.tax_details[0]?.amount ?? '')
      || expenseTaxRate !== (selectedExpense.tax_details[0]?.rate ?? '')
  )))
  const hasUnsavedChanges = $derived(invoiceFormDirty || expenseFormDirty)
  const amountDifference = $derived(Number(grossAmount || 0) - Number(amount || 0))
  const reviewIssues = $derived.by((): ReviewIssue[] => {
    if (!selectedInvoice) return []
    const issues: ReviewIssue[] = []
    if (selectedInvoice.is_excluded && !selectedInvoice.is_duplicate) {
      issues.push({ tone: 'muted', label: '已排除', detail: '不计入批次金额与后续交付，原件仍完整保留。' })
    }
    if (selectedInvoice.is_duplicate) {
      issues.push({
        tone: selectedInvoice.is_excluded ? 'muted' : 'danger',
        label: selectedInvoice.is_excluded ? '已确认重复' : '疑似重复',
        detail: selectedInvoice.is_excluded
          ? '已确认不计入批次总额；原件仍可关联到保留费用。'
          : selectedInvoice.duplicate_reason ?? '需明确判断后才能完成审核。',
      })
    }
    if (selectedExpense && !selectedExpense.transaction_date_confirmed) {
      issues.push({
        tone: 'warning',
        label: '实际发生日期待确认',
        detail: `当前候选日期为 ${transactionDate || selectedExpense.transaction_date}，请按业务事实确认。`,
        action: 'confirm-date',
        actionLabel: `确认使用 ${transactionDate || selectedExpense.transaction_date}`,
      })
    }
    if (selectedExpense && !selectedExpense.category_confirmed) {
      issues.push({
        tone: 'warning',
        label: '费用类型待确认',
        detail: '系统未找到高置信分类依据，请根据消费事实选择并确认。',
        action: 'confirm-category',
        actionLabel: `确认类型：${EXPENSE_CATEGORY_LABELS[expenseCategory]}`,
      })
    }
    if (Math.abs(amountDifference) >= 0.005) {
      issues.push({
        tone: 'warning',
        label: '费用金额与票面金额不同',
        detail: `费用按实际金额 ${formatAmount(grossAmount || '0')} 计入；票面金额为 ${formatAmount(amount || '0')}。`,
      })
    }
    if (!selectedExpense?.counterparty_name.trim()) {
      issues.push({ tone: 'warning', label: '交易方缺失', detail: '建议从原件核对并补充。' })
    }
    if (issues.length === 0) issues.push({ tone: 'ok', label: '未发现阻断项', detail: '保存后可继续核对下一笔费用。' })
    return issues
  })

  function optional(value: string): string | null {
    const trimmed = value.trim()
    return trimmed ? trimmed : null
  }

  function validateExpenseForm(): string | null {
    const decimalPattern = /^(?:\d+(?:\.\d*)?|\.\d+)$/
    const normalizedCurrency = currencyCode.trim().toUpperCase()
    if (!transactionDate) return '请选择实际发生日期'
    if (!decimalPattern.test(grossAmount.trim())) return '实际报销金额格式无效'
    if (Number(grossAmount) < 0) return '实际报销金额不能小于 0'
    if (!/^[A-Z]{3}$/.test(normalizedCurrency)) return '币种必须是 3 位大写字母，例如 CNY'
    if (description.length > 500) return '业务说明不能超过 500 个字符'
    if (counterpartyName.length > 200) return '交易方名称不能超过 200 个字符'
    if (expenseTaxAmount.trim()) {
      if (!decimalPattern.test(expenseTaxAmount.trim())) return '费用税额格式无效'
      if (Number(expenseTaxAmount) > Number(grossAmount)) return '费用税额不能大于实际报销金额'
    }
    if (expenseTaxRate.trim() && !decimalPattern.test(expenseTaxRate.trim())) return '费用税率格式无效，例如 0.06'
    currencyCode = normalizedCurrency
    return null
  }

  function documentRoleLabel(role: DocumentRole): string {
    return { itinerary: '行程单', detail: '明细', supporting: '其他材料', duplicate_copy: '重复副本', main_invoice: '主发票' }[role]
  }

  function isPaperInvoiceExpense(expense: ExpenseItem | null): boolean {
    if (!expense) return false
    try {
      const provenance = JSON.parse(expense.provenance_json) as Record<string, unknown>
      return provenance.main_invoice === 'paper_not_imported'
    } catch {
      return false
    }
  }

  function expenseForInvoice(invoiceId: number): ExpenseItem | null {
    return expenseItems.find((expense) => expense.primary_invoice_id === invoiceId) ?? null
  }

  function populateForm(invoice: Invoice) {
    invoiceNumber = invoice.invoice_number
    issueDate = invoice.issue_date
    amount = invoice.amount
    taxAmount = invoice.tax_amount ?? ''
    buyerName = invoice.buyer_name ?? ''
    sellerName = invoice.seller_name ?? ''
    ticketType = invoice.ticket_type
    city = invoice.city ?? ''
    departureTime = invoice.departure_time ? invoice.departure_time.replace(' ', 'T').slice(0, 16) : ''
    checkinDate = invoice.checkin_date ?? ''
    duplicateTargetExpenseId = ''
    if (invoice.is_duplicate) {
      const currentExpense = expenseForInvoice(invoice.id)
      const ranked = expenseItems
        .filter((expense) => expense.inclusion_status === 'included' && expense.id !== currentExpense?.id)
        .map((expense) => {
          const target = invoices.find((candidate) => candidate.id === expense.primary_invoice_id)
          const score = (target?.invoice_number === invoice.invoice_number ? 8 : 0)
            + (target?.issue_date === invoice.issue_date ? 4 : 0)
            + (expense.gross_amount === invoice.amount ? 2 : 0)
            + (target?.seller_name === invoice.seller_name ? 1 : 0)
          return { expense, score }
        })
        .sort((left, right) => right.score - left.score || left.expense.id - right.expense.id)
      duplicateTargetExpenseId = ranked[0] ? String(ranked[0].expense.id) : ''
    }
  }

  function populateExpenseForm(expense: ExpenseItem | null) {
    if (!expense) return
    expenseCategory = expense.category_code
    expenseCategoryConfirmed = expense.category_confirmed
    transactionDate = expense.transaction_date
    transactionDateConfirmed = expense.transaction_date_confirmed
    description = expense.description
    counterpartyName = expense.counterparty_name
    expenseCity = expense.location.city_name ?? ''
    provinceName = expense.location.province_name ?? ''
    countryCode = expense.location.country_code ?? 'CN'
    paymentMethod = expense.payment_method
    grossAmount = expense.gross_amount
    currencyCode = expense.currency_code
    expenseTaxAmount = expense.tax_details[0]?.amount ?? ''
    expenseTaxRate = expense.tax_details[0]?.rate ?? ''
  }

  async function saveInvoice(): Promise<boolean> {
    if (!selectedInvoice || !canEdit || working !== null || !invoiceFormDirty) return !invoiceFormDirty
    working = 'save-invoice'
    actionError = null
    const result = await invokeSafe<Invoice>('update_invoice_review', {
      invoiceId: selectedInvoice.id,
      input: {
        invoice_number: invoiceNumber,
        issue_date: issueDate,
        amount,
        tax_amount: optional(taxAmount),
        buyer_name: optional(buyerName),
        seller_name: optional(sellerName),
        ticket_type: ticketType,
        city: optional(city),
        departure_time: optional(departureTime),
        checkin_date: optional(checkinDate),
      },
    })
    working = null
    if (!result.ok) { actionError = describeError(result.error); return false }
    statusMessage = '票面字段已保存。'
    return true
  }

  async function saveExpense(): Promise<boolean> {
    if (!selectedExpense || !canEdit || working !== null || !expenseFormDirty) return !expenseFormDirty
    const validationError = validateExpenseForm()
    if (validationError) { actionError = validationError; return false }
    working = 'save-expense'
    actionError = null
    const result = await invokeSafe<ExpenseItem>('update_expense_item', {
      expenseItemId: selectedExpense.id,
      input: {
        category_code: expenseCategory,
        category_confirmed: expenseCategoryConfirmed,
        transaction_date: transactionDate,
        transaction_date_confirmed: transactionDateConfirmed,
        description,
        counterparty_name: counterpartyName,
        location: {
          city_name: optional(expenseCity),
          city_code: selectedExpense.location.city_code,
          province_name: optional(provinceName),
          province_code: selectedExpense.location.province_code,
          country_code: optional(countryCode),
        },
        payment_method: paymentMethod,
        gross_amount: grossAmount,
        currency_code: currencyCode,
        tax_details: expenseTaxAmount.trim()
          ? [{ amount: expenseTaxAmount, rate: optional(expenseTaxRate), source: 'manual_review' }]
          : [],
      },
    })
    working = null
    if (!result.ok) { actionError = describeError(result.error); return false }
    statusMessage = '费用字段已保存；这些字段独立于 Concur 配置。'
    return true
  }

  async function saveDirtyForms(): Promise<boolean> {
    if (!canEdit || !hasUnsavedChanges) return true
    const needsExpenseSave = expenseFormDirty
    const needsInvoiceSave = invoiceFormDirty
    if (needsExpenseSave && !(await saveExpense())) return false
    if (needsInvoiceSave && !(await saveInvoice())) return false
    if (needsExpenseSave || needsInvoiceSave) {
      statusMessage = '费用与票面字段已保存；本地费用字段保持独立于 Concur 配置。'
      await onChanged()
    }
    return true
  }

  async function navigateTo(invoiceId: number) {
    if (invoiceId === selectedInvoiceId || !(await saveDirtyForms())) return
    selectedInvoiceId = invoiceId
    previewDocumentId = null
    statusMessage = null
    actionError = null
  }

  async function selectRelative(direction: -1 | 1) {
    const nextInvoiceId = adjacentReviewInvoiceId(queueInvoiceIds, selectedInvoiceId, direction)
    if (nextInvoiceId !== null) await navigateTo(nextInvoiceId)
  }

  async function saveAndNextExpense() {
    if (!(await saveDirtyForms())) return
    const nextInvoiceId = adjacentReviewInvoiceId(queueInvoiceIds, selectedInvoiceId, 1)
    if (nextInvoiceId !== null) await navigateTo(nextInvoiceId)
    else onBack(`已到“${reviewQueue.label}”筛选结果末尾。`)
  }

  async function confirmIssue(action: NonNullable<ReviewIssue['action']>) {
    if (!selectedExpense || !canEdit || working !== null) return
    if (action === 'confirm-date') transactionDateConfirmed = true
    else expenseCategoryConfirmed = true
    if (!(await saveExpense())) return
    statusMessage = action === 'confirm-date'
      ? `已确认实际发生日期 ${transactionDate}。`
      : `已确认费用类型“${EXPENSE_CATEGORY_LABELS[expenseCategory]}”。`
    await onChanged()
  }

  async function returnToList() {
    if (!(await saveDirtyForms())) return
    onBack()
  }

  async function attachDocument() {
    if (!selectedExpense || !canEdit || working !== null) return
    const selected = await open({
      multiple: false,
      directory: false,
      filters: [{ name: '费用材料', extensions: ['pdf', 'ofd', 'xml', 'png', 'jpg', 'jpeg', 'webp', 'bmp'] }],
    })
    if (typeof selected !== 'string') return
    working = 'attach'
    actionError = null
    const result = await invokeSafe('attach_expense_document', {
      expenseItemId: selectedExpense.id,
      role: attachRole,
      sourcePath: selected,
    })
    working = null
    if (!result.ok) { actionError = describeError(result.error); return }
    statusMessage = '材料已复制到软件数据目录并挂载到该费用。'
    await onChanged()
  }

  async function removeDocument(documentId: number) {
    confirmation = null
    if (!canEdit || working !== null) return
    working = 'remove-document'
    actionError = null
    const result = await invokeSafe<void>('remove_expense_document', { documentId })
    working = null
    if (!result.ok) { actionError = describeError(result.error); return }
    if (previewDocumentId === documentId) previewDocumentId = null
    statusMessage = '材料挂载已移除，原文件仍保留。'
    await onChanged()
  }

  async function linkDuplicateCopy() {
    if (!selectedInvoice?.is_duplicate || !duplicateTargetExpenseId || working !== null) return
    working = 'link-duplicate'
    actionError = null
    const result = await invokeSafe('link_duplicate_invoice_to_expense', {
      sourceInvoiceId: selectedInvoice.id,
      targetExpenseItemId: Number(duplicateTargetExpenseId),
    })
    working = null
    if (!result.ok) { actionError = describeError(result.error); return }
    statusMessage = '重复原件已关联到保留费用，仍不会重复计入金额。'
    await onChanged()
  }

  async function confirmDuplicate() {
    confirmation = null
    if (!selectedInvoice?.is_duplicate || selectedInvoice.is_excluded || working !== null) return
    working = 'confirm-duplicate'
    const result = await invokeSafe<void>('confirm_duplicate_flag', { invoiceId: selectedInvoice.id })
    working = null
    if (!result.ok) { actionError = describeError(result.error); return }
    statusMessage = '已确认重复，该笔不计入总额。'
    await onChanged()
  }

  async function markDistinct() {
    confirmation = null
    if (!selectedInvoice?.is_duplicate || working !== null) return
    working = 'mark-distinct'
    const result = await invokeSafe<void>('clear_duplicate_flag', { invoiceId: selectedInvoice.id })
    working = null
    if (!result.ok) { actionError = describeError(result.error); return }
    statusMessage = '已确认为非重复，实际金额恢复计入总额。'
    await onChanged()
  }

  async function toggleExcluded() {
    const nextExcluded = !selectedInvoice?.is_excluded
    confirmation = null
    if (!selectedInvoice || !canEdit || working !== null) return
    working = 'exclude'
    const result = await invokeSafe<void>('set_invoice_excluded', {
      invoiceId: selectedInvoice.id,
      excluded: nextExcluded,
    })
    working = null
    if (!result.ok) { actionError = describeError(result.error); return }
    statusMessage = nextExcluded ? '已排除，不再计入批次总额。' : '已恢复计入。'
    await onChanged()
  }

  function openViewer() {
    viewerCollapsed = false
    viewerOpen = true
  }

  function collapseViewer() {
    viewerFullscreen = false
    viewerCollapsed = true
    viewerOpen = false
  }

  function toggleViewerFullscreen() {
    viewerCollapsed = false
    viewerOpen = true
    viewerFullscreen = !viewerFullscreen
  }

  function handleKeydown(event: KeyboardEvent) {
    if (event.key === 'Escape' && viewerFullscreen) {
      event.preventDefault()
      viewerFullscreen = false
    } else if (event.ctrlKey && event.key.toLowerCase() === 's') {
      event.preventDefault()
      void saveDirtyForms()
    } else if (event.ctrlKey && event.key === 'Enter') {
      event.preventDefault()
      void saveAndNextExpense()
    } else if (event.altKey && event.key === 'ArrowLeft') {
      event.preventDefault()
      void selectRelative(-1)
    } else if (event.altKey && event.key === 'ArrowRight') {
      event.preventDefault()
      void selectRelative(1)
    }
  }

  $effect(() => {
    const invoice = selectedInvoice
    const expense = selectedExpense
    const signature = invoice && expense
      ? `${invoice.id}:${invoice.created_at}:${invoice.is_duplicate}:${invoice.is_excluded}:${expense.updated_at}`
      : ''
    if (invoice && expense && signature && signature !== loadedSignature) {
      populateForm(invoice)
      populateExpenseForm(expense)
      loadedSignature = signature
    }
    if (expense && expense.id !== previewExpenseId) {
      previewDocumentId = expense.documents.some((document) => document.role === 'main_invoice')
        ? null
        : expense.documents[0]?.id ?? null
      previewExpenseId = expense.id
    }
  })

  $effect(() => {
    if (!viewerFullscreen) return
    const previousOverflow = document.body.style.overflow
    document.body.style.overflow = 'hidden'
    return () => { document.body.style.overflow = previousOverflow }
  })
</script>

<svelte:window onkeydown={handleKeydown} />

<div class="expense-page">
  <header class="expense-header">
    <button class="back" type="button" onclick={() => void returnToList()}>‹ 返回费用清单</button>
    {#if selectedInvoice && selectedExpense}
      <div class="header-row">
        <div class="expense-identity">
          <span class="eyebrow">单条费用核对</span>
          <h1>{expenseCategoryLabel(selectedExpense)} <strong>{formatAmount(selectedExpense.gross_amount, selectedExpense.currency_code)}</strong></h1>
          <p>{selectedExpense.counterparty_name || selectedInvoice.seller_name || '交易方待补充'} · {selectedExpense.transaction_date || selectedInvoice.issue_date}</p>
        </div>
        <div class="record-navigation" aria-label="费用记录导航">
          <span>{reviewQueue.label} · {selectedIndex + 1} / {orderedInvoices.length}</span>
          <button type="button" aria-label="上一条费用" onclick={() => void selectRelative(-1)} disabled={selectedIndex <= 0}>‹</button>
          <button type="button" aria-label="下一条费用" onclick={() => void selectRelative(1)} disabled={selectedIndex >= orderedInvoices.length - 1}>›</button>
          <button class="open-original" type="button" onclick={openViewer}>查看原件</button>
          <button class="next-issue" type="button" onclick={() => void saveAndNextExpense()} disabled={working !== null}>保存并查看下一笔</button>
        </div>
      </div>
    {/if}
  </header>

  {#if selectedInvoice && selectedExpense}
    <div class:viewer-collapsed={viewerCollapsed} class="expense-layout">
      <main class="form-pane">
        <section class="issue-strip" aria-label="当前核对事项">
          {#each reviewIssues as issue}
            <article class:danger={issue.tone === 'danger'} class:warning={issue.tone === 'warning'} class:muted={issue.tone === 'muted'} class:ok={issue.tone === 'ok'}>
              <strong>{issue.label}</strong><div><span>{issue.detail}</span>{#if issue.action}<button type="button" onclick={() => void confirmIssue(issue.action!)} disabled={!canEdit || working !== null}>{issue.actionLabel}</button>{/if}</div>
            </article>
          {/each}
        </section>

        {#if actionError}<p class="action-message error" role="alert">{actionError}</p>{/if}
        {#if statusMessage}<p class="action-message" role="status">{statusMessage}</p>{/if}

        <form class="expense-form" onsubmit={(event) => { event.preventDefault(); void saveDirtyForms() }}>
          <section class="form-section primary-fields">
            <header><div><span>费用信息</span><h2>用于本软件整理与报销计算</h2></div><small>与 Concur 字段解耦</small></header>
            <div class="field-grid">
              <label><span>费用类型 *</span><select bind:value={expenseCategory} disabled={!canEdit}>{#each EXPENSE_CATEGORIES as type}<option value={type}>{EXPENSE_CATEGORY_LABELS[type]}</option>{/each}</select><small>来源：{selectedExpense ? expenseCategorySourceLabel(selectedExpense.category_source) : '系统建议'} · {expenseCategoryConfirmed ? '已确认' : '尚未确认'}</small></label>
              <label><span>实际发生日期 *</span><input type="date" bind:value={transactionDate} disabled={!canEdit} /><small>来源：{selectedExpense ? transactionDateSourceLabel(selectedExpense.transaction_date_source) : '系统候选'}</small></label>
              <label class="wide"><span>业务说明</span><input bind:value={description} maxlength="500" disabled={!canEdit} placeholder="例如：客户拜访期间餐费" /></label>
              <label class="wide"><span>交易方 *</span><input bind:value={counterpartyName} maxlength="200" disabled={!canEdit} /></label>
              <label><span>城市</span><input bind:value={expenseCity} disabled={!canEdit} /></label>
              <label><span>省/州</span><input bind:value={provinceName} disabled={!canEdit} /></label>
              <label><span>付款方式</span><select bind:value={paymentMethod} disabled={!canEdit}><option value="unknown">待确认</option><option value="personal_card">个人卡</option><option value="corporate_card">公司卡</option><option value="cash">现金</option><option value="other">其他</option></select></label>
              <label><span>实际报销金额 *</span><input inputmode="decimal" bind:value={grossAmount} disabled={!canEdit} /></label>
              <label><span>币种</span><input bind:value={currencyCode} maxlength="3" disabled={!canEdit} /></label>
              <label><span>国家/地区代码</span><input bind:value={countryCode} disabled={!canEdit} /></label>
              <label class="confirmation wide"><input type="checkbox" bind:checked={expenseCategoryConfirmed} disabled={!canEdit} /><span><strong>我已核对费用类型</strong><small>未确认会阻止完成审核；“其他”也需要明确确认</small></span></label>
              <label class="confirmation wide"><input type="checkbox" bind:checked={transactionDateConfirmed} disabled={!canEdit} /><span><strong>我已核对实际发生日期</strong><small>未确认会阻止完成审核</small></span></label>
            </div>
          </section>

          <details class="form-section disclosure" open={selectedInvoice.is_duplicate}>
            <summary><span><strong>重复与计入状态</strong><small>{selectedInvoice.is_duplicate ? '需要人工判断' : selectedInvoice.is_excluded ? '已排除' : '正常计入'}</small></span></summary>
            <div class="disclosure-body">
              <div class="inclusion-summary">
                <span>当前处理</span>
                <strong>{selectedInvoice.is_duplicate ? (selectedInvoice.is_excluded ? '重复 · 未计入' : '疑似重复 · 未计入') : (selectedInvoice.is_excluded ? '已排除 · 未计入' : '已计入')}</strong>
                <small>重复发票只有在明确标记“不是重复”后才恢复计入。</small>
              </div>
              {#if selectedInvoice.is_duplicate}
                <label><span>关联到保留费用</span><select bind:value={duplicateTargetExpenseId} disabled={!canEdit}><option value="">选择一条保留费用</option>{#each duplicateTargets as target}<option value={String(target.id)}>#{target.id} · {target.counterparty_name || '交易方待补充'} · {formatAmount(target.gross_amount)}</option>{/each}</select></label>
                {#if duplicateComparisonExpense && duplicateComparisonInvoice}
                  <div class="comparison"><div><span>当前疑似重复</span><strong>{selectedInvoice.invoice_number || '无票号'}</strong><small>{selectedInvoice.issue_date} · {formatAmount(selectedInvoice.amount)}</small></div><div><span>保留费用</span><strong>{duplicateComparisonInvoice.invoice_number || '无票号'}</strong><small>{duplicateComparisonInvoice.issue_date} · {formatAmount(duplicateComparisonExpense.gross_amount)}</small></div></div>
                {/if}
                <div class="button-row"><button type="button" onclick={() => void linkDuplicateCopy()} disabled={!duplicateTargetExpenseId || !canEdit || working !== null}>关联原件</button><button type="button" onclick={() => (confirmation = { kind: 'confirm-duplicate' })} disabled={selectedInvoice.is_excluded || !canEdit}>确认重复</button><button class="danger-outline" type="button" onclick={() => (confirmation = { kind: 'mark-distinct' })} disabled={!canEdit}>不是重复</button></div>
              {:else}
                <button class="secondary" type="button" onclick={() => (confirmation = { kind: 'toggle-excluded', excluding: !selectedInvoice.is_excluded })} disabled={!canEdit}>{selectedInvoice.is_excluded ? '恢复计入' : '从本批次排除'}</button>
              {/if}
            </div>
          </details>

          <details class="form-section disclosure">
            <summary><span><strong>{usesPaperInvoice ? '行程单提取字段' : '票面字段'}</strong><small>{usesPaperInvoice ? '纸质发票未录入；按电子行程单核对' : '解析结果与原件核对'}</small></span><b>{formatAmount(amount || '0')}</b></summary>
            <div class="disclosure-body field-grid">
              <label><span>{usesPaperInvoice ? '发票号码（纸质票）' : '发票号码'}</span><input bind:value={invoiceNumber} disabled={!canEdit} placeholder={usesPaperInvoice ? '未录入' : ''} /></label>
              <label><span>开票日期</span><input type="date" bind:value={issueDate} disabled={!canEdit} /></label>
              <label><span>票面金额</span><input inputmode="decimal" bind:value={amount} disabled={!canEdit} /></label>
              <label><span>票面税额</span><input inputmode="decimal" bind:value={taxAmount} disabled={!canEdit} /></label>
              <label class="wide"><span>销售方</span><input bind:value={sellerName} disabled={!canEdit} /></label>
              <label class="wide"><span>购买方</span><input bind:value={buyerName} disabled={!canEdit} /></label>
              <label><span>票据类别</span><select bind:value={ticketType} disabled={!canEdit}>{#each TICKET_TYPES as type}<option value={type}>{TICKET_TYPE_LABELS[type]}</option>{/each}</select></label>
              <label><span>票面城市</span><input bind:value={city} disabled={!canEdit} /></label>
              <label><span>出发时间</span><input type="datetime-local" bind:value={departureTime} disabled={!canEdit} /></label>
              <label><span>入住日期</span><input type="date" bind:value={checkinDate} disabled={!canEdit} /></label>
              <label><span>费用税额</span><input inputmode="decimal" bind:value={expenseTaxAmount} disabled={!canEdit} /></label>
              <label><span>费用税率</span><input bind:value={expenseTaxRate} disabled={!canEdit} placeholder="例如 0.06" /></label>
            </div>
          </details>

          <details class="form-section disclosure" open>
            <summary><span><strong>原件与配套材料</strong><small>{Math.max(1, selectedExpense.documents.length)} 份文件</small></span></summary>
            <div class="disclosure-body documents">
              {#if hasMainInvoiceDocument}
                <button class:active={previewDocumentId === null} type="button" onclick={() => { previewDocumentId = null; openViewer() }}><span>主发票</span><strong>{selectedInvoice.file_path.split(/[\\/]/).pop() ?? '原始发票'}</strong><small>作为该费用的主凭证</small></button>
              {:else if usesPaperInvoice}
                <p class="paper-invoice-note"><strong>纸质发票未录入</strong><span>本费用由滴滴电子行程单创建；实际金额与日期来自行程单，纸质票由用户后续按公司要求处理。</span></p>
              {/if}
              {#each supportingDocuments as document}
                <div class="document-row"><button class:active={previewDocumentId === document.id} type="button" onclick={() => { previewDocumentId = document.id; openViewer() }}><span>{documentRoleLabel(document.role)}</span><strong>{document.original_name}</strong>{#if usesPaperInvoice && document.role === 'itinerary'}<small>创建本费用的电子凭证</small>{/if}</button>{#if canEdit && !(usesPaperInvoice && document.role === 'itinerary')}<button class="remove" type="button" aria-label={`移除 ${document.original_name}`} onclick={() => (confirmation = { kind: 'remove-document', documentId: document.id })}>移除</button>{/if}</div>
              {/each}
              {#if canEdit}<div class="attach-row"><select bind:value={attachRole}><option value="itinerary">行程单</option><option value="detail">消费明细</option><option value="supporting">其他材料</option><option value="duplicate_copy">重复副本</option></select><button type="button" onclick={() => void attachDocument()} disabled={working !== null}>添加本地文件</button></div>{/if}
            </div>
          </details>

          <section class="group-reference">
            <span>当前归组</span><strong>{currentGroup ? displayGroupTitle(currentGroup, expenseItems) : '尚未归组'}</strong><small>{currentGroup ? `${currentGroup.start_date} 至 ${currentGroup.end_date} · ${currentGroup.members.length} 笔费用` : '请在归组视图建立或调整归组。'}</small>
          </section>
        </form>
      </main>

      <aside class:open={viewerOpen} class:collapsed={viewerCollapsed} class:fullscreen={viewerFullscreen} class="viewer-pane" aria-label="原始凭证查看器">
        <header><div><span>原始凭证</span><strong>{previewDocument?.original_name ?? selectedInvoice.file_path.split(/[\\/]/).pop() ?? '主发票'}</strong></div><nav class="viewer-actions" aria-label="原件查看器布局"><button type="button" onclick={toggleViewerFullscreen}>{viewerFullscreen ? '退出全屏' : '全屏查看'}</button><button type="button" onclick={collapseViewer}>折叠原件</button></nav></header>
        <OriginalPreview invoice={selectedInvoice} document={previewDocument} />
      </aside>
    </div>

    <footer class="save-bar">
      <div><span class:dirty={hasUnsavedChanges}>{working ? '正在保存…' : hasUnsavedChanges ? '有未保存更改' : '所有更改已保存'}</span><small>Ctrl+S 保存 · Ctrl+Enter 保存并查看下一笔 · Alt+←/→ 在当前筛选结果中切换</small></div>
      <button class:visible={viewerCollapsed} class="viewer-toggle" type="button" onclick={openViewer}>查看原件</button>
      <button class="secondary" type="button" onclick={() => void returnToList()}>返回清单</button>
      <button class="primary" type="button" onclick={() => void saveAndNextExpense()} disabled={!canEdit || working !== null}>{working ? '保存中…' : '保存并查看下一笔'}</button>
    </footer>
  {:else}
    <main class="missing"><h1>费用记录不可用</h1><p>该费用可能已被移除，请返回清单刷新。</p><button type="button" onclick={() => onBack()}>返回费用清单</button></main>
  {/if}
</div>

{#if confirmation?.kind === 'remove-document'}
  <ConfirmDialog title="移除材料挂载" message="只解除该材料与费用的关系，软件数据目录中的原文件仍会保留。" confirmLabel="移除挂载" tone="danger" busy={working !== null} onConfirm={() => void removeDocument(confirmation?.kind === 'remove-document' ? confirmation.documentId : 0)} onCancel={() => (confirmation = null)} />
{:else if confirmation?.kind === 'confirm-duplicate'}
  <ConfirmDialog title="确认这是重复发票" message="该费用和金额将保持不计入批次总额，原始文件仍会保留。" confirmLabel="确认重复" tone="danger" busy={working !== null} onConfirm={() => void confirmDuplicate()} onCancel={() => (confirmation = null)} />
{:else if confirmation?.kind === 'mark-distinct'}
  <ConfirmDialog title="确认不是重复发票" message="该费用将恢复计入批次总额。请先核对票号、日期、金额和销售方。" confirmLabel="恢复计入" busy={working !== null} onConfirm={() => void markDistinct()} onCancel={() => (confirmation = null)} />
{:else if confirmation?.kind === 'toggle-excluded'}
  <ConfirmDialog title={confirmation.excluding ? '从本批次排除' : '恢复计入本批次'} message={confirmation.excluding ? '该费用将不计入总额，但原件和审核记录仍会保留。' : '该费用将重新计入批次总额。'} confirmLabel={confirmation.excluding ? '排除费用' : '恢复计入'} tone={confirmation.excluding ? 'danger' : 'primary'} busy={working !== null} onConfirm={() => void toggleExcluded()} onCancel={() => (confirmation = null)} />
{/if}

<style>
  .expense-page{min-height:100vh;padding-bottom:78px;background:#f4f5f6;color:#17232d}.expense-header{position:sticky;top:0;z-index:45;padding:1rem 1.4rem .95rem;border-bottom:1px solid #ccd3d7;background:rgba(255,255,255,.97);backdrop-filter:blur(12px)}.back{padding:0;border:0;background:transparent;color:#136b52;font-weight:700;cursor:pointer}.header-row{display:flex;align-items:flex-end;justify-content:space-between;gap:1.5rem;margin-top:.8rem}.eyebrow{color:#69777f;font-family:'IBM Plex Mono',monospace;font-size:.7rem;font-weight:700;letter-spacing:.08em;text-transform:uppercase}.expense-identity h1{margin:.2rem 0 0;font-size:1.55rem;letter-spacing:-.025em}.expense-identity h1 strong{margin-left:.45rem;color:#136b52}.expense-identity p{margin:.25rem 0 0;color:#627078}.record-navigation{display:flex;align-items:center;gap:.45rem}.record-navigation>span{margin-right:.35rem;color:#58666e;font-family:'IBM Plex Mono',monospace;font-size:.82rem}.record-navigation button{min-height:36px;padding:.45rem .72rem;border:1px solid #b9c3c8;background:#fff;color:#17232d;font-weight:700;cursor:pointer}.record-navigation button:disabled{opacity:.4;cursor:not-allowed}.record-navigation .next-issue{border-color:#136b52;background:#136b52;color:#fff}
  .expense-layout{display:grid;grid-template-columns:minmax(520px,56%) minmax(390px,44%);min-height:calc(100vh - 184px)}.expense-layout.viewer-collapsed{grid-template-columns:minmax(0,1fr)}.expense-layout.viewer-collapsed .form-pane{border-right:0}.form-pane{min-width:0;padding:1.15rem 1.35rem 2rem;border-right:1px solid #ccd3d7}.viewer-pane{position:sticky;top:124px;height:calc(100vh - 202px);min-width:0;overflow:hidden;background:#e7eaec}.viewer-pane.collapsed{display:none}.viewer-pane.fullscreen{position:fixed;inset:0;z-index:140;display:block;height:100vh;background:#e7eaec}.viewer-pane>header{display:flex;align-items:center;justify-content:space-between;gap:1rem;height:54px;padding:.6rem .9rem;border-bottom:1px solid #c5cdd1;background:#fff}.viewer-pane>header div{display:grid;min-width:0}.viewer-pane>header span{color:#69777f;font-size:.7rem}.viewer-pane>header strong{overflow:hidden;text-overflow:ellipsis;white-space:nowrap}.viewer-actions{display:flex;flex:none;gap:.4rem}.viewer-actions button{min-height:34px;padding:.38rem .58rem;border:1px solid #aeb9bf;background:#fff;color:#315043;font-size:.76rem;font-weight:700;cursor:pointer}.viewer-pane :global(.preview-shell){height:calc(100% - 54px);min-height:0;border:0}
  .issue-strip{display:grid;gap:.5rem;margin-bottom:1rem}.issue-strip article{display:grid;grid-template-columns:150px 1fr;gap:.75rem;padding:.7rem .75rem;border-left:4px solid #7a8991;background:#fff}.issue-strip article.danger{border-color:#b3453e;background:#fff1f0}.issue-strip article.warning{border-color:#c47a16;background:#fff7e7}.issue-strip article.ok{border-color:#136b52;background:#edf6f1}.issue-strip article>div{display:grid;justify-items:start;gap:.5rem}.issue-strip article span{color:#536169;font-size:.84rem;line-height:1.45}.issue-strip article button{min-height:36px;padding:.42rem .65rem;border:1px solid #136b52;background:#fff;color:#136b52;font-weight:700;cursor:pointer}.issue-strip article button:hover{background:#edf6f1}.issue-strip article button:disabled{opacity:.45;cursor:not-allowed}.action-message{margin:.6rem 0;padding:.65rem .75rem;border-left:4px solid #136b52;background:#edf6f1;color:#24533f}.action-message.error{border-color:#b3453e;background:#fff1f0;color:#862f2a}
  .expense-form{display:grid;gap:.9rem}.form-section,.group-reference{border:1px solid #cbd2d6;background:#fff}.form-section>header{display:flex;align-items:flex-start;justify-content:space-between;gap:1rem;padding:.9rem 1rem;border-bottom:1px solid #e0e5e7}.form-section>header span,.group-reference>span{color:#69777f;font-size:.72rem;font-weight:700;letter-spacing:.05em;text-transform:uppercase}.form-section h2{margin:.15rem 0 0;font-size:1rem}.form-section>header small{color:#136b52;font-weight:700}.field-grid{display:grid;grid-template-columns:repeat(2,minmax(0,1fr));gap:.8rem;padding:1rem}.field-grid label,.disclosure-body>label{display:grid;gap:.35rem}.field-grid label>span,.disclosure-body>label>span{color:#526068;font-size:.78rem;font-weight:700}.field-grid .wide{grid-column:1/-1}input,select{min-width:0;height:40px;padding:.5rem .62rem;border:1px solid #aeb9bf;border-radius:2px;background:#fff;color:#17232d;font:inherit}input:focus,select:focus{outline:3px solid rgba(19,107,82,.14);border-color:#136b52}input:disabled,select:disabled{background:#f0f2f3;color:#536169}.confirmation{display:flex!important;grid-template-columns:auto 1fr;align-items:center;padding:.7rem;border-left:3px solid #c47a16;background:#fff7e7}.confirmation input{width:18px;height:18px}.confirmation span{display:grid}.confirmation small{font-weight:400}
  .disclosure>summary{display:flex;align-items:center;justify-content:space-between;gap:1rem;padding:.85rem 1rem;cursor:pointer;list-style:none}.disclosure>summary::-webkit-details-marker{display:none}.disclosure>summary span{display:grid;gap:.18rem}.disclosure>summary small{color:#69777f;font-weight:400}.disclosure>summary b{color:#136b52}.disclosure[open]>summary{border-bottom:1px solid #e0e5e7}.disclosure-body{padding:1rem}.disclosure-body.field-grid{padding:1rem}.inclusion-summary{display:grid;gap:.2rem;margin-bottom:.8rem;padding:.7rem;border-left:3px solid #c47a16;background:#fff7e7}.inclusion-summary span,.inclusion-summary small{color:#6b5832;font-size:.78rem}.comparison{display:grid;grid-template-columns:1fr 1fr;gap:1px;margin:.8rem 0;background:#d8dfe2}.comparison>div{display:grid;gap:.2rem;padding:.75rem;background:#f7f8f8}.comparison span,.comparison small{color:#65737a;font-size:.75rem}.button-row,.attach-row{display:flex;flex-wrap:wrap;gap:.55rem;margin-top:.8rem}.button-row button,.attach-row button,.secondary,.primary,.viewer-toggle,.missing button{padding:.58rem .8rem;border:1px solid #136b52;background:#fff;color:#136b52;font-weight:700;cursor:pointer}.button-row button:disabled{opacity:.45}.button-row .danger-outline{border-color:#b3453e;color:#b3453e}.documents{display:grid;gap:.45rem}.paper-invoice-note{display:grid;gap:.2rem;margin:0;padding:.65rem .75rem;border-left:4px solid #315f8a;background:#edf3f8}.paper-invoice-note strong{color:#274c6d}.paper-invoice-note span{color:#4d6275;font-size:.75rem;line-height:1.45}.documents>button,.document-row>button:first-child{display:grid;grid-template-columns:100px 1fr;gap:.5rem;padding:.65rem .75rem;border:1px solid #d1d8dc;background:#f8f9f9;color:#17232d;text-align:left;cursor:pointer}.documents button.active{border-color:#136b52;background:#edf6f1}.documents button span{color:#65737a;font-size:.75rem}.documents button small{grid-column:2;color:#65737a}.document-row{display:grid;grid-template-columns:1fr auto;gap:.4rem}.document-row .remove{border:0;background:transparent;color:#b3453e;cursor:pointer}.attach-row select{flex:1}.group-reference{display:grid;grid-template-columns:110px 1fr;gap:.25rem 1rem;padding:.8rem 1rem}.group-reference small{grid-column:2;color:#637078}
  .save-bar{position:fixed;right:0;bottom:0;left:var(--app-sidebar-width,224px);z-index:70;display:flex;align-items:center;justify-content:flex-end;gap:.65rem;min-height:66px;padding:.65rem 1.4rem;border-top:1px solid #aeb9bf;background:rgba(255,255,255,.97);box-shadow:0 -8px 24px rgba(30,42,48,.08)}.save-bar>div{display:grid;margin-right:auto}.save-bar span{color:#136b52;font-weight:700}.save-bar span.dirty{color:#9b620e}.save-bar small{color:#6b777d}.save-bar .primary{background:#136b52;color:#fff}.viewer-toggle{display:none}.viewer-toggle.visible{display:inline-block}.missing{padding:4rem 2rem}.missing button{margin-top:1rem}
  @media(max-width:1120px){.expense-layout{display:block}.form-pane{border-right:0}.viewer-pane{position:fixed;inset:0 0 66px var(--app-sidebar-width,224px);z-index:80;display:none;height:auto}.viewer-pane.open{display:block}.viewer-pane.fullscreen{inset:0;z-index:140;height:100vh}.viewer-toggle{display:inline-block}.viewer-pane :global(.preview-shell){height:calc(100% - 54px)}}
  @media(max-width:820px){.save-bar{left:0}.viewer-pane{left:0}.header-row{display:grid}.record-navigation{flex-wrap:wrap}.expense-header{padding-inline:1rem}.form-pane{padding-inline:1rem}.save-bar small,.save-bar .secondary{display:none}}
  @media(max-width:650px){.field-grid,.comparison{grid-template-columns:1fr}.field-grid .wide{grid-column:auto}.issue-strip article{grid-template-columns:1fr}.record-navigation .next-issue{display:none}.expense-identity h1{font-size:1.25rem}.documents>button,.document-row>button:first-child{grid-template-columns:1fr}.documents button small{grid-column:auto}.group-reference{grid-template-columns:1fr}.group-reference small{grid-column:auto}}
</style>
