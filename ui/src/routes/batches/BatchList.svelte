<script lang="ts">
  import { invokeSafe, describeError } from '../../lib/ipc'
  import type { Batch } from '../../lib/types'
  import { STATUS_LABELS, STATUS_COLORS, formatAmount, formatDate } from '../../lib/types'
  import CreateBatchModal from './CreateBatchModal.svelte'
  import BatchDetail from './BatchDetail.svelte'

  let batches = $state<Batch[]>([])
  let loading = $state(true)
  let error = $state<string | null>(null)
  let showCreateModal = $state(false)
  let selectedBatch = $state<number | null>(null)

  async function loadBatches() {
    loading = true
    error = null
    const result = await invokeSafe<Batch[]>('list_batches')
    loading = false

    if (result.ok) {
      batches = result.data
    } else {
      error = describeError(result.error)
    }
  }

  async function handleCreate(name: string, month: string) {
    const result = await invokeSafe<number>('create_batch', { name, month })
    if (result.ok) {
      showCreateModal = false
      await loadBatches()
    } else {
      alert(describeError(result.error))
    }
  }

  async function handleDelete(id: number) {
    if (!confirm('确定要删除这个批次吗？')) return

    const result = await invokeSafe<void>('delete_batch', { id })
    if (result.ok) {
      await loadBatches()
    } else {
      alert(describeError(result.error))
    }
  }

  $effect(() => {
    loadBatches()
  })
</script>

<div class="container">
  <div class="header">
    <h1>批次管理</h1>
    <button class="btn-primary" onclick={() => (showCreateModal = true)}>
      创建批次
    </button>
  </div>

  {#if loading}
    <p class="loading">加载中...</p>
  {:else if error}
    <p class="error">{error}</p>
  {:else if batches.length === 0}
    <p class="empty">暂无批次，点击"创建批次"开始</p>
  {:else}
    <table class="batch-table">
      <thead>
        <tr>
          <th>批次名称</th>
          <th>月份</th>
          <th>状态</th>
          <th>总金额</th>
          <th>发票数</th>
          <th>创建时间</th>
          <th>操作</th>
        </tr>
      </thead>
      <tbody>
        {#each batches as batch (batch.id)}
          <tr class:selected={selectedBatch === batch.id}>
            <td>
              <button
                class="link-btn"
                onclick={() => (selectedBatch = batch.id)}
              >
                {batch.name}
              </button>
            </td>
            <td>{batch.month}</td>
            <td>
              <span
                class="status-badge"
                style="background-color: {STATUS_COLORS[batch.status]}"
              >
                {STATUS_LABELS[batch.status]}
              </span>
            </td>
            <td>{formatAmount(batch.total_amount)}</td>
            <td>{batch.invoice_count}</td>
            <td>{formatDate(batch.created_at)}</td>
            <td>
              {#if batch.status === 'draft'}
                <button
                  class="btn-danger btn-sm"
                  onclick={() => handleDelete(batch.id)}
                >
                  删除
                </button>
              {/if}
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  {/if}

  {#if showCreateModal}
    <CreateBatchModal
      onSubmit={handleCreate}
      onCancel={() => (showCreateModal = false)}
    />
  {/if}

  {#if selectedBatch !== null}
    <div class="detail-panel">
      <button class="close-btn" onclick={() => (selectedBatch = null)}>✕</button>
      <BatchDetail batchId={selectedBatch} onUpdate={loadBatches} />
    </div>
  {/if}
</div>

<style>
  .container { padding: 2rem; max-width: 1200px; margin: 0 auto; }
  .header { display: flex; justify-content: space-between; align-items: center; margin-bottom: 2rem; }
  h1 { margin: 0; }

  .batch-table { width: 100%; border-collapse: collapse; background: #fff; border-radius: 8px; overflow: hidden; }
  .batch-table th,
  .batch-table td { padding: 0.75rem 1rem; text-align: left; border-bottom: 1px solid #eee; }
  .batch-table th { background: #f5f5f5; font-weight: 600; }
  .batch-table tbody tr:hover { background: #f9f9f9; }
  .batch-table tbody tr.selected { background: #e3f2fd; }

  .status-badge { padding: 0.25rem 0.5rem; border-radius: 4px; color: #fff; font-size: 0.85rem; }

  .link-btn { background: none; border: none; color: #0070f3; cursor: pointer; padding: 0; text-decoration: underline; }
  .link-btn:hover { color: #0058c4; }

  .btn-primary { padding: 0.5rem 1rem; background: #0070f3; color: #fff; border: none; border-radius: 4px; cursor: pointer; }
  .btn-primary:hover { background: #0058c4; }

  .btn-danger { padding: 0.5rem 1rem; background: #c33; color: #fff; border: none; border-radius: 4px; cursor: pointer; }
  .btn-danger:hover { background: #a22; }

  .btn-sm { padding: 0.25rem 0.5rem; font-size: 0.85rem; }

  .loading, .error, .empty { padding: 2rem; text-align: center; }
  .error { color: #c33; }

  .detail-panel {
    position: fixed;
    top: 0;
    right: 0;
    bottom: 0;
    width: 400px;
    background: #fff;
    box-shadow: -2px 0 8px rgba(0,0,0,0.1);
    overflow-y: auto;
    z-index: 100;
  }

  .close-btn {
    position: absolute;
    top: 1rem;
    right: 1rem;
    background: none;
    border: none;
    font-size: 1.5rem;
    cursor: pointer;
    color: #999;
  }
  .close-btn:hover { color: #333; }
</style>
