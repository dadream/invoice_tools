<script lang="ts">
  import { invokeSafe, describeError } from '../../lib/ipc'
  import type { Batch, Invoice, ParsedInvoice, TicketType } from '../../lib/types'
  import { STATUS_LABELS, STATUS_COLORS, ALLOWED_TRANSITIONS, formatAmount, formatDate } from '../../lib/types'
  import InvoicePicker from '../invoices/InvoicePicker.svelte'
  import ParseResultCard from '../invoices/ParseResultCard.svelte'
  import InvoiceList from '../invoices/InvoiceList.svelte'

  interface Props {
    batchId: number
    onUpdate: () => Promise<void>
  }

  let { batchId, onUpdate }: Props = $props()

  let batch = $state<Batch | null>(null)
  let loading = $state(true)
  let error = $state<string | null>(null)
  let transitioning = $state(false)

  let invoices = $state<Invoice[]>([])
  let invoicesError = $state<string | null>(null)
  // 待确认的解析结果；非空时用卡片替换选择区
  let pending = $state<{ parsed: ParsedInvoice; ticketType: TicketType } | null>(null)

  // 只有草稿批次能加票/删票，与后端校验一致（前后端双重拦截）
  const canEdit = $derived(batch?.status === 'draft')

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

  async function loadInvoices() {
    invoicesError = null
    const result = await invokeSafe<Invoice[]>('list_batch_invoices', { batchId })

    if (result.ok) {
      invoices = result.data
    } else {
      invoices = []
      invoicesError = describeError(result.error)
    }
  }

  /** 加票/删票后批次的金额合计与张数都会变，批次和列表都要刷新 */
  async function refreshAll() {
    await loadBatch()
    await loadInvoices()
    await onUpdate()
  }

  async function handleAdded() {
    pending = null
    await refreshAll()
  }

  async function handleTransition(newStatus: string) {
    if (!confirm(`确定要将批次状态改为"${STATUS_LABELS[newStatus as keyof typeof STATUS_LABELS]}"吗？`)) {
      return
    }

    transitioning = true
    // 参数名必须是 camelCase：#[tauri::command] 默认把 Rust 的 new_status
    // 重写为 newStatus（DTO 字段仍是 snake_case，两套大小写并存）
    const result = await invokeSafe<void>('transition_batch_status', {
      id: batchId,
      newStatus,
    })
    transitioning = false

    if (result.ok) {
      // 离开 Draft 后不能再加票，丢弃未确认的解析结果
      pending = null
      await loadBatch()
      await onUpdate()
    } else {
      alert(describeError(result.error))
    }
  }

  $effect(() => {
    // 读一下 batchId 让 effect 跟踪它；切批次时丢弃上一张待确认的票，
    // 否则会把 A 批次解析出的发票加到 B 批次
    batchId
    pending = null
    loadBatch()
    loadInvoices()
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

    <section class="info-section invoice-section">
      {#if canEdit}
        {#if pending}
          <ParseResultCard
            {batchId}
            parsed={pending.parsed}
            ticketType={pending.ticketType}
            onAdded={handleAdded}
            onCancel={() => (pending = null)}
          />
        {:else}
          <InvoicePicker
            onParsed={(parsed, ticketType) => (pending = { parsed, ticketType })}
          />
        {/if}
      {/if}

      {#if invoicesError}
        <p class="error" role="alert">{invoicesError}</p>
      {/if}

      <InvoiceList {invoices} {canEdit} onDeleted={refreshAll} />

      {#if !canEdit}
        <p class="locked">当前状态不是草稿，不能增删发票</p>
      {/if}
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

  .invoice-section { padding-top: 1rem; border-top: 1px solid #eee; }
  .locked { margin: 0; color: #999; font-size: 0.8rem; }

  .loading, .error { padding: 2rem 1rem; text-align: center; }
  .error { color: #c33; }
</style>
