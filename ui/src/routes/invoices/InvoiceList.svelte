<script lang="ts">
  import { describeError, invokeSafe } from '../../lib/ipc'
  import type { Invoice } from '../../lib/types'
  import {
    TICKET_TYPE_LABELS,
    formatAmount,
    sumAmounts,
  } from '../../lib/types'
  import InvoiceEditModal from './InvoiceEditModal.svelte'

  interface Props {
    invoices: Invoice[]
    /** 仅 Draft 批次可编辑。 */
    canEdit: boolean
    onChanged: () => Promise<void>
  }

  let { invoices, canEdit, onChanged }: Props = $props()

  let deletingId = $state<number | null>(null)
  let clearingDuplicateId = $state<number | null>(null)
  let editingInvoice = $state<Invoice | null>(null)
  let error = $state<string | null>(null)

  const displayTotal = $derived(sumAmounts(invoices.map((invoice) => invoice.amount)))

  async function handleDelete(id: number) {
    if (!confirm('确定要从批次中移除这张发票吗？该操作不属于审核撤销记录。')) return
    deletingId = id
    error = null
    const result = await invokeSafe<void>('delete_invoice', { invoiceId: id })
    deletingId = null
    if (result.ok) {
      await onChanged()
    } else {
      error = describeError(result.error)
    }
  }

  async function handleClearDuplicate(id: number) {
    if (!confirm('确认这张发票不是重复项吗？该判断会写入审核记录，可在审核历史中撤销。')) return
    clearingDuplicateId = id
    error = null
    const result = await invokeSafe<void>('clear_duplicate_flag', { invoiceId: id })
    clearingDuplicateId = null
    if (result.ok) {
      await onChanged()
    } else {
      error = describeError(result.error)
    }
  }

  async function handleEdited() {
    editingInvoice = null
    await onChanged()
  }
</script>

<section class="list">
  <h3>发票明细（{invoices.length} 张）</h3>

  {#if error}
    <p class="error" role="alert">{error}</p>
  {/if}

  {#if invoices.length === 0}
    <p class="empty">该批次还没有发票</p>
  {:else}
    <div class="table-scroll">
      <table class="invoice-table">
        <thead>
          <tr>
            <th>发票号</th>
            <th>日期</th>
            <th>票种</th>
            <th class="num">金额</th>
            <th>销方</th>
            {#if canEdit}<th>操作</th>{/if}
          </tr>
        </thead>
        <tbody>
          {#each invoices as invoice (invoice.id)}
            <tr class:duplicate-row={invoice.is_duplicate}>
              <td class="number" title={invoice.invoice_number}>
                {invoice.invoice_number}
                {#if invoice.is_duplicate}
                  <span class="duplicate-icon" title={invoice.duplicate_reason ?? '标记为重复'}>重复</span>
                {/if}
              </td>
              <td>{invoice.issue_date}</td>
              <td>{TICKET_TYPE_LABELS[invoice.ticket_type] ?? invoice.ticket_type}</td>
              <td class="num">{formatAmount(invoice.amount)}</td>
              <td class="seller" title={invoice.seller_name ?? ''}>{invoice.seller_name ?? '—'}</td>
              {#if canEdit}
                <td class="actions">
                  <button class="btn-secondary btn-sm" onclick={() => (editingInvoice = invoice)}>编辑</button>
                  {#if invoice.is_duplicate}
                    <button
                      class="btn-secondary btn-sm"
                      onclick={() => handleClearDuplicate(invoice.id)}
                      disabled={clearingDuplicateId === invoice.id}
                    >
                      {clearingDuplicateId === invoice.id ? '处理中' : '确认非重复'}
                    </button>
                  {/if}
                  <button
                    class="btn-danger btn-sm"
                    onclick={() => handleDelete(invoice.id)}
                    disabled={deletingId === invoice.id}
                  >
                    {deletingId === invoice.id ? '删除中' : '删除'}
                  </button>
                </td>
              {/if}
            </tr>
          {/each}
        </tbody>
        <tfoot>
          <tr>
            <td colspan="3">合计</td>
            <td class="num total">{formatAmount(displayTotal)}</td>
            <td colspan={canEdit ? 2 : 1}></td>
          </tr>
        </tfoot>
      </table>
    </div>
    <p class="footnote">合计为前端展示值，精确金额以批次统计为准。</p>
  {/if}
</section>

{#if editingInvoice}
  <InvoiceEditModal
    invoice={editingInvoice}
    onSaved={handleEdited}
    onCancel={() => (editingInvoice = null)}
  />
{/if}

<style>
  .list { margin-bottom: 1.5rem; }
  h3 { margin: 0 0 1rem; font-size: 1rem; font-weight: 600; }
  .table-scroll { overflow-x: auto; }
  .invoice-table { width: 100%; min-width: 820px; border-collapse: collapse; font-size: 0.8rem; }
  .invoice-table th,
  .invoice-table td { padding: 0.4rem 0.5rem; text-align: left; border-bottom: 1px solid #eee; vertical-align: top; }
  .invoice-table th { background: #f5f5f5; font-weight: 600; }
  .invoice-table tbody tr:hover { background: #f9f9f9; }
  .invoice-table .num { text-align: right; }
  .duplicate-row { background: #fff0f0; }
  .duplicate-row:hover { background: #ffe8e8 !important; }
  .number { font-family: var(--font-mono); max-width: 9rem; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .duplicate-icon { margin-left: 0.25rem; font-size: 0.9rem; cursor: help; }
  .seller { max-width: 8rem; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .actions { display: flex; gap: 0.25rem; flex-wrap: wrap; min-width: 9rem; }
  tfoot td { font-weight: 600; border-bottom: none; }
  .total { color: var(--pine); }
  .footnote { margin: 0.5rem 0 0; font-size: 0.75rem; color: #999; }
  .empty { padding: 1rem 0; color: #999; font-size: 0.85rem; }
  .btn-danger, .btn-secondary { border: none; border-radius: 4px; cursor: pointer; }
  .btn-danger { background: var(--risk); color: #fff; }
  .btn-danger:hover:not(:disabled) { background: #942f29; }
  .btn-secondary { background: #eee; color: #333; }
  .btn-secondary:hover:not(:disabled) { background: #ddd; }
  button:disabled { opacity: 0.5; cursor: not-allowed; }
  .btn-sm { padding: 0.2rem 0.45rem; font-size: 0.75rem; }
  .error { margin: 0 0 0.75rem; padding: 0.5rem 0.75rem; background: #fdecec; border-radius: 4px; color: var(--risk); font-size: 0.85rem; }
</style>
