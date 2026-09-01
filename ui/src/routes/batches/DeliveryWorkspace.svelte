<script lang="ts">
  import { save } from '@tauri-apps/plugin-dialog'
  import { describeError, invokeSafe } from '../../lib/ipc'
  import type { Batch, BatchReviewSnapshot, ConcurDraftCapability, DeliveryTask } from '../../lib/types'
  import { formatAmount, formatDate } from '../../lib/types'
  import ConfirmDialog from '../../lib/ConfirmDialog.svelte'
  import ConcurDeliveryPanel from './ConcurDeliveryPanel.svelte'

  interface Props {
    batchId: number
    onBackToList: () => void
    onBackToReview: () => void
    onReviewReopened: () => void
  }
  let { batchId, onBackToList, onBackToReview, onReviewReopened }: Props = $props()

  let batch = $state<Batch | null>(null)
  let snapshot = $state<BatchReviewSnapshot | null>(null)
  let tasks = $state<DeliveryTask[]>([])
  let loading = $state(true)
  let exporting = $state(false)
  let reopening = $state(false)
  let error = $state<string | null>(null)
  let notice = $state<string | null>(null)
  let showConcur = $state(false)
  let confirmingReopen = $state(false)
  let concurCapability = $state<ConcurDraftCapability | null>(null)

  interface ExcelExportResult { path: string; bytes: number }

  function taskFor(kind: DeliveryTask['kind']): DeliveryTask | null {
    return tasks.find((task) => task.kind === kind && task.review_snapshot_id === snapshot?.id) ?? null
  }
  function taskLabel(task: DeliveryTask | null): string {
    if (!task) return '尚未执行'
    return { pending: '等待执行', running: '执行中', succeeded: '已完成', failed: '失败，可重试' }[task.status]
  }

  async function load() {
    loading = true
    error = null
    const [batchResult, snapshotResult, tasksResult, capabilityResult] = await Promise.all([
      invokeSafe<Batch>('get_batch', { id: batchId }),
      invokeSafe<BatchReviewSnapshot | null>('get_active_review_snapshot', { batchId }),
      invokeSafe<DeliveryTask[]>('list_delivery_tasks', { batchId }),
      invokeSafe<ConcurDraftCapability>('get_concur_draft_capability'),
    ])
    loading = false
    if (!batchResult.ok) { error = describeError(batchResult.error); return }
    batch = batchResult.data
    if (!snapshotResult.ok) { error = describeError(snapshotResult.error); return }
    snapshot = snapshotResult.data
    if (!tasksResult.ok) { error = describeError(tasksResult.error); return }
    tasks = tasksResult.data
    if (capabilityResult.ok) concurCapability = capabilityResult.data
  }

  function safeFileName(value: string): string {
    const sanitized = value.replace(/[<>:"/\\|?*]/g, '_').trim()
    return sanitized || `批次-${batchId}`
  }

  async function exportExcel() {
    if (!batch || !snapshot || exporting) return
    const destination = await save({
      title: '导出审核后的费用清单',
      defaultPath: `${safeFileName(batch.name)}-审核版本V${snapshot.version}.xlsx`,
      filters: [{ name: 'Excel 工作簿', extensions: ['xlsx'] }],
    })
    if (!destination) return
    exporting = true
    error = null
    notice = null
    const result = await invokeSafe<ExcelExportResult>('export_batch_excel_to_path', {
      batchId,
      destinationPath: destination,
    })
    exporting = false
    if (!result.ok) { error = describeError(result.error); await load(); return }
    notice = `Excel 已按审核版本 V${snapshot.version} 保存：${result.data.path}`
    await load()
  }

  function openConcur() { if (snapshot) { error = null; showConcur = true } }

  async function reopenReview() {
    if (!snapshot || reopening) return
    confirmingReopen = false
    reopening = true
    error = null
    const result = await invokeSafe<void>('reopen_batch_review', { batchId })
    reopening = false
    if (!result.ok) { error = describeError(result.error); return }
    onReviewReopened()
  }

  $effect(() => { batchId; void load() })
</script>

<div class="delivery-page">
  <header>
    <div class="top-actions">
      <button type="button" onclick={onBackToList}>← 批次列表</button>
      <button type="button" onclick={onBackToReview}>查看审核结果</button>
    </div>
    {#if batch}
      <div class="heading-row">
        <div><span class="eyebrow">审核后交付</span><h1>{batch.name}</h1><p>审核已冻结。请选择一种交付方式；两种方式可以先后执行。</p></div>
        {#if snapshot}<div class="version"><span>活动版本</span><strong>V{snapshot.version}</strong><code>{snapshot.content_sha256.slice(0, 12)}</code></div>{/if}
      </div>
    {/if}
  </header>

  {#if loading}
    <p class="state">正在读取审核版本…</p>
  {:else if error && !snapshot}
    <section class="missing"><strong>没有可交付的审核版本</strong><p>{error}</p><button type="button" onclick={onBackToReview}>返回批次审核</button></section>
  {:else if batch && snapshot}
    <main>
      <section class="snapshot-bar">
        <div><span>冻结时间</span><strong>{formatDate(snapshot.created_at)}</strong></div>
        <div><span>计入费用</span><strong>{snapshot.invoice_count} 条</strong></div>
        <div><span>审核总额</span><strong>{formatAmount(snapshot.total_amount)}</strong></div>
        <button class="reopen" type="button" onclick={() => (confirmingReopen = true)} disabled={reopening}>{reopening ? '正在重新打开…' : '修改数据并重新审核'}</button>
      </section>

      {#if notice}<p class="notice" role="status">{notice}</p>{/if}
      {#if error}<p class="error" role="alert">{error}</p>{/if}

      <section class="choice-heading"><span class="eyebrow">交付方式</span><h2>下一步要把审核结果送到哪里？</h2><p>所有交付都绑定审核版本 V{snapshot.version}，不会读取后来发生变化的草稿数据。</p></section>
      <div class="choice-grid">
        <article>
          <div class="card-top"><span class="index">A</span><span class:done={taskFor('excel')?.status === 'succeeded'} class="task-state">{taskLabel(taskFor('excel'))}</span></div>
          <h3>导出 Excel</h3>
          <p>生成可人工检查、归档或提交给其他报销系统的费用明细。</p>
          <ul><li>使用软件稳定字段</li><li>不依赖 Concur 映射</li><li>重复项不会进入合计</li></ul>
          <button class="primary" type="button" onclick={exportExcel} disabled={exporting}>{exporting ? '正在生成…' : taskFor('excel')?.status === 'succeeded' ? '再次下载 Excel' : '导出 Excel'}</button>
        </article>
        <article>
          <div class="card-top"><span class="index">B</span><span class:done={taskFor('concur')?.status === 'succeeded'} class="task-state">{taskLabel(taskFor('concur'))}</span></div>
          <h3>上传到 Concur</h3>
          <p>按已配置映射创建费用草稿、填写字段并上传发票原件；最终提交仍由用户在 Concur 完成。</p>
          <ul><li>先检查必填字段缺口</li><li>按租户配置映射费用类型</li><li>仅创建草稿，不自动提交</li></ul>
          <div class:available={concurCapability?.enabled} class="capability"><strong>{concurCapability?.enabled ? '当前环境可执行外部写入' : '当前仅可完成映射与预检'}</strong><span>{concurCapability?.reason ?? '正在检查 Concur 适配器能力…'}</span></div>
          <button class="primary" type="button" onclick={openConcur}>配置映射并预检</button>
        </article>
      </div>

      {#if showConcur}
        <ConcurDeliveryPanel {batchId} {batch} {snapshot} {onBackToReview} />
      {/if}

      {#if tasks.length > 0}
        <details class="history"><summary>交付记录（{tasks.length}）</summary><ul>{#each tasks as task}<li><span>{task.kind === 'excel' ? 'Excel' : 'Concur'} · 审核版本 #{task.review_snapshot_id}</span><strong>{taskLabel(task)}</strong><small>{formatDate(task.updated_at)}</small></li>{/each}</ul></details>
      {/if}
    </main>
  {/if}
</div>

{#if confirmingReopen}
  <ConfirmDialog title="重新打开批次审核" message="当前审核快照会立即失效。已有交付记录仍保留，但不能再基于旧版本发起新交付；修改完成后需要重新完成审核。" confirmLabel="重新打开审核" tone="danger" busy={reopening} onConfirm={reopenReview} onCancel={() => (confirmingReopen = false)} />
{/if}

<style>
  .delivery-page{min-height:100vh;background:#f4f5f6;color:#17232d}.delivery-page>header{padding:1.4rem 2rem 1.5rem;border-bottom:1px solid #cbd2d6;background:#fff}.top-actions{display:flex;justify-content:space-between}.top-actions button{padding:0;border:0;background:transparent;color:var(--pine,#136b52);font-weight:700;cursor:pointer}.heading-row{display:flex;justify-content:space-between;gap:2rem;align-items:flex-end;margin-top:1.5rem}.eyebrow,.index{color:#68767d;font-family:'IBM Plex Mono',monospace;font-size:.72rem;font-weight:700;letter-spacing:.08em;text-transform:uppercase}h1{margin:.25rem 0 0;font-size:clamp(1.7rem,2.5vw,2.5rem);letter-spacing:-.035em}.heading-row p,.choice-heading p{margin:.4rem 0 0;color:#596870}.version{display:grid;grid-template-columns:auto auto;gap:.15rem .55rem;align-items:baseline;padding:.65rem .8rem;border-left:4px solid var(--pine,#136b52);background:#f5f7f7}.version span{color:#657078;font-size:.72rem}.version strong{font-size:1.4rem}.version code{grid-column:1/-1;color:#596870;font-size:.68rem}
  main{padding:1.5rem 2rem 3rem}.snapshot-bar{display:grid;grid-template-columns:repeat(3,minmax(120px,1fr)) auto;border:1px solid #cbd2d6;background:#fff}.snapshot-bar>div{display:grid;gap:.25rem;padding:.8rem 1rem;border-right:1px solid #d9dfe2}.snapshot-bar span{color:#657078;font-size:.72rem}.snapshot-bar strong{font-size:1rem}.reopen{margin:.65rem;padding:.5rem .75rem;border:1px solid #8b958e;background:#fff;color:#344149;cursor:pointer}.choice-heading{margin:2rem 0 1rem}.choice-heading h2{margin:.25rem 0 0;font-size:1.3rem}.choice-grid{display:grid;grid-template-columns:repeat(2,minmax(0,1fr));gap:1rem}.choice-grid article{display:flex;min-height:330px;flex-direction:column;padding:1.2rem;border:1px solid #c2cbd0;background:#fff}.card-top{display:flex;justify-content:space-between}.task-state{padding:.22rem .45rem;background:#eff1f2;color:#5c6870;font-size:.72rem}.task-state.done{background:#dcece3;color:#1f5b43}.choice-grid h3{margin:1.15rem 0 .45rem;font-size:1.3rem}.choice-grid p{margin:0;color:#596870;line-height:1.55}.choice-grid ul{display:grid;gap:.4rem;margin:1rem 0;padding-left:1.1rem;color:#4e5a53;font-size:.85rem}.capability{display:grid;gap:.15rem;margin:0 0 1rem;padding:.55rem .65rem;border-left:3px solid #c47a16;background:#fff7e7}.capability.available{border-left-color:#136b52;background:#edf6f1}.capability strong{font-size:.78rem}.capability span{color:#65737a;font-size:.72rem;line-height:1.4}.primary{margin-top:auto;padding:.68rem .9rem;border:1px solid var(--pine,#136b52);background:var(--pine,#136b52);color:#fff;font-weight:700;cursor:pointer}.primary:disabled,.reopen:disabled{opacity:.5;cursor:not-allowed}
  .notice,.error{margin:1rem 0;padding:.75rem .9rem;border-left:4px solid var(--pine,#136b52);background:#e7f1eb;color:#24533f}.error{border-color:var(--risk,#b3453e);background:#f8e9e7;color:#862f2a}.history{margin-top:1.5rem;border-top:1px solid #cfc8b8;padding-top:.8rem}.history summary{cursor:pointer;font-weight:700}.history ul{display:grid;gap:.4rem;padding:0;list-style:none}.history li{display:grid;grid-template-columns:1fr auto auto;gap:1rem;padding:.55rem .7rem;background:#fbfaf6}.history small{color:#657068}.state,.missing{margin:2rem;padding:1rem}.missing{border-left:4px solid var(--risk,#b3453e);background:#f8e9e7}.missing p{color:#862f2a}.missing button{padding:.5rem .75rem;border:1px solid #8b958e;background:#fff}
  @media(max-width:900px){.choice-grid{grid-template-columns:1fr}.snapshot-bar{grid-template-columns:repeat(3,1fr)}.snapshot-bar .reopen{grid-column:1/-1}}@media(max-width:680px){.delivery-page>header,main{padding-inline:1rem}.heading-row{display:grid}.snapshot-bar{grid-template-columns:1fr}.snapshot-bar>div{border-right:0;border-bottom:1px solid #d9d3c7}.history li{grid-template-columns:1fr}}
</style>
