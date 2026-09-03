<script lang="ts">
  import { describeError, invokeSafe } from '../../lib/ipc'
  import type { Batch } from '../../lib/types'
  import { STATUS_COLORS, STATUS_LABELS, formatAmount } from '../../lib/types'
  import ConfirmDialog from '../../lib/ConfirmDialog.svelte'
  import BatchDetail from './BatchDetail.svelte'
  import CreateBatchModal from './CreateBatchModal.svelte'
  import DeliveryWorkspace from './DeliveryWorkspace.svelte'

  type Screen = 'list' | 'review' | 'delivery'
  type BatchSortKey = 'name' | 'created_at'
  type SortDirection = 'asc' | 'desc'
  let batches = $state<Batch[]>([])
  let loading = $state(true)
  let error = $state<string | null>(null)
  let showCreateModal = $state(false)
  let selectedBatch = $state<number | null>(null)
  let screen = $state<Screen>('list')
  let deleteCandidate = $state<Batch | null>(null)
  let searchQuery = $state('')
  let statusFilter = $state<'all' | 'draft' | 'ready'>('all')
  let sortKey = $state<BatchSortKey | null>(null)
  let sortDirection = $state<SortDirection>('asc')

  const visibleBatches = $derived.by(() => {
    const filtered = batches.filter((batch) =>
      (statusFilter === 'all' || (statusFilter === 'draft' ? batch.status === 'draft' : batch.status !== 'draft'))
        && (!searchQuery.trim() || batch.name.toLocaleLowerCase().includes(searchQuery.trim().toLocaleLowerCase())),
    )
    if (sortKey === null) return filtered
    return [...filtered].sort((left, right) => {
      const compared = sortKey === 'name'
        ? left.name.localeCompare(right.name, 'zh-CN', { numeric: true, sensitivity: 'base' })
        : left.created_at.localeCompare(right.created_at)
      return sortDirection === 'asc' ? compared : -compared
    })
  })
  const draftCount = $derived(batches.filter((batch) => batch.status === 'draft').length)
  const readyCount = $derived(batches.filter((batch) => batch.status !== 'draft').length)
  const totalManagedAmount = $derived(batches.reduce((sum, batch) => sum + Number(batch.total_amount), 0).toFixed(2))

  async function loadBatches() {
    loading = true
    error = null
    const result = await invokeSafe<Batch[]>('list_batches')
    loading = false
    if (result.ok) batches = result.data
    else error = describeError(result.error)
  }

  async function handleCreate(name: string): Promise<string | null> {
    const result = await invokeSafe<number>('create_batch', { name })
    if (!result.ok) return describeError(result.error)
    showCreateModal = false
    await loadBatches()
    selectedBatch = result.data
    screen = 'review'
    return null
  }

  async function handleDelete(id: number) {
    deleteCandidate = null
    const result = await invokeSafe<void>('delete_batch', { id })
    if (result.ok) await loadBatches()
    else error = describeError(result.error)
  }

  function openBatch(batch: Batch) {
    selectedBatch = batch.id
    screen = 'review'
  }
  function openBatchDelivery(batch: Batch) {
    selectedBatch = batch.id
    screen = 'delivery'
  }
  function backToList() { selectedBatch = null; screen = 'list'; void loadBatches() }
  function openReview() { screen = 'review' }
  function openDelivery() { screen = 'delivery' }
  function executeDeleteBatch() { if (deleteCandidate) void handleDelete(deleteCandidate.id) }
  function toggleSort(key: BatchSortKey) {
    if (sortKey === key) sortDirection = sortDirection === 'asc' ? 'desc' : 'asc'
    else { sortKey = key; sortDirection = 'asc' }
  }
  function sortIndicator(key: BatchSortKey): string {
    if (sortKey !== key) return '↕'
    return sortDirection === 'asc' ? '↑' : '↓'
  }
  function formatCreatedDate(value: string): string {
    return value.match(/^\d{4}-\d{2}-\d{2}/)?.[0] ?? value
  }

  $effect(() => { void loadBatches() })
</script>

