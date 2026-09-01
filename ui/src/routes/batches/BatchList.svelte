<script lang="ts">
  import { describeError, invokeSafe } from '../../lib/ipc'
  import type { Batch } from '../../lib/types'
  import { STATUS_COLORS, STATUS_LABELS, formatAmount, formatDate } from '../../lib/types'
  import ConfirmDialog from '../../lib/ConfirmDialog.svelte'
  import BatchDetail from './BatchDetail.svelte'
  import CreateBatchModal from './CreateBatchModal.svelte'
  import DeliveryWorkspace from './DeliveryWorkspace.svelte'

  type Screen = 'list' | 'review' | 'delivery'
  let batches = $state<Batch[]>([])
  let loading = $state(true)
  let error = $state<string | null>(null)
  let showCreateModal = $state(false)
  let selectedBatch = $state<number | null>(null)
  let screen = $state<Screen>('list')
  let deleteCandidate = $state<Batch | null>(null)
  let searchQuery = $state('')
  let statusFilter = $state<'all' | 'draft' | 'ready'>('all')

  const visibleBatches = $derived(batches.filter((batch) =>
    (statusFilter === 'all' || (statusFilter === 'draft' ? batch.status === 'draft' : batch.status !== 'draft'))
      && (!searchQuery.trim() || batch.name.toLocaleLowerCase().includes(searchQuery.trim().toLocaleLowerCase())),
  ))
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
    screen = batch.status === 'draft' ? 'review' : 'delivery'
  }
  function backToList() { selectedBatch = null; screen = 'list'; void loadBatches() }
  function openReview() { screen = 'review' }
  function openDelivery() { screen = 'delivery' }
  function executeDeleteBatch() { if (deleteCandidate) void handleDelete(deleteCandidate.id) }

  $effect(() => { void loadBatches() })
</script>

