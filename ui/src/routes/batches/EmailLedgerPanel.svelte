<script lang="ts">
  import { listen, type UnlistenFn } from '@tauri-apps/api/event'
  import { open } from '@tauri-apps/plugin-dialog'
  import { onDestroy } from 'svelte'
  import { describeError, invokeSafe } from '../../lib/ipc'
  import type { EmailImportAttachment, EmailImportMessage, EmailImportMessageStatus } from '../../lib/types'

  interface Props {
    batchId: number
    batchName: string
    batchMonth: string
    messages: EmailImportMessage[]
    canEdit: boolean
    onChanged: () => Promise<void>
    onOpenInvoice: (invoiceId: number) => void
  }

  interface PipelineComplete { batch_id: number }
  interface PipelineError { stage: string; message: string }
  type Filter = 'all' | 'action' | 'imported' | 'not_invoice'

  let { batchId, batchName, batchMonth, messages, canEdit, onChanged, onOpenInvoice }: Props = $props()
  let selectedId = $state<number | null>(null)
  let filter = $state<Filter>('all')
  let working = $state(false)
  let error = $state<string | null>(null)
  let notice = $state<string | null>(null)
  let activePipelineId = $state<string | null>(null)
  let unlisteners: UnlistenFn[] = []

  const actionableStatuses: EmailImportMessageStatus[] = ['manual_download', 'needs_confirmation', 'failed']
  const filteredMessages = $derived(messages.filter((message) => {
    if (filter === 'action') return message.resolution_status === 'open' && actionableStatuses.includes(message.status)
    if (filter === 'imported') return message.status === 'imported'
    if (filter === 'not_invoice') return message.status === 'not_invoice'
    return true
  }))
  const selected = $derived(messages.find((message) => message.id === selectedId) ?? filteredMessages[0] ?? null)
  const actionCount = $derived(messages.filter((message) => message.resolution_status === 'open' && actionableStatuses.includes(message.status)).length)
  const importedCount = $derived(messages.filter((message) => message.status === 'imported').length)
  const relatedCount = $derived(messages.filter((message) => message.status === 'needs_attachment_review').length)

  const statusLabels: Record<EmailImportMessageStatus, string> = {
    imported: '已导入',
    needs_attachment_review: '材料待处理',
    manual_download: '需手工下载',
    needs_confirmation: '需确认',
    not_invoice: '非发票邮件',
    failed: '处理失败',
  }
  const attachmentLabels: Record<EmailImportAttachment['status'], string> = {
    invoice: '发票', supporting: '配套材料', duplicate: '重复文件', not_invoice: '已跳过', unsupported: '不支持', failed: '处理失败',
  }
  const roleLabels: Record<EmailImportAttachment['role_hint'], string> = {
    invoice: '主发票', itinerary: '行程单', detail: '消费明细', supporting: '其他材料', unknown: '待判断',
  }

  function cleanupListeners() { unlisteners.forEach((unlisten) => unlisten()); unlisteners = [] }
  function formatBytes(bytes: number): string {
    if (bytes < 1024) return `${bytes} B`
    if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} KiB`
    return `${(bytes / 1024 / 1024).toFixed(1)} MiB`
  }
  function today(): string {
    const value = new Date()
    const year = value.getFullYear()
    const month = String(value.getMonth() + 1).padStart(2, '0')
    const day = String(value.getDate()).padStart(2, '0')
    return `${year}-${month}-${day}`
  }

  async function resolveMessage(action: 'resolve' | 'ignore' | 'reopen') {
    if (!selected || working) return
    working = true; error = null; notice = null
    const result = await invokeSafe<void>('resolve_email_import_message', { messageId: selected.id, action })
    working = false
    if (!result.ok) { error = describeError(result.error); return }
    notice = action === 'reopen' ? '邮件已重新打开。' : action === 'ignore' ? '已记录为无需处理。' : '已记录为处理完成。'
    await onChanged()
  }

  async function importDownloadedFiles() {
    if (!selected || working || !canEdit) return
    const picked = await open({
      multiple: true,
      directory: false,
      filters: [{ name: '发票与配套材料', extensions: ['xml', 'ofd', 'pdf', 'png', 'jpg', 'jpeg', 'webp', 'bmp'] }],
    })
    const paths = Array.isArray(picked) ? picked : picked ? [picked] : []
    if (paths.length === 0) return
    cleanupListeners()
    error = null; notice = null; working = true
    const pipelineId = crypto.randomUUID()
    activePipelineId = pipelineId
    unlisteners.push(await listen<PipelineComplete>(`pipeline:complete:${pipelineId}`, async () => {
      cleanupListeners(); working = false; activePipelineId = null
      notice = '下载文件已导入，并已关联到这封邮件。'
      await onChanged()
    }))
    unlisteners.push(await listen<PipelineError>(`pipeline:error:${pipelineId}`, (event) => {
      cleanupListeners(); working = false; activePipelineId = null
      error = `[${event.payload.stage}] ${event.payload.message}`
    }))
    const day = today()
    const result = await invokeSafe<void>('start_pipeline', {
      pipelineId,
      config: {
        batch_name: batchName,
        month: batchMonth,
        target_batch_id: batchId,
        source: { kind: 'local', paths, target_email_message_id: selected.id },
        date_range: { start: day, end: day },
      },
    })
    if (!result.ok) {
      cleanupListeners(); working = false; activePipelineId = null
      error = describeError(result.error)
    }
  }

  $effect(() => {
    if (selectedId !== null && !messages.some((message) => message.id === selectedId)) selectedId = null
  })
  onDestroy(cleanupListeners)
</script>

{#if messages.length === 0}
  <section class="legacy-empty">
    <span>邮件台账</span>
    <h3>这个批次没有逐封邮件记录</h3>
    <p>它可能由本地文件创建，或是在邮件台账功能上线前导入。现有费用和原件不会受影响；重新从邮箱导入时将建立完整台账。</p>
  </section>
{:else}
  <section class="ledger-shell">
    <header class="ledger-summary">
      <div><span>邮件处理台账</span><strong>{messages.length}</strong><small>扫描邮件</small></div>
      <div><span>已导入</span><strong>{importedCount}</strong><small>含有效发票</small></div>
      <div><span>材料待处理</span><strong>{relatedCount}</strong><small>不计入金额</small></div>
      <div class:attention={actionCount > 0}><span>需要操作</span><strong>{actionCount}</strong><small>下载、确认或重试</small></div>
    </header>

    {#if error}<p class="message error" role="alert">{error}</p>{/if}
    {#if notice}<p class="message notice" role="status">{notice}</p>{/if}

    <div class="ledger-workspace">
      <aside class="message-column">
        <nav aria-label="邮件台账筛选">
          <button class:active={filter === 'all'} type="button" onclick={() => (filter = 'all')}>全部 {messages.length}</button>
          <button class:active={filter === 'action'} type="button" onclick={() => (filter = 'action')}>待操作 {actionCount}</button>
          <button class:active={filter === 'imported'} type="button" onclick={() => (filter = 'imported')}>已导入 {importedCount}</button>
          <button class:active={filter === 'not_invoice'} type="button" onclick={() => (filter = 'not_invoice')}>非发票</button>
        </nav>
        <div class="message-list">
          {#each filteredMessages as message (message.id)}
            <button class:selected={selected?.id === message.id} type="button" onclick={() => (selectedId = message.id)}>
              <div><span class={`state ${message.status}`}>{statusLabels[message.status]}</span>{#if message.resolution_status !== 'open'}<i>{message.resolution_status === 'ignored' ? '已忽略' : '已处理'}</i>{/if}</div>
              <strong>{message.subject || '（无主题）'}</strong>
              <small>{message.sender || '未知发件人'}</small>
              <small>{message.received_at ?? '收件时间未知'} · {message.attachments.length} 个附件</small>
            </button>
          {:else}
            <p class="filter-empty">当前筛选没有邮件。</p>
          {/each}
        </div>
      </aside>

      <article class="message-detail">
        {#if selected}
          <header>
            <div><span class={`state ${selected.status}`}>{statusLabels[selected.status]}</span><h3>{selected.subject || '（无主题）'}</h3><p>{selected.sender || '未知发件人'} · {selected.received_at ?? '收件时间未知'} · {selected.mailbox_folder}</p></div>
            <code>UID {selected.uid}</code>
          </header>

          {#if selected.status === 'manual_download'}
            <div class="action-guidance"><strong>软件没有访问邮件正文链接</strong><p>请先在邮箱客户端核对发件人和域名，手工下载文件，再从这里导入。导入文件会继续保留与本邮件的关系。</p></div>
          {:else if selected.status === 'needs_confirmation'}
            <div class="action-guidance"><strong>检测到开票通知，但没有取得凭证</strong><p>请在邮箱或订单平台确认发票是否另行发送；取得文件后使用“下载后导入”。</p></div>
          {:else if selected.status === 'failed'}
            <div class="action-guidance risk"><strong>邮件处理未完成</strong><p>错误类别：{selected.error_category ?? 'unknown'}。请重新运行邮箱导入，或从邮箱下载文件后补充导入。</p></div>
          {/if}

          <section class="attachment-pack">
            <div class="pack-title"><div><span>同一邮件材料包</span><h4>{selected.attachments.length} 个逻辑附件</h4></div><small>这些文件来自同一封邮件，审核归属时应一起核对。</small></div>
            {#each selected.attachments as attachment (attachment.id)}
              <div class="attachment-row">
                <div class="file-mark">{attachment.original_name.split('.').pop()?.slice(0, 4).toUpperCase() || 'FILE'}</div>
                <div class="file-info"><strong>{attachment.original_name}</strong><span>{attachmentLabels[attachment.status]} · {roleLabels[attachment.role_hint]} · {formatBytes(attachment.byte_len)}</span>{#if attachment.container_name}<small>来自压缩包：{attachment.container_name}</small>{/if}{#if attachment.manual_import}<small>用户下载后补充导入</small>{/if}</div>
                <div class="file-action">
                  {#if attachment.reported_invoice_id !== null}<button type="button" onclick={() => onOpenInvoice(attachment.reported_invoice_id!)}>查看费用</button>{:else if attachment.pending_document_id !== null}<span>待挂载材料 #{attachment.pending_document_id}</span>{:else}<span>{attachment.reason}</span>{/if}
                </div>
              </div>
            {:else}
              <p class="no-attachment">邮件没有可直接取得的具名附件。</p>
            {/each}
          </section>

          {#if canEdit}
            <footer>
              <button class="primary" type="button" disabled={working} onclick={importDownloadedFiles}>{working && activePipelineId ? '正在导入…' : '下载后导入文件'}</button>
              {#if selected.resolution_status === 'open'}
                <button type="button" disabled={working} onclick={() => resolveMessage('resolve')}>标记已处理</button>
                <button type="button" disabled={working} onclick={() => resolveMessage('ignore')}>确认无需处理</button>
              {:else}
                <button type="button" disabled={working} onclick={() => resolveMessage('reopen')}>重新打开</button>
              {/if}
            </footer>
          {/if}
        {:else}
          <p class="detail-empty">请选择一封邮件查看处理结果。</p>
        {/if}
      </article>
    </div>
  </section>
{/if}

<style>
  .legacy-empty{max-width:720px;padding:2rem;border-left:5px solid #8d968f;background:#fff}.legacy-empty>span,.ledger-summary span,.pack-title span{color:#657068;font-family:'IBM Plex Mono',monospace;font-size:.7rem;font-weight:700;letter-spacing:.07em;text-transform:uppercase}.legacy-empty h3{margin:.35rem 0}.legacy-empty p{margin:0;color:#5d6962;line-height:1.6}.ledger-shell{border:1px solid #cbd2d6;background:#fff}.ledger-summary{display:grid;grid-template-columns:repeat(4,1fr);border-bottom:1px solid #cbd2d6;background:#f0ece3}.ledger-summary>div{display:grid;grid-template-columns:1fr auto;gap:.1rem .5rem;padding:.75rem 1rem;border-right:1px solid #d9d3c7}.ledger-summary>div:last-child{border-right:0}.ledger-summary strong{grid-row:1/3;grid-column:2;font-family:'IBM Plex Mono',monospace;font-size:1.5rem}.ledger-summary small{color:#657068}.ledger-summary .attention{background:#fff4d9;color:#7b4f0b}.message{margin:.75rem;padding:.65rem .75rem;border-left:4px solid}.message.error{border-color:#b3453e;background:#f8e9e7;color:#862f2a}.message.notice{border-color:#136b52;background:#e7f1eb;color:#24533f}.ledger-workspace{display:grid;grid-template-columns:minmax(330px,38%) minmax(480px,62%);min-height:560px}.message-column{border-right:1px solid #cbd2d6;background:#f7f6f2}.message-column nav{display:flex;gap:.25rem;padding:.55rem;border-bottom:1px solid #d9d3c7;overflow-x:auto}.message-column nav button{padding:.4rem .55rem;border:1px solid transparent;background:transparent;color:#59645e;white-space:nowrap;cursor:pointer}.message-column nav button.active{border-color:#136b52;background:#fff;color:#136b52;font-weight:700}.message-list{max-height:620px;overflow:auto}.message-list>button{display:grid;width:100%;gap:.25rem;padding:.8rem .9rem;border:0;border-bottom:1px solid #ddd8cf;border-left:4px solid transparent;background:transparent;color:#17221d;text-align:left;cursor:pointer}.message-list>button.selected{border-left-color:#136b52;background:#fff}.message-list>button>div{display:flex;align-items:center;justify-content:space-between}.message-list strong{overflow:hidden;text-overflow:ellipsis;white-space:nowrap}.message-list small{overflow:hidden;color:#657068;text-overflow:ellipsis;white-space:nowrap}.message-list i{color:#657068;font-size:.72rem;font-style:normal}.state{display:inline-flex;width:max-content;padding:.18rem .38rem;border-left:3px solid #657068;background:#ecefed;color:#45514b;font-size:.7rem;font-weight:700}.state.imported{border-color:#136b52;background:#e7f1eb;color:#24533f}.state.manual_download,.state.needs_confirmation,.state.needs_attachment_review{border-color:#c47a16;background:#fff4d9;color:#7b4f0b}.state.failed{border-color:#b3453e;background:#f8e9e7;color:#862f2a}.filter-empty,.detail-empty,.no-attachment{padding:1.25rem;color:#657068}.message-detail{min-width:0}.message-detail>header{display:flex;justify-content:space-between;gap:1rem;padding:1rem 1.2rem;border-bottom:1px solid #d9dfe2}.message-detail h3{margin:.45rem 0 .2rem;font-size:1.2rem}.message-detail header p{margin:0;color:#65737a}.message-detail code{height:max-content;padding:.3rem .45rem;background:#f0ece3;color:#536159}.action-guidance{margin:1rem 1.2rem;padding:.75rem .85rem;border-left:4px solid #c47a16;background:#fff4d9;color:#6f531d}.action-guidance.risk{border-color:#b3453e;background:#f8e9e7;color:#862f2a}.action-guidance p{margin:.25rem 0 0;line-height:1.5}.attachment-pack{padding:0 1.2rem 1rem}.pack-title{display:flex;align-items:end;justify-content:space-between;gap:1rem;padding:.8rem 0;border-bottom:2px solid #17221d}.pack-title h4{margin:.25rem 0 0}.pack-title small{color:#657068}.attachment-row{display:grid;grid-template-columns:52px minmax(0,1fr) minmax(130px,auto);gap:.8rem;align-items:center;padding:.75rem 0;border-bottom:1px solid #d9dfe2}.file-mark{display:grid;height:44px;place-items:center;border:1px solid #8d968f;background:#f0ece3;font-family:'IBM Plex Mono',monospace;font-size:.7rem}.file-info{display:grid;gap:.15rem;min-width:0}.file-info strong{overflow:hidden;text-overflow:ellipsis;white-space:nowrap}.file-info span,.file-info small,.file-action span{color:#657068;font-size:.76rem}.file-action{text-align:right}.file-action button{padding:.38rem .5rem;border:1px solid #136b52;background:#fff;color:#136b52;font-weight:700;cursor:pointer}.message-detail>footer{display:flex;gap:.55rem;justify-content:flex-end;padding:.8rem 1.2rem;border-top:1px solid #cbd2d6;background:#f7f6f2}.message-detail>footer button{padding:.5rem .7rem;border:1px solid #8d968f;background:#fff;color:#344139;cursor:pointer}.message-detail>footer .primary{margin-right:auto;border-color:#136b52;background:#136b52;color:#fff;font-weight:700}.message-detail>footer button:disabled{opacity:.55;cursor:not-allowed}@media(max-width:1000px){.ledger-summary{grid-template-columns:repeat(2,1fr)}.ledger-workspace{grid-template-columns:1fr}.message-column{border-right:0;border-bottom:1px solid #cbd2d6}.message-list{max-height:300px}.message-detail{min-height:480px}}@media(max-width:620px){.ledger-summary{grid-template-columns:1fr 1fr}.attachment-row{grid-template-columns:44px minmax(0,1fr)}.file-action{grid-column:2;text-align:left}.message-detail>footer{flex-wrap:wrap}.message-detail>footer .primary{width:100%;margin:0}.pack-title{display:grid}}
</style>
