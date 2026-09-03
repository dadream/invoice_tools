<script lang="ts">
  import { listen, type UnlistenFn } from '@tauri-apps/api/event'
  import { getCurrentWebview } from '@tauri-apps/api/webview'
  import { open } from '@tauri-apps/plugin-dialog'
  import { onMount } from 'svelte'
  import { invokeSafe } from '../../lib/ipc'
  import type { CollectedEmailAttachment, CollectedEmailMessage, EmailCollectionTask } from '../../lib/types'

  type SourceKind = 'collection' | 'local'
  interface Props { batchId: number; batchName: string; batchMonth: string; onImported: () => Promise<void> }
  interface StageProgress { stage: string; progress: number; current?: number; total?: number; message: string }
  interface PipelineComplete {
    batch_id: number; invoice_count: number; total_amount: string; link_only_email_count: number; pending_document_count: number
    source_file_count: number; parsed_document_count: number; canonical_invoice_count: number; duplicate_document_count: number
  }
  interface PipelineError { stage: string; message: string }
  interface LocalInputPreview { parseable_files: number; skipped: number; duplicates: number; total_bytes: number }
  interface RecoverablePipeline {
    pipeline_id: string; batch_name: string; target_batch_id?: number; month: string
    source_kind: 'local' | 'collection_import' | 'email'; stage: string
    status: 'failed' | 'interrupted'; last_error?: string; updated_at: string
  }

  let { batchId, batchName, batchMonth, onImported }: Props = $props()
  let sourceKind = $state<SourceKind>('collection')
  let collectionTasks = $state<EmailCollectionTask[]>([])
  let selectedTaskId = $state<number | null>(null)
  let collectionMessages = $state<CollectedEmailMessage[]>([])
  let loadingTaskMaterials = $state(false)
  let localPaths = $state<string[]>([])
  let running = $state(false)
  let cancelling = $state(false)
  let activePipelineId = $state<string | null>(null)
  let stage = $state('')
  let progress = $state(0)
  let progressMessage = $state('')
  let error = $state<string | null>(null)
  let notice = $state<string | null>(null)
  let recoverablePipelines = $state<RecoverablePipeline[]>([])
  let loadingSources = $state(true)
  let localPreview = $state<LocalInputPreview | null>(null)
  let previewing = $state(false)
  let dropHovering = $state(false)
  let dropUnlisten: UnlistenFn | null = null
  let unlisteners: UnlistenFn[] = []
  let taskLoadRequest = 0

  const selectedTask = $derived(collectionTasks.find((item) => item.id === selectedTaskId) ?? null)
  const importableAttachments = $derived(collectionMessages.flatMap((message) => message.attachments.filter(isImportable)))
  const importableAttachmentIds = $derived(importableAttachments.map((item) => item.id))
  const reusedAttachments = $derived(importableAttachments.filter((item) => item.used_batch_ids.length > 0))
  const importableMessageCount = $derived(collectionMessages.filter((message) => message.attachments.some(isImportable)).length)
  const openActionableMessages = $derived(collectionMessages.filter((message) => message.resolution_status === 'open' && ['manual_download', 'needs_confirmation', 'failed'].includes(message.status)).length)
  const stageNames: Record<string, string> = { collect: '装载', parse: '解析', dedupe: '去重', group: '归组', review: '写入批次' }

  function isImportable(item: CollectedEmailAttachment): boolean {
    return Boolean(item.stored_path) && !item.user_excluded && (item.status === 'candidate' || item.status === 'supporting_candidate')
  }
  function pathName(path: string): string { return path.split(/[\\/]/).filter(Boolean).pop() || path }
  function formatBytes(bytes: number): string { return bytes < 1024 * 1024 ? `${Math.max(1, Math.round(bytes / 1024))} KiB` : `${(bytes / 1024 / 1024).toFixed(1)} MiB` }
  function cleanup() { unlisteners.forEach((unlisten) => unlisten()); unlisteners = [] }
  function batchDateRange() {
    const [year, month] = batchMonth.split('-').map(Number)
    const last = new Date(year, month, 0).getDate()
    return { start: `${batchMonth}-01`, end: `${batchMonth}-${String(last).padStart(2, '0')}` }
  }

  async function loadCollectionTasks() {
    loadingSources = true
    const result = await invokeSafe<EmailCollectionTask[]>('list_email_collection_tasks')
    loadingSources = false
    if (!result.ok) { error = result.error.message; return }
    collectionTasks = result.data.filter((item) => ['review', 'completed'].includes(item.status) && item.candidate_file_count > 0)
    if (selectedTaskId === null && collectionTasks.length > 0) await chooseCollectionTask(collectionTasks[0].id)
  }
  async function chooseCollectionTask(taskId: number) {
    const requestId = ++taskLoadRequest
    selectedTaskId = taskId; collectionMessages = []; loadingTaskMaterials = true; error = null
    const result = await invokeSafe<CollectedEmailMessage[]>('list_collected_email_messages', { taskId })
    if (requestId !== taskLoadRequest) return
    loadingTaskMaterials = false
    if (!result.ok) { error = result.error.message; return }
    collectionMessages = result.data
  }

  async function previewLocalPaths(): Promise<boolean> {
    if (localPaths.length === 0) { localPreview = null; return false }
    previewing = true; error = null
    const selectedPaths = [...localPaths]
    const result = await invokeSafe<LocalInputPreview>('preview_local_import', { paths: selectedPaths })
    previewing = false
    if (!result.ok) { localPreview = null; error = result.error.message; return false }
    if (selectedPaths.join('\n') !== localPaths.join('\n')) return false
    localPreview = result.data
    if (result.data.parseable_files === 0) error = '所选内容中没有可处理的发票或配套材料文件'
    return result.data.parseable_files > 0
  }
  async function addLocalPaths(paths: string[]) {
    sourceKind = 'local'; localPaths = Array.from(new Set([...localPaths, ...paths])); localPreview = null
    await previewLocalPaths()
  }
  async function chooseFiles() {
    const selected = await open({ multiple: true, directory: false, filters: [{ name: '发票文件', extensions: ['xml', 'ofd', 'pdf', 'png', 'jpg', 'jpeg', 'webp', 'bmp'] }] })
    const values = Array.isArray(selected) ? selected : selected ? [selected] : []
    if (values.length > 0) await addLocalPaths(values)
  }
  async function chooseFolder() {
    const selected = await open({ multiple: false, directory: true })
    if (typeof selected === 'string') await addLocalPaths([selected])
  }

  async function loadRecoverablePipelines() {
    const result = await invokeSafe<RecoverablePipeline[]>('list_recoverable_pipelines')
    if (result.ok) recoverablePipelines = result.data.filter((item) => item.target_batch_id === batchId)
  }
  async function attachListeners(pipelineId: string) {
    cleanup()
    unlisteners.push(await listen<StageProgress>(`pipeline:progress:${pipelineId}`, (event) => {
      stage = event.payload.stage; progress = event.payload.progress; progressMessage = event.payload.message
    }))
    unlisteners.push(await listen<PipelineError>(`pipeline:error:${pipelineId}`, (event) => {
      error = `[${stageNames[event.payload.stage] ?? event.payload.stage}] ${event.payload.message}`
      running = false; cancelling = false; activePipelineId = null; cleanup(); void loadRecoverablePipelines()
    }))
    unlisteners.push(await listen<{ message: string }>(`pipeline:cancelled:${pipelineId}`, (event) => {
      notice = event.payload.message; running = false; cancelling = false; activePipelineId = null; cleanup(); void loadRecoverablePipelines()
    }))
    unlisteners.push(await listen<PipelineComplete>(`pipeline:complete:${pipelineId}`, (event) => {
      const result = event.payload
      const reconciliation = result.source_file_count > 0
        ? `已核对 ${result.source_file_count} 个唯一文件：识别 ${result.parsed_document_count} 份发票文档，归并为 ${result.canonical_invoice_count} 张唯一发票，${result.duplicate_document_count} 份同票副本已挂载。`
        : ''
      notice = `${reconciliation} 已加入 ${result.invoice_count} 笔费用。${result.pending_document_count > 0 ? `另有 ${result.pending_document_count} 份材料等待挂载或忽略。` : ''}`.trim()
      running = false; cancelling = false; activePipelineId = null; progress = 1; cleanup(); void loadRecoverablePipelines(); void onImported()
    }))
  }
  async function resumeImport(item: RecoverablePipeline) {
    error = null; notice = null; running = true; activePipelineId = item.pipeline_id
    stage = item.stage; progress = 0; progressMessage = '正在校验检查点并恢复导入…'
    await attachListeners(item.pipeline_id)
    const result = await invokeSafe<void>('resume_pipeline', { pipelineId: item.pipeline_id })
    if (!result.ok) { error = result.error.message; running = false; activePipelineId = null; cleanup(); await loadRecoverablePipelines() }
  }
  async function startImport() {
    error = null; notice = null
    let source: Record<string, unknown>
    if (sourceKind === 'collection') {
      if (!selectedTask || importableAttachmentIds.length === 0) { error = '所选收集任务没有可导入的材料'; return }
      const snapshot = await invokeSafe<number>('create_batch_collection_import', { batchId, taskId: selectedTask.id, attachmentIds: importableAttachmentIds })
      if (!snapshot.ok) { error = snapshot.error.message; return }
      source = { kind: 'collection_import', import_id: snapshot.data }
    } else {
      if (localPaths.length === 0) { error = '请先选择发票文件或文件夹'; return }
      if ((!localPreview || localPreview.parseable_files === 0) && !(await previewLocalPaths())) return
      source = { kind: 'local', paths: localPaths }
    }
    const pipelineId = crypto.randomUUID()
    activePipelineId = pipelineId; running = true; cancelling = false; stage = 'collect'; progress = 0; progressMessage = '正在建立批次导入任务…'
    await attachListeners(pipelineId)
    const range = batchDateRange()
    const result = await invokeSafe<void>('start_pipeline', {
      pipelineId, config: { batch_name: batchName, month: batchMonth, target_batch_id: batchId, source, date_range: range },
    })
    if (!result.ok) { error = result.error.message; running = false; activePipelineId = null; cleanup(); await loadRecoverablePipelines() }
  }
  async function cancelImport() {
    if (!activePipelineId || cancelling) return
    cancelling = true
    const result = await invokeSafe<void>('cancel_pipeline', { pipelineId: activePipelineId })
    if (!result.ok) { error = result.error.message; cancelling = false }
  }

  onMount(() => {
    void Promise.all([loadCollectionTasks(), loadRecoverablePipelines()])
    let disposed = false
    void getCurrentWebview().onDragDropEvent((event) => {
      if (running || sourceKind !== 'local') return
      if (event.payload.type === 'enter' || event.payload.type === 'over') dropHovering = true
      if (event.payload.type === 'leave') dropHovering = false
      if (event.payload.type === 'drop') { dropHovering = false; if (event.payload.paths.length > 0) void addLocalPaths(event.payload.paths) }
    }).then((unlisten) => { if (disposed) unlisten(); else dropUnlisten = unlisten })
    return () => { disposed = true; dropUnlisten?.(); cleanup() }
  })
