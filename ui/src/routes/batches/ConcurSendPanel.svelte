<script lang="ts">
  import { invokeSafe, describeError } from '../../lib/ipc'
  import type { BatchStatus, Invoice } from '../../lib/types'

  interface Capability {
    enabled: boolean
    manualSendOnly: boolean
    maxAttachmentsPerMessage: number
    maxAttachmentMib: number
    maxMessageAttachmentMib: number
    supportedFormats: string[]
  }

  interface Session {
    batchId: number
    senderEmail: string
    recipientEmail: string
    trialInvoiceId: number
    trialStatus: 'not_started' | 'sending' | 'sent' | 'confirmed' | 'failed' | 'unknown'
    confirmedBehavior: 'receipt_library' | 'expenseit' | null
    confirmedAt: string | null
  }

  interface SendItem {
    invoiceId: number
    attachmentName: string
    attachmentBytes: number | null
    status: 'pending' | 'sending' | 'sent' | 'failed' | 'unknown'
    attemptCount: number
    lastError: string | null
    sentAt: string | null
  }

  interface SendStatus {
    enabled: boolean
    session: Session | null
    items: SendItem[]
  }

  interface SendResult {
    outcome: 'sent' | 'failed' | 'unknown' | 'skipped' | 'complete'
    sentCount: number
    failedCount: number
    unknownCount: number
    skippedCount: number
    messageIds: string[]
    message: string
  }

  interface Props {
    batchId: number
    batchStatus: BatchStatus
    invoices: Invoice[]
  }

  let { batchId, batchStatus, invoices }: Props = $props()
  let capability = $state<Capability | null>(null)
  let status = $state<SendStatus | null>(null)
  let loading = $state(true)
  let working = $state(false)
  let error = $state<string | null>(null)
  let notice = $state<string | null>(null)
  let recipientEmail = $state('')
  let trialInvoiceId = $state<number | null>(null)
  let planConfirmed = $state(false)
  let sendConfirmed = $state(false)

  const canPrepare = $derived(['approved', 'completed'].includes(batchStatus))
  const eligibleInvoices = $derived(invoices.filter((invoice) => !invoice.is_excluded))
  const trialItem = $derived(
    status?.session
      ? status.items.find((item) => item.invoiceId === status?.session?.trialInvoiceId) ?? null
      : null,
  )
  const counts = $derived.by(() => {
    const values = { pending: 0, sending: 0, sent: 0, failed: 0, unknown: 0 }
    for (const item of status?.items ?? []) values[item.status] += 1
    return values
  })

  const STATUS_TEXT: Record<SendItem['status'], string> = {
    pending: '待发送',
    sending: '发送中',
    sent: '已发送',
    failed: '可重试',
    unknown: '送达未知',
  }

  function shortInvoice(invoice: Invoice): string {
    const suffix = invoice.invoice_number.slice(-6)
    return `${invoice.issue_date} · ¥${invoice.amount} · 票号后六位 ${suffix}`
  }

  function formatBytes(bytes: number | null): string {
    if (bytes === null) return '文件不可读'
    if (bytes < 1024) return `${bytes} B`
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KiB`
    return `${(bytes / 1024 / 1024).toFixed(1)} MiB`
  }

  async function load() {
    loading = true
    error = null
    const [capabilityResult, statusResult] = await Promise.all([
      invokeSafe<Capability>('get_concur_capability'),
      invokeSafe<SendStatus>('get_concur_send_status', { batchId }),
    ])
    loading = false
    if (!capabilityResult.ok) {
      error = describeError(capabilityResult.error)
      return
    }
    capability = capabilityResult.data
    if (!statusResult.ok) {
      error = describeError(statusResult.error)
      return
    }
    status = statusResult.data
    if (status.session) {
      recipientEmail = status.session.recipientEmail
      trialInvoiceId = status.session.trialInvoiceId
    } else if (trialInvoiceId === null && eligibleInvoices.length > 0) {
      trialInvoiceId = eligibleInvoices[0].id
    }
  }

  async function preparePlan() {
    if (!planConfirmed || trialInvoiceId === null) return
    working = true
    error = null
    notice = null
    const result = await invokeSafe<SendStatus>('prepare_concur_send', {
      batchId,
      recipientEmail,
      trialInvoiceId,
    })
    working = false
    if (!result.ok) {
      error = describeError(result.error)
      return
    }
    status = result.data
    notice = '发送计划已固定并校验哈希；此步骤未联网，也未发送邮件。'
    sendConfirmed = false
  }

  async function sendTrial() {
    if (!sendConfirmed) return
    await runSend('send_concur_trial')
  }

  async function sendRemaining() {
    if (!sendConfirmed) return
    await runSend('send_concur_remaining')
  }

  async function runSend(command: 'send_concur_trial' | 'send_concur_remaining') {
    working = true
    error = null
    notice = null
    const result = await invokeSafe<SendResult>(command, { batchId, userConfirmed: true })
    working = false
    sendConfirmed = false
    if (!result.ok) {
      error = describeError(result.error)
      return
    }
    notice = `${result.data.message}（成功 ${result.data.sentCount}，可重试 ${result.data.failedCount}，未知 ${result.data.unknownCount}）`
    await load()
  }

  async function confirmBehavior(behavior: 'receipt_library' | 'expenseit') {
    working = true
    error = null
    const result = await invokeSafe<SendStatus>('confirm_concur_trial', { batchId, behavior })
    working = false
    if (!result.ok) {
      error = describeError(result.error)
      return
    }
    status = result.data
    notice = '试发结果已记录，现在只会发送剩余待处理收据。'
  }

  async function resolveUnknown(invoiceId: number, delivered: boolean) {
    working = true
    error = null
    const result = await invokeSafe<SendStatus>('resolve_concur_unknown', {
      batchId,
      invoiceId,
      delivered,
    })
    working = false
    if (!result.ok) {
      error = describeError(result.error)
      return
    }
    status = result.data
    notice = delivered
      ? '已按您在 Concur 中的核对结果标记为送达，不会重发。'
      : '已标记为未送达，该收据现在可安全重试。'
  }

  $effect(() => {
    batchId
    status = null
    notice = null
    error = null
    planConfirmed = false
    sendConfirmed = false
    load()
  })
</script>

<section class="concur-panel" aria-labelledby="concur-title">
  <div class="heading">
    <div>
      <h3 id="concur-title">发送到 Concur 收据库</h3>
      <p>只发送您审核后的收据附件；不会创建、填写、关联或提交报销单。</p>
    </div>
    {#if capability}
      <span class:enabled={capability.enabled} class="build-state">
        {capability.enabled ? '真实发送已启用' : '内部 Alpha：真实发送关闭'}
      </span>
    {/if}
  </div>

  {#if loading}
    <p class="muted">正在读取本地发送状态...</p>
  {:else}
    {#if capability && !capability.enabled}
      <div class="safety-note" role="status">
        当前构建在后端硬性关闭 SMTP。您可以建立和检查计划，但按钮不会连接邮箱或 Concur。
      </div>
    {/if}

    {#if error}<p class="error" role="alert">{error}</p>{/if}
    {#if notice}<p class="notice" role="status">{notice}</p>{/if}

    {#if !canPrepare}
      <p class="muted">批次审批完成后才可建立 Concur 发送计划。</p>
    {:else if !status?.session}
      <div class="setup-grid">
        <label>
          <span>Concur 收件地址</span>
          <input
            type="email"
            bind:value={recipientEmail}
            placeholder="粘贴贵公司 Concur 提供的收件地址"
            autocomplete="off"
          />
          <small>系统不猜测或默认收件地址。发件邮箱须已在您的 Concur 账户中验证。</small>
        </label>
        <label>
          <span>选择一张代表性收据试发</span>
          <select bind:value={trialInvoiceId} disabled={eligibleInvoices.length === 0}>
            {#each eligibleInvoices as invoice}
              <option value={invoice.id}>{shortInvoice(invoice)}</option>
            {/each}
          </select>
        </label>
      </div>
      <p class="format-note">
        支持 {capability?.supportedFormats.join('、')}；单件不超过 {capability?.maxAttachmentMib} MiB。
        XML/OFD 必须先转换为受支持的可视收据文件。
      </p>
      <label class="confirmation">
        <input type="checkbox" bind:checked={planConfirmed} />
        <span>我已核对收件地址、试发收据，并确认当前发件邮箱已在 Concur 中验证。</span>
      </label>
      <button
        class="primary"
        onclick={preparePlan}
        disabled={working || !planConfirmed || !recipientEmail || trialInvoiceId === null}
      >
        {working ? '校验中...' : '建立试发计划（不联网）'}
      </button>
    {:else}
      <dl class="session-summary">
        <dt>发件邮箱</dt><dd>{status.session.senderEmail}</dd>
        <dt>Concur 收件地址</dt><dd>{status.session.recipientEmail}</dd>
        <dt>试发附件</dt><dd>{trialItem?.attachmentName ?? '—'} · {formatBytes(trialItem?.attachmentBytes ?? null)}</dd>
        <dt>批次进度</dt><dd>已发送 {counts.sent} · 待发送 {counts.pending} · 可重试 {counts.failed} · 未知 {counts.unknown}</dd>
      </dl>

      {#if status.session.trialStatus === 'not_started' || status.session.trialStatus === 'failed'}
        <div class="step-card">
          <strong>步骤 1：发送一张测试收据</strong>
          <p>仅发送上方一张附件。SMTP 接受不代表 Concur 已处理，发送后必须到 Concur 核对。</p>
          <label class="confirmation">
            <input type="checkbox" bind:checked={sendConfirmed} disabled={!capability?.enabled} />
            <span>我确认现在向上述地址发送这一张测试收据。</span>
          </label>
          <button class="primary" onclick={sendTrial} disabled={working || !sendConfirmed || !capability?.enabled}>
            {working ? '发送中...' : '发送一张测试收据'}
          </button>
        </div>
      {:else if status.session.trialStatus === 'sending'}
        <p class="warning">试发正在进行，请等待结果，不要重复点击或关闭应用。</p>
      {:else if status.session.trialStatus === 'unknown'}
        <div class="step-card danger">
          <strong>试发送达结果未知</strong>
          <p>请先登录 Concur 搜索这张收据。未核对前系统禁止重试。</p>
          <div class="button-row">
            <button onclick={() => resolveUnknown(status!.session!.trialInvoiceId, true)} disabled={working}>
              已在 Concur 看到，标记送达
            </button>
            <button onclick={() => resolveUnknown(status!.session!.trialInvoiceId, false)} disabled={working}>
              已确认未看到，允许重试
            </button>
          </div>
        </div>
      {:else if status.session.trialStatus === 'sent'}
        <div class="step-card">
          <strong>步骤 2：到 Concur 核对试发结果</strong>
          <p>请选择实际观察到的行为。该选择一经确认不能更改。</p>
          <div class="button-row">
            <button class="primary" onclick={() => confirmBehavior('receipt_library')} disabled={working}>
              只进入 Available Receipts
            </button>
            <button onclick={() => confirmBehavior('expenseit')} disabled={working}>
              自动生成了费用条目
            </button>
          </div>
        </div>
      {:else if status.session.trialStatus === 'confirmed'}
        <div class="step-card">
          <strong>步骤 3：发送剩余收据</strong>
          <p>
            已确认租户行为：{status.session.confirmedBehavior === 'expenseit' ? '生成费用条目' : '进入收据库'}。
            每封最多 {capability?.maxAttachmentsPerMessage} 个附件、合计不超过 {capability?.maxMessageAttachmentMib} MiB；已成功项不会重发。
          </p>
          {#if counts.pending + counts.failed > 0}
            <label class="confirmation">
              <input type="checkbox" bind:checked={sendConfirmed} disabled={!capability?.enabled || counts.unknown > 0} />
              <span>我确认发送剩余 {counts.pending + counts.failed} 张待处理收据。</span>
            </label>
            <button class="primary" onclick={sendRemaining} disabled={working || !sendConfirmed || !capability?.enabled || counts.unknown > 0}>
              {working ? '发送中...' : `发送剩余 ${counts.pending + counts.failed} 张`}
            </button>
          {:else}
            <p class="success">当前计划中的收据均已处理。</p>
          {/if}
        </div>
      {/if}

      {#if status.items.length > 0}
        <details class="item-list" open={counts.failed + counts.unknown > 0}>
          <summary>查看逐张发送状态（{status.items.length}）</summary>
          <div class="table-wrap">
            <table>
              <thead><tr><th>附件</th><th>大小</th><th>状态</th><th>尝试</th><th>处理</th></tr></thead>
              <tbody>
                {#each status.items as item}
                  <tr class:problem={item.status === 'failed' || item.status === 'unknown'}>
                    <td><code>{item.attachmentName}</code></td>
                    <td>{formatBytes(item.attachmentBytes)}</td>
                    <td>{STATUS_TEXT[item.status]}</td>
                    <td>{item.attemptCount}</td>
                    <td>
                      {#if item.status === 'unknown'}
                        <div class="compact-actions">
                          <button onclick={() => resolveUnknown(item.invoiceId, true)} disabled={working}>已送达</button>
                          <button onclick={() => resolveUnknown(item.invoiceId, false)} disabled={working}>未送达</button>
                        </div>
                      {:else if item.lastError}
                        <span title={item.lastError}>发送前失败，可重试</span>
                      {:else}—{/if}
                    </td>
                  </tr>
                {/each}
              </tbody>
            </table>
          </div>
        </details>
      {/if}
    {/if}
  {/if}
</section>

<style>
  .concur-panel { margin-top: 1.25rem; padding: 1rem; border: 1px solid var(--line); background: #fbfaf6; }
  .heading { display: flex; justify-content: space-between; gap: 1rem; align-items: flex-start; }
  h3 { margin: 0 0 0.35rem; font-size: 1rem; }
  .heading p, .step-card p { margin: 0; color: #59645e; line-height: 1.5; font-size: 0.88rem; }
  .build-state { flex: none; padding: 0.3rem 0.55rem; border-radius: 999px; background: #ece7dc; color: #665d4d; font-size: 0.76rem; font-weight: 600; }
  .build-state.enabled { background: #dcece3; color: #1f5b43; }
  .safety-note, .notice, .warning, .error { margin: 0.9rem 0; padding: 0.7rem 0.8rem; border-left: 4px solid #c28b32; background: #fff4d9; font-size: 0.86rem; line-height: 1.5; }
  .notice { border-color: #3f8b68; background: #e8f3ed; color: #1e573f; }
  .error { border-color: #b4433d; background: #fbe9e7; color: #8b2924; }
  .warning { color: #7b4b07; }
  .muted, .format-note { color: #69746e; font-size: 0.86rem; }
  .setup-grid { display: grid; gap: 0.9rem; margin-top: 1rem; }
  label > span { display: block; margin-bottom: 0.35rem; font-weight: 600; font-size: 0.86rem; }
  input[type='email'], select { width: 100%; box-sizing: border-box; padding: 0.55rem 0.65rem; border: 1px solid #bfc5bf; border-radius: 4px; background: #fff; }
  small { display: block; margin-top: 0.35rem; color: #69746e; }
  .confirmation { display: flex; gap: 0.55rem; align-items: flex-start; margin: 0.8rem 0; padding: 0.7rem; background: #f3f0e8; }
  .confirmation > span { margin: 0; font-weight: 500; line-height: 1.45; }
  .confirmation input { margin-top: 0.2rem; }
  button { padding: 0.48rem 0.75rem; border: 1px solid #9ba49d; border-radius: 4px; background: #fff; color: #2e3b34; cursor: pointer; }
  button.primary { border-color: var(--pine); background: var(--pine); color: #fff; }
  button:disabled { opacity: 0.5; cursor: not-allowed; }
  .session-summary { margin: 1rem 0; display: grid; grid-template-columns: 130px minmax(0, 1fr); gap: 0.45rem 0.8rem; }
  .session-summary dt { color: #657068; font-weight: 600; }
  .session-summary dd { margin: 0; overflow-wrap: anywhere; }
  .step-card { margin-top: 0.9rem; padding: 0.85rem; border: 1px solid #d4cdbc; background: #fff; }
  .step-card strong { display: block; margin-bottom: 0.4rem; }
  .step-card.danger { border-color: #c56b65; background: #fff6f5; }
  .button-row, .compact-actions { display: flex; gap: 0.5rem; flex-wrap: wrap; margin-top: 0.75rem; }
  .success { color: #1f684b !important; font-weight: 600; }
  .item-list { margin-top: 1rem; border-top: 1px solid var(--line); padding-top: 0.75rem; }
  .item-list summary { cursor: pointer; font-weight: 600; }
  .table-wrap { overflow-x: auto; margin-top: 0.65rem; }
  table { width: 100%; border-collapse: collapse; font-size: 0.78rem; }
  th, td { padding: 0.45rem; border-bottom: 1px solid #e2ded5; text-align: left; vertical-align: top; }
  th { color: #59645e; }
  td code { overflow-wrap: anywhere; }
  tr.problem { background: #fff8ed; }
  .compact-actions { margin: 0; }
  .compact-actions button { padding: 0.25rem 0.4rem; font-size: 0.72rem; }
  @media (max-width: 720px) {
    .heading { display: grid; }
    .session-summary { grid-template-columns: 1fr; }
    .session-summary dt { margin-top: 0.35rem; }
  }
</style>
