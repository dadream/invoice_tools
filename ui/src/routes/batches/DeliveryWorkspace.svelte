<script lang="ts">
  import { listen, type UnlistenFn } from '@tauri-apps/api/event'
  import { save } from '@tauri-apps/plugin-dialog'
  import { describeError, invokeSafe, toAppError } from '../../lib/ipc'
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
  let exporting = $state<'excel' | 'pdf' | null>(null)
  let reopening = $state(false)
  let error = $state<string | null>(null)
  let notice = $state<string | null>(null)
  let showConcur = $state(false)
  let confirmingReopen = $state(false)
  let concurCapability = $state<ConcurDraftCapability | null>(null)
  let pdfWarnings = $state<string[]>([])
  let pdfProgress = $state<PrintPdfProgress | null>(null)
  let openingPdf = $state<'file' | 'folder' | null>(null)

  interface ExcelExportResult { path: string; bytes: number }
  interface PrintPdfExportResult {
    path: string
    bytes: number
    review_version: number
    expense_count: number
    material_count: number
    rendered_material_count: number
    page_count: number
    warning_count: number
    warnings: string[]
  }
  interface PrintPdfProgress {
    export_id: string
    batch_id: number
    phase: 'preparing' | 'converting' | 'writing' | 'verifying' | 'completed' | 'failed'
    current: number
    total: number
    material_name: string | null
    message: string
  }

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

  async function refreshTasks() {
    const result = await invokeSafe<DeliveryTask[]>('list_delivery_tasks', { batchId })
    if (result.ok) tasks = result.data
    else error = describeError(result.error)
  }

  function safeFileName(value: string): string {
    const sanitized = value.replace(/[<>:"/\\|?*]/g, '_').trim()
    return sanitized || `批次-${batchId}`
  }

  function pdfProgressPercent(progress: PrintPdfProgress): number {
    if (progress.phase === 'completed') return 100
    if (progress.phase === 'verifying') return 98
    if (progress.phase === 'writing') return 94
    if (progress.phase === 'converting' && progress.total > 0) {
      return Math.min(90, Math.max(6, Math.round(progress.current / progress.total * 88)))
    }
    return progress.phase === 'failed' ? 0 : 3
  }

  async function exportExcel() {
    if (!batch || !snapshot || exporting !== null) return
    exporting = 'excel'
    error = null
    notice = null
    pdfWarnings = []
    try {
      const destination = await save({
        title: '导出审核后的费用清单',
        defaultPath: `${safeFileName(batch.name)}-审核版本V${snapshot.version}.xlsx`,
        filters: [{ name: 'Excel 工作簿', extensions: ['xlsx'] }],
      })
      if (!destination) return
      const result = await invokeSafe<ExcelExportResult>('export_batch_excel_to_path', {
        batchId,
        destinationPath: destination,
      })
      if (!result.ok) {
        await refreshTasks()
        error = describeError(result.error)
        return
      }
      notice = `Excel 已按审核版本 V${snapshot.version} 保存：${result.data.path}`
      await refreshTasks()
    } catch (caught) {
      error = `无法打开 Windows 保存窗口：${describeError(toAppError(caught))}`
    } finally {
      exporting = null
    }
  }

  async function exportPrintPdf() {
    if (!batch || !snapshot || exporting !== null) return
    exporting = 'pdf'
    error = null
    notice = null
    pdfWarnings = []
    pdfProgress = null
    let unlisten: UnlistenFn | null = null
    try {
      const destination = await save({
        title: '导出报销材料打印 PDF',
        defaultPath: `${safeFileName(batch.name)}-审核版本V${snapshot.version}-报销材料.pdf`,
        filters: [{ name: 'PDF 文档', extensions: ['pdf'] }],
      })
      if (!destination) { pdfProgress = null; return }
      const exportId = crypto.randomUUID()
      pdfProgress = {
        export_id: exportId,
        batch_id: batchId,
        phase: 'preparing',
        current: 0,
        total: 0,
        material_name: null,
        message: '正在启动 PDF 导出',
      }
      unlisten = await listen<PrintPdfProgress>(`print-pdf:progress:${exportId}`, (event) => {
        if (event.payload.export_id === exportId && event.payload.batch_id === batchId) {
          pdfProgress = event.payload
        }
      })
      const result = await invokeSafe<PrintPdfExportResult>('export_batch_print_pdf_to_path', {
        batchId,
        destinationPath: destination,
        exportId,
      })
      if (!result.ok) {
        await refreshTasks()
        error = describeError(result.error)
        if (pdfProgress?.phase !== 'failed') {
          pdfProgress = pdfProgress
            ? { ...pdfProgress, phase: 'failed', message: error }
            : null
        }
        return
      }
      pdfWarnings = result.data.warnings
      notice = result.data.warning_count > 0
        ? `报销材料 PDF 已保存，共 ${result.data.page_count} 页；${result.data.rendered_material_count}/${result.data.material_count} 份材料已生成打印页面，另有 ${result.data.warning_count} 项异常未写入 PDF：${result.data.path}`
        : `报销材料 PDF 已保存，共 ${result.data.page_count} 页、${result.data.material_count} 份材料；第一页即为报销凭证：${result.data.path}`
      await refreshTasks()
    } catch (caught) {
      error = `无法完成 PDF 导出：${describeError(toAppError(caught))}`
      if (pdfProgress) pdfProgress = { ...pdfProgress, phase: 'failed', message: error }
    } finally {
      unlisten?.()
      exporting = null
    }
  }

  async function openPdfOutput(reveal: boolean) {
    const task = taskFor('pdf')
    if (!task?.output_path || openingPdf !== null) return
    openingPdf = reveal ? 'folder' : 'file'
    error = null
    const result = await invokeSafe<void>('open_delivery_pdf', { batchId, taskId: task.id, reveal })
    openingPdf = null
    if (!result.ok) error = describeError(result.error)
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
      {#if exporting === 'pdf' && pdfProgress}
        <section class="pdf-progress" aria-live="polite" aria-label="PDF 导出进度">
          <div class="progress-heading">
            <div><span>正在导出报销材料</span><strong>{pdfProgress.message}</strong></div>
            <b>{pdfProgressPercent(pdfProgress)}%</b>
          </div>
          <progress max="100" value={pdfProgressPercent(pdfProgress)}></progress>
          <div class="progress-detail">
            <span>{pdfProgress.total > 0 ? `${pdfProgress.current} / ${pdfProgress.total} 份材料` : '正在读取材料清单'}</span>
            {#if pdfProgress.material_name}<code title={pdfProgress.material_name}>{pdfProgress.material_name}</code>{/if}
          </div>
        </section>
      {/if}
      {#if pdfWarnings.length > 0}
        <details class="pdf-warnings"><summary>查看未写入 PDF 的 {pdfWarnings.length} 项材料异常</summary><ol>{#each pdfWarnings as warning}<li>{warning}</li>{/each}</ol></details>
      {/if}
      {#if taskFor('pdf')?.status === 'succeeded' && taskFor('pdf')?.output_path}
        <div class="pdf-output-actions">
          <span>最近导出的报销材料 PDF</span>
          <button type="button" onclick={() => openPdfOutput(false)} disabled={openingPdf !== null}>{openingPdf === 'file' ? '正在打开…' : '打开 PDF'}</button>
          <button type="button" onclick={() => openPdfOutput(true)} disabled={openingPdf !== null}>{openingPdf === 'folder' ? '正在定位…' : '打开所在文件夹'}</button>
        </div>
      {/if}

      <section class="choice-heading"><span class="eyebrow">交付方式</span><h2>下一步要把审核结果送到哪里？</h2><p>所有交付都绑定审核版本 V{snapshot.version}，不会读取后来发生变化的草稿数据。</p></section>
      <div class="choice-grid">
        <article>
          <div class="card-top"><span class="index">A</span><span class:done={taskFor('excel')?.status === 'succeeded'} class="task-state">{taskLabel(taskFor('excel'))}</span></div>
          <h3>导出文件</h3>
          <p>导出可检查的费用明细，或生成可直接打印的报销材料 PDF。</p>
          <ul><li>Excel 使用软件稳定字段</li><li>PDF 第一页直接是发票或配套凭证，不含封面和目录</li><li>材料连续排列并带全局页码</li><li>重复项和未计入费用不会导出</li></ul>
          <div class="export-buttons">
            <button class="primary" type="button" onclick={exportExcel} disabled={exporting !== null}>{exporting === 'excel' ? '正在导出 Excel…' : taskFor('excel')?.status === 'succeeded' ? '再次导出 Excel' : '导出 Excel'}</button>
            <button class="secondary" type="button" onclick={exportPrintPdf} disabled={exporting !== null}>{exporting === 'pdf' ? '正在生成打印 PDF…' : taskFor('pdf')?.status === 'succeeded' ? '再次导出打印 PDF' : '导出打印 PDF'}</button>
          </div>
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
        <details class="history"><summary>交付记录（{tasks.length}）</summary><ul>{#each tasks as task}<li><span>{{ excel: 'Excel', pdf: '打印 PDF', concur: 'Concur' }[task.kind]} · 审核版本 #{task.review_snapshot_id}</span><strong>{taskLabel(task)}</strong><small>{formatDate(task.updated_at)}</small></li>{/each}</ul></details>
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
  .export-buttons{display:grid;gap:.55rem;margin-top:auto}.export-buttons .primary{margin-top:0}.secondary{padding:.68rem .9rem;border:1px solid var(--pine,#136b52);background:#fff;color:var(--pine,#136b52);font-weight:700;cursor:pointer}.secondary:disabled{opacity:.5;cursor:not-allowed}.notice,.error{margin:1rem 0;padding:.75rem .9rem;border-left:4px solid var(--pine,#136b52);background:#e7f1eb;color:#24533f}.error{border-color:var(--risk,#b3453e);background:#f8e9e7;color:#862f2a}.pdf-progress{display:grid;gap:.7rem;margin:1rem 0;padding:1rem;border:1px solid #9eb9ad;background:#fff}.progress-heading,.progress-detail{display:flex;align-items:center;justify-content:space-between;gap:1rem}.progress-heading>div{display:grid;gap:.2rem}.progress-heading span,.progress-detail{color:#65737a;font-size:.75rem}.progress-heading strong{font-size:.92rem}.progress-heading b{color:#136b52;font-family:'IBM Plex Mono',monospace}.pdf-progress progress{width:100%;height:9px;border:0;background:#dfe6e2;accent-color:#136b52}.progress-detail code{max-width:65%;overflow:hidden;padding:.2rem .35rem;background:#f0ece3;color:#536159;text-overflow:ellipsis;white-space:nowrap}.pdf-output-actions{display:flex;align-items:center;justify-content:flex-end;gap:.5rem;margin:1rem 0}.pdf-output-actions span{margin-right:auto;color:#65737a;font-size:.76rem}.pdf-output-actions button{padding:.48rem .65rem;border:1px solid #136b52;background:#fff;color:#136b52;font-weight:700;cursor:pointer}.pdf-output-actions button:disabled{opacity:.5;cursor:not-allowed}.pdf-warnings{margin:1rem 0;padding:.75rem .9rem;border-left:4px solid #c47a16;background:#fff7e7;color:#6f531d}.pdf-warnings summary{cursor:pointer;font-weight:700}.pdf-warnings ol{display:grid;gap:.35rem;margin:.65rem 0 0;padding-left:1.25rem;font-size:.82rem}.history{margin-top:1.5rem;border-top:1px solid #cfc8b8;padding-top:.8rem}.history summary{cursor:pointer;font-weight:700}.history ul{display:grid;gap:.4rem;padding:0;list-style:none}.history li{display:grid;grid-template-columns:1fr auto auto;gap:1rem;padding:.55rem .7rem;background:#fbfaf6}.history small{color:#657068}.state,.missing{margin:2rem;padding:1rem}.missing{border-left:4px solid var(--risk,#b3453e);background:#f8e9e7}.missing p{color:#862f2a}.missing button{padding:.5rem .75rem;border:1px solid #8b958e;background:#fff}
  @media(max-width:900px){.choice-grid{grid-template-columns:1fr}.snapshot-bar{grid-template-columns:repeat(3,1fr)}.snapshot-bar .reopen{grid-column:1/-1}}@media(max-width:680px){.delivery-page>header,main{padding-inline:1rem}.heading-row{display:grid}.snapshot-bar{grid-template-columns:1fr}.snapshot-bar>div{border-right:0;border-bottom:1px solid #d9d3c7}.history li{grid-template-columns:1fr}}
</style>