{#if screen === 'review' && selectedBatch !== null}
  <BatchDetail batchId={selectedBatch} onUpdate={loadBatches} onBack={backToList} onOpenDelivery={openDelivery} />
{:else if screen === 'delivery' && selectedBatch !== null}
  <DeliveryWorkspace batchId={selectedBatch} onBackToList={backToList} onBackToReview={openReview} onReviewReopened={openReview} />
{:else}
  <div class="container">
    <header class="page-header">
      <div><span class="eyebrow">本地报销工作台</span><h1>批次</h1><p>创建整理容器，导入费用材料，完成审核后再选择交付方式。</p></div>
      <button class="create" type="button" onclick={() => (showCreateModal = true)}>＋ 新建批次</button>
    </header>

    <section class="task-summary" aria-label="批次任务概览">
      <button class:active={statusFilter === 'draft'} type="button" onclick={() => (statusFilter = statusFilter === 'draft' ? 'all' : 'draft')}><span>需要继续处理</span><strong>{draftCount}</strong><small>草稿批次</small></button>
      <button class:active={statusFilter === 'ready'} type="button" onclick={() => (statusFilter = statusFilter === 'ready' ? 'all' : 'ready')}><span>可交付或已交付</span><strong>{readyCount}</strong><small>已完成审核</small></button>
      <div><span>批次内计入金额</span><strong>{formatAmount(totalManagedAmount)}</strong><small>所有批次当前合计</small></div>
    </section>

    {#if loading}
      <p class="state">正在读取批次…</p>
    {:else if error}
      <p class="state error" role="alert">{error}</p>
    {:else if batches.length === 0}
      <section class="empty"><span>00</span><h2>还没有批次</h2><p>先创建一个批次；进入后选择邮件收集任务中的附件，或导入本地发票文件。</p><button type="button" onclick={() => (showCreateModal = true)}>创建第一个批次</button></section>
    {:else}
      <section class="batch-card" aria-labelledby="batch-list-title">
        <div class="list-heading"><div><span class="eyebrow">批次管理</span><h2 id="batch-list-title">按最近更新排序</h2></div><div class="list-tools"><label><span class="sr-only">搜索批次</span><input bind:value={searchQuery} placeholder="搜索批次名称" /></label><select bind:value={statusFilter} aria-label="批次状态"><option value="all">全部状态</option><option value="draft">草稿</option><option value="ready">已完成审核</option></select><strong>{visibleBatches.length}</strong></div></div>
        <div class="table-wrap">
          <table>
            <thead><tr><th>批次名称</th><th>状态</th><th>计入金额</th><th>费用数</th><th>创建时间</th><th><span class="sr-only">操作</span></th></tr></thead>
            <tbody>
              {#each visibleBatches as batch (batch.id)}
                <tr>
                  <td><button class="batch-link" type="button" onclick={() => openBatch(batch)}><strong>{batch.name}</strong><small>批次 #{batch.id}</small></button></td>
                  <td><span class="status" style={`--status-color:${STATUS_COLORS[batch.status]}`}>{STATUS_LABELS[batch.status]}</span></td>
                  <td class="amount">{formatAmount(batch.total_amount)}</td>
                  <td>{batch.invoice_count}</td>
                  <td>{formatDate(batch.created_at)}</td>
                  <td class="actions"><button class="open" type="button" onclick={() => openBatch(batch)}>{batch.status === 'draft' ? '继续审核' : '查看交付'} →</button>{#if batch.status === 'draft'}<button class="delete" type="button" onclick={() => (deleteCandidate = batch)}>删除</button>{/if}</td>
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
  .container{max-width:1500px;margin:0 auto;padding:2rem;color:#17232d}.page-header{display:flex;justify-content:space-between;gap:2rem;align-items:flex-end}.eyebrow{color:#68767d;font-family:'IBM Plex Mono',monospace;font-size:.72rem;font-weight:700;letter-spacing:.08em;text-transform:uppercase}h1{margin:.2rem 0 0;font-size:clamp(2rem,3vw,3rem);letter-spacing:-.045em}.page-header p{margin:.4rem 0 0;color:#596870}.create,.empty button{padding:.7rem 1rem;border:1px solid var(--pine,#136b52);background:var(--pine,#136b52);color:#fff;font-weight:700;cursor:pointer}
  .task-summary{display:grid;grid-template-columns:repeat(3,minmax(0,1fr));gap:.8rem;margin:1.5rem 0}.task-summary>button,.task-summary>div{display:grid;grid-template-columns:1fr auto;gap:.15rem .8rem;padding:.85rem 1rem;border:1px solid #cbd2d6;border-left:4px solid #7a8991;background:#fff;color:#17232d;text-align:left}.task-summary>button{cursor:pointer}.task-summary>button.active{border-left-color:#136b52;background:#edf6f1}.task-summary span{color:#65737a;font-size:.78rem}.task-summary strong{grid-row:1/3;grid-column:2;font-size:1.25rem;color:#136b52}.task-summary small{color:#69767d}
  .batch-card{border:1px solid #c2cbd0;background:#fff}.list-heading{display:flex;justify-content:space-between;align-items:end;gap:1rem;padding:1rem 1.15rem;border-bottom:1px solid #cbd2d6}.list-heading h2{margin:.2rem 0 0;font-size:1.1rem}.list-tools{display:flex;align-items:center;gap:.55rem}.list-tools input,.list-tools select{height:38px;padding:.45rem .6rem;border:1px solid #aeb9bf;background:#fff;font:inherit}.list-tools strong{min-width:34px;color:#136b52;font-family:'IBM Plex Mono',monospace;font-size:1.3rem;text-align:right}.table-wrap{overflow-x:auto}table{width:100%;border-collapse:collapse}th,td{padding:.8rem 1rem;border-bottom:1px solid #e0e5e7;text-align:left;vertical-align:middle}th{background:#f1f3f4;color:#596870;font-size:.75rem}tbody tr:hover{background:#f7faf8}.batch-link{display:grid;gap:.2rem;padding:0;border:0;background:transparent;color:#17232d;text-align:left;cursor:pointer}.batch-link strong{color:var(--pine,#136b52);font-size:.94rem}.batch-link small{color:#657078}.status{padding:.3rem .5rem;border-left:3px solid var(--status-color);background:#f5f7f7;font-size:.76rem;font-weight:700;white-space:nowrap}.amount{font-family:'IBM Plex Mono',monospace;font-weight:700}.actions{display:flex;gap:.7rem;justify-content:flex-end;white-space:nowrap}.open,.delete{padding:0;border:0;background:transparent;color:var(--pine,#136b52);font-weight:700;cursor:pointer}.delete{color:var(--risk,#b3453e);font-weight:500}.no-results{padding:2.5rem;color:#69767d;text-align:center}
  .state,.empty{margin-top:1.5rem;padding:2rem;border:1px solid #cfc8b8;background:#fbfaf6}.state.error{border-left:4px solid var(--risk,#b3453e);color:#862f2a}.empty{text-align:center}.empty>span{font-family:'IBM Plex Mono',monospace;color:#9aa19c}.empty h2{margin:.4rem 0}.empty p{color:#59645e}.sr-only{position:absolute;width:1px;height:1px;padding:0;margin:-1px;overflow:hidden;clip:rect(0,0,0,0);white-space:nowrap;border:0}
  @media(max-width:900px){.task-summary{grid-template-columns:1fr}.list-heading{align-items:stretch;flex-direction:column}.list-tools{flex-wrap:wrap}}@media(max-width:680px){.container{padding:1rem}.page-header{display:grid}.create{width:100%}.list-tools{display:grid}.list-tools input,.list-tools select{width:100%}}
</style>
