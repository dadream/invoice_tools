<script module lang="ts">
  const selectedGroupByBatch = new Map<number, number>()
</script>

<script lang="ts">
  import { untrack } from 'svelte'
  import { describeError, invokeSafe } from '../../lib/ipc'
  import {
    displayGroupTitle, groupTransportEvidenceStatus, transportDocumentKindForInvoice,
    transportRouteForInvoice,
    type TransportEvidenceStatus,
  } from '../../lib/grouping'
  import type { ReviewQueueContext } from '../../lib/reviewQueue'
  import type { BatchGrouping, ExpenseItem, Invoice, InvoiceGroup, InvoiceGroupMember } from '../../lib/types'
  import { expenseCategoryLabel, formatAmount } from '../../lib/types'
  import ConfirmDialog from '../../lib/ConfirmDialog.svelte'

  interface Props {
    batchId: number
    grouping: BatchGrouping | null
    expenseItems: ExpenseItem[]
    invoices: Invoice[]
    groupingError: string | null
    canEdit: boolean
    onChanged: () => Promise<void>
    onOpenInvoice: (invoiceId: number, queue: ReviewQueueContext) => void
  }
  interface GroupingRecomputeResult {
    invoice_count: number
    group_count: number
    business_trip_count: number
    unresolved_transport_count: number
  }
  type Confirmation = { kind: 'merge'; sourceGroupId: number } | { kind: 'confirm-all' } | { kind: 'recompute' }

  interface UndoExclusion { invoiceId: number; label: string }
  type GroupExpenseSort = 'category' | 'date'

  const CATEGORY_SORT_ORDER: Record<string, number> = {
    rail: 0,
    flight: 1,
    hotel: 2,
    meal: 3,
    city_transport: 4,
    courier_logistics: 5,
    other: 6,
  }

  let { batchId, grouping, expenseItems, invoices, groupingError, canEdit, onChanged, onOpenInvoice }: Props = $props()
  let title = $state('')
  let newGroupKind = $state<'business_trip' | 'local_month'>('business_trip')
  let startDate = $state('')
  let endDate = $state('')
  let moveTargets = $state<Record<number, string>>({})
  let mergeTargets = $state<Record<number, string>>({})
  let expandedMoves = $state<Record<number, boolean>>({})
  let showGroupTools = $state(false)
  let working = $state<string | null>(null)
  let actionError = $state<string | null>(null)
  let actionNotice = $state<string | null>(null)
  let confirmation = $state<Confirmation | null>(null)
  let selectedGroupId = $state<number | null>(untrack(() => selectedGroupByBatch.get(batchId) ?? null))
  let undoExclusion = $state<UndoExclusion | null>(null)
  let groupingView = $state<'overview' | 'review'>('overview')
  let expenseSort = $state<GroupExpenseSort>('category')

  function ambiguities(raw: string): unknown[] | null {
    try { const value: unknown = JSON.parse(raw); return Array.isArray(value) ? value : null } catch { return null }
  }
  const ambiguityItems = $derived(grouping ? ambiguities(grouping.ambiguities_json) : [])
  const effectiveAmbiguityItems = $derived(ambiguityItems === null ? null : ambiguityItems.filter((item) => !ambiguityIsResolved(item)))
  const ambiguityTotal = $derived(effectiveAmbiguityItems?.length ?? (effectiveAmbiguityItems === null ? null : 0))
  const pendingGroupCount = $derived(grouping?.groups.filter((group) => group.requires_review).length ?? 0)
  const confirmedGroupCount = $derived((grouping?.groups.length ?? 0) - pendingGroupCount)
  const hasPendingReview = $derived(grouping !== null && (ambiguityTotal === null || ambiguityTotal > 0 || pendingGroupCount > 0))
  const problemGroupCount = $derived(grouping?.groups.filter((group) => groupStateLabel(group) === '需要处理').length ?? 0)
  const selectedGroup = $derived(grouping?.groups.find((group) => group.id === selectedGroupId) ?? grouping?.groups[0] ?? null)
  const selectedGroupIndex = $derived(selectedGroup ? grouping?.groups.findIndex((group) => group.id === selectedGroup.id) ?? -1 : -1)
  const previousGroup = $derived(selectedGroupIndex > 0 ? grouping?.groups[selectedGroupIndex - 1] ?? null : null)
  const nextGroup = $derived(selectedGroupIndex >= 0 && selectedGroupIndex < (grouping?.groups.length ?? 0) - 1 ? grouping?.groups[selectedGroupIndex + 1] ?? null : null)

  function expenseForInvoice(invoiceId: number): ExpenseItem | null {
    return expenseItems.find((expense) => expense.primary_invoice_id === invoiceId) ?? null
  }
  function invoiceForId(invoiceId: number): Invoice | null {
    return invoices.find((invoice) => invoice.id === invoiceId) ?? null
  }
  function expensesForGroup(group: InvoiceGroup): ExpenseItem[] {
    return membersForGroup(group).map((member) => expenseForInvoice(member.invoice_id)).filter((expense): expense is ExpenseItem => expense !== null)
  }
  function membersForGroup(group: InvoiceGroup): InvoiceGroupMember[] {
    return group.members.filter((member) => expenseForInvoice(member.invoice_id)?.inclusion_status === 'included').sort((left, right) => {
      const leftExpense = expenseForInvoice(left.invoice_id)
      const rightExpense = expenseForInvoice(right.invoice_id)
      if (expenseSort === 'category') {
        const categoryOrder = (CATEGORY_SORT_ORDER[leftExpense?.category_code ?? 'other'] ?? 99)
          - (CATEGORY_SORT_ORDER[rightExpense?.category_code ?? 'other'] ?? 99)
        if (categoryOrder !== 0) return categoryOrder
      }
      const dateOrder = (leftExpense?.transaction_date ?? '').localeCompare(rightExpense?.transaction_date ?? '')
      if (dateOrder !== 0) return dateOrder
      if (expenseSort === 'date') {
        const categoryOrder = (CATEGORY_SORT_ORDER[leftExpense?.category_code ?? 'other'] ?? 99)
          - (CATEGORY_SORT_ORDER[rightExpense?.category_code ?? 'other'] ?? 99)
        if (categoryOrder !== 0) return categoryOrder
      }
      return left.input_index - right.input_index
    })
  }
  function groupAmount(group: InvoiceGroup): string {
    return expensesForGroup(group).filter((expense) => expense.inclusion_status === 'included').reduce((sum, expense) => sum + Number(expense.gross_amount), 0).toFixed(2)
  }
  function groupCities(group: InvoiceGroup): string[] {
    return Array.from(new Set(expensesForGroup(group).map((expense) => expense.location.city_name).filter((city): city is string => Boolean(city))))
  }
  function semanticDestination(group: InvoiceGroup): string {
    return displayGroupTitle(group, expenseItems).replace(/出差$/, '').split('、').at(-1)
      ?? groupCities(group).at(-1) ?? '目的地待确认'
  }
  function semanticGroupTitle(group: InvoiceGroup): string {
    return displayGroupTitle(group, expenseItems)
  }
  function formatDatePart(value: string): string {
    const match = value.match(/^\d{4}-(\d{2})-(\d{2})/)
    return match ? `${Number(match[1])}月${Number(match[2])}日` : value
  }
  function formatDateRange(group: InvoiceGroup): string {
    if (group.start_date === group.end_date) return formatDatePart(group.start_date)
    const start = group.start_date.match(/^(\d{4})-(\d{2})-(\d{2})/)
    const end = group.end_date.match(/^(\d{4})-(\d{2})-(\d{2})/)
    if (!start || !end) return `${group.start_date}–${group.end_date}`
    return start[1] === end[1] && start[2] === end[2]
      ? `${Number(start[2])}月${Number(start[3])}日–${Number(end[3])}日`
      : `${Number(start[2])}月${Number(start[3])}日–${Number(end[2])}月${Number(end[3])}日`
  }
  function transportExpenses(group: InvoiceGroup): ExpenseItem[] {
    return expensesForGroup(group).filter((expense) =>
      (expense.category_code === 'rail' || expense.category_code === 'flight')
      && !isTransportAdjustment(group, expense),
    ).sort((left, right) =>
      (invoiceForId(left.primary_invoice_id)?.departure_time ?? left.transaction_date).localeCompare(invoiceForId(right.primary_invoice_id)?.departure_time ?? right.transaction_date),
    )
  }
  function isTransportAdjustment(group: InvoiceGroup, expense: ExpenseItem): boolean {
    const kind = transportDocumentKindForInvoice(group, expense.primary_invoice_id)
    return kind === 'refund' || kind === 'change'
  }
  function transportPlace(expense: ExpenseItem): string {
    return expense.location.city_name || invoiceForId(expense.primary_invoice_id)?.city || '地点待确认'
  }
  function routePoints(group: InvoiceGroup): string[] {
    const points = transportExpenses(group).map(transportPlace)
    const unique = points.filter((point, index) => index === 0 || point !== points[index - 1])
    if (unique.length > 1 && unique[0] !== unique.at(-1)) unique.push(unique[0])
    if (unique.length === 1 && semanticDestination(group) !== unique[0]) unique.push(semanticDestination(group))
    return unique
  }
  function routeLabel(group: InvoiceGroup): string {
    const routes = transportExpenses(group)
      .map((expense) => transportRouteForInvoice(group, expense.primary_invoice_id))
      .filter((route): route is string => Boolean(route))
    if (routes.length > 0) return routes.join('；')
    const points = routePoints(group)
    if (points.length > 1) return points.join(' → ')
    const cities = groupCities(group)
    return cities.length > 0 ? cities.join(' → ') : '地点待核对'
  }
  function departureTime(expense: ExpenseItem): string {
    return invoiceForId(expense.primary_invoice_id)?.departure_time?.match(/[T ](\d{2}:\d{2})/)?.[1] ?? ''
  }
  function groupAnchors(group: InvoiceGroup): ExpenseItem[] {
    return expensesForGroup(group).filter((expense) =>
      ((expense.category_code === 'rail' || expense.category_code === 'flight') && !isTransportAdjustment(group, expense))
      || expense.documents.some((document) => document.role === 'itinerary'),
    )
  }
  function transportEvidenceStatus(group: InvoiceGroup): TransportEvidenceStatus {
    return groupTransportEvidenceStatus(group, expenseItems)
  }
  function ambiguityRecord(value: unknown): Record<string, unknown> | null {
    return value !== null && typeof value === 'object' && !Array.isArray(value) ? value as Record<string, unknown> : null
  }
  function ambiguityKind(value: unknown): string {
    const kind = ambiguityRecord(value)?.kind
    return typeof kind === 'string' ? kind : ''
  }
  function ambiguityDescription(value: unknown): string {
    const record = ambiguityRecord(value); return typeof record?.description === 'string' ? record.description : '归组依据需要人工核对'
  }
  function ambiguityCandidates(value: unknown): string[] {
    const candidates = ambiguityRecord(value)?.candidates
    return Array.isArray(candidates) ? candidates.filter((item): item is string => typeof item === 'string') : []
  }
  function ambiguityInvoices(value: unknown): string {
    const indexes = ambiguityRecord(value)?.involved_invoice_ids
    if (!Array.isArray(indexes)) return ''
    const labels = indexes.filter((index): index is number => Number.isInteger(index)).map((index) => grouping?.groups.flatMap((group) => group.members).find((member) => member.input_index === index)?.invoice_number).filter((number): number is string => Boolean(number))
    return labels.length > 0 ? `涉及票据：${labels.join('、')}` : ''
  }
  function ambiguityBelongsToGroup(value: unknown, group: InvoiceGroup): boolean {
    const indexes = ambiguityRecord(value)?.involved_invoice_ids
    if (!Array.isArray(indexes) || indexes.length === 0) return false
    const memberIndexes = new Set(group.members.map((member) => member.input_index))
    return indexes.every((index) => typeof index === 'number' && memberIndexes.has(index))
  }
  function ambiguityIsResolved(value: unknown): boolean {
    if (ambiguityKind(value) !== 'MissingTransportEvidence') return false
    const group = grouping?.groups.find((candidate) => ambiguityBelongsToGroup(value, candidate))
    return group ? transportEvidenceStatus(group) === 'company_paid' || transportEvidenceStatus(group) === 'not_required' : false
  }
  function ambiguitiesForGroup(group: InvoiceGroup): unknown[] {
    return effectiveAmbiguityItems?.filter((item) => ambiguityBelongsToGroup(item, group)) ?? []
  }
  function groupHasAmbiguity(group: InvoiceGroup): boolean {
    if (effectiveAmbiguityItems === null) return true
    return ambiguitiesForGroup(group).length > 0
  }
  function groupKindLabel(kind: string): string {
    return { business_trip: '差旅行程', local_month: '市内消费', courier_month: '快递物流', needs_review: '待归组', manual: '人工归组', excluded: '未计入' }[kind] ?? kind
  }
  function groupStateLabel(group: InvoiceGroup): string {
    if (!group.requires_review) return '已确认'
    const transportUndecided = group.kind === 'business_trip'
      && groupAnchors(group).length === 0
      && transportEvidenceStatus(group) === 'missing'
    if (groupHasAmbiguity(group) || transportUndecided) return '需要处理'
    if (group.evidence_json.includes('manual_')) return '已调整'
    return '系统建议'
  }
  function groupStateTone(group: InvoiceGroup): string {
    return { '已确认': 'confirmed', '需要处理': 'warning', '已调整': 'adjusted', '系统建议': 'suggested' }[groupStateLabel(group)] ?? 'suggested'
  }
  function expenseTitle(expense: ExpenseItem, group: InvoiceGroup): string {
    const category = expenseCategoryLabel(expense)
    if (expense.category_code === 'rail' || expense.category_code === 'flight') {
      const documentKind = transportDocumentKindForInvoice(group, expense.primary_invoice_id)
      if (documentKind === 'refund') return `${category}退票费｜${transportPlace(expense)}`
      if (documentKind === 'change') return `${category}改签费｜${transportPlace(expense)}`
      const route = transportRouteForInvoice(group, expense.primary_invoice_id)
      if (route) return `${category}｜${route}`
      const index = transportExpenses(group).findIndex((item) => item.id === expense.id)
      return `${category}｜${transportPlace(expense)} → ${routePoints(group)[index + 1] ?? semanticDestination(group)}`
    }
    return `${category}${expense.counterparty_name.trim() ? `｜${expense.counterparty_name.trim()}` : ''}`
  }
  function humanMatchReason(expense: ExpenseItem, group: InvoiceGroup): string {
    const documentKind = transportDocumentKindForInvoice(group, expense.primary_invoice_id)
    if (documentKind === 'refund') return '退票费已按原路线和发生日期挂到所属出差，不作为实际行程节点。'
    if (documentKind === 'change') return '改签费已挂到所属出差，不作为独立行程节点。'
    if (expense.category_code === 'rail' || expense.category_code === 'flight') return '按有效交通票的日期和路线作为本行程锚点。'
    if (group.kind === 'business_trip') return '费用日期落在本次行程范围内；请确认它确实属于本次出差。'
    if (group.kind === 'local_month') return '未找到与差旅行程匹配的可靠依据，暂按月份归入市内消费。'
    if (group.kind === 'courier_month') return '快递/物流费用按实际发生月份独立归组，不参与差旅行程匹配。'
    return '当前归属来自系统建议或人工调整。'
  }
  function canConfirmGroup(group: InvoiceGroup): boolean {
    return group.requires_review && (
      group.kind !== 'business_trip'
      || groupAnchors(group).length > 0
      || transportEvidenceStatus(group) === 'company_paid'
      || transportEvidenceStatus(group) === 'not_required'
    )
  }
  function openGroupedInvoice(invoiceId: number) {
    const group = selectedGroup
    onOpenInvoice(invoiceId, {
      invoiceIds: group ? membersForGroup(group).map((member) => member.invoice_id) : [invoiceId],
      label: group ? semanticGroupTitle(group) : '归组费用',
    })
  }

  function selectGroup(groupId: number) {
    selectedGroupId = groupId
    expandedMoves = {}
  }

  function openGroup(groupId: number) {
    selectGroup(groupId)
    groupingView = 'review'
  }

  function returnToOverview() {
    groupingView = 'overview'
    showGroupTools = false
  }

  async function run(command: string, args: Record<string, unknown>, key: string) {
    working = key; actionError = null; actionNotice = null; undoExclusion = null
    const result = await invokeSafe<unknown>(command, args); working = null
    if (!result.ok) { actionError = describeError(result.error); return false }
    await onChanged(); return true
  }
  async function recomputeGrouping() {
    if (working !== null) return
    confirmation = null
    working = 'recompute'; actionError = null; actionNotice = null
    const result = await invokeSafe<GroupingRecomputeResult>('recompute_batch_grouping', { batchId }); working = null
    if (!result.ok) { actionError = describeError(result.error); return }
    const unresolved = result.data.unresolved_transport_count > 0 ? `；${result.data.unresolved_transport_count} 条交通费用需要人工确定路线` : ''
    actionNotice = `已重新分析 ${result.data.invoice_count} 笔费用，形成 ${result.data.group_count} 个归组（差旅行程 ${result.data.business_trip_count} 组）${unresolved}。`
    await onChanged()
  }
  async function setTransportEvidence(groupId: number, status: Exclude<TransportEvidenceStatus, 'present'>) {
    if (!canEdit || working !== null) return
    if (await run('set_group_transport_evidence', { batchId, groupId, status }, `transport-${groupId}`)) {
      actionNotice = status === 'company_paid'
        ? '已记录交通由公司统一购买，本组无需个人交通票即可确认。'
        : status === 'not_required'
          ? '已记录本组无需个人交通凭证。'
          : '已恢复为交通情况待确认。'
    }
  }
  async function createGroup(event: SubmitEvent) {
    event.preventDefault()
    if (await run('create_manual_group', { batchId, kind: newGroupKind, title, startDate, endDate }, 'create')) { title = ''; startDate = ''; endDate = '' }
  }
  async function moveInvoice(invoiceId: number) {
    const target = Number(moveTargets[invoiceId]); if (!Number.isInteger(target)) return
    if (await run('move_invoice_group', { batchId, invoiceId, targetGroupId: target }, `move-${invoiceId}`)) {
      expandedMoves[invoiceId] = false
      moveTargets[invoiceId] = ''
    }
  }
  async function mergeGroup(sourceGroupId: number) {
    const target = Number(mergeTargets[sourceGroupId]); if (!Number.isInteger(target)) return
    confirmation = null
    if (await run('merge_groups', { batchId, sourceGroupId, targetGroupId: target }, `merge-${sourceGroupId}`)) { selectGroup(target); showGroupTools = false }
  }
  async function confirmSelectedGroup(groupId: number) {
    const available = grouping?.groups ?? []
    const index = available.findIndex((group) => group.id === groupId)
    const next = [...available.slice(index + 1), ...available.slice(0, index)].find((group) => group.requires_review && group.id !== groupId)
    if (await run('confirm_invoice_group', { batchId, groupId }, `confirm-group-${groupId}`)) {
      if (next) openGroup(next.id)
      else returnToOverview()
      actionNotice = next ? '本组已确认，已进入下一条待审核归组。' : '所有归组均已逐组确认。'
    }
  }
  async function confirmGrouping() { confirmation = null; await run('confirm_grouping', { batchId }, 'confirm-all') }

  async function excludeInvoice(invoiceId: number) {
    if (!canEdit || working !== null) return
    const expense = expenseForInvoice(invoiceId)
    working = `exclude-${invoiceId}`
    actionError = null
    actionNotice = null
    const result = await invokeSafe<void>('set_invoice_excluded', { invoiceId, excluded: true })
    working = null
    if (!result.ok) { actionError = describeError(result.error); return }
    undoExclusion = { invoiceId, label: expense?.counterparty_name.trim() || invoiceForId(invoiceId)?.invoice_number || `费用 #${invoiceId}` }
    await onChanged()
  }

  async function restoreExcludedInvoice() {
    const pending = undoExclusion
    if (!pending || working !== null) return
    working = `restore-${pending.invoiceId}`
    actionError = null
    const result = await invokeSafe<void>('set_invoice_excluded', { invoiceId: pending.invoiceId, excluded: false })
    working = null
    if (!result.ok) { actionError = describeError(result.error); return }
    undoExclusion = null
    actionNotice = '费用已恢复计入，并回到原归组。'
    await onChanged()
  }

  $effect(() => {
    const available = grouping?.groups ?? []
    if (available.length === 0) selectedGroupId = null
    else if (selectedGroupId === null || !available.some((group) => group.id === selectedGroupId)) selectedGroupId = available.find((group) => group.requires_review)?.id ?? available[0].id
    showGroupTools = false
  })
  $effect(() => {
    if (selectedGroupId !== null) selectedGroupByBatch.set(batchId, selectedGroupId)
  })