</script>

<section class="import-panel" aria-label="向批次导入材料">
  <div class="source-switch" role="tablist" aria-label="导入方式">
    <button type="button" role="tab" aria-selected={sourceKind === 'collection'} class:active={sourceKind === 'collection'} onclick={() => (sourceKind = 'collection')} disabled={running}><span>01</span><strong>从邮件收集任务导入</strong><small>选择已保存的原始附件</small></button>
    <button type="button" role="tab" aria-selected={sourceKind === 'local'} class:active={sourceKind === 'local'} onclick={() => (sourceKind = 'local')} disabled={running}><span>02</span><strong>导入本地发票文件</strong><small>不导入 EML 邮件文件</small></button>
  </div>

  {#if !running && recoverablePipelines.length > 0}<div class="recovery"><div><strong>{recoverablePipelines.length} 个导入任务可以恢复</strong><span>将从最后一个已验证检查点继续。</span></div>{#each recoverablePipelines as item (item.pipeline_id)}<button type="button" onclick={() => resumeImport(item)}>恢复{item.source_kind === 'collection_import' ? '收集材料导入' : '本地导入'} · {stageNames[item.stage] ?? item.stage}</button>{/each}</div>{/if}

  {#if !running && sourceKind === 'collection'}
    <div class="collection-picker">
      <section class="step"><header><b>1</b><div><strong>选择邮件收集任务</strong><span>选中后将导入该任务的全部可处理材料，无需逐项勾选。</span></div></header>{#if loadingSources}<p class="state">正在读取收集任务…</p>{:else if collectionTasks.length === 0}<div class="empty-source"><strong>没有可用的收集材料</strong><span>请先到主导航“邮件收集”创建并运行任务。</span></div>{:else}<div class="task-options">{#each collectionTasks as task (task.id)}<button class:selected={selectedTaskId === task.id} type="button" onclick={() => chooseCollectionTask(task.id)}><strong>{task.name}</strong><span>{task.date_start} → {task.date_end}</span><small>{task.candidate_file_count} 个文件 · {task.actionable_message_count > 0 ? `${task.actionable_message_count} 封仍待处理` : '来源审核无阻断'}</small></button>{/each}</div>{/if}</section>
      {#if selectedTask}
        <section class="step confirm-step"><header><b>2</b><div><strong>全量导入并解析</strong><span>系统会保留同一邮件中发票、行程单和明细的来源关联。</span></div></header>{#if loadingTaskMaterials}<p class="state">正在汇总任务材料…</p>{:else}<dl><div><dt>包含邮件</dt><dd>{importableMessageCount}</dd></div><div><dt>全部可处理材料</dt><dd>{importableAttachments.length}</dd></div><div class:risk={reusedAttachments.length > 0}><dt>跨批次重复使用</dt><dd>{reusedAttachments.length}</dd></div><div class:risk={openActionableMessages > 0}><dt>来源待操作邮件</dt><dd>{openActionableMessages}</dd></div></dl><p class="import-note">已明确标记为无效、未下载成功或不属于候选材料的附件不会进入批次。</p>{#if reusedAttachments.length > 0}<p class="risk-note">其中 {reusedAttachments.length} 个材料已用于其他批次。它们仍会进入解析，但疑似重复发票默认不计入总额。</p>{/if}<button class="start" type="button" onclick={startImport} disabled={importableAttachments.length === 0}>导入全部 {importableAttachments.length} 个材料并解析</button>{/if}</section>
      {/if}
    </div>
  {:else if !running}
    <div class="local-picker"><div class:hovering={dropHovering} class="drop-zone"><strong>{dropHovering ? '松开即可加入' : '将发票文件或文件夹拖到窗口'}</strong><span>仅导入 PDF / OFD / XML / 发票图片，不导入 EML。</span></div><div class="picker-actions"><button type="button" onclick={chooseFiles}>选择发票文件</button><button type="button" onclick={chooseFolder}>选择文件夹</button></div>{#if localPaths.length > 0}<ul>{#each localPaths as path (path)}<li><span title={path}>{pathName(path)}</span><button type="button" onclick={async () => { localPaths = localPaths.filter((item) => item !== path); localPreview = null; if (localPaths.length > 0) await previewLocalPaths() }}>移除</button></li>{/each}</ul>{/if}{#if previewing}<div class="preflight">正在检查所选内容…</div>{:else if localPreview}<div class="preflight"><strong>{localPreview.parseable_files} 个可处理文件 · {formatBytes(localPreview.total_bytes)}</strong><span>{localPreview.duplicates} 个同内容文件、{localPreview.skipped} 个不支持项会跳过</span></div>{/if}<button class="start" type="button" onclick={startImport} disabled={previewing || localPaths.length === 0}>{localPreview ? `导入 ${localPreview.parseable_files} 个本地文件` : '检查并导入本地文件'}</button></div>
  {:else}
    <div class="progress" aria-live="polite"><div><span>{stageNames[stage] ?? stage}</span><strong>{Math.round(progress * 100)}%</strong></div><div class="track"><i style={`width:${Math.max(2, progress * 100)}%`}></i></div><p>{progressMessage}</p><button type="button" onclick={cancelImport} disabled={cancelling}>{cancelling ? '正在安全停止…' : '安全停止'}</button></div>
  {/if}
  {#if error}<p class="message error" role="alert">{error}</p>{/if}{#if notice}<p class="message notice" role="status">{notice}</p>{/if}
</section>

<style>
  .import-panel{background:#fff;color:#17232d}.source-switch{display:grid;grid-template-columns:1fr 1fr}.source-switch button{display:grid;grid-template-columns:auto 1fr;gap:.12rem .6rem;padding:.8rem 1rem;border:0;border-right:1px solid #d3dade;border-bottom:3px solid transparent;background:#eef1f1;color:#5e6c73;text-align:left;cursor:pointer}.source-switch button:last-child{border-right:0}.source-switch button.active{border-bottom-color:#136b52;background:#fff;color:#17232d}.source-switch button>span{grid-row:1/3;color:#7a878d;font:.7rem var(--font-mono)}.source-switch small{grid-column:2;color:#6c7980}.recovery{display:flex;gap:.6rem;align-items:center;padding:.7rem .9rem;border-bottom:1px solid #dccb9f;background:#fff6df}.recovery>div{display:grid;margin-right:auto}.recovery span{color:#765c28;font-size:.7rem}.recovery button{padding:.4rem .55rem;border:1px solid #a8873d;background:#fff;color:#76520a;cursor:pointer}.collection-picker,.local-picker,.progress{padding:1rem}.step{margin-bottom:.9rem;border:1px solid #cbd2d6}.step>header{display:flex;gap:.65rem;align-items:center;padding:.7rem .8rem;border-bottom:1px solid #dbe1e4;background:#f4f6f6}.step>header>b{display:grid;width:28px;height:28px;place-items:center;background:#17221d;color:#fff;font:.7rem var(--font-mono)}.step>header>div{display:grid}.step>header span{color:#68767d;font-size:.72rem}.state,.empty-source{padding:1rem;color:#68767d}.empty-source{display:grid;gap:.2rem}.task-options{display:grid;grid-template-columns:repeat(2,minmax(0,1fr));gap:.55rem;padding:.7rem}.task-options button{display:grid;gap:.18rem;padding:.65rem .7rem;border:1px solid #c4cdd1;border-left:4px solid #849198;background:#fff;color:#17232d;text-align:left;cursor:pointer}.task-options button.selected{border-left-color:#136b52;background:#edf6f1}.task-options span,.task-options small{color:#65737a;font-size:.72rem}.confirm-step{padding-bottom:.8rem}.confirm-step dl{display:grid;grid-template-columns:repeat(4,1fr);margin:0}.confirm-step dl>div{display:grid;gap:.15rem;padding:.7rem .8rem;border-right:1px solid #dbe1e4}.confirm-step dl>div:last-child{border-right:0}.confirm-step dt{color:#65737a;font-size:.7rem}.confirm-step dd{margin:0;color:#136b52;font:700 1.2rem var(--font-mono)}.confirm-step .risk dd{color:#a0630a}.import-note{margin:.7rem .8rem;color:#5d6b72;font-size:.74rem}.risk-note{margin:.7rem .8rem;padding:.55rem .65rem;border-left:4px solid #c47a16;background:#fff3d9;color:#76500e;font-size:.74rem}.start{display:block;margin:.8rem .8rem 0 auto;padding:.65rem .9rem;border:1px solid #136b52;background:#136b52;color:#fff;font-weight:700;cursor:pointer}.start:disabled{opacity:.45;cursor:not-allowed}.drop-zone{display:grid;gap:.2rem;padding:1.2rem;border:1px dashed #8d9a93;background:#f7faf8;text-align:center}.drop-zone.hovering{border-color:#136b52;background:#e7f1eb}.drop-zone span{color:#68767d;font-size:.74rem}.picker-actions{display:flex;gap:.55rem;margin-top:.65rem}.picker-actions button,.progress button{padding:.48rem .65rem;border:1px solid #8f9ca2;background:#fff;color:#344149;cursor:pointer}.local-picker ul{display:grid;gap:.3rem;max-height:130px;margin:.7rem 0;padding:0;overflow:auto;list-style:none}.local-picker li{display:flex;justify-content:space-between;padding:.35rem .5rem;background:#f1f3f3;font-size:.75rem}.local-picker li button{border:0;background:transparent;color:#b3453e;cursor:pointer}.preflight{display:grid;margin-top:.7rem;padding:.6rem .7rem;border-left:4px solid #136b52;background:#edf6f1}.preflight span{color:#52655b;font-size:.72rem}.progress>div:first-child{display:flex;justify-content:space-between}.track{height:7px;margin:.45rem 0;background:#dce2df}.track i{display:block;height:100%;background:#136b52}.progress p{color:#65737a}.message{margin:.75rem 1rem;padding:.65rem .75rem;border-left:4px solid}.message.error{border-color:#b3453e;background:#f8e9e7;color:#862f2a}.message.notice{border-color:#136b52;background:#edf6f1;color:#24533f}@media(max-width:760px){.source-switch,.task-options{grid-template-columns:1fr}.confirm-step dl{grid-template-columns:repeat(2,1fr)}}
</style>
