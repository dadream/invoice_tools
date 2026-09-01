<script lang="ts">
  import { describeError, invokeSafe } from '../../lib/ipc'
  import type {
    Batch, BatchCollectionImport, BatchGrouping, BatchReviewSnapshot, ExpenseItem, Invoice,
    PendingInvoiceDocument, ReviewAction,
  } from '../../lib/types'
  import { STATUS_COLORS, STATUS_LABELS, formatAmount, formatDate } from '../../lib/types'
  import ConfirmDialog from '../../lib/ConfirmDialog.svelte'
  import BatchImportPanel from './BatchImportPanel.svelte'
  import ExpenseReviewPage from './ExpenseReviewPage.svelte'
  import GroupingSummary from './GroupingSummary.svelte'
  import ReviewHistory from './ReviewHistory.svelte'
  import ReviewWorkspace from './ReviewWorkspace.svelte'

  type BatchView = 'expenses' | 'groups'
  interface Props {
    batchId: number
    onUpdate: () => Promise<void>
    onBack: () => void
    onOpenDelivery: () => void
  }
  interface ExpenseCategoryReanalysisResult {
    scanned_count: number
    changed_count: number
    confirmed_count: number
    suggestion_count: number
    remaining_unclassified_count: number
  }

  let { batchId, onUpdate, onBack, onOpenDelivery }: Props = $props()
  let batch = $state<Batch | null>(null)
  let invoices = $state<Invoice[]>([])
  let expenseItems = $state<ExpenseItem[]>([])
  let pendingDocuments = $state<PendingInvoiceDocument[]>([])
  let collectionSources = $state<BatchCollectionImport[]>([])
  let grouping = $state<BatchGrouping | null>(null)
  let reviewActions = $state<ReviewAction[]>([])
  let activeView = $state<BatchView>('expenses')
  let loading = $state(true)
  let finishing = $state(false)
  let reanalyzingCategories = $state(false)
  let error = $state<string | null>(null)
  let invoicesError = $state<string | null>(null)
  let expensesError = $state<string | null>(null)
  let groupingError = $state<string | null>(null)
  let reviewError = $state<string | null>(null)
  let sourceError = $state<string | null>(null)
  let notice = $state<string | null>(null)
  let confirmingCompletion = $state(false)
  let showCompletionChecklist = $state(false)
  let showImport = $state(false)
  let selectedReviewInvoiceId = $state<number | null>(null)

  const canEdit = $derived(batch?.status === 'draft')
  const suspectedDuplicateCount = $derived(expenseItems.filter((item) => item.inclusion_status === 'duplicate_suspect').length)
  const confirmedDuplicateCount = $derived(invoices.filter((item) => item.is_duplicate && item.is_excluded).length)
  const duplicateCount = $derived(suspectedDuplicateCount + confirmedDuplicateCount)
  const duplicateExcludedExpenses = $derived(expenseItems.filter((expense) => {
    const invoice = invoices.find((item) => item.id === expense.primary_invoice_id)
    return expense.inclusion_status === 'duplicate_suspect' || (invoice?.is_duplicate && expense.inclusion_status !== 'included')
  }))
  const duplicateExcludedAmount = $derived(duplicateExcludedExpenses.reduce((sum, expense) => sum + Number(expense.gross_amount), 0).toFixed(2))
  const otherExcludedExpenses = $derived(expenseItems.filter((expense) => {
    const invoice = invoices.find((item) => item.id === expense.primary_invoice_id)
    return expense.inclusion_status === 'excluded' && !invoice?.is_duplicate
  }))
  const otherExcludedAmount = $derived(otherExcludedExpenses.reduce((sum, expense) => sum + Number(expense.gross_amount), 0).toFixed(2))
  const includedExpenses = $derived(expenseItems.filter((item) => item.inclusion_status === 'included'))
  const includedInvoiceIds = $derived(new Set(includedExpenses.map((item) => item.primary_invoice_id)))
  const invalidSignatureCount = $derived(invoices.filter((item) => includedInvoiceIds.has(item.id) && item.verification_result === 'invalid').length)
  const unconfirmedDateCount = $derived(includedExpenses.filter((item) => !item.transaction_date_confirmed).length)
  const unconfirmedCategoryCount = $derived(includedExpenses.filter((item) => !item.category_confirmed).length)
  const pendingGroupingCount = $derived(grouping?.groups.filter((group) => group.requires_review).length ?? 0)
  const unresolvedMaterialCount = $derived(pendingDocuments.filter((document) => document.status === 'pending').length)
  const hasGroupingAmbiguities = $derived.by(() => {
    if (!grouping) return false
    try {
      const value: unknown = JSON.parse(grouping.ambiguities_json)
      return !Array.isArray(value) || value.length > 0
    } catch {
      return true
    }
  })
  const missingGroupingSnapshot = $derived(includedExpenses.length > 0 && grouping === null)
  const anchorlessBusinessTripCount = $derived(grouping?.groups.filter((group) =>
    group.kind === 'business_trip' && !group.members.some((member) => {
      const expense = expenseItems.find((item) => item.primary_invoice_id === member.invoice_id)
      if (!expense || expense.inclusion_status !== 'included') return false
      return expense.category_code === 'rail' || expense.category_code === 'flight' ||
        expense.documents.some((document) => document.role === 'itinerary')
    }),
  ).length ?? 0)
  const completionBlocked = $derived(
    includedExpenses.length === 0 || unconfirmedCategoryCount > 0 || unconfirmedDateCount > 0 || invalidSignatureCount > 0 || unresolvedMaterialCount > 0 || missingGroupingSnapshot || anchorlessBusinessTripCount > 0 || pendingGroupingCount > 0 || hasGroupingAmbiguities,
  )

  function completionHint(): string {
    if (includedExpenses.length === 0) return '至少需要 1 条可计入费用'
    if (unconfirmedCategoryCount > 0) return `${unconfirmedCategoryCount} 条费用类型待确认`
    if (unconfirmedDateCount > 0) return `${unconfirmedDateCount} 条实际发生日期待确认`
    if (invalidSignatureCount > 0) return `${invalidSignatureCount} 条签章校验失败待处理`
    if (unresolvedMaterialCount > 0) return `${unresolvedMaterialCount} 份材料待挂载或明确忽略`
    if (missingGroupingSnapshot) return '尚未形成归组结果'
    if (anchorlessBusinessTripCount > 0) return `${anchorlessBusinessTripCount} 个差旅行程缺少铁路、航空或行程单锚点`
    if (pendingGroupingCount > 0 || hasGroupingAmbiguities) return '归组仍有待确认项'
    return '所有阻断项已处理，可以完成审核'
  }

  async function loadBatch() {
    const result = await invokeSafe<Batch>('get_batch', { id: batchId })
    if (result.ok) batch = result.data
    else error = describeError(result.error)
  }
  async function loadInvoices() {
    invoicesError = null
    const result = await invokeSafe<Invoice[]>('list_batch_invoices', { batchId })
    if (result.ok) invoices = result.data
    else { invoices = []; invoicesError = describeError(result.error) }
  }
  async function loadExpenses() {
    expensesError = null
    const result = await invokeSafe<ExpenseItem[]>('list_expense_items', { batchId })
    if (result.ok) expenseItems = result.data
    else { expenseItems = []; expensesError = describeError(result.error) }
  }
  async function loadPendingDocuments() {
    const result = await invokeSafe<PendingInvoiceDocument[]>('list_pending_invoice_documents', { batchId })
    pendingDocuments = result.ok ? result.data : []
    if (!result.ok && !expensesError) expensesError = describeError(result.error)
  }
  async function loadCollectionSources() {
    sourceError = null
    const result = await invokeSafe<BatchCollectionImport[]>('list_batch_collection_sources', { batchId })
    if (result.ok) collectionSources = result.data
    else { collectionSources = []; sourceError = describeError(result.error) }
  }
  async function loadGrouping() {
    groupingError = null
    const result = await invokeSafe<BatchGrouping | null>('get_batch_grouping', { batchId })
    if (result.ok) grouping = result.data
    else { grouping = null; groupingError = describeError(result.error) }
  }
  async function loadReviewActions() {
    reviewError = null
    const result = await invokeSafe<ReviewAction[]>('list_review_actions', { batchId })
    if (result.ok) reviewActions = result.data
    else { reviewActions = []; reviewError = describeError(result.error) }
  }
  async function refreshAll() {
    await Promise.all([loadBatch(), loadInvoices(), loadExpenses(), loadPendingDocuments(), loadCollectionSources(), loadGrouping(), loadReviewActions()])
    await onUpdate()
  }
  async function reanalyzeCategories() {
    if (reanalyzingCategories) return
    reanalyzingCategories = true
    error = null
    const result = await invokeSafe<ExpenseCategoryReanalysisResult>('reanalyze_expense_categories', { batchId })
    reanalyzingCategories = false
    if (!result.ok) { error = describeError(result.error); return }
    notice = result.data.changed_count > 0
      ? `已重新识别 ${result.data.changed_count} 条费用类型：${result.data.confirmed_count} 条由发票项目直接确认，${result.data.suggestion_count} 条为待确认建议；仍有 ${result.data.remaining_unclassified_count} 条“其他”。`
      : `没有发现新的费用类型；仍有 ${result.data.remaining_unclassified_count} 条需要人工分类。`
    await refreshAll()
  }
  async function handleImported() { showImport = false; notice = '导入完成，请继续核对费用字段、重复项与归组。'; await refreshAll() }
  async function completeReview() {
    if (!batch || completionBlocked || finishing) return
    confirmingCompletion = false
    finishing = true
    error = null
    const result = await invokeSafe<BatchReviewSnapshot>('complete_batch_review', { batchId })
    finishing = false
    if (!result.ok) { error = describeError(result.error); return }
    await onUpdate()
    onOpenDelivery()
  }

  $effect(() => {
    batchId
    selectedReviewInvoiceId = null
    showImport = false
    showCompletionChecklist = false
    notice = null
    error = null
    loading = true
    Promise.all([loadBatch(), loadInvoices(), loadExpenses(), loadPendingDocuments(), loadCollectionSources(), loadGrouping(), loadReviewActions()]).finally(() => { loading = false })
  })