</script>

<section class="info-section grouping-section">
  {#if groupingError}
    <p class="error page-message" role="alert">{groupingError}</p>
  {:else if grouping}
    <div class="group-toolbar">
      <div class="group-toolbar-summary"><strong>归组</strong><span>{grouping.groups.length} 组</span><i class="confirmed">已确认 {confirmedGroupCount}</i><i class="pending">待审核 {pendingGroupCount}</i>{#if problemGroupCount > 0}<i class="problem">需要处理 {problemGroupCount}</i>{/if}</div>
      {#if canEdit}<button class="recompute-button" type="button" onclick={() => (confirmation = { kind: 'recompute' })} disabled={working !== null}>{working === 'recompute' ? '正在生成…' : '重新生成归组'}</button>{/if}
    </div>
    {#if effectiveAmbiguityItems === null}
      <aside class="invalid page-message"><strong>归组待确认数据无法读取</strong><p>请重新分析归组；在数据恢复前不要完成审核。</p></aside>
    {/if}
    {#if actionError}<p class="error action-error" role="alert">{actionError}</p>{/if}
    {#if actionNotice}<p class="action-notice" role="status">{actionNotice}</p>{/if}

    {#if grouping.groups.length === 0}
      <p class="empty page-message">没有形成可用归组，请在下方新建人工归组。</p>
    {:else if groupingView === 'overview'}
      <section class="group-overview" aria-label="归组总览">
        <header><div><strong>归组清单</strong><span>优先处理“需要处理”和“系统建议”的归组</span></div><b>{grouping.groups.length} 组</b></header>
        <div class="overview-table" role="table" aria-label="归组清单">
          <div class="overview-table-head" role="row"><span role="columnheader">归组名称</span><span role="columnheader">日期与路线</span><span role="columnheader">费用</span><span role="columnheader">计入金额</span><span role="columnheader">状态</span><span role="columnheader">操作</span></div>
          {#each grouping.groups as group (group.id)}
            <div class="overview-row" role="row">
              <div class="overview-name" role="cell"><button type="button" aria-label={`${group.requires_review ? '审核' : '查看'}归组 ${semanticGroupTitle(group)}`} onclick={() => openGroup(group.id)}>{semanticGroupTitle(group)}</button><span>{groupKindLabel(group.kind)}{#if ambiguitiesForGroup(group).length > 0}<em>{ambiguitiesForGroup(group).length} 个问题</em>{/if}</span></div>
              <div class="overview-route" role="cell"><strong>{formatDateRange(group)}</strong><span>{group.kind === 'business_trip' ? routeLabel(group) : semanticGroupTitle(group)}</span></div>
              <span role="cell">{membersForGroup(group).length} 项</span>
              <div class="overview-amount" role="cell"><strong>{formatAmount(groupAmount(group))}</strong></div>
              <span role="cell"><span class={`state-pill ${groupStateTone(group)}`}>{groupStateLabel(group)}</span></span>
              <button class="review-group" type="button" onclick={() => openGroup(group.id)}>{group.requires_review ? '审核归组' : '查看归组'}</button>
            </div>
          {/each}
        </div>
      </section>
    {:else if selectedGroup}
      <section class="group-review-page" aria-label={`审核归组 ${semanticGroupTitle(selectedGroup)}`}>
        <nav class="review-navigation" aria-label="归组审核导航">
          <button class="back-overview" type="button" onclick={returnToOverview}>← 返回归组列表</button>
          <span>第 {selectedGroupIndex + 1} / {grouping.groups.length} 组</span>
          <div><button type="button" onclick={() => previousGroup && openGroup(previousGroup.id)} disabled={!previousGroup}>上一组</button><button type="button" onclick={() => nextGroup && openGroup(nextGroup.id)} disabled={!nextGroup}>下一组</button></div>
        </nav>
          <article class="group-card">
            <header class="group-header"><div><span class="eyebrow">{groupKindLabel(selectedGroup.kind)}</span><h4>{semanticGroupTitle(selectedGroup)}</h4><p>{formatDateRange(selectedGroup)} · {membersForGroup(selectedGroup).length} 项费用 · {formatAmount(groupAmount(selectedGroup))}</p></div><span class={`state-pill large ${groupStateTone(selectedGroup)}`}>{groupStateLabel(selectedGroup)}</span></header>

            {#if selectedGroup.kind === 'business_trip'}
              <details class:missing={groupAnchors(selectedGroup).length === 0} class="route-panel">
                <summary><span>行程路线</span><strong>{routeLabel(selectedGroup)}</strong><span class="route-toggle">展开明细</span></summary>
                {#if transportExpenses(selectedGroup).length > 0}
                  <ol>{#each transportExpenses(selectedGroup) as expense, index (expense.id)}{@const points = routePoints(selectedGroup)}<li><span class="route-dot"></span><div><strong>{transportRouteForInvoice(selectedGroup, expense.primary_invoice_id) ?? `${transportPlace(expense)} → ${points[index + 1] ?? semanticDestination(selectedGroup)}`}</strong><small>{formatDatePart(expense.transaction_date)}{departureTime(expense) ? ` ${departureTime(expense)}` : ''} · {expenseCategoryLabel(expense)} · {formatAmount(expense.gross_amount, expense.currency_code)}</small></div></li>{/each}</ol>
                {:else if transportEvidenceStatus(selectedGroup) === 'company_paid'}<p>交通由公司统一购买，本软件不要求用户补充个人交通票。</p>
                {:else if transportEvidenceStatus(selectedGroup) === 'not_required'}<p>本组已确认无需个人交通凭证。</p>
                {:else if groupAnchors(selectedGroup).length > 0}<p>已关联行程材料，请结合住宿和当地费用核对本次出差。</p>
                {:else}<p>系统依据异地住宿建立了出差候选，请确认交通凭证情况。</p>{/if}
              </details>
              {#if groupAnchors(selectedGroup).length === 0}
                <section class="transport-decision" aria-label="交通凭证情况">
                  <div><span>交通凭证情况</span><strong>{transportEvidenceStatus(selectedGroup) === 'company_paid' ? '公司统一购买' : transportEvidenceStatus(selectedGroup) === 'not_required' ? '无需个人提供' : '待确认'}</strong><small>不创建虚构交通费用，只记录本次出差为何没有个人交通票。</small></div>
                  {#if canEdit}<div class="decision-actions"><button class:active={transportEvidenceStatus(selectedGroup) === 'company_paid'} type="button" onclick={() => void setTransportEvidence(selectedGroup.id, 'company_paid')} disabled={working !== null}>公司统一购买</button><button class:active={transportEvidenceStatus(selectedGroup) === 'not_required'} type="button" onclick={() => void setTransportEvidence(selectedGroup.id, 'not_required')} disabled={working !== null}>无需个人凭证</button>{#if transportEvidenceStatus(selectedGroup) !== 'missing'}<button type="button" onclick={() => void setTransportEvidence(selectedGroup.id, 'missing')} disabled={working !== null}>改为待确认</button>{/if}</div>{/if}
                </section>
              {/if}
            {/if}

            {#if ambiguitiesForGroup(selectedGroup).length > 0 && selectedGroup.requires_review}
              <section class="group-issues" aria-label="本组需要处理的问题">
                <header><strong>本组需要处理</strong><span>{ambiguitiesForGroup(selectedGroup).length} 项</span></header>
                <ul>{#each ambiguitiesForGroup(selectedGroup) as ambiguity}<li><strong>{ambiguityDescription(ambiguity)}</strong>{#if ambiguityInvoices(ambiguity)}<small>{ambiguityInvoices(ambiguity)}</small>{/if}{#if ambiguityCandidates(ambiguity).length > 0}<span>核对方向：{ambiguityCandidates(ambiguity).join(' / ')}</span>{/if}</li>{/each}</ul>
              </section>
            {/if}

            <section class="member-section" aria-label="本组费用">
              <header><div><strong>本组费用</strong><span>{expenseSort === 'category' ? '同类费用内按发生日期排序' : '同日费用按类型排序'}</span></div><div class="sort-switch" role="group" aria-label="本组费用排序方式"><span>排序</span><button class:active={expenseSort === 'category'} type="button" aria-pressed={expenseSort === 'category'} onclick={() => (expenseSort = 'category')}>按类型</button><button class:active={expenseSort === 'date'} type="button" aria-pressed={expenseSort === 'date'} onclick={() => (expenseSort = 'date')}>按日期</button></div></header>
              {#if membersForGroup(selectedGroup).length === 0}
                <p class="empty-member">暂无费用。可在“更多操作”中把空组并入其他归组。</p>
              {:else}
                <ul>
                  {#each membersForGroup(selectedGroup) as member (member.invoice_id)}
                    {@const expense = expenseForInvoice(member.invoice_id)}
                    <li class="expense-card">
                      {#if expense}
                        <div class="expense-main"><div class="expense-title"><span class={`category-dot category-${expense.category_code}`}></span><strong>{expenseTitle(expense, selectedGroup)}</strong></div><div class="expense-meta"><span>{formatDatePart(expense.transaction_date)}</span><b>{formatAmount(expense.gross_amount, expense.currency_code)}</b><span>{Math.max(1, expense.documents.length)} 份材料</span></div><div class="expense-secondary"><p title={humanMatchReason(expense, selectedGroup)}>{humanMatchReason(expense, selectedGroup)}</p><details class="evidence"><summary>票据与依据</summary><div><span>发票号码：{member.invoice_number}</span><span>系统记录：{member.match_reason}</span></div></details></div></div>
                        <div class="expense-action">
                          {#if canEdit && grouping.groups.length > 1 && expandedMoves[member.invoice_id]}
                            <div class="move-controls" role="dialog" aria-label="调整费用归组">
                              <label><span>目标归组</span><select aria-label={`选择发票 ${member.invoice_number} 的目标归组`} value={moveTargets[member.invoice_id] ?? ''} onchange={(event) => (moveTargets[member.invoice_id] = event.currentTarget.value)}><option value="">请选择目标归组</option>{#each grouping.groups.filter((candidate) => candidate.id !== selectedGroup.id) as candidate}<option value={String(candidate.id)}>{semanticGroupTitle(candidate)} · {formatDateRange(candidate)}</option>{/each}</select></label>
                              <div><button class="secondary" type="button" onclick={() => { expandedMoves[member.invoice_id] = false; moveTargets[member.invoice_id] = '' }}>取消</button><button class="primary" type="button" onclick={() => void moveInvoice(member.invoice_id)} disabled={!moveTargets[member.invoice_id] || working !== null}>{working === `move-${member.invoice_id}` ? '移动中…' : '确认移动'}</button></div>
                            </div>
                          {/if}
                          <div class="expense-action-bar">
                            <button class="card-action" type="button" onclick={() => openGroupedInvoice(member.invoice_id)}>查看发票</button>
                            {#if canEdit && grouping.groups.length > 1}<button class:active={expandedMoves[member.invoice_id]} class="card-action" type="button" onclick={() => (expandedMoves[member.invoice_id] = !expandedMoves[member.invoice_id])}>{expandedMoves[member.invoice_id] ? '收起调整' : '调整归组'}</button>{/if}
                            {#if canEdit}<span class="danger-divider" aria-hidden="true"></span><button class="exclude-action" type="button" onclick={() => void excludeInvoice(member.invoice_id)} disabled={working !== null}>{working === `exclude-${member.invoice_id}` ? '处理中…' : '移至未计入'}</button>{/if}
                          </div>
                        </div>
                      {:else}<div class="expense-main"><strong>费用数据缺失</strong><p>发票 {member.invoice_number} 没有对应费用记录，请返回费用清单检查。</p></div>{/if}
                    </li>
                  {/each}
                </ul>
              {/if}
            </section>

            {#if undoExclusion}<div class="undo-notice" role="status"><span>“{undoExclusion.label}”已移至未计入清单。</span><button type="button" onclick={() => void restoreExcludedInvoice()} disabled={working !== null}>{working === `restore-${undoExclusion.invoiceId}` ? '恢复中…' : '撤销'}</button></div>{/if}

            <footer class="group-footer">
              {#if canEdit && grouping.groups.length > 1}<div class="more-tools"><button class="secondary" type="button" onclick={() => (showGroupTools = !showGroupTools)}>{showGroupTools ? '收起更多操作' : '更多操作'}</button>{#if showGroupTools}<div class="merge-controls"><select aria-label={`选择归组 ${selectedGroup.title} 的合并目标`} value={mergeTargets[selectedGroup.id] ?? ''} onchange={(event) => (mergeTargets[selectedGroup.id] = event.currentTarget.value)}><option value="">将本组合并到…</option>{#each grouping.groups.filter((candidate) => candidate.id !== selectedGroup.id) as candidate}<option value={String(candidate.id)}>{semanticGroupTitle(candidate)} · {formatDateRange(candidate)}</option>{/each}</select><button type="button" onclick={() => (confirmation = { kind: 'merge', sourceGroupId: selectedGroup.id })} disabled={!mergeTargets[selectedGroup.id] || working !== null}>合并本组</button></div>{/if}</div>{/if}
              {#if canEdit && selectedGroup.requires_review}<button class="primary" type="button" onclick={() => void confirmSelectedGroup(selectedGroup.id)} disabled={!canConfirmGroup(selectedGroup) || working !== null}>{working === `confirm-group-${selectedGroup.id}` ? '确认中…' : '确认本组并继续'}</button>{:else if !selectedGroup.requires_review}<span class="confirmed-copy">✓ 本组已确认</span>{/if}
            </footer>
          </article>
      </section>
    {/if}

    {#if canEdit && pendingGroupCount === 0 && hasPendingReview}<div class="final-confirm"><div><strong>所有归组均已逐组确认</strong><span>还有批次级判断需要接受后才能完成审核。</span></div><button class="primary" type="button" onclick={() => (confirmation = { kind: 'confirm-all' })} disabled={working !== null}>完成归组审核</button></div>{/if}
  {:else}<p class="empty page-message">该批次没有归组结果。请重新分析或新建人工归组。</p>{/if}

  {#if canEdit}<details class="create-group-panel"><summary>新建人工归组</summary><form class="create-group" onsubmit={createGroup}><p>差旅行程至少需要一张铁路/航空票，或带已挂载行程单的费用。</p><select bind:value={newGroupKind} aria-label="归组类型"><option value="business_trip">差旅行程</option><option value="local_month">市内/非差旅集合</option></select><input bind:value={title} required maxlength="100" placeholder="归组名称" aria-label="归组名称" /><input type="date" bind:value={startDate} required aria-label="开始日期" /><input type="date" bind:value={endDate} required aria-label="结束日期" /><button class="primary" type="submit" disabled={working !== null}>{working === 'create' ? '新建中…' : '新建归组'}</button></form></details>{/if}
</section>

{#if confirmation?.kind === 'merge'}
  <ConfirmDialog title="合并两个归组" message="本组会被删除，其中的费用和材料会移动到所选目标归组；计入金额不会改变。" confirmLabel="确认合并" busy={working !== null} onConfirm={() => confirmation?.kind === 'merge' && void mergeGroup(confirmation.sourceGroupId)} onCancel={() => (confirmation = null)} />
{:else if confirmation?.kind === 'confirm-all'}
  <ConfirmDialog title="完成归组审核" message="所有归组已逐组核对。此操作会接受剩余批次级判断，并完成归组审核。" confirmLabel="完成归组" busy={working !== null} onConfirm={() => void confirmGrouping()} onCancel={() => (confirmation = null)} />
{:else if confirmation?.kind === 'recompute'}
  <ConfirmDialog title="重新生成归组" message="将按当前计入费用重新生成归组，可能替换尚未确认的人工移动结果；费用字段和计入金额不会改变。" confirmLabel="重新生成" busy={working !== null} onConfirm={() => void recomputeGrouping()} onCancel={() => (confirmation = null)} />
{/if}

<style>
  .info-section{margin-bottom:2rem}.grouping-section{border:1px solid #cbd2d6;background:#fff;color:#17232d}.eyebrow{color:#136b52;font-size:.72rem;font-weight:750;letter-spacing:.08em;text-transform:uppercase}h4{margin:0;color:#14222c;font-size:1.35rem}.group-header p{margin:.35rem 0 0;color:#64748b;font-size:.86rem;line-height:1.5}button,select,input{font:inherit}button{min-height:38px;padding:.48rem .72rem;border:1px solid #657068;background:#fff;color:#344139;cursor:pointer}button:disabled{opacity:.48;cursor:not-allowed}button.primary{border-color:#136b52;background:#136b52;color:#fff;font-weight:700}button.secondary{border-color:#9ba8a1;color:#315043}select,input{min-width:0;min-height:38px;padding:.45rem .55rem;border:1px solid #aeb6af;background:#fff}
  .group-toolbar{position:relative;z-index:5;display:flex;align-items:center;justify-content:space-between;gap:1rem;min-height:54px;padding:.55rem 1.3rem;border-bottom:1px solid #d7dde0;background:#f7f9f8}.group-toolbar-summary{display:flex;flex-wrap:wrap;align-items:center;gap:.45rem}.group-toolbar-summary>strong{font-size:1rem}.group-toolbar-summary>span{margin-right:.25rem;color:#64748b;font-size:.8rem}.group-toolbar-summary i{padding:.24rem .48rem;border-radius:999px;font-size:.7rem;font-style:normal;font-weight:700}.group-toolbar-summary .confirmed{background:#e3f2e9;color:#17613f}.group-toolbar-summary .pending{background:#e9eef8;color:#375a8c}.group-toolbar-summary .problem{background:#fff0d5;color:#865610}.recompute-button{min-height:34px;padding:.38rem .62rem;border-color:#9ba8a1;color:#315043;font-size:.8rem;font-weight:700}.page-message{margin:0;padding:.8rem 1.3rem;border-bottom:1px solid #ead8b6}.page-message p{margin:.25rem 0 0}.invalid{border-left:4px solid #b3453e;background:#f8e9e7;color:#862f2a}
  .state-pill{display:inline-flex;align-items:center;justify-content:center;min-width:62px;padding:.24rem .42rem;border-radius:999px;font-size:.69rem;font-weight:750;white-space:nowrap}.state-pill.confirmed{background:#e3f2e9;color:#17613f}.state-pill.warning{background:#fff0d5;color:#865610}.state-pill.suggested{background:#e9eef8;color:#375a8c}.state-pill.adjusted{background:#eee8f8;color:#654c8e}.state-pill.large{min-width:76px;padding:.35rem .6rem;font-size:.76rem}
  .group-overview{overflow:visible;background:#fff}.group-overview>header{display:flex;align-items:center;justify-content:space-between;gap:1rem;padding:1rem 1.3rem;border-bottom:1px solid #cbd2d6}.group-overview>header>div{display:grid;gap:.2rem}.group-overview>header span{color:#65737a;font-size:.78rem}.group-overview>header b{font:1.15rem 'IBM Plex Mono',monospace;color:#136b52}.overview-table{display:grid;overflow:visible}.overview-table-head,.overview-row{display:grid;grid-template-columns:minmax(210px,1.35fr) minmax(220px,1.5fr) 90px 120px 105px 100px;gap:1rem;align-items:center;padding:.75rem 1.3rem}.overview-table-head{border-bottom:1px solid #cbd2d6;background:#f3f5f5;color:#637078;font-size:.72rem;font-weight:700}.overview-row{min-height:76px;border-bottom:1px solid #e1e5e7}.overview-row:hover{background:#fafbf9}.overview-name,.overview-route{display:grid;gap:.2rem;min-width:0}.overview-name>button{min-height:0;width:fit-content;padding:0;border:0;background:transparent;color:#136b52;font-weight:750;text-align:left;text-decoration:underline;text-decoration-color:transparent;text-underline-offset:3px;overflow-wrap:anywhere}.overview-name>button:hover,.overview-name>button:focus-visible{text-decoration-color:currentColor}.overview-name span,.overview-route span{color:#65737a;font-size:.76rem}.overview-name em{margin-left:.45rem;padding:.16rem .35rem;background:#fff0d5;color:#865610;font-size:.67rem;font-style:normal;font-weight:700}.overview-route strong{font-size:.82rem}.overview-amount{font-family:'IBM Plex Mono',monospace;white-space:nowrap}.review-group{border-color:#136b52;color:#136b52;font-weight:700}.group-review-page{display:block;min-height:0;overflow:visible;background:#fff}.review-navigation{display:grid;grid-template-columns:1fr auto 1fr;gap:1rem;align-items:center;padding:.7rem 1.15rem;border-bottom:1px solid #cbd2d6;background:#f5f7f7}.review-navigation>span{color:#65737a;font-size:.78rem;font-weight:700}.review-navigation>div{display:flex;justify-content:flex-end;gap:.4rem}.review-navigation button{min-height:34px;padding:.38rem .6rem}.back-overview{width:max-content;border:0;background:transparent;color:#136b52;font-weight:700}
  .group-card{display:flex;min-width:0;min-height:0;flex-direction:column;overflow:visible;padding:1rem 1.3rem}.group-header{display:flex;flex:none;justify-content:space-between;align-items:flex-start;gap:1rem}.route-panel{flex:none;margin-top:.7rem;padding:.65rem .8rem;border-left:4px solid #136b52;background:#eef7f2}.route-panel.missing{border-left-color:#c47a16;background:#fff7e8}.route-panel>summary{display:grid;grid-template-columns:auto minmax(0,1fr) auto;gap:.7rem;align-items:center;cursor:pointer;list-style:none}.route-panel>summary::-webkit-details-marker{display:none}.route-panel>summary span:first-child{color:#5d6f65;font-size:.72rem;white-space:nowrap}.route-panel>summary strong{font-size:.92rem;overflow-wrap:anywhere}.route-toggle{color:#136b52;font-size:.72rem;font-weight:700;white-space:nowrap}.route-panel[open] .route-toggle{font-size:0}.route-panel[open] .route-toggle::after{content:'收起明细';font-size:.72rem}.route-panel ol{display:grid;gap:.55rem;margin:.65rem 0 0;padding:.6rem 0 0;border-top:1px solid #cfe0d7;list-style:none}.route-panel li{display:grid;grid-template-columns:12px minmax(0,1fr);gap:.6rem}.route-panel li>div{display:grid;gap:.14rem}.route-panel li small{color:#52645a}.route-panel>p{margin:.65rem 0 0;padding-top:.6rem;border-top:1px solid #cfe0d7;color:#52645a;font-size:.8rem}.route-dot{width:9px;height:9px;margin-top:.26rem;border:2px solid #136b52;border-radius:50%;background:#fff}.group-issues{flex:none;margin-top:.7rem;border:1px solid #e2c58f;background:#fffaf0}.group-issues>header{display:flex;justify-content:space-between;gap:1rem;padding:.55rem .75rem;border-bottom:1px solid #ead8b6;color:#765114}.group-issues>header span{font-size:.75rem}.group-issues ul{display:grid;margin:0;padding:0 .75rem;list-style:none}.group-issues li{display:grid;gap:.18rem;padding:.55rem 0;border-top:1px solid #eee1c8}.group-issues li:first-child{border-top:0}.group-issues small,.group-issues li span{color:#76571e;font-size:.74rem}
  .member-section{display:block;min-height:0;flex:none;margin-top:.7rem;overflow:visible;border:1px solid #d3dade}.member-section>header{display:flex;justify-content:space-between;gap:1rem;padding:.6rem .9rem;border-bottom:1px solid #dbe1e4;background:#f5f7f7}.member-section>header>div:first-child{display:flex;gap:.65rem;align-items:baseline}.member-section>header>div:first-child span{color:#69767d;font-size:.72rem}.sort-switch{display:flex;align-items:center;gap:.25rem}.sort-switch>span{margin-right:.15rem;color:#69767d;font-size:.7rem}.sort-switch button{min-height:30px;padding:.3rem .48rem;border-color:#aab4af;background:#fff;color:#4f5d56;font-size:.74rem;font-weight:700}.sort-switch button.active{border-color:#136b52;background:#e8f4ee;color:#0d5a43}.member-section ul{display:block;min-height:0;margin:0;padding:0 1rem;overflow:visible;list-style:none}.expense-card{position:relative;display:grid;grid-template-columns:minmax(0,1fr) auto;gap:.8rem 1.5rem;align-items:end;overflow:visible;padding:1rem 0;border-top:1px solid #e5e7eb}.expense-card:first-child{border-top:0}.expense-main{display:grid;gap:.38rem;min-width:0}.expense-title{display:flex;gap:.55rem;align-items:flex-start;min-width:0}.expense-title strong{line-height:1.45;overflow-wrap:anywhere}.category-dot{flex:none;width:9px;height:9px;margin-top:.38rem;border-radius:50%;background:#7b8790}.category-rail,.category-flight{background:#286aa5}.category-hotel{background:#6b8b3e}.category-meal{background:#c47a16}.category-city_transport{background:#6e5ca8}.category-courier_logistics{background:#278477}.expense-meta{display:flex;flex-wrap:wrap;gap:.3rem .8rem;align-items:center;color:#51616b;font-size:.78rem}.expense-meta b{color:#17232d}.expense-secondary{display:grid;gap:.32rem}.expense-secondary>p{margin:0;color:#64748b;font-size:.8rem;line-height:1.5}.evidence summary{width:fit-content;color:#315f8a;font-size:.76rem;cursor:pointer}.evidence div{display:grid;gap:.18rem;margin-top:.35rem;padding:.5rem .6rem;border-left:3px solid #9badb5;background:#f5f7f7;color:#65727a;font-size:.72rem;overflow-wrap:anywhere}.expense-action{position:relative;display:flex;min-width:max-content;justify-content:flex-end;align-items:flex-end;align-self:end}.expense-action-bar{display:flex;flex-wrap:nowrap;align-items:center;justify-content:flex-end;gap:.45rem}.card-action,.exclude-action{min-height:36px;padding:.42rem .65rem;background:#fff;font-size:.8rem;font-weight:700}.card-action{border-color:#9ba8a1;color:#315043}.card-action.active{border-color:#136b52;background:#edf6f1;color:#136b52}.danger-divider{align-self:stretch;width:1px;margin:0 .15rem;background:#d6dcda}.exclude-action{border-color:#b85a52;color:#9b342d}.exclude-action:hover{background:#fff1f0}.move-controls{position:absolute;right:0;bottom:calc(100% + .45rem);z-index:25;display:grid;width:min(560px,70vw);grid-template-columns:minmax(220px,1fr) auto;gap:.75rem;align-items:end;padding:.75rem;border:1px solid #aebdb6;background:#fff;box-shadow:0 12px 30px rgba(32,47,40,.18)}.move-controls label{display:grid;gap:.3rem}.move-controls label span{color:#5e6b65;font-size:.72rem;font-weight:700}.move-controls label select{width:100%}.move-controls>div{display:flex;gap:.45rem}.empty-member{margin:0;padding:1rem;color:#69767d}.undo-notice{display:flex;flex:none;align-items:center;justify-content:space-between;gap:1rem;margin-top:.6rem;padding:.55rem .7rem;border-left:4px solid #136b52;background:#edf6f1;color:#24533f;font-size:.8rem}.undo-notice button{min-height:30px;padding:.3rem .55rem;border-color:#136b52;color:#136b52;font-weight:700}
  .transport-decision{display:flex;align-items:center;justify-content:space-between;gap:1rem;margin-top:.7rem;padding:.75rem .9rem;border:1px solid #d3dade;background:#fff}.transport-decision>div:first-child{display:grid;gap:.15rem}.transport-decision span,.transport-decision small{color:#68757c;font-size:.74rem}.decision-actions{display:flex;flex-wrap:wrap;justify-content:flex-end;gap:.4rem}.decision-actions button{min-height:34px;padding:.4rem .6rem;border:1px solid #9eaaa4;background:#fff;color:#315043;font-weight:700;cursor:pointer}.decision-actions button.active{border-color:#136b52;background:#e8f4ee;color:#0d5a43}
  .group-footer{display:flex;flex:none;justify-content:space-between;gap:1rem;align-items:flex-start;margin-top:.7rem;padding-top:.7rem;border-top:1px solid #dbe1e4}.more-tools{display:flex;flex-wrap:wrap;gap:.5rem}.merge-controls{display:flex;gap:.4rem}.confirmed-copy{padding:.45rem .6rem;color:#17613f;font-weight:700}.final-confirm{display:flex;justify-content:space-between;gap:1rem;align-items:center;padding:1rem 1.3rem;border-top:1px solid #cbd2d6;background:#eef7f2}.final-confirm>div{display:grid;gap:.2rem}.final-confirm span{color:#52645a;font-size:.78rem}.create-group-panel{border-top:1px solid #cbd2d6;background:#f5f7f7}.create-group-panel>summary{padding:.85rem 1.3rem;color:#315043;font-weight:700;cursor:pointer}.create-group{display:grid;grid-template-columns:minmax(160px,auto) minmax(180px,1fr) auto auto auto;gap:.55rem;align-items:end;padding:0 1.3rem 1rem}.create-group p{grid-column:1/-1;margin:0;color:#64748b;font-size:.77rem}.error{color:#a2302a}.action-error,.action-notice{margin:.8rem 1.3rem;padding:.65rem .75rem}.action-error{background:#fef2f2}.action-notice{border-left:4px solid #136b52;background:#e7f1eb;color:#254b3d;font-size:.82rem}
  @media(max-width:1100px){.overview-table-head,.overview-row{grid-template-columns:minmax(180px,1.2fr) minmax(200px,1.3fr) 75px 110px 95px}.overview-table-head span:nth-child(3),.overview-row>[role='cell']:nth-child(3){display:none}.create-group{grid-template-columns:1fr 1fr}}@media(max-width:900px){.expense-card{grid-template-columns:minmax(0,1fr)}.expense-action{grid-column:1/-1;min-width:0}}@media(max-width:720px){.group-toolbar,.group-header,.group-footer,.final-confirm{align-items:stretch;flex-direction:column}.recompute-button{align-self:flex-end}.overview-table-head{display:none}.overview-row{grid-template-columns:1fr auto;padding:1rem}.overview-route{grid-column:1/-1}.overview-row>[role='cell']:nth-child(3){display:block}.overview-row>[role='cell']:nth-child(5){grid-column:1}.review-group{grid-column:2;grid-row:1}.group-review-page{min-height:0}.review-navigation{grid-template-columns:1fr auto}.review-navigation>span{display:none}.group-card{min-height:0;overflow:visible;padding:1rem}.route-panel>summary{grid-template-columns:1fr auto}.route-panel>summary span:first-child{display:none}.member-section{min-height:0}.member-section>header{flex-direction:column}.sort-switch{justify-content:flex-start}.expense-action{justify-content:flex-start}.expense-action-bar{justify-content:flex-start}.danger-divider{height:36px}.merge-controls,.move-controls{display:grid;grid-template-columns:1fr;width:min(100%,calc(100vw - 4rem))}.move-controls{right:auto;left:0}.move-controls>div{justify-content:flex-end}.create-group{grid-template-columns:1fr}}
</style>
