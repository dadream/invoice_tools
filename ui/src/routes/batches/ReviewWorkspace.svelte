<script module lang="ts">
  interface StoredListState {
    search: string
    listMode: 'included' | 'excluded' | 'pending'
    status: 'all' | 'problems' | 'duplicates' | 'unconfirmed' | 'unclassified'
    category: string
    group: string
    sortKey: 'date' | 'amount' | 'category' | 'status'
    sortDirection: 'asc' | 'desc'
  }
  const listStateByBatch = new Map<number, StoredListState>()
</script>

<script lang="ts">
  import { untrack } from 'svelte'
  import { describeError, invokeSafe } from '../../lib/ipc'
  import { blockingExpenseReviewIssues, type ExpenseReviewIssueCode } from '../../lib/expenseReview'
  import { displayGroupTitle } from '../../lib/grouping'
  import { countUniqueOfdFiles } from '../../lib/invoice'
  import { canConvertDidiItinerary, pendingDocumentReasonLabel } from '../../lib/pendingDocuments'
  import type { ReviewQueueContext } from '../../lib/reviewQueue'
  import type { BatchGrouping, ExpenseCategory, ExpenseItem, Invoice, PendingInvoiceDocument } from '../../lib/types'
  import {
    EXPENSE_CATEGORIES, EXPENSE_CATEGORY_LABELS, TICKET_TYPE_LABELS, expenseCategoryLabel,
    formatAmount, transactionDateSourceLabel,
  } from '../../lib/types'
  import ConfirmDialog from '../../lib/ConfirmDialog.svelte'
  import OriginalPreview from './OriginalPreview.svelte'

  interface Props {
    batchId: number
    invoices: Invoice[]
    expenseItems: ExpenseItem[]
    pendingDocuments: PendingInvoiceDocument[]
    grouping: BatchGrouping | null
    canEdit: boolean
    onChanged: () => Promise<void>
    onOpenInvoice: (invoiceId: number, queue: ReviewQueueContext) => void
  }

  let { batchId, invoices, expenseItems, pendingDocuments, grouping, canEdit, onChanged, onOpenInvoice }: Props = $props()
  const initial = untrack(() => listStateByBatch.get(batchId))
  let searchQuery = $state(initial?.search ?? '')
  let listMode = $state<StoredListState['listMode']>(initial?.listMode ?? 'included')
  let selectedStatus = $state<StoredListState['status']>(initial?.status ?? 'all')
  let selectedCategory = $state<'all' | 'unclassified' | ExpenseCategory>((initial?.category as 'all' | 'unclassified' | ExpenseCategory | undefined) ?? 'all')
  let selectedGroup = $state(initial?.group ?? 'all')
  let sortKey = $state<StoredListState['sortKey']>(initial?.sortKey ?? 'date')
  let sortDirection = $state<StoredListState['sortDirection']>(initial?.sortDirection ?? 'asc')
  let selectedPendingDocumentId = $state<number | null>(null)
  let pendingTargetExpenseId = $state('')
  let pendingRole = $state<'itinerary' | 'detail' | 'supporting'>('supporting')
  let working = $state(false)
  let actionError = $state<string | null>(null)
  let statusMessage = $state<string | null>(null)
  let confirmingIgnore = $state(false)
  let restoringInvoiceId = $state<number | null>(null)

  const groups = $derived(grouping?.groups ?? [])
  const unresolvedPendingDocuments = $derived(pendingDocuments.filter((document) => document.status === 'pending'))
  const selectedPendingDocument = $derived(unresolvedPendingDocuments.find((document) => document.id === selectedPendingDocumentId) ?? null)
  const includedInvoices = $derived(invoices.filter((invoice) => isIncluded(invoice)))
  const excludedInvoices = $derived(invoices.filter((invoice) => !isIncluded(invoice)))
  const activeInvoices = $derived(listMode === 'included' ? includedInvoices : listMode === 'excluded' ? excludedInvoices : [])
  const visibleInvoices = $derived.by(() => {
    let result = activeInvoices
    if (selectedGroup !== 'all') {
      const group = groups.find((candidate) => String(candidate.id) === selectedGroup)
      const memberIds = new Set(group?.members.map((member) => member.invoice_id) ?? [])
      result = result.filter((invoice) => memberIds.has(invoice.id))
    }
    if (selectedCategory === 'unclassified') {
      result = result.filter((invoice) => !expenseForInvoice(invoice.id)?.category_confirmed)
    } else if (selectedCategory !== 'all') {
      result = result.filter((invoice) => expenseForInvoice(invoice.id)?.category_code === selectedCategory)
    }
    if (selectedStatus === 'problems') result = result.filter((invoice) => problemScore(invoice) > 0)
    if (selectedStatus === 'duplicates') result = result.filter((invoice) => invoice.is_duplicate)
    if (selectedStatus === 'unconfirmed') result = result.filter((invoice) => !expenseForInvoice(invoice.id)?.transaction_date_confirmed)
    if (selectedStatus === 'unclassified') result = result.filter((invoice) => !expenseForInvoice(invoice.id)?.category_confirmed)
    const query = searchQuery.trim().toLocaleLowerCase()
    if (query) {
      result = result.filter((invoice) => {
        const expense = expenseForInvoice(invoice.id)
        return [invoice.invoice_number, invoice.seller_name, expense?.counterparty_name, expense?.description, expense?.location.city_name]
          .some((value) => value?.toLocaleLowerCase().includes(query))
      })
    }
    return [...result].sort(compareInvoices)
  })
  const problemCount = $derived(activeInvoices.filter((invoice) => problemScore(invoice) > 0).length)
  const duplicateCount = $derived(activeInvoices.filter((invoice) => invoice.is_duplicate).length)
  const unconfirmedCount = $derived(activeInvoices.filter((invoice) => !expenseForInvoice(invoice.id)?.transaction_date_confirmed).length)
  const unclassifiedCount = $derived(activeInvoices.filter((invoice) => !expenseForInvoice(invoice.id)?.category_confirmed).length)
  const ofdOriginalCount = $derived(countUniqueOfdFiles([
    ...invoices.map((invoice) => invoice.file_path),
    ...expenseItems.flatMap((expense) => expense.documents.map((document) => document.file_path)),
  ]))

  function expenseForInvoice(invoiceId: number): ExpenseItem | null {
    return expenseItems.find((expense) => expense.primary_invoice_id === invoiceId) ?? null
  }
  function isIncluded(invoice: Invoice): boolean {
    const expense = expenseForInvoice(invoice.id)
    return expense ? expense.inclusion_status === 'included' : !invoice.is_excluded && !invoice.is_duplicate
  }
  function selectListMode(next: StoredListState['listMode']) {
    listMode = next
    selectedStatus = 'all'
    selectedGroup = 'all'
  }
  function groupForInvoice(invoiceId: number) {
    return groups.find((group) => group.members.some((member) => member.invoice_id === invoiceId)) ?? null
  }
  function reviewIssues(invoice: Invoice): ExpenseReviewIssueCode[] {
    return blockingExpenseReviewIssues(invoice, expenseForInvoice(invoice.id))
  }
  function problemScore(invoice: Invoice): number { return reviewIssues(invoice).length }
  function issueLabels(invoice: Invoice, expense: ExpenseItem | null): { label: string; tone: 'warning' | 'muted' | 'ok' }[] {
    if (invoice.is_duplicate && invoice.is_excluded) return [{ label: '重复 · 未计入', tone: 'muted' }]
    if (invoice.is_excluded || expense?.inclusion_status === 'excluded') return [{ label: '已排除', tone: 'muted' }]
    const issues = blockingExpenseReviewIssues(invoice, expense)
    if (issues.length > 1) return [{ label: `待处理 ${issues.length} 项`, tone: 'warning' }]
    if (issues[0] === 'duplicate') return [{ label: '疑似重复', tone: 'warning' }]
    if (issues[0] === 'category') return [{ label: '待分类', tone: 'warning' }]
    if (issues[0] === 'date') return [{ label: '日期待确认', tone: 'warning' }]
    if (!expense?.counterparty_name.trim() && !(invoice.seller_name ?? '').trim()) return [{ label: '待补交易方', tone: 'warning' }]
    return [{ label: '已核对', tone: 'ok' }]
  }
  function documentCount(expense: ExpenseItem | null): number {
    return expense ? Math.max(1, expense.documents.length) : 1
  }
  function expenseTypeDetail(invoice: Invoice, expense: ExpenseItem | null): string {
    const details: string[] = []
    const ticketType = TICKET_TYPE_LABELS[invoice.ticket_type]
    const category = expense ? expenseCategoryLabel(expense) : ''
    if (ticketType && ticketType !== category && ticketType !== '其他') details.push(ticketType)
    details.push(`${documentCount(expense)} 份材料`)
    return details.join(' · ')
  }
  function pendingRoleLabel(role: PendingInvoiceDocument['proposed_role']): string {
    if (role === 'itinerary') return '行程单'
    if (role === 'detail') return '消费明细'
    return '其他材料'
  }
  function pendingImportedDate(value: string): string {
    const parsed = new Date(value)
    return Number.isNaN(parsed.getTime()) ? value.slice(0, 10) : parsed.toLocaleDateString('zh-CN')
  }
  function compareInvoices(left: Invoice, right: Invoice): number {
    const leftExpense = expenseForInvoice(left.id)
    const rightExpense = expenseForInvoice(right.id)
    let comparison = 0
    if (sortKey === 'date') comparison = (leftExpense?.transaction_date ?? left.issue_date).localeCompare(rightExpense?.transaction_date ?? right.issue_date)
    if (sortKey === 'amount') comparison = Number(leftExpense?.gross_amount ?? left.amount) - Number(rightExpense?.gross_amount ?? right.amount)
    if (sortKey === 'category') comparison = expenseCategoryLabel(leftExpense ?? { category_code: 'other', category_confirmed: false }).localeCompare(expenseCategoryLabel(rightExpense ?? { category_code: 'other', category_confirmed: false }), 'zh-CN')
    if (sortKey === 'status') comparison = problemScore(left) - problemScore(right)
    return comparison * (sortDirection === 'asc' ? 1 : -1) || left.id - right.id
  }
  function setSort(next: StoredListState['sortKey']) {
    if (sortKey === next) sortDirection = sortDirection === 'asc' ? 'desc' : 'asc'
    else { sortKey = next; sortDirection = next === 'amount' || next === 'status' ? 'desc' : 'asc' }
  }
  function sortIndicator(key: StoredListState['sortKey']): string {
    if (sortKey !== key) return ''
    return sortDirection === 'asc' ? '↑' : '↓'
  }

  function reviewQueueLabel(): string {
    if (listMode === 'excluded') return '未计入费用'
    if (selectedStatus === 'unconfirmed') return '日期待确认'
    if (selectedStatus === 'unclassified' || selectedCategory === 'unclassified') return '费用类型待确认'
    if (selectedStatus === 'duplicates') return '重复判断'
    if (selectedStatus === 'problems') return '待处理费用'
    if (selectedCategory !== 'all') return `${EXPENSE_CATEGORY_LABELS[selectedCategory]}费用`
    if (selectedGroup !== 'all') {
      const group = groups.find((candidate) => String(candidate.id) === selectedGroup)
      if (group) return displayGroupTitle(group, expenseItems)
    }
    if (searchQuery.trim()) return '搜索结果'
    return '本次费用'
  }

  function openInvoice(invoiceId: number) {
    onOpenInvoice(invoiceId, {
      invoiceIds: visibleInvoices.map((invoice) => invoice.id),
      label: reviewQueueLabel(),
    })
  }

  async function assignPendingDocument() {
    if (!selectedPendingDocument || !pendingTargetExpenseId || working) return
    const nextDocumentId = unresolvedPendingDocuments.find((document) => document.id !== selectedPendingDocument.id)?.id ?? null
    working = true
    actionError = null
    const result = await invokeSafe('assign_pending_invoice_document', { pendingDocumentId: selectedPendingDocument.id, expenseItemId: Number(pendingTargetExpenseId), role: pendingRole })
    working = false
    if (!result.ok) { actionError = describeError(result.error); return }
    statusMessage = '材料已挂载到所选费用，不会独立计费。'
    pendingTargetExpenseId = ''
    await onChanged()
    selectedPendingDocumentId = listMode === 'pending' ? nextDocumentId : null
  }
  async function ignorePendingDocument() {
    if (!selectedPendingDocument || working) return
    const nextDocumentId = unresolvedPendingDocuments.find((document) => document.id !== selectedPendingDocument.id)?.id ?? null
    confirmingIgnore = false
    working = true
    actionError = null
    const result = await invokeSafe<void>('ignore_pending_invoice_document', { pendingDocumentId: selectedPendingDocument.id })
    working = false
    if (!result.ok) { actionError = describeError(result.error); return }
    statusMessage = '该文件已明确忽略，原文件仍保留。'
    await onChanged()
    selectedPendingDocumentId = listMode === 'pending' ? nextDocumentId : null
  }
  async function convertPendingDocumentToExpense() {
    if (!selectedPendingDocument || !canConvertDidiItinerary(selectedPendingDocument) || working) return
    const nextDocumentId = unresolvedPendingDocuments.find((document) => document.id !== selectedPendingDocument.id)?.id ?? null
    working = true
    actionError = null
    const result = await invokeSafe<ExpenseItem>('convert_didi_itinerary_to_expense', { pendingDocumentId: selectedPendingDocument.id })
    working = false
    if (!result.ok) { actionError = describeError(result.error); return }
    statusMessage = `已根据滴滴行程单创建费用 #${result.data.id}，请在“本次费用”中核对。`
    pendingTargetExpenseId = ''
    await onChanged()
    selectedPendingDocumentId = listMode === 'pending' ? nextDocumentId : null
  }
  function openPending(document: PendingInvoiceDocument) {
    selectedPendingDocumentId = document.id
    pendingTargetExpenseId = ''
    pendingRole = document.proposed_role
    actionError = null
  }

  async function restoreInvoice(invoice: Invoice) {
    if (!canEdit || restoringInvoiceId !== null || invoice.is_duplicate) return
    restoringInvoiceId = invoice.id
    actionError = null
    const result = await invokeSafe<void>('set_invoice_excluded', { invoiceId: invoice.id, excluded: false })
    restoringInvoiceId = null
    if (!result.ok) { actionError = describeError(result.error); return }
    statusMessage = '费用已恢复计入，并从未计入清单移除。'
    await onChanged()
  }

  $effect(() => {
    listStateByBatch.set(batchId, { search: searchQuery, listMode, status: selectedStatus, category: selectedCategory, group: selectedGroup, sortKey, sortDirection })
  })