{#if screen === 'review' && selectedBatch !== null}
  <BatchDetail batchId={selectedBatch} onUpdate={loadBatches} onBack={backToList} onOpenDelivery={openDelivery} />
{:else if screen === 'delivery' && selectedBatch !== null}
  <DeliveryWorkspace batchId={selectedBatch} onBackToList={backToList} onBackToReview={openReview} onReviewReopened={openReview} />
{:else}
  <div class="container">
    <header class="page-header">
      <div><span class="eyebrow">本地报销工作台</span><h1>报销批次</h1><p>创建整理容器，导入费用材料，完成审核后再选择交付方式。</p></div>
      <button class="create" type="button" onclick={() => (showCreateModal = true)}>＋ 新建批次任务</button>
    </header>

    <section class="task-summary" aria-label="批次任务概览">
      <button class:active={statusFilter === 'draft'} type="button" disabled={draftCount === 0} onclick={() => (statusFilter = statusFilter === 'draft' ? 'all' : 'draft')}><span>草稿批次</span><strong>{draftCount}</strong></button>
      <button class:active={statusFilter === 'ready'} type="button" disabled={readyCount === 0} onclick={() => (statusFilter = statusFilter === 'ready' ? 'all' : 'ready')}><span>已完成审核</span><strong>{readyCount}</strong></button>
      <div><span>当前合计</span><strong>{formatAmount(totalManagedAmount)}</strong></div>
    </section>

    {#if loading}
      <p class="state">正在读取批次…</p>
    {:else if error}
      <p class="state error" role="alert">{error}</p>
    {:else if batches.length === 0}
      <section class="empty"><span>00</span><h2>还没有批次</h2><p>先创建一个批次；进入后选择邮件收集任务中的附件，或导入本地发票文件。</p><button type="button" onclick={() => (showCreateModal = true)}>创建第一个批次</button></section>
    {:else}
      <section class="batch-card" aria-label="报销批次列表">
        <div class="list-heading"><div class="list-tools"><label><span class="sr-only">搜索批次</span><input bind:value={searchQuery} placeholder="搜索批次名称" /></label><select bind:value={statusFilter} aria-label="批次状态"><option value="all">全部状态</option><option value="draft">草稿</option><option value="ready">已完成审核</option></select></div><strong>共 {visibleBatches.length} 项</strong></div>
        <div class="table-wrap">
          <table>
            <thead><tr><th aria-sort={sortKey === 'name' ? (sortDirection === 'asc' ? 'ascending' : 'descending') : 'none'}><button class="sort-header" type="button" onclick={() => toggleSort('name')}>批次 <span aria-hidden="true">{sortIndicator('name')}</span></button></th><th aria-sort={sortKey === 'created_at' ? (sortDirection === 'asc' ? 'ascending' : 'descending') : 'none'}><button class="sort-header" type="button" onclick={() => toggleSort('created_at')}>创建时间 <span aria-hidden="true">{sortIndicator('created_at')}</span></button></th><th>费用数</th><th>计入金额</th><th>状态</th><th>操作</th></tr></thead>
            <tbody>
              {#each visibleBatches as batch (batch.id)}
                <tr>
                  <td><button class="batch-link" type="button" onclick={() => openBatch(batch)}><strong>{batch.name}</strong><small>批次 #{batch.id}</small></button></td>
                  <td>{formatCreatedDate(batch.created_at)}</td>
                  <td>{batch.invoice_count}</td>
                  <td class="amount">{formatAmount(batch.total_amount)}</td>
                  <td><span class="batch-status" style={`--status-color:${STATUS_COLORS[batch.status]}`}>{STATUS_LABELS[batch.status]}</span></td>
                  <td><div class="actions"><button class="open" type="button" onclick={() => openBatch(batch)}>查看</button>{#if batch.status !== 'draft'}<span class="action-divider" aria-hidden="true">|</span><button class="open" type="button" onclick={() => openBatchDelivery(batch)}>交付</button>{/if}{#if batch.status === 'draft'}<button class="delete" type="button" onclick={() => (deleteCandidate = batch)}>删除</button>{/if}</div></td>
                </tr>
              {:else}<tr><td colspan="6" class="no-results">没有符合条件的批次。</td></tr>{/each}
            </tbody>
          </table>
        </div>
      </section>
    {/if}
  </div>

  {#if showCreateModal}<CreateBatchModal onSubmit={handleCreate} onCancel={() => (showCreateModal = false)} />{/if}
  {#if deleteCandidate}<ConfirmDialog title="删除报销批次" message={`将删除“${deleteCandidate.name}”及其中尚未交付的费用、归组和原件关联。此操作不能通过审核历史撤销。`} confirmLabel="确认删除" tone="danger" onConfirm={executeDeleteBatch} onCancel={() => (deleteCandidate = null)} />{/if}
{/if}

<style>
  .container{max-width:1500px;margin:0 auto;padding:2rem;color:#17232d}.page-header{display:flex;justify-content:space-between;gap:2rem;align-items:flex-end}.eyebrow{color:#68767d;font-family:'IBM Plex Mono',monospace;font-size:.72rem;font-weight:700;letter-spacing:.08em;text-transform:uppercase}h1{margin:.18rem 0 0;font-size:clamp(1.7rem,2.5vw,2.75rem);letter-spacing:-.04em}.page-header p{margin:.4rem 0 0;color:#596870}.create{padding:.68rem .9rem;border:1px solid #136b52;background:#136b52;color:#fff;font-weight:700;cursor:pointer}.empty button{padding:.7rem 1rem;border:1px solid var(--pine,#136b52);background:var(--pine,#136b52);color:#fff;font-weight:700;cursor:pointer}
  .task-summary{display:grid;grid-template-columns:repeat(3,minmax(0,1fr));gap:.7rem;margin:1rem 0}.task-summary>button,.task-summary>div{display:grid;grid-template-columns:1fr auto;gap:.4rem;padding:.75rem .9rem;border:1px solid #cbd2d6;border-left:4px solid #78888f;background:#fff;color:#17232d;font:inherit;text-align:left}.task-summary>button{cursor:pointer}.task-summary>button.active{border-left-color:#136b52;background:#edf6f1}.task-summary>button:disabled{cursor:default;opacity:.68}.task-summary span{color:#65737a;font-size:.77rem}.task-summary strong{grid-column:2;color:#136b52;font:1.25rem 'IBM Plex Mono',monospace}
  .batch-card{border:1px solid #c2cbd0;background:#fff}.list-heading{display:flex;justify-content:space-between;align-items:center;gap:1rem;padding:1rem 1.15rem;border-bottom:1px solid #cbd2d6}.list-heading>strong{min-width:70px;color:#136b52;font-family:'IBM Plex Mono',monospace;font-size:1.05rem;text-align:right;white-space:nowrap}.list-tools{display:flex;align-items:center;gap:.55rem}.list-tools input,.list-tools select{height:38px;padding:.45rem .6rem;border:1px solid #aeb9bf;background:#fff;font:inherit}.list-tools input{width:260px}.list-tools select{width:150px}.table-wrap{overflow-x:auto}table{width:100%;border-collapse:collapse}th,td{padding:.8rem 1rem;border-bottom:1px solid #e0e5e7;text-align:left;vertical-align:middle}th{padding:.75rem 1rem;background:#f1f3f4;color:#596870;font-size:.74rem;white-space:nowrap}.sort-header{display:inline-flex;align-items:center;gap:.25rem;padding:0;border:0;background:transparent;color:inherit;font:inherit;font-weight:700;cursor:pointer}.sort-header span{color:#136b52}tbody tr:hover{background:#f7faf8}.batch-link{display:grid;gap:.15rem;padding:0;border:0;background:transparent;color:#136b52;text-align:left;cursor:pointer}.batch-link strong{font-size:1.05rem}.batch-link small{color:#6c7980}.amount{font-family:'IBM Plex Mono',monospace;font-weight:700}.actions{display:flex;align-items:center;gap:.55rem;justify-content:flex-start;white-space:nowrap}.action-divider{color:#a4ada8}.open,.delete{padding:0;border:0;background:transparent;color:var(--pine,#136b52);font-weight:700;text-decoration:none;cursor:pointer}.delete{margin-left:.15rem;color:var(--risk,#b3453e);font-weight:500}.no-results{padding:2.5rem;color:#69767d;text-align:center}
  .batch-status{display:inline-block;padding:.34rem .55rem;border-left:4px solid var(--status-color);background:#f5f7f7;color:#344149;font-size:.78rem;font-weight:700;white-space:nowrap}
  .state,.empty{margin-top:1.5rem;padding:2rem;border:1px solid #cfc8b8;background:#fbfaf6}.state.error{border-left:4px solid var(--risk,#b3453e);color:#862f2a}.empty{text-align:center}.empty>span{font-family:'IBM Plex Mono',monospace;color:#9aa19c}.empty h2{margin:.4rem 0}.empty p{color:#59645e}.sr-only{position:absolute;width:1px;height:1px;padding:0;margin:-1px;overflow:hidden;clip:rect(0,0,0,0);white-space:nowrap;border:0}
  @media(max-width:900px){.task-summary{grid-template-columns:1fr}.list-heading{align-items:stretch;flex-direction:column}.list-tools{flex-wrap:wrap}}@media(max-width:680px){.container{padding:1rem}.page-header{display:grid}.create{width:100%}.list-tools{display:grid}.list-tools input,.list-tools select{width:100%}}
</style>
