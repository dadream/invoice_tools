<script lang="ts">
  import { invokeSafe, describeError } from '../../lib/ipc'
  import type { Batch } from '../../lib/types'
  import { STATUS_LABELS, STATUS_COLORS, ALLOWED_TRANSITIONS, formatAmount, formatDate } from '../../lib/types'

  interface Props {
    batchId: number
    onUpdate: () => Promise<void>
  }

  let { batchId, onUpdate }: Props = $props()

  let batch = $state<Batch | null>(null)
  let loading = $state(true)
  let error = $state<string | null>(null)
  let transitioning = $state(false)

  async function loadBatch() {
    loading = true
    error = null
    const result = await invokeSafe<Batch>('get_batch', { id: batchId })
    loading = false

    if (result.ok) {
      batch = result.data
    } else {
      error = describeError(result.error)
    }
  }

  async function handleTransition(newStatus: string) {
    if (!confirm(`确定要将批次状态改为"${STATUS_LABELS[newStatus as keyof typeof STATUS_LABELS]}"吗？`)) {
      return
    }

    transitioning = true
    const result = await invokeSafe<void>('transition_batch_status', {
      id: batchId,
      new_status: newStatus,
    })
    transitioning = false

    if (result.ok) {
      await loadBatch()
      await onUpdate()
    } else {
      alert(describeError(result.error))
    }
  }

  $effect(() => {
    loadBatch()
  })
</script>

<div class="detail">
  {#if loading}
    <p class="loading">加载中...</p>
  {:else if error}
    <p class="error">{error}</p>
  {:else if batch}
    <h2>{batch.name}</h2>

    <section class="info-section">
      <h3>基本信息</h3>
      <dl>
        <dt>批次 ID</dt>
        <dd>{batch.id}</dd>

        <dt>月份</dt>
        <dd>{batch.month}</dd>

        <dt>状态</dt>
        <dd>
          <span
            class="status-badge"
            style="background-color: {STATUS_COLORS[batch.status]}"
          >
            {STATUS_LABELS[batch.status]}
          </span>
        </dd>

        <dt>总金额</dt>
        <dd class="amount">{formatAmount(batch.total_amount)}</dd>

        <dt>发票数量</dt>
        <dd>{batch.invoice_count} 张</dd>
      </dl>
    </section>

    <section class="info-section">
      <h3>时间线</h3>
      <dl>
        <dt>创建时间</dt>
        <dd>{formatDate(batch.created_at)}</dd>

        {#if batch.submitted_at}
          <dt>提交时间</dt>
          <dd>{formatDate(batch.submitted_at)}</dd>
        {/if}

        {#if batch.approved_at}
          <dt>审批时间</dt>
          <dd>{formatDate(batch.approved_at)}</dd>
        {/if}

        {#if batch.completed_at}
          <dt>完成时间</dt>
          <dd>{formatDate(batch.completed_at)}</dd>
        {/if}

        {#if batch.rejected_at}
          <dt>驳回时间</dt>
          <dd class="rejected">{formatDate(batch.rejected_at)}</dd>
        {/if}
      </dl>
    </section>

    <section class="actions">
      <h3>状态操作</h3>
      {#if ALLOWED_TRANSITIONS[batch.status].length > 0}
        <div class="action-buttons">
          {#each ALLOWED_TRANSITIONS[batch.status] as targetStatus}
            <button
              class="btn-action"
              onclick={() => handleTransition(targetStatus)}
              disabled={transitioning}
            >
              {STATUS_LABELS[targetStatus]}
            </button>
          {/each}
        </div>
      {:else}
        <p class="no-actions">当前状态不能转换</p>
      {/if}
    </section>
  {/if}
</div>

<style>
  .detail { padding: 4rem 1.5rem 1.5rem; }
  h2 { margin: 0 0 1.5rem; font-size: 1.5rem; }
  h3 { margin: 0 0 1rem; font-size: 1rem; font-weight: 600; }

  .info-section { margin-bottom: 2rem; }

  dl { margin: 0; display: grid; grid-template-columns: 100px 1fr; gap: 0.5rem 1rem; }
  dt { font-weight: 500; color: #666; }
  dd { margin: 0; }

  .amount { font-weight: 600; color: #0070f3; font-size: 1.1rem; }
  .rejected { color: #c33; }

  .status-badge { padding: 0.25rem 0.5rem; border-radius: 4px; color: #fff; font-size: 0.85rem; }

  .actions { padding-top: 1rem; border-top: 1px solid #eee; }
  .action-buttons { display: flex; gap: 0.5rem; flex-wrap: wrap; }
  .btn-action {
    padding: 0.5rem 1rem;
    background: #0070f3;
    color: #fff;
    border: none;
    border-radius: 4px;
    cursor: pointer;
    font-size: 0.9rem;
  }
  .btn-action:hover:not(:disabled) { background: #0058c4; }
  .btn-action:disabled { opacity: 0.5; cursor: not-allowed; }

  .no-actions { color: #999; font-size: 0.9rem; }

  .loading, .error { padding: 2rem 1rem; text-align: center; }
  .error { color: #c33; }
</style>
