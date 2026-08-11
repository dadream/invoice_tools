<script lang="ts">
  import { invokeSafe, describeError } from '../../lib/ipc'
  import type { DuplicateCheck, Invoice, ParsedInvoice, TicketType } from '../../lib/types'
  import {
    PARSE_LEVEL_HINTS,
    PARSE_LEVEL_SEVERITY,
    TICKET_TYPE_LABELS,
    formatAmount,
  } from '../../lib/types'
  import { fileName } from '../../lib/invoice'

  interface Props {
    batchId: number
    parsed: ParsedInvoice
    /** 用户在选择阶段挑的票种，入库时要原样回传给后端重新解析 */
    ticketType: TicketType
    onAdded: (invoice: Invoice) => Promise<void>
    onCancel: () => void
  }

  let { batchId, parsed, ticketType, onAdded, onCancel }: Props = $props()

  let duplicate = $state<DuplicateCheck | null>(null)
  let checking = $state(true)
  let adding = $state(false)
  let error = $state<string | null>(null)

  const severity = $derived(PARSE_LEVEL_SEVERITY[parsed.parse_level] ?? 'warn')
  const levelHint = $derived(PARSE_LEVEL_HINTS[parsed.parse_level] ?? '解析级别未知，请人工核对')
  const isDuplicate = $derived(duplicate?.is_duplicate === true)
  // 查重未出结论前也不放行，避免抢在结论前把重复票塞进批次
  const canConfirm = $derived(!checking && !adding && !isDuplicate)

  // parsed 换了一张票就重新查重；invoice_number 变化即触发
  $effect(() => {
    const invoiceNumber = parsed.invoice_number
    let stale = false

    checking = true
    duplicate = null
    error = null

    invokeSafe<DuplicateCheck>('check_duplicate', { invoiceNumber }).then((result) => {
      if (stale) return
      checking = false
      if (result.ok) {
        duplicate = result.data
      } else {
        error = describeError(result.error)
      }
    })

    return () => {
      stale = true
    }
  })

  async function handleConfirm() {
    adding = true
    error = null

    // 只回传路径与票种，字段由后端重新解析，不信任前端数据
    const result = await invokeSafe<Invoice>('add_invoice_to_batch', {
      batchId,
      path: parsed.source_path,
      ticketType,
    })

    if (result.ok) {
      await onAdded(result.data)
      // 成功后由父组件卸载本卡片，这里不再改 adding，避免闪一下按钮可用态
    } else {
      adding = false
      error = describeError(result.error)
    }
  }
</script>

<section class="card">
  <header class="card-head">
    <h3>确认发票信息</h3>
    <span class="level level-{severity}">{parsed.parse_level}</span>
  </header>
  <p class="level-hint level-hint-{severity}">
    {levelHint}（置信度 {(parsed.confidence * 100).toFixed(0)}%）
  </p>

  {#if checking}
    <p class="banner banner-info">正在检查是否重复报销...</p>
  {:else if isDuplicate}
    <p class="banner banner-danger" role="alert">
      该发票已报销{duplicate?.existing_batch_name
        ? `，在《${duplicate.existing_batch_name}》中`
        : ''}，不能重复添加
    </p>
  {/if}

  <dl>
    <dt>发票号码</dt>
    <dd class="required">{parsed.invoice_number}</dd>

    <dt>开票日期</dt>
    <dd class="required">{parsed.issue_date}</dd>

    <dt>价税合计</dt>
    <dd class="required amount">{formatAmount(parsed.total_amount)}</dd>

    <dt>票种</dt>
    <dd>{TICKET_TYPE_LABELS[parsed.ticket_type] ?? parsed.ticket_type}</dd>

    <dt>税额</dt>
    <dd>
      {#if parsed.tax_amount}{formatAmount(parsed.tax_amount)}{:else}<span class="missing">未识别</span>{/if}
    </dd>

    <dt>税率</dt>
    <dd>
      {#if parsed.tax_rate}{parsed.tax_rate}{:else}<span class="missing">未识别</span>{/if}
    </dd>

    <dt>购方</dt>
    <dd>
      {#if parsed.buyer_name}{parsed.buyer_name}{:else}<span class="missing">未识别</span>{/if}
    </dd>

    <dt>销方</dt>
    <dd>
      {#if parsed.seller_name}{parsed.seller_name}{:else}<span class="missing">未识别</span>{/if}
    </dd>

    {#if parsed.city}
      <dt>城市</dt>
      <dd>{parsed.city}</dd>
    {/if}

    {#if parsed.departure_time}
      <dt>出发时间</dt>
      <dd>{parsed.departure_time}</dd>
    {/if}

    {#if parsed.checkin_date}
      <dt>入住日期</dt>
      <dd>{parsed.checkin_date}</dd>
    {/if}

    <dt>源文件</dt>
    <dd class="file" title={parsed.source_path}>{fileName(parsed.source_path)}</dd>
  </dl>

  {#if error}
    <p class="banner banner-danger" role="alert">{error}</p>
  {/if}

  <div class="card-actions">
    <button class="btn-secondary" onclick={onCancel} disabled={adding}>取消</button>
    <button class="btn-primary" onclick={handleConfirm} disabled={!canConfirm}>
      {adding ? '添加中...' : '确认添加'}
    </button>
  </div>
</section>

<style>
  .card {
    margin-bottom: 2rem;
    padding: 1rem;
    border: 1px solid #ddd;
    border-radius: 8px;
    background: #fff;
  }

  .card-head { display: flex; justify-content: space-between; align-items: center; }
  h3 { margin: 0; font-size: 1rem; font-weight: 600; }

  .level { padding: 0.15rem 0.45rem; border-radius: 4px; color: #fff; font-size: 0.8rem; }
  .level-ok { background: #0a7; }
  .level-warn { background: #d98000; }
  .level-danger { background: #c33; }

  .level-hint { margin: 0.4rem 0 1rem; font-size: 0.8rem; }
  .level-hint-ok { color: #666; }
  .level-hint-warn { color: #a06000; }
  .level-hint-danger { color: #c33; }

  .banner { margin: 0 0 1rem; padding: 0.5rem 0.75rem; border-radius: 4px; font-size: 0.85rem; }
  .banner-info { background: #f0f4f8; color: #666; }
  .banner-danger { background: #fdecec; color: #c33; font-weight: 500; }

  dl { margin: 0; display: grid; grid-template-columns: 80px 1fr; gap: 0.4rem 0.75rem; }
  dt { font-weight: 500; color: #666; font-size: 0.85rem; }
  dd { margin: 0; font-size: 0.85rem; word-break: break-all; }

  .required { font-weight: 600; }
  .amount { color: #0070f3; }
  .missing { color: #bbb; }
  .file { color: #666; }

  .card-actions { display: flex; gap: 0.5rem; justify-content: flex-end; margin-top: 1.25rem; }

  .btn-primary,
  .btn-secondary {
    padding: 0.5rem 1rem;
    border: none;
    border-radius: 4px;
    cursor: pointer;
    font-size: 0.9rem;
  }
  .btn-primary { background: #0070f3; color: #fff; }
  .btn-primary:hover:not(:disabled) { background: #0058c4; }
  .btn-secondary { background: #eee; color: #333; }
  .btn-secondary:hover:not(:disabled) { background: #ddd; }
  .btn-primary:disabled,
  .btn-secondary:disabled { opacity: 0.5; cursor: not-allowed; }
</style>