</script>

{#if selectedReviewInvoiceId !== null}
  <ExpenseReviewPage
    {batchId}
    initialInvoiceId={selectedReviewInvoiceId}
    {invoices}
    {expenseItems}
    {grouping}
    {canEdit}
    onChanged={refreshAll}
    onBack={() => (selectedReviewInvoiceId = null)}
  />
{:else}
<div class="batch-page">
  <header class="batch-header">
    <button class="back" type="button" onclick={onBack}>← 批次列表</button>
    {#if batch}
      <div class="title-row">
        <div>
          <span class="eyebrow">批次 #{batch.id}</span>
          <h1>{batch.name}</h1>
          <p>创建于 {formatDate(batch.created_at)} · {batch.invoice_count} 条计入费用</p>
        </div>
        <span class="status" style={`--status-color:${STATUS_COLORS[batch.status]}`}>{STATUS_LABELS[batch.status]}</span>
      </div>
    {/if}
  </header>

  {#if loading}
    <p class="state-message">正在读取批次…</p>
  {:else if error && !batch}
    <p class="state-message error" role="alert">{error}</p>
  {:else if batch}
    <nav class="view-tabs" aria-label="批次视图">
      <button type="button" class:active={activeView === 'expenses'} aria-current={activeView === 'expenses' ? 'page' : undefined} onclick={() => (activeView = 'expenses')}>
        <span>01</span><strong>费用清单</strong><small>{expenseItems.length} 条费用</small>
      </button>
      <button type="button" class:active={activeView === 'groups'} aria-current={activeView === 'groups' ? 'page' : undefined} onclick={() => (activeView = 'groups')}>
        <span>02</span><strong>归组</strong><small>{grouping?.groups.length ?? 0} 个归组</small>
      </button>
    </nav>

    {#if notice}<p class="notice" role="status">{notice}</p>{/if}
    {#if error}<p class="page-error" role="alert">{error}</p>{/if}

    <main class="view-body">
      {#if activeView === 'expenses'}
        <section class="view-intro">
          <div><span class="eyebrow">费用清单视图</span><h2>核对每条费用与原始材料</h2><p>这里只使用软件自己的稳定字段；目标系统映射在完成审核后处理。</p></div>
          {#if canEdit && unconfirmedCategoryCount > 0}<div class="category-reanalysis"><strong>{unconfirmedCategoryCount} 条费用类型待确认</strong><span>可重新读取发票项目，餐饮等明确类型会自动补全。</span><button type="button" onclick={() => void reanalyzeCategories()} disabled={reanalyzingCategories}>{reanalyzingCategories ? '识别中…' : '重新识别费用类型'}</button></div>{/if}
          {#if duplicateCount > 0}
            <div class="duplicate-rule"><strong>{suspectedDuplicateCount} 条待判断 · {confirmedDuplicateCount} 条已确认重复</strong><span>全部不计入批次总额；只有明确选择“不是重复”后才恢复计入。</span></div>
          {/if}
          {#if unresolvedMaterialCount > 0}<div class="material-rule"><strong>{unresolvedMaterialCount} 份待挂载材料</strong><span>先归入某笔费用或明确标记为无关文件；材料本身不计金额。</span></div>{/if}
        </section>

        {#if canEdit}
          <div class="import-callout"><div><strong>{expenseItems.length === 0 ? '本批次尚无费用' : '继续导入材料'}</strong><span>从独立邮件收集任务选择材料，或直接导入本地发票文件。</span></div><button type="button" onclick={() => (showImport = true)}>导入材料</button></div>
        {/if}

        {#if sourceError}<p class="page-error inline" role="alert">{sourceError}</p>{/if}
        {#if collectionSources.length > 0}
          <section class="source-summary" aria-label="批次来源摘要">
            <div><span class="eyebrow">来源引用</span><strong>{collectionSources.length} 次邮件材料导入</strong></div>
            <ul>{#each collectionSources as source (source.id)}<li><strong>{source.task_name}</strong><span>{source.item_count} 个文件 · {source.status === 'completed' || source.status === 'legacy' ? '已导入' : source.status === 'failed' ? '导入失败，可恢复' : '处理中'}</span></li>{/each}</ul>
          </section>
        {/if}

        {#if invoicesError}<p class="page-error inline" role="alert">{invoicesError}</p>{/if}
        {#if expensesError}<p class="page-error inline" role="alert">{expensesError}</p>{/if}
        <div class="workspace-scroll"><ReviewWorkspace {batchId} {invoices} {expenseItems} {pendingDocuments} {grouping} {canEdit} onChanged={refreshAll} onOpenInvoice={(invoiceId) => (selectedReviewInvoiceId = invoiceId)} /></div>
        <details class="history-drawer">
          <summary>审核历史与撤销（{reviewActions.length}）</summary>
          <ReviewHistory {batchId} actions={reviewActions} {reviewError} {canEdit} onChanged={refreshAll} />
        </details>
      {:else}
        <section class="view-intro">
          <div><span class="eyebrow">归组视图</span><h2>确定费用、发票与行程材料的归属</h2><p>先建立明确归组，再把相关发票、行程单和明细挂到同一费用记录下。</p></div>
        </section>
        <GroupingSummary {batchId} {grouping} {expenseItems} {invoices} {groupingError} {canEdit} onChanged={refreshAll} onOpenInvoice={(invoiceId) => (selectedReviewInvoiceId = invoiceId)} />
      {/if}
    </main>

    <footer class="review-footer">
      <div class="totals"><span>本次计入</span><strong>{formatAmount(batch.total_amount)}</strong><small>{includedExpenses.length} 条费用</small>{#if duplicateCount > 0}<i>{suspectedDuplicateCount} 条待判断、{confirmedDuplicateCount} 条已确认重复，共 {formatAmount(duplicateExcludedAmount)} 未计入</i>{/if}{#if otherExcludedExpenses.length > 0}<i>{otherExcludedExpenses.length} 条其他排除，共 {formatAmount(otherExcludedAmount)} 未计入</i>{/if}</div>
      <div class:ready={!completionBlocked} class="completion-state"><span>{completionBlocked ? '尚不能完成审核' : '审核检查通过'}</span><strong>{completionHint()}</strong></div>
      {#if canEdit}
        <button class="complete" type="button" onclick={() => (showCompletionChecklist = true)} disabled={finishing}>{finishing ? '正在冻结审核版本…' : '完成审核'}</button>
      {:else}
        <button class="complete" type="button" onclick={onOpenDelivery}>进入交付选项</button>
      {/if}
    </footer>
  {/if}
</div>

{#if showImport && batch}
  <div class="panel-backdrop" role="presentation" onclick={(event) => event.target === event.currentTarget && (showImport = false)}>
    <div class="import-drawer" role="dialog" aria-modal="true" aria-labelledby="import-title">
      <header><div><span class="eyebrow">批次内处理</span><h2 id="import-title">导入并解析材料</h2><p>选择独立收集任务中的附件，或本地发票文件；批次不再直接访问邮箱。</p></div><button type="button" aria-label="关闭" onclick={() => (showImport = false)}>×</button></header>
      <BatchImportPanel {batchId} batchName={batch.name} batchMonth={batch.month} onImported={handleImported} />
    </div>
  </div>
{/if}

{#if showCompletionChecklist}
  <div class="panel-backdrop" role="presentation" onclick={(event) => event.target === event.currentTarget && (showCompletionChecklist = false)}>
    <div class="completion-dialog" role="dialog" aria-modal="true" aria-labelledby="completion-title">
      <header><div><span class="eyebrow">完成审核检查</span><h2 id="completion-title">{completionBlocked ? '还有项目需要处理' : '可以完成本地审核'}</h2><p>所有项目同时通过后，才冻结本批次并进入 Excel 导出或 Concur 上传。</p></div><button type="button" aria-label="关闭" onclick={() => (showCompletionChecklist = false)}>×</button></header>
      <ul>
        <li class:passed={includedExpenses.length > 0}><b>{includedExpenses.length > 0 ? '✓' : '!'}</b><div><strong>至少一条计入费用</strong><span>{includedExpenses.length} 条费用，合计 {formatAmount(batch?.total_amount ?? '0')}</span></div></li>
        <li class:passed={unconfirmedCategoryCount === 0}><b>{unconfirmedCategoryCount === 0 ? '✓' : '!'}</b><div><strong>费用类型已确认</strong><span>{unconfirmedCategoryCount === 0 ? '全部通过' : `${unconfirmedCategoryCount} 条待分类或确认`}</span></div></li>
        <li class:passed={unconfirmedDateCount === 0}><b>{unconfirmedDateCount === 0 ? '✓' : '!'}</b><div><strong>实际发生日期已确认</strong><span>{unconfirmedDateCount === 0 ? '全部通过' : `${unconfirmedDateCount} 条待确认`}</span></div></li>
        <li class:passed={invalidSignatureCount === 0}><b>{invalidSignatureCount === 0 ? '✓' : '!'}</b><div><strong>签章异常已处理</strong><span>{invalidSignatureCount === 0 ? '全部通过' : `${invalidSignatureCount} 条失败`}</span></div></li>
        <li class:passed={unresolvedMaterialCount === 0}><b>{unresolvedMaterialCount === 0 ? '✓' : '!'}</b><div><strong>配套材料已归属</strong><span>{unresolvedMaterialCount === 0 ? '全部通过' : `${unresolvedMaterialCount} 份待挂载或忽略`}</span></div></li>
        <li class:passed={!missingGroupingSnapshot && anchorlessBusinessTripCount === 0 && pendingGroupingCount === 0 && !hasGroupingAmbiguities}><b>{!missingGroupingSnapshot && anchorlessBusinessTripCount === 0 && pendingGroupingCount === 0 && !hasGroupingAmbiguities ? '✓' : '!'}</b><div><strong>归组审核已完成</strong><span>{missingGroupingSnapshot ? '尚未形成归组结果' : anchorlessBusinessTripCount > 0 ? `${anchorlessBusinessTripCount} 个差旅行程缺少锚点` : pendingGroupingCount > 0 || hasGroupingAmbiguities ? '仍有待确认项' : '全部通过'}</span></div></li>
      </ul>
      <footer><button type="button" class="secondary" onclick={() => (showCompletionChecklist = false)}>返回处理</button><button type="button" class="complete" disabled={completionBlocked} onclick={() => { showCompletionChecklist = false; confirmingCompletion = true }}>继续完成审核</button></footer>
    </div>
  </div>
{/if}

{#if confirmingCompletion}
  <ConfirmDialog title="完成本地审核" message="将冻结当前费用、计入状态、归组和附件版本。后续交付都使用该版本；若继续修改，需要重新完成审核。" confirmLabel="冻结并进入交付" busy={finishing} onConfirm={completeReview} onCancel={() => (confirmingCompletion = false)} />
{/if}
{/if}

<style>
  .batch-page{min-height:100vh;padding-bottom:104px;background:#f4f5f6;color:#17232d}.batch-header{padding:1.2rem 2rem 1rem;border-bottom:1px solid #ccd3d7;background:#fff}.back{padding:0;border:0;background:transparent;color:var(--pine,#136b52);font-weight:700;cursor:pointer}.title-row{display:flex;align-items:flex-end;justify-content:space-between;gap:2rem;margin-top:1rem}.eyebrow{color:#68767d;font-family:'IBM Plex Mono',monospace;font-size:.72rem;font-weight:700;letter-spacing:.08em;text-transform:uppercase}h1{margin:.25rem 0 0;font-size:clamp(1.65rem,2.4vw,2.25rem);letter-spacing:-.035em}.title-row p,.view-intro p{margin:.35rem 0 0;color:#596870}.status{flex:none;padding:.38rem .65rem;border-left:4px solid var(--status-color);background:#f5f7f7;color:#344149;font-size:.82rem;font-weight:700}
  .view-tabs{display:flex;border-bottom:1px solid #ccd3d7;background:#fff}.view-tabs button{display:grid;grid-template-columns:auto 1fr auto;gap:.75rem;align-items:baseline;min-width:280px;padding:.8rem 2rem;border:0;border-bottom:3px solid transparent;background:transparent;color:#596870;text-align:left;cursor:pointer}.view-tabs button.active{border-bottom-color:var(--pine,#136b52);color:#17232d}.view-tabs span{font-family:'IBM Plex Mono',monospace;font-size:.72rem}.view-tabs strong{font-size:1rem}.view-tabs small{color:#68767d}
  .view-body{padding:1.35rem 2rem 2rem}.view-intro{display:flex;justify-content:space-between;gap:1rem;align-items:flex-start;margin-bottom:1.15rem}.view-intro h2{margin:.25rem 0 0;font-size:1.25rem}.duplicate-rule,.material-rule,.category-reanalysis{display:grid;gap:.22rem;max-width:370px;padding:.75rem .9rem;border-left:4px solid var(--amber,#c47a16);background:#fff4d9}.duplicate-rule span,.material-rule span,.category-reanalysis span{color:#6f531d;font-size:.8rem;line-height:1.45}.material-rule{border-left-color:var(--risk,#b3453e);background:#f8e9e7}.material-rule span{color:#862f2a}.category-reanalysis{border-left-color:#315f8a;background:#edf3f8}.category-reanalysis span{color:#4d6275}.category-reanalysis button{margin-top:.35rem;padding:.42rem .55rem;border:1px solid #315f8a;background:#fff;color:#315f8a;font-weight:700;cursor:pointer}.category-reanalysis button:disabled{opacity:.5;cursor:not-allowed}
  .import-callout{display:flex;align-items:center;justify-content:space-between;gap:1rem;margin-bottom:1rem;padding:.75rem .9rem;border:1px solid #cbd2d6;background:#fff}.import-callout div{display:grid;gap:.2rem}.import-callout span{color:#65737a;font-size:.82rem}.import-callout button{padding:.58rem .85rem;border:1px solid #136b52;background:#136b52;color:#fff;font-weight:700;cursor:pointer}.history-drawer{margin:1rem 0;border:1px solid #cbd2d6;background:#fff}.history-drawer>summary{padding:.75rem .9rem;cursor:pointer;font-weight:700}.workspace-scroll{overflow-x:auto}.history-drawer :global(.history-section){margin:0;padding:1rem;border-top:1px solid #d9dfe2}
  .source-summary{display:grid;grid-template-columns:minmax(190px,auto) 1fr;gap:1rem;align-items:start;margin-bottom:1rem;padding:.75rem .9rem;border:1px solid #cbd2d6;background:#f8faf9}.source-summary>div{display:grid;gap:.18rem}.source-summary ul{display:flex;flex-wrap:wrap;gap:.45rem;margin:0;padding:0;list-style:none}.source-summary li{display:grid;gap:.1rem;padding:.4rem .55rem;border-left:3px solid #136b52;background:#edf6f1}.source-summary li strong{font-size:.78rem}.source-summary li span{color:#576a60;font-size:.7rem}
  .notice,.page-error{margin:1rem 2rem 0;padding:.7rem .85rem;border-left:4px solid var(--pine,#136b52);background:#e7f1eb;color:#24533f}.page-error{border-color:var(--risk,#b3453e);background:#f8e9e7;color:#862f2a}.page-error.inline{margin:0 0 1rem}.state-message{padding:3rem 2rem;color:#59645e}.state-message.error{color:var(--risk,#b3453e)}
  .review-footer{position:fixed;right:0;bottom:0;left:224px;z-index:90;display:grid;grid-template-columns:minmax(260px,1fr) minmax(280px,auto) auto;gap:1rem;align-items:center;min-height:82px;padding:.75rem 2rem;border-top:1px solid #9eabb1;background:rgba(255,255,255,.97);box-shadow:0 -8px 28px rgba(38,47,42,.08);backdrop-filter:blur(10px)}.totals{display:flex;align-items:baseline;gap:.65rem;min-width:0}.totals>span{color:#657078}.totals strong{color:var(--pine,#136b52);font-size:1.45rem}.totals small{color:#596870}.totals i{color:#8a570d;font-size:.76rem;font-style:normal}.completion-state{display:grid;gap:.16rem;padding-left:.75rem;border-left:3px solid var(--amber,#c47a16)}.completion-state.ready{border-left-color:var(--pine,#136b52)}.completion-state span{color:#657078;font-size:.72rem}.completion-state strong{font-size:.84rem}.complete{min-width:132px;padding:.72rem 1rem;border:1px solid var(--pine,#136b52);background:var(--pine,#136b52);color:#fff;font-weight:700;cursor:pointer}.complete:disabled{opacity:.45;cursor:not-allowed}
  .panel-backdrop{position:fixed;inset:0;z-index:110;background:rgba(20,31,36,.42)}.import-drawer{position:absolute;top:0;right:0;bottom:0;width:min(760px,calc(100vw - 224px));overflow:auto;background:#f4f5f6;box-shadow:-18px 0 40px rgba(20,31,36,.18)}.import-drawer>header,.completion-dialog>header{display:flex;align-items:flex-start;justify-content:space-between;gap:1rem;padding:1.1rem 1.25rem;border-bottom:1px solid #cbd2d6;background:#fff}.import-drawer h2,.completion-dialog h2{margin:.2rem 0 0}.import-drawer header p,.completion-dialog header p{margin:.3rem 0 0;color:#65737a;line-height:1.45}.import-drawer header button,.completion-dialog header button{border:0;background:transparent;font-size:1.6rem;cursor:pointer}.import-drawer :global(.import-panel){margin:1rem;border:1px solid #cbd2d6;background:#fff}.completion-dialog{position:absolute;top:50%;left:calc(50% + 112px);width:min(620px,calc(100vw - 264px));transform:translate(-50%,-50%);background:#fff;box-shadow:0 18px 60px rgba(20,31,36,.25)}.completion-dialog ul{display:grid;gap:0;margin:0;padding:0 1.25rem;list-style:none}.completion-dialog li{display:grid;grid-template-columns:32px 1fr;gap:.65rem;padding:.8rem 0;border-bottom:1px solid #e0e5e7}.completion-dialog li b{display:grid;place-items:center;width:26px;height:26px;border-radius:50%;background:#fff1f0;color:#b3453e}.completion-dialog li.passed b{background:#edf6f1;color:#136b52}.completion-dialog li div{display:grid;gap:.18rem}.completion-dialog li span{color:#65737a;font-size:.8rem}.completion-dialog footer{display:flex;justify-content:flex-end;gap:.6rem;padding:1rem 1.25rem}.completion-dialog .secondary{padding:.7rem .9rem;border:1px solid #aeb9bf;background:#fff;color:#344149;font-weight:700;cursor:pointer}
  @media(max-width:1000px){.review-footer{grid-template-columns:1fr auto}.completion-state{display:none}}@media(max-width:820px){.review-footer{left:0}.import-drawer{width:100%}.completion-dialog{left:50%;width:calc(100vw - 2rem)}}@media(max-width:760px){.batch-header,.view-body{padding-inline:1rem}.title-row,.view-intro,.import-callout{display:grid}.view-tabs button{grid-template-columns:auto 1fr;min-width:50%;padding-inline:1rem}.view-tabs small{display:none}.review-footer{padding-inline:1rem}.totals i,.totals small{display:none}}
</style>