</script>

<section class="expense-list" aria-label="费用清单">
  <header class="list-toolbar">
    <nav class="list-tabs" aria-label="费用计入状态">
      <button class:active={listMode === 'included'} type="button" onclick={() => selectListMode('included')}><strong>本次费用</strong><span>{includedInvoices.length}</span></button>
      <button class:active={listMode === 'excluded'} type="button" onclick={() => selectListMode('excluded')}><strong>未计入</strong><span>{excludedInvoices.length}</span></button>
      <button class:active={listMode === 'pending'} type="button" onclick={() => selectListMode('pending')}><strong>待处理材料</strong><span>{unresolvedPendingDocuments.length}</span></button>
    </nav>
    <span class="result-count">{listMode === 'pending' ? `${unresolvedPendingDocuments.length} 份材料` : `显示 ${visibleInvoices.length} / ${activeInvoices.length}`}</span>
    {#if listMode === 'included'}
      <div class="summary-counters" aria-label="费用待确认统计"><button class:active={selectedStatus === 'problems'} type="button" onclick={() => (selectedStatus = selectedStatus === 'problems' ? 'all' : 'problems')}><span>待确认</span><strong>{problemCount}</strong></button><button class:active={selectedStatus === 'unclassified'} type="button" onclick={() => (selectedStatus = selectedStatus === 'unclassified' ? 'all' : 'unclassified')}><span>类型</span><strong>{unclassifiedCount}</strong></button><button class:active={selectedStatus === 'unconfirmed'} type="button" onclick={() => (selectedStatus = selectedStatus === 'unconfirmed' ? 'all' : 'unconfirmed')}><span>日期</span><strong>{unconfirmedCount}</strong></button><button class:active={selectedStatus === 'duplicates'} type="button" onclick={() => (selectedStatus = selectedStatus === 'duplicates' ? 'all' : 'duplicates')}><span>重复</span><strong>{duplicateCount}</strong></button></div>
    {/if}
  </header>

  {#if ofdOriginalCount > 0 && listMode !== 'pending'}
    <section class="attention-strip" aria-label="原件状态">
      <span class="ofd-chip" title="当前 MVP 不提供签章或真伪验证；OFD 原件仍随费用保存，不影响整理、归组和报销。">OFD 原件 {ofdOriginalCount} 份 <small>ⓘ</small></span>
    </section>
  {/if}
  {#if statusMessage}<p class="status-message" role="status">{statusMessage}</p>{/if}
  {#if actionError}<p class="status-message error" role="alert">{actionError}</p>{/if}

  {#if listMode === 'pending'}
    <div class="material-list" role="table" aria-label="待处理材料">
      <div class="material-list-head" role="row"><span role="columnheader">文件</span><span role="columnheader">系统判断</span><span role="columnheader">待处理原因</span><span role="columnheader">来源 / 导入时间</span><span role="columnheader">操作</span></div>
      {#each unresolvedPendingDocuments as document (document.id)}
        <div class="material-row" role="row">
          <div class="material-file" role="cell" title={document.original_name}><strong>{document.original_name}</strong></div>
          <div class="material-role" role="cell"><span>{pendingRoleLabel(document.proposed_role)}</span>{#if canConvertDidiItinerary(document)}<small>可转为费用</small>{/if}</div>
          <div class="material-reason" role="cell" title={pendingDocumentReasonLabel(document.detection_reason)}><span>{pendingDocumentReasonLabel(document.detection_reason)}</span></div>
          <div class="material-source" role="cell"><strong>批次导入</strong><small>{pendingImportedDate(document.created_at)}</small></div>
          <div class="material-row-action" role="cell"><button type="button" onclick={() => openPending(document)}>查看并处理</button></div>
        </div>
      {:else}
        <div class="material-empty"><strong>待处理材料已清空</strong><span>所有材料均已挂载到费用或明确忽略。</span><button type="button" onclick={() => selectListMode('included')}>返回本次费用</button></div>
      {/each}
    </div>
  {:else}
    <div class="filters" aria-label="费用筛选">
    <label class="search"><span class="sr-only">搜索费用</span><input bind:value={searchQuery} aria-label="搜索费用" placeholder="搜索供应商、发票号、城市或说明" /></label>
    <label><span class="sr-only">状态</span><select bind:value={selectedStatus} aria-label="按状态筛选"><option value="all">全部状态</option><option value="problems">仅待确认</option><option value="unclassified">类型待确认</option><option value="duplicates">重复判断</option><option value="unconfirmed">日期待确认</option></select></label>
    <label><span class="sr-only">费用类型</span><select bind:value={selectedCategory} aria-label="按费用类型筛选"><option value="all">全部类型</option><option value="unclassified">待分类</option>{#each EXPENSE_CATEGORIES as type}<option value={type}>{EXPENSE_CATEGORY_LABELS[type]}</option>{/each}</select></label>
    <label><span class="sr-only">归组</span><select bind:value={selectedGroup} aria-label="按归组筛选"><option value="all">全部归组</option>{#each groups as group}<option value={String(group.id)}>{displayGroupTitle(group, expenseItems)}</option>{/each}</select></label>
    {#if searchQuery || selectedStatus !== 'all' || selectedCategory !== 'all' || selectedGroup !== 'all'}<button class="clear" type="button" onclick={() => { searchQuery = ''; selectedStatus = 'all'; selectedCategory = 'all'; selectedGroup = 'all' }}>清除筛选</button>{/if}
    </div>

    <div class="table-shell">
      <table>
      <colgroup><col class="date-column" /><col class="type-column" /><col class="amount-column" /><col class="party-column" /><col class="group-column" /><col class="status-column" /><col class="action-column" /></colgroup>
      <thead><tr><th class="date-cell"><button class="sort-button" type="button" onclick={() => setSort('date')}>实际日期 {sortIndicator('date')}</button></th><th class="type-cell"><button class="sort-button" type="button" onclick={() => setSort('category')}>费用类型 {sortIndicator('category')}</button></th><th class="amount"><button class="sort-button" type="button" onclick={() => setSort('amount')}>实际金额 {sortIndicator('amount')}</button></th><th class="party-cell">交易方 / 地点</th><th class="group-cell">归组</th><th class="status-cell"><button class="sort-button" type="button" onclick={() => setSort('status')}>状态 {sortIndicator('status')}</button></th><th class="action-cell"><span class="sr-only">打开</span></th></tr></thead>
      <tbody>
        {#each visibleInvoices as invoice}
          {@const expense = expenseForInvoice(invoice.id)}
          {@const group = groupForInvoice(invoice.id)}
          {@const issues = issueLabels(invoice, expense)}
          <tr class:problem={problemScore(invoice) > 0}>
            <td class="date-cell" title={expense ? transactionDateSourceLabel(expense.transaction_date_source) : '开票日期候选'}><strong>{expense?.transaction_date || invoice.issue_date || '待确认'}</strong>{#if !expense?.transaction_date_confirmed}<small class="pending">{expense ? transactionDateSourceLabel(expense.transaction_date_source) : '开票日期候选'} · 待确认</small>{/if}</td>
            <td class="type-cell"><button class="expense-link" type="button" title={`${TICKET_TYPE_LABELS[invoice.ticket_type]} · ${invoice.invoice_number || '无发票号码'} · ${documentCount(expense)} 份材料`} onclick={() => openInvoice(invoice.id)}><strong class:pending={expense && !expense.category_confirmed}>{expense ? expenseCategoryLabel(expense) : '待分类'}</strong><span>{expenseTypeDetail(invoice, expense)}</span></button></td>
            <td class="amount"><strong>{formatAmount(expense?.gross_amount ?? invoice.amount, expense?.currency_code ?? 'CNY')}</strong>{#if invoice.is_duplicate || invoice.is_excluded}<small>未计入</small>{/if}</td>
            <td class="party-cell" title={`${expense?.counterparty_name || invoice.seller_name || '交易方待补充'} · ${expense?.location.city_name || invoice.city || '地点待核对'}`}><strong>{expense?.counterparty_name || invoice.seller_name || '交易方待补充'}</strong><small>{expense?.location.city_name || invoice.city || '地点待核对'}</small></td>
            <td class="group-cell" title={group ? displayGroupTitle(group, expenseItems) : '尚未归组'}><span>{group ? displayGroupTitle(group, expenseItems) : '尚未归组'}</span>{#if group?.requires_review}<small>需确认</small>{/if}</td>
            <td class="status-cell"><div class="state-list">{#each issues as issue}<span class:warning={issue.tone === 'warning'} class:muted={issue.tone === 'muted'} class="state-badge">{issue.label}</span>{/each}</div></td>
            <td class="action-cell"><div class="row-actions"><button class="open-row" type="button" aria-label={`查看费用 ${invoice.invoice_number}`} onclick={() => openInvoice(invoice.id)}>{listMode === 'excluded' ? '查看' : '›'}</button>{#if listMode === 'excluded' && invoice.is_excluded && !invoice.is_duplicate}<button class="restore-row" type="button" onclick={() => void restoreInvoice(invoice)} disabled={!canEdit || restoringInvoiceId !== null}>{restoringInvoiceId === invoice.id ? '恢复中…' : '恢复计入'}</button>{/if}</div></td>
          </tr>
        {:else}<tr><td class="empty" colspan="7">当前筛选条件下没有费用。</td></tr>{/each}
      </tbody>
      </table>
    </div>
  {/if}
</section>

{#if selectedPendingDocument}
  <div class="drawer-backdrop" role="presentation" onclick={(event) => event.target === event.currentTarget && (selectedPendingDocumentId = null)}>
    <div class="material-drawer" role="dialog" aria-modal="true" aria-labelledby="material-title">
      <header><div><span class="eyebrow">配套材料处理</span><h2 id="material-title">{selectedPendingDocument.original_name}</h2><p>{pendingDocumentReasonLabel(selectedPendingDocument.detection_reason)}</p></div><button type="button" aria-label="关闭" onclick={() => (selectedPendingDocumentId = null)}>×</button></header>
      <div class="material-body">
        <div class="material-preview"><OriginalPreview invoice={null} pendingDocument={selectedPendingDocument} /></div>
        <form onsubmit={(event) => { event.preventDefault(); void assignPendingDocument() }}>
          <h3>挂载到费用</h3><p>选择所属费用与材料角色。挂载后材料随该费用进入后续整理，但不单独计费。</p>
          <label><span>所属费用 *</span><select bind:value={pendingTargetExpenseId}><option value="">请选择费用</option>{#each expenseItems.filter((expense) => expense.inclusion_status === 'included') as expense}<option value={String(expense.id)}>#{expense.id} · {expense.transaction_date} · {expense.counterparty_name || '交易方待补充'} · {formatAmount(expense.gross_amount)}</option>{/each}</select></label>
          <label><span>材料角色 *</span><select bind:value={pendingRole}><option value="itinerary">行程单</option><option value="detail">消费明细</option><option value="supporting">其他材料</option></select></label>
          {#if canConvertDidiItinerary(selectedPendingDocument)}<p class="conversion-note"><strong>只有滴滴电子行程单、纸质发票未导入？</strong><span>可直接按行程单金额和日期创建一笔出租车费用，之后仍可在费用页面核对。</span></p>{/if}
          {#if actionError}<p class="drawer-error" role="alert">{actionError}</p>{/if}
          <footer class="material-actions">
            <div class="ignore-action"><button class="ignore" type="button" onclick={() => (confirmingIgnore = true)} disabled={!canEdit || working}>明确忽略</button><small>保留原文件，但不再列为待处理材料</small></div>
            <div class="primary-actions">{#if canConvertDidiItinerary(selectedPendingDocument)}<button class="convert" type="button" onclick={() => void convertPendingDocumentToExpense()} disabled={!canEdit || working}>{working ? '处理中…' : '转为出租车费用'}</button>{/if}<button class="assign" type="submit" disabled={!canEdit || !pendingTargetExpenseId || working}>{working ? '处理中…' : '挂载材料'}</button></div>
          </footer>
        </form>
      </div>
    </div>
  </div>
{/if}

{#if confirmingIgnore}
  <ConfirmDialog title="忽略这份材料" message="该文件不会再阻止完成审核，也不会挂载到任何费用；原文件仍会保留。" confirmLabel="明确忽略" tone="danger" busy={working} onConfirm={() => void ignorePendingDocument()} onCancel={() => (confirmingIgnore = false)} />
{/if}

<style>
  .expense-list{container:expense-list/inline-size;border:1px solid #cbd2d6;background:#fff;color:#17232d}.list-toolbar{display:flex;align-items:center;gap:.75rem;min-height:52px;padding:0 .7rem;border-bottom:1px solid #cbd2d6;background:#f7f8f8}.list-tabs{display:flex;align-self:stretch}.list-tabs button{display:flex;align-items:center;gap:.45rem;min-width:112px;padding:.55rem .7rem;border:0;border-bottom:3px solid transparent;background:transparent;color:#65737a;cursor:pointer}.list-tabs button.active{border-bottom-color:#136b52;background:#fff;color:#17232d}.list-tabs button span{display:inline-grid;min-width:22px;height:22px;place-items:center;border-radius:999px;background:#e2e7e5;color:#4f5f57;font-size:.7rem;font-weight:700}.list-tabs button.active span{background:#dceee6;color:#136b52}.result-count{margin-right:auto;color:#65737a;font-size:.78rem;white-space:nowrap}.summary-counters{display:flex;gap:.35rem}.summary-counters button{display:flex;align-items:center;gap:.4rem;min-height:32px;padding:.35rem .5rem;border:1px solid #c3ccd0;background:#fff;color:#59676e;cursor:pointer}.summary-counters button.active{border-color:#136b52;background:#edf6f1;color:#174f3d}.summary-counters span{font-size:.72rem}.summary-counters strong{font-size:.85rem}.eyebrow{color:#6b787f;font-family:'IBM Plex Mono',monospace;font-size:.7rem;font-weight:700;letter-spacing:.08em;text-transform:uppercase}
  .attention-strip{display:flex;align-items:center;gap:.55rem;padding:.45rem .7rem;border-bottom:1px solid #d7dde0;background:#fbfcfb}.ofd-chip{padding:.35rem .55rem;border:1px solid #bdd8ca;background:#edf6f1;color:#3f6254;font-size:.76rem}.ofd-chip small{font-size:.7rem}.status-message{margin:0;padding:.55rem .75rem;border-bottom:1px solid #bdd8ca;background:#edf6f1;color:#24533f}.status-message.error{border-color:#e2b8b5;background:#fff1f0;color:#862f2a}
  .material-list{min-width:0}.material-list-head,.material-row{display:grid;grid-template-columns:minmax(180px,1.25fr) 120px minmax(210px,1.4fr) 150px 110px;gap:.75rem;align-items:center}.material-list-head{padding:.6rem .75rem;border-bottom:1px solid #aeb9bf;background:#f1f3f4;color:#5b6870;font-size:.72rem;font-weight:700}.material-row{padding:.72rem .75rem;border-bottom:1px solid #e0e5e7}.material-row:hover{background:#f7faf8}.material-file,.material-reason,.material-source{min-width:0}.material-file strong,.material-reason span,.material-source strong,.material-source small{display:block;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}.material-file strong,.material-source strong{font-size:.84rem}.material-source small{margin-top:.16rem;color:#6b787f;font-size:.7rem}.material-role{display:grid;justify-items:start;gap:.2rem}.material-role span{display:inline-flex;padding:.24rem .42rem;border-left:3px solid #c47a16;background:#fff7e7;color:#81540f;font-size:.72rem;font-weight:700}.material-role small{color:#136b52;font-size:.66rem;font-weight:700}.material-reason span{color:#4f5e65;font-size:.78rem}.material-row-action{text-align:right}.material-row-action button,.material-empty button{min-height:32px;padding:.36rem .55rem;border:1px solid #136b52;background:#fff;color:#136b52;font-weight:700;white-space:nowrap;cursor:pointer}.material-empty{display:grid;justify-items:center;gap:.4rem;padding:3rem 1rem;color:#65737a;text-align:center}.material-empty strong{color:#24533f;font-size:1rem}.material-empty button{margin-top:.4rem}
  .filters{display:grid;grid-template-columns:minmax(220px,1fr) minmax(115px,145px) minmax(125px,155px) minmax(150px,200px) auto;gap:.5rem;align-items:center;padding:.5rem .7rem;border-bottom:1px solid #d7dde0;background:#f7f8f8}.filters label{min-width:0}.filters input,.filters select{width:100%;height:36px;min-width:0;padding:.4rem .55rem;border:1px solid #aeb9bf;background:#fff;color:#17232d;font:inherit;font-size:.82rem}.filters input:focus,.filters select:focus{outline:3px solid rgba(19,107,82,.13);border-color:#136b52}.clear{height:36px;padding:.4rem .55rem;border:0;background:transparent;color:#136b52;font-weight:700;white-space:nowrap;cursor:pointer}
  .table-shell{width:100%;overflow-x:hidden}table{width:100%;table-layout:fixed;border-collapse:collapse}.date-column{width:11%}.type-column{width:19%}.amount-column{width:11%}.party-column{width:21%}.group-column{width:17%}.status-column{width:10%}.action-column{width:11%}th{position:sticky;top:0;z-index:2;padding:.6rem .65rem;border-bottom:1px solid #aeb9bf;background:#f1f3f4;color:#5b6870;font-size:.72rem;text-align:left}td{min-width:0;overflow:hidden;padding:.62rem .65rem;border-bottom:1px solid #e0e5e7;vertical-align:middle}tbody tr:hover{background:#f7faf8}tbody tr.problem{box-shadow:inset 3px 0 #c47a16}td strong,td small,.group-cell>span,.expense-link span{display:block;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}td strong{font-size:.84rem}td small{margin-top:.16rem;color:#6b787f;font-size:.7rem}td .pending{color:#9a5f08}.sort-button{max-width:100%;overflow:hidden;padding:0;border:0;background:transparent;color:inherit;font:inherit;font-weight:700;text-overflow:ellipsis;white-space:nowrap;cursor:pointer}.sort-button:hover{color:#136b52}.expense-link{display:grid;width:100%;min-width:0;gap:.15rem;padding:0;border:0;background:transparent;color:#17232d;text-align:left;cursor:pointer}.expense-link:hover strong{color:#136b52;text-decoration:underline}.expense-link span{color:#65737a;font-size:.72rem}.state-list{display:flex;flex-wrap:wrap;gap:.2rem}.state-badge{display:inline-flex;padding:.22rem .36rem;border-left:3px solid #136b52;background:#edf6f1;color:#24533f;font-size:.7rem;font-weight:700;white-space:nowrap}.state-badge.warning{border-color:#c47a16;background:#fff7e7;color:#81540f}.state-badge.muted{border-color:#7a878e;background:#eff1f2;color:#5b676d}.amount{text-align:right;white-space:nowrap}.amount .sort-button{width:100%;text-align:right}.amount small{color:#9a5f08}.row-actions{display:flex;align-items:center;justify-content:flex-end;gap:.25rem;white-space:nowrap}.open-row{min-width:30px;height:32px;padding:0 .35rem;border:0;background:transparent;color:#136b52;font-size:1rem;font-weight:700;cursor:pointer}.restore-row{min-height:30px;padding:.3rem .42rem;border:1px solid #136b52;background:#fff;color:#136b52;font-size:.7rem;font-weight:700;cursor:pointer}.restore-row:disabled{opacity:.45;cursor:not-allowed}.empty{padding:3rem!important;color:#68757c;text-align:center}.sr-only{position:absolute;width:1px;height:1px;overflow:hidden;clip:rect(0,0,0,0);white-space:nowrap}
  .drawer-backdrop{position:fixed;inset:0;z-index:100;background:rgba(20,31,36,.42)}.material-drawer{position:absolute;top:0;right:0;bottom:0;display:grid;grid-template-rows:auto minmax(0,1fr);width:min(1040px,calc(100vw - var(--app-sidebar-width,224px)));overflow:hidden;background:#f4f5f6;box-shadow:-18px 0 40px rgba(20,31,36,.18)}.material-drawer>header{display:flex;align-items:flex-start;justify-content:space-between;gap:1rem;padding:1rem 1.2rem;border-bottom:1px solid #cbd2d6;background:#fff}.material-drawer h2{margin:.2rem 0 0;font-size:1.2rem}.material-drawer header p{margin:.25rem 0 0;color:#65737a}.material-drawer header button{border:0;background:transparent;font-size:1.6rem;cursor:pointer}.material-body{display:grid;grid-template-columns:minmax(420px,60%) minmax(300px,40%);min-height:0}.material-preview{min-width:0;min-height:0;overflow:hidden;border-right:1px solid #cbd2d6}.material-preview :global(.preview-shell){height:100%;min-height:0;border:0}.material-body form{box-sizing:border-box;display:flex;min-width:0;min-height:0;height:100%;flex-direction:column;gap:.9rem;overflow-y:auto;padding:1.25rem;background:#fff}.material-body h3{margin:0}.material-body form>p{margin:0;color:#65737a;line-height:1.5}.material-body label{display:grid;flex:none;gap:.35rem}.material-body label span{color:#5d6a71;font-size:.78rem;font-weight:700}.material-body select{height:42px;padding:.5rem .6rem;border:1px solid #aeb9bf;background:#fff;font:inherit}.conversion-note{display:grid;gap:.2rem;padding:.65rem .7rem;border-left:4px solid #315f8a;background:#edf3f8}.conversion-note strong{color:#274c6d;font-size:.78rem}.conversion-note span{color:#4d6275;font-size:.72rem}.material-actions{position:sticky;right:0;bottom:-1.25rem;left:0;z-index:3;display:flex;flex:none;align-items:center;justify-content:space-between;gap:.75rem;margin:auto -1.25rem -1.25rem;padding:.8rem 1.25rem;border-top:1px solid #cbd2d6;background:#fff;box-shadow:0 -8px 20px rgba(38,47,42,.08)}.material-actions button{min-height:38px;padding:.55rem .8rem;font-weight:700;cursor:pointer}.ignore-action,.primary-actions{display:flex;align-items:center;gap:.6rem}.ignore-action small{max-width:180px;color:#7a6664;font-size:.68rem;line-height:1.35}.ignore{border:1px solid #b3453e;background:#fff;color:#a53630}.ignore:hover:not(:disabled){background:#fff1f0}.convert{border:1px solid #315f8a;background:#fff;color:#315f8a}.assign{border:1px solid #136b52;background:#136b52;color:#fff}.material-body button:disabled{opacity:.45;cursor:not-allowed}.drawer-error{padding:.6rem;border-left:3px solid #b3453e;background:#fff1f0;color:#862f2a}
  @container expense-list (max-width:900px){.list-toolbar{align-items:stretch;flex-wrap:wrap;padding-top:.35rem}.list-tabs{height:42px}.result-count{align-self:center}.summary-counters{width:100%;padding-bottom:.4rem}.summary-counters button{flex:1;justify-content:center}.filters{grid-template-columns:1fr 1fr}.filters .search{grid-column:1/-1}.material-list-head{display:none}.material-row{grid-template-columns:minmax(0,1fr) auto;gap:.4rem .75rem}.material-file{grid-column:1}.material-role{grid-column:2;grid-row:1}.material-reason,.material-source{grid-column:1/-1}.material-row-action{grid-column:1/-1;text-align:left}colgroup,thead{display:none}table,tbody{display:block}tbody tr{position:relative;display:grid;grid-template-columns:minmax(0,1fr) auto;gap:.35rem .75rem;padding:.7rem 3rem .7rem .75rem;border-bottom:1px solid #d7dde0}td{display:block;padding:0;border:0}.date-cell{grid-column:1;grid-row:1}.amount{grid-column:2;grid-row:1;text-align:right}.type-cell{grid-column:1/-1;grid-row:2}.party-cell{grid-column:1/-1;grid-row:3}.group-cell{grid-column:1/-1;grid-row:4}.status-cell{grid-column:1/-1;grid-row:5}.action-cell{position:absolute;top:.5rem;right:.45rem}.group-cell:before{content:'归组 · ';color:#6b787f;font-size:.7rem}.group-cell>span{display:inline}.empty{display:block;padding:2rem!important}}
  @media(max-width:1100px){.material-body{grid-template-columns:1fr}.material-preview{display:none}.material-drawer{width:min(600px,calc(100vw - var(--app-sidebar-width,224px)))}}@media(max-width:820px){.material-drawer{width:100%}}@media(max-width:700px){.attention-strip{flex-wrap:wrap}.filters{grid-template-columns:1fr}.filters .search{grid-column:auto}.summary-counters span{display:none}.material-actions{align-items:stretch;flex-wrap:wrap}.ignore-action{display:grid;gap:.25rem}.ignore-action small{max-width:150px}.primary-actions{margin-left:auto}.list-tabs{width:100%}.list-tabs button{min-width:0;flex:1;padding-inline:.4rem;font-size:.78rem}}
</style>
