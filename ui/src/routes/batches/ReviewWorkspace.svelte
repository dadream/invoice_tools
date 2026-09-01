<script module lang="ts">
  interface StoredListState {
    search: string
    listMode: 'included' | 'excluded'
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
  import { displayGroupTitle } from '../../lib/grouping'
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
    onOpenInvoice: (invoiceId: number) => void
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
  const activeInvoices = $derived(listMode === 'included' ? includedInvoices : excludedInvoices)
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
  function problemScore(invoice: Invoice): number {
    const expense = expenseForInvoice(invoice.id)
    const group = groupForInvoice(invoice.id)
    return (invoice.verification_result === 'invalid' ? 16 : 0)
      + (invoice.is_duplicate && !invoice.is_excluded ? 12 : 0)
      + (!expense?.transaction_date_confirmed ? 8 : 0)
      + (!expense?.category_confirmed ? 8 : 0)
      + (group?.requires_review ? 4 : 0)
      + (!expense?.counterparty_name.trim() ? 2 : 0)
  }
  function issueLabels(invoice: Invoice, expense: ExpenseItem | null): { label: string; tone: 'warning' | 'muted' }[] {
    const issues: { label: string; tone: 'warning' | 'muted' }[] = []
    if (invoice.is_duplicate && !invoice.is_excluded) issues.push({ label: '疑似重复', tone: 'warning' })
    if (invoice.is_duplicate && invoice.is_excluded) issues.push({ label: '重复 · 未计入', tone: 'muted' })
    else if (invoice.is_excluded || expense?.inclusion_status === 'excluded') issues.push({ label: '已排除', tone: 'muted' })
    if (expense && !expense.category_confirmed) issues.push({ label: '待分类', tone: 'warning' })
    if (expense && !expense.transaction_date_confirmed) issues.push({ label: '日期待确认', tone: 'warning' })
    if (groupForInvoice(invoice.id)?.requires_review) issues.push({ label: '归组待确认', tone: 'warning' })
    if (invoice.verification_result === 'invalid') issues.push({ label: '签章异常', tone: 'warning' })
    if (invoice.verification_result === 'unsupported') issues.push({ label: '验签暂不支持', tone: 'muted' })
    return issues
  }
  function documentCount(expense: ExpenseItem | null): number { return 1 + (expense?.documents.length ?? 0) }
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
    if (sortKey !== key) return '↕'
    return sortDirection === 'asc' ? '↑' : '↓'
  }

  async function assignPendingDocument() {
    if (!selectedPendingDocument || !pendingTargetExpenseId || working) return
    working = true
    actionError = null
    const result = await invokeSafe('assign_pending_invoice_document', { pendingDocumentId: selectedPendingDocument.id, expenseItemId: Number(pendingTargetExpenseId), role: pendingRole })
    working = false
    if (!result.ok) { actionError = describeError(result.error); return }
    statusMessage = '材料已挂载到所选费用，不会独立计费。'
    selectedPendingDocumentId = null
    pendingTargetExpenseId = ''
    await onChanged()
  }
  async function ignorePendingDocument() {
    if (!selectedPendingDocument || working) return
    confirmingIgnore = false
    working = true
    actionError = null
    const result = await invokeSafe<void>('ignore_pending_invoice_document', { pendingDocumentId: selectedPendingDocument.id })
    working = false
    if (!result.ok) { actionError = describeError(result.error); return }
    statusMessage = '该文件已明确忽略，原文件仍保留。'
    selectedPendingDocumentId = null
    await onChanged()
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
  <nav class="list-tabs" aria-label="费用计入状态">
    <button class:active={listMode === 'included'} type="button" onclick={() => selectListMode('included')}><strong>本次费用</strong><span>{includedInvoices.length}</span></button>
    <button class:active={listMode === 'excluded'} type="button" onclick={() => selectListMode('excluded')}><strong>未计入</strong><span>{excludedInvoices.length}</span></button>
  </nav>
  <header class="list-heading">
    <div><span class="eyebrow">{listMode === 'included' ? '费用清单' : '未计入清单'}</span><h2>{visibleInvoices.length} / {activeInvoices.length} 条费用</h2><p>{listMode === 'included' ? '这里只显示计入本次报销的费用，默认按实际发生日期升序。' : '这些费用不参与批次总额、归组和后续交付，原件仍然保留。'}</p></div>
    <div class="summary-counters"><button class:active={selectedStatus === 'problems'} type="button" onclick={() => (selectedStatus = selectedStatus === 'problems' ? 'all' : 'problems')}><span>待处理</span><strong>{problemCount}</strong></button><button class:active={selectedStatus === 'unclassified'} type="button" onclick={() => (selectedStatus = selectedStatus === 'unclassified' ? 'all' : 'unclassified')}><span>待分类</span><strong>{unclassifiedCount}</strong></button><button class:active={selectedStatus === 'duplicates'} type="button" onclick={() => (selectedStatus = selectedStatus === 'duplicates' ? 'all' : 'duplicates')}><span>重复判断</span><strong>{duplicateCount}</strong></button><button class:active={selectedStatus === 'unconfirmed'} type="button" onclick={() => (selectedStatus = selectedStatus === 'unconfirmed' ? 'all' : 'unconfirmed')}><span>日期待确认</span><strong>{unconfirmedCount}</strong></button></div>
  </header>

  {#if unresolvedPendingDocuments.length > 0}
    <section class="pending-banner">
      <div><strong>{unresolvedPendingDocuments.length} 份配套材料待处理</strong><span>行程单和明细必须挂到一笔费用上；材料本身不生成金额。</span></div>
      <div class="pending-files">{#each unresolvedPendingDocuments.slice(0, 3) as document}<button type="button" onclick={() => openPending(document)}>{document.original_name}</button>{/each}{#if unresolvedPendingDocuments.length > 3}<span>另有 {unresolvedPendingDocuments.length - 3} 份</span>{/if}</div>
    </section>
  {/if}
  {#if statusMessage}<p class="status-message" role="status">{statusMessage}</p>{/if}
  {#if actionError}<p class="status-message error" role="alert">{actionError}</p>{/if}

  <div class="filters" aria-label="费用筛选">
    <label class="search"><span>搜索费用</span><input bind:value={searchQuery} placeholder="供应商、发票号、城市或说明" /></label>
    <label><span>状态</span><select bind:value={selectedStatus}><option value="all">全部状态</option><option value="problems">仅待处理</option><option value="unclassified">待分类</option><option value="duplicates">重复判断</option><option value="unconfirmed">日期待确认</option></select></label>
    <label><span>费用类型</span><select bind:value={selectedCategory}><option value="all">全部类型</option><option value="unclassified">待分类</option>{#each EXPENSE_CATEGORIES as type}<option value={type}>{EXPENSE_CATEGORY_LABELS[type]}</option>{/each}</select></label>
    <label><span>归组</span><select bind:value={selectedGroup}><option value="all">全部归组</option>{#each groups as group}<option value={group.id}>{displayGroupTitle(group, expenseItems)}</option>{/each}</select></label>
    {#if searchQuery || selectedStatus !== 'all' || selectedCategory !== 'all' || selectedGroup !== 'all'}<button class="clear" type="button" onclick={() => { searchQuery = ''; selectedStatus = 'all'; selectedCategory = 'all'; selectedGroup = 'all' }}>清除筛选</button>{/if}
  </div>

  <div class="table-shell">
    <table>
      <thead><tr><th><button class="sort-button" type="button" onclick={() => setSort('date')}>实际日期 {sortIndicator('date')}</button></th><th><button class="sort-button" type="button" onclick={() => setSort('category')}>费用类型 {sortIndicator('category')}</button></th><th class="amount"><button class="sort-button" type="button" onclick={() => setSort('amount')}>实际金额 {sortIndicator('amount')}</button></th><th>交易方 / 地点</th><th>归组</th><th><button class="sort-button" type="button" onclick={() => setSort('status')}>状态 {sortIndicator('status')}</button></th><th><span class="sr-only">打开</span></th></tr></thead>
      <tbody>
        {#each visibleInvoices as invoice}
          {@const expense = expenseForInvoice(invoice.id)}
          {@const group = groupForInvoice(invoice.id)}
          {@const issues = issueLabels(invoice, expense)}
          <tr class:problem={problemScore(invoice) > 0}>
            <td><strong>{expense?.transaction_date || invoice.issue_date || '待确认'}</strong><small class:pending={expense && !expense.transaction_date_confirmed}>{expense ? transactionDateSourceLabel(expense.transaction_date_source) : '开票日期候选'}{#if expense && !expense.transaction_date_confirmed} · 待确认{/if}</small></td>
            <td><button class="expense-link" type="button" onclick={() => onOpenInvoice(invoice.id)}><strong class:pending={expense && !expense.category_confirmed}>{expense ? expenseCategoryLabel(expense) : '待分类'}</strong><span>票据：{TICKET_TYPE_LABELS[invoice.ticket_type]} · {invoice.invoice_number || '无发票号码'} · {documentCount(expense)} 份文件</span></button></td>
            <td class="amount"><strong>{formatAmount(expense?.gross_amount ?? invoice.amount, expense?.currency_code ?? 'CNY')}</strong>{#if invoice.is_duplicate || invoice.is_excluded}<small>未计入</small>{/if}</td>
            <td><strong>{expense?.counterparty_name || invoice.seller_name || '交易方待补充'}</strong><small>{expense?.location.city_name || invoice.city || '地点待核对'}</small></td>
            <td><span>{group ? displayGroupTitle(group, expenseItems) : '尚未归组'}</span>{#if group?.requires_review}<small>需确认</small>{/if}</td>
            <td><div class="state-list">{#each issues as issue}<span class:warning={issue.tone === 'warning'} class:muted={issue.tone === 'muted'} class="state-badge">{issue.label}</span>{:else}<span class="state-badge">已核对</span>{/each}</div></td>
            <td><div class="row-actions"><button class="open-row" type="button" aria-label={`查看费用 ${invoice.invoice_number}`} onclick={() => onOpenInvoice(invoice.id)}>{listMode === 'excluded' ? '查看' : '›'}</button>{#if listMode === 'excluded' && invoice.is_excluded && !invoice.is_duplicate}<button class="restore-row" type="button" onclick={() => void restoreInvoice(invoice)} disabled={!canEdit || restoringInvoiceId !== null}>{restoringInvoiceId === invoice.id ? '恢复中…' : '恢复计入'}</button>{/if}</div></td>
          </tr>
        {:else}<tr><td class="empty" colspan="7">当前筛选条件下没有费用。</td></tr>{/each}
      </tbody>
    </table>
  </div>
</section>

{#if selectedPendingDocument}
  <div class="drawer-backdrop" role="presentation" onclick={(event) => event.target === event.currentTarget && (selectedPendingDocumentId = null)}>
    <div class="material-drawer" role="dialog" aria-modal="true" aria-labelledby="material-title">
      <header><div><span class="eyebrow">配套材料处理</span><h2 id="material-title">{selectedPendingDocument.original_name}</h2><p>{selectedPendingDocument.detection_reason}</p></div><button type="button" aria-label="关闭" onclick={() => (selectedPendingDocumentId = null)}>×</button></header>
      <div class="material-body">
        <div class="material-preview"><OriginalPreview invoice={null} pendingDocument={selectedPendingDocument} /></div>
        <form onsubmit={(event) => { event.preventDefault(); void assignPendingDocument() }}>
          <h3>挂载到费用</h3><p>选择所属费用与材料角色。挂载后材料随该费用进入后续整理，但不单独计费。</p>
          <label><span>所属费用 *</span><select bind:value={pendingTargetExpenseId}><option value="">请选择费用</option>{#each expenseItems.filter((expense) => expense.inclusion_status === 'included') as expense}<option value={expense.id}>#{expense.id} · {expense.transaction_date} · {expense.counterparty_name || '交易方待补充'} · {formatAmount(expense.gross_amount)}</option>{/each}</select></label>
          <label><span>材料角色 *</span><select bind:value={pendingRole}><option value="itinerary">行程单</option><option value="detail">消费明细</option><option value="supporting">其他材料</option></select></label>
          {#if actionError}<p class="drawer-error" role="alert">{actionError}</p>{/if}
          <footer><button class="ignore" type="button" onclick={() => (confirmingIgnore = true)} disabled={!canEdit || working}>明确忽略</button><button class="assign" type="submit" disabled={!canEdit || !pendingTargetExpenseId || working}>{working ? '处理中…' : '挂载材料'}</button></footer>
        </form>
      </div>
    </div>
  </div>
{/if}

{#if confirmingIgnore}
  <ConfirmDialog title="忽略这份材料" message="该文件不会再阻止完成审核，也不会挂载到任何费用；原文件仍会保留。" confirmLabel="明确忽略" tone="danger" busy={working} onConfirm={() => void ignorePendingDocument()} onCancel={() => (confirmingIgnore = false)} />
{/if}

<style>
  .expense-list{border:1px solid #cbd2d6;background:#fff;color:#17232d}.list-tabs{display:flex;border-bottom:1px solid #cbd2d6;background:#f7f8f8}.list-tabs button{display:flex;align-items:center;gap:.55rem;min-width:150px;padding:.78rem 1.1rem;border:0;border-bottom:3px solid transparent;background:transparent;color:#65737a;cursor:pointer}.list-tabs button.active{border-bottom-color:#136b52;background:#fff;color:#17232d}.list-tabs button span{display:inline-grid;min-width:24px;height:24px;place-items:center;border-radius:999px;background:#e2e7e5;color:#4f5f57;font-size:.72rem;font-weight:700}.list-tabs button.active span{background:#dceee6;color:#136b52}.list-heading{display:flex;align-items:flex-end;justify-content:space-between;gap:1.5rem;padding:1rem 1.1rem;border-bottom:1px solid #d7dde0}.eyebrow{color:#6b787f;font-family:'IBM Plex Mono',monospace;font-size:.7rem;font-weight:700;letter-spacing:.08em;text-transform:uppercase}.list-heading h2{margin:.18rem 0 0;font-size:1.15rem}.list-heading p{margin:.25rem 0 0;color:#65737a;font-size:.84rem}.summary-counters{display:flex;gap:.5rem}.summary-counters button{display:grid;grid-template-columns:auto auto;gap:.2rem .65rem;align-items:baseline;min-width:105px;padding:.55rem .7rem;border:1px solid #c3ccd0;background:#f7f8f8;color:#59676e;text-align:left;cursor:pointer}.summary-counters button.active{border-color:#136b52;background:#edf6f1;color:#174f3d}.summary-counters span{font-size:.72rem}.summary-counters strong{font-size:1.05rem}
  .pending-banner{display:flex;align-items:center;justify-content:space-between;gap:1rem;padding:.75rem 1rem;border-bottom:1px solid #e2c78d;background:#fff7e7}.pending-banner>div:first-child{display:grid;gap:.2rem}.pending-banner span{color:#6d5932;font-size:.8rem}.pending-files{display:flex;align-items:center;gap:.4rem}.pending-files button{max-width:180px;overflow:hidden;padding:.4rem .55rem;border:1px solid #c47a16;background:#fff;color:#8a570d;text-overflow:ellipsis;white-space:nowrap;cursor:pointer}.status-message{margin:0;padding:.65rem 1rem;border-bottom:1px solid #bdd8ca;background:#edf6f1;color:#24533f}.status-message.error{border-color:#e2b8b5;background:#fff1f0;color:#862f2a}
  .filters{display:grid;grid-template-columns:minmax(240px,1fr) repeat(3,minmax(130px,190px)) auto;gap:.7rem;align-items:end;padding:.8rem 1rem;border-bottom:1px solid #d7dde0;background:#f7f8f8}.filters label{display:grid;gap:.3rem}.filters label>span{color:#65737a;font-size:.72rem;font-weight:700}.filters input,.filters select{height:38px;min-width:0;padding:.45rem .6rem;border:1px solid #aeb9bf;background:#fff;color:#17232d;font:inherit}.filters input:focus,.filters select:focus{outline:3px solid rgba(19,107,82,.13);border-color:#136b52}.clear{height:38px;padding:.45rem .65rem;border:0;background:transparent;color:#136b52;font-weight:700;cursor:pointer}
  .table-shell{overflow:auto}table{width:100%;min-width:1120px;border-collapse:collapse}th{position:sticky;top:0;z-index:2;padding:.65rem .75rem;border-bottom:1px solid #aeb9bf;background:#f1f3f4;color:#5b6870;font-size:.72rem;text-align:left}td{padding:.7rem .75rem;border-bottom:1px solid #e0e5e7;vertical-align:middle}tbody tr:hover{background:#f7faf8}tbody tr.problem{box-shadow:inset 3px 0 #c47a16}td strong{display:block;font-size:.86rem}td small{display:block;margin-top:.18rem;color:#6b787f;font-size:.72rem}td .pending{color:#9a5f08}.sort-button{padding:0;border:0;background:transparent;color:inherit;font:inherit;font-weight:700;cursor:pointer}.sort-button:hover{color:#136b52}.expense-link{display:grid;gap:.2rem;padding:0;border:0;background:transparent;color:#17232d;text-align:left;cursor:pointer}.expense-link:hover strong{color:#136b52;text-decoration:underline}.expense-link span{color:#65737a;font-size:.75rem}.state-list{display:flex;flex-wrap:wrap;gap:.25rem}.state-badge{display:inline-flex;padding:.24rem .42rem;border-left:3px solid #136b52;background:#edf6f1;color:#24533f;font-size:.75rem;font-weight:700}.state-badge.warning{border-color:#c47a16;background:#fff7e7;color:#81540f}.state-badge.muted{border-color:#7a878e;background:#eff1f2;color:#5b676d}.amount{text-align:right;white-space:nowrap}.amount .sort-button{width:100%;text-align:right}.amount small{color:#9a5f08}.row-actions{display:flex;align-items:center;justify-content:flex-end;gap:.35rem;white-space:nowrap}.open-row{min-width:34px;height:34px;padding:0 .45rem;border:0;background:transparent;color:#136b52;font-size:1rem;font-weight:700;cursor:pointer}.restore-row{min-height:32px;padding:.35rem .55rem;border:1px solid #136b52;background:#fff;color:#136b52;font-size:.75rem;font-weight:700;cursor:pointer}.restore-row:disabled{opacity:.45;cursor:not-allowed}.empty{padding:3rem!important;color:#68757c;text-align:center}.sr-only{position:absolute;width:1px;height:1px;overflow:hidden;clip:rect(0,0,0,0)}
  .drawer-backdrop{position:fixed;inset:0;z-index:100;background:rgba(20,31,36,.42)}.material-drawer{position:absolute;top:0;right:0;bottom:0;width:min(1040px,calc(100vw - 224px));background:#f4f5f6;box-shadow:-18px 0 40px rgba(20,31,36,.18)}.material-drawer>header{display:flex;align-items:flex-start;justify-content:space-between;gap:1rem;padding:1rem 1.2rem;border-bottom:1px solid #cbd2d6;background:#fff}.material-drawer h2{margin:.2rem 0 0;font-size:1.2rem}.material-drawer header p{margin:.25rem 0 0;color:#65737a}.material-drawer header button{border:0;background:transparent;font-size:1.6rem;cursor:pointer}.material-body{display:grid;grid-template-columns:minmax(420px,60%) minmax(300px,40%);height:calc(100% - 88px)}.material-preview{min-width:0;overflow:hidden;border-right:1px solid #cbd2d6}.material-preview :global(.preview-shell){height:100%;min-height:0;border:0}.material-body form{display:grid;align-content:start;gap:.9rem;padding:1.25rem;background:#fff}.material-body h3{margin:0}.material-body form>p{margin:0;color:#65737a;line-height:1.5}.material-body label{display:grid;gap:.35rem}.material-body label span{color:#5d6a71;font-size:.78rem;font-weight:700}.material-body select{height:42px;padding:.5rem .6rem;border:1px solid #aeb9bf;background:#fff;font:inherit}.material-body footer{display:flex;justify-content:flex-end;gap:.6rem;margin-top:.4rem}.material-body footer button{padding:.6rem .8rem;font-weight:700;cursor:pointer}.ignore{border:1px solid #b3453e;background:#fff;color:#b3453e}.assign{border:1px solid #136b52;background:#136b52;color:#fff}.material-body button:disabled{opacity:.45;cursor:not-allowed}.drawer-error{padding:.6rem;border-left:3px solid #b3453e;background:#fff1f0;color:#862f2a}
  @media(max-width:1100px){.filters{grid-template-columns:1fr 1fr}.filters .search{grid-column:1/-1}.summary-counters{display:none}.material-body{grid-template-columns:1fr}.material-preview{display:none}.material-drawer{width:min(520px,calc(100vw - 224px))}}@media(max-width:820px){.material-drawer{width:100%}}@media(max-width:700px){.list-heading,.pending-banner{display:grid}.pending-files{flex-wrap:wrap}.filters{grid-template-columns:1fr}.filters .search{grid-column:auto}}
</style>
