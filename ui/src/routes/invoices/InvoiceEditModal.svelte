<script lang="ts">
  import { describeError, invokeSafe } from '../../lib/ipc'
  import type { Invoice, TicketType } from '../../lib/types'
  import { TICKET_TYPES, TICKET_TYPE_LABELS } from '../../lib/types'

  interface Props {
    invoice: Invoice
    onSaved: () => Promise<void>
    onCancel: () => void
  }

  let { invoice, onSaved, onCancel }: Props = $props()
  let invoiceNumber = $state('')
  let issueDate = $state('')
  let amount = $state('')
  let taxAmount = $state('')
  let buyerName = $state('')
  let sellerName = $state('')
  let ticketType = $state<TicketType>('other')
  let city = $state('')
  let departureTime = $state('')
  let checkinDate = $state('')
  let saving = $state(false)
  let error = $state<string | null>(null)

  $effect(() => {
    invoiceNumber = invoice.invoice_number
    issueDate = invoice.issue_date
    amount = invoice.amount
    taxAmount = invoice.tax_amount ?? ''
    buyerName = invoice.buyer_name ?? ''
    sellerName = invoice.seller_name ?? ''
    ticketType = invoice.ticket_type
    city = invoice.city ?? ''
    departureTime = invoice.departure_time
      ? invoice.departure_time.replace(' ', 'T').slice(0, 16)
      : ''
    checkinDate = invoice.checkin_date ?? ''
  })

  function optional(value: string): string | null {
    const trimmed = value.trim()
    return trimmed.length > 0 ? trimmed : null
  }

  async function save() {
    saving = true
    error = null
    const result = await invokeSafe<Invoice>('update_invoice_review', {
      invoiceId: invoice.id,
      input: {
        invoice_number: invoiceNumber,
        issue_date: issueDate,
        amount,
        tax_amount: optional(taxAmount),
        buyer_name: optional(buyerName),
        seller_name: optional(sellerName),
        ticket_type: ticketType,
        city: optional(city),
        departure_time: optional(departureTime),
        checkin_date: optional(checkinDate),
      },
    })
    saving = false
    if (!result.ok) {
      error = describeError(result.error)
      return
    }
    await onSaved()
  }

  function submit(event: SubmitEvent) {
    event.preventDefault()
    void save()
  }
</script>

<div class="backdrop" role="presentation" onclick={(event) => event.target === event.currentTarget && onCancel()}>
  <div class="modal" role="dialog" aria-modal="true" aria-labelledby="invoice-edit-title">
    <header>
      <div>
        <h3 id="invoice-edit-title">编辑发票字段</h3>
        <p>保存后会记录审核历史；日期、城市或票种变化后需重新确认归组。</p>
      </div>
      <button type="button" class="close" aria-label="关闭" onclick={onCancel}>×</button>
    </header>

    <form onsubmit={submit}>
      <div class="grid">
        <label class="wide">
          <span>发票号 *</span>
          <input bind:value={invoiceNumber} required maxlength="64" autocomplete="off" />
        </label>
        <label>
          <span>开票日期 *</span>
          <input type="date" bind:value={issueDate} required />
        </label>
        <label>
          <span>票据类型 *</span>
          <select bind:value={ticketType}>
            {#each TICKET_TYPES as type}
              <option value={type}>{TICKET_TYPE_LABELS[type]}</option>
            {/each}
          </select>
        </label>
        <label>
          <span>含税金额 *</span>
          <input bind:value={amount} inputmode="decimal" required placeholder="0.00" />
        </label>
        <label>
          <span>税额</span>
          <input bind:value={taxAmount} inputmode="decimal" placeholder="可留空" />
        </label>
        <label>
          <span>购方</span>
          <input bind:value={buyerName} maxlength="200" />
        </label>
        <label>
          <span>销方</span>
          <input bind:value={sellerName} maxlength="200" />
        </label>
        <label>
          <span>城市</span>
          <input bind:value={city} maxlength="100" />
        </label>
        <label>
          <span>出发时间</span>
          <input type="datetime-local" bind:value={departureTime} />
        </label>
        <label>
          <span>入住日期</span>
          <input type="date" bind:value={checkinDate} />
        </label>
      </div>

      {#if error}
        <p class="error" role="alert">{error}</p>
      {/if}

      <footer>
        <button type="button" class="secondary" onclick={onCancel} disabled={saving}>取消</button>
        <button type="submit" class="primary" disabled={saving}>
          {saving ? '保存中…' : '保存修改'}
        </button>
      </footer>
    </form>
  </div>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    z-index: 1000;
    display: grid;
    place-items: center;
    padding: 1rem;
    background: rgb(15 23 42 / 45%);
  }
  .modal {
    width: min(760px, 100%);
    max-height: calc(100vh - 2rem);
    overflow: auto;
    border-radius: 10px;
    background: #fff;
    box-shadow: 0 20px 45px rgb(15 23 42 / 25%);
  }
  header { display: flex; justify-content: space-between; gap: 1rem; padding: 1.1rem 1.25rem; border-bottom: 1px solid #e5e7eb; }
  h3 { margin: 0; font-size: 1.1rem; }
  header p { margin: 0.35rem 0 0; color: #64748b; font-size: 0.82rem; }
  .close { align-self: flex-start; border: 0; background: transparent; color: #64748b; font-size: 1.5rem; cursor: pointer; }
  form { padding: 1.25rem; }
  .grid { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 0.85rem 1rem; }
  label { display: grid; gap: 0.3rem; }
  label.wide { grid-column: 1 / -1; }
  label span { color: #475569; font-size: 0.8rem; font-weight: 600; }
  input, select { min-width: 0; padding: 0.55rem 0.65rem; border: 1px solid #cbd5e1; border-radius: 5px; background: #fff; font: inherit; }
  input:focus, select:focus { outline: 2px solid #bfdbfe; border-color: #2563eb; }
  .error { margin: 1rem 0 0; padding: 0.65rem; border-radius: 5px; background: #fef2f2; color: #b91c1c; font-size: 0.85rem; }
  footer { display: flex; justify-content: flex-end; gap: 0.6rem; margin-top: 1.1rem; }
  button { padding: 0.5rem 0.9rem; border-radius: 5px; cursor: pointer; }
  button:disabled { opacity: 0.55; cursor: not-allowed; }
  .secondary { border: 1px solid #cbd5e1; background: #fff; color: #334155; }
  .primary { border: 1px solid #1d4ed8; background: #2563eb; color: #fff; }
  @media (max-width: 640px) {
    .grid { grid-template-columns: 1fr; }
    label.wide { grid-column: auto; }
  }
</style>
