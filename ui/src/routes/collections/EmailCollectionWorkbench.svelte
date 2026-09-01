<script lang="ts">
  import { listen, type UnlistenFn } from '@tauri-apps/api/event'
  import { open } from '@tauri-apps/plugin-dialog'
  import { onMount } from 'svelte'
  import ConfirmDialog from '../../lib/ConfirmDialog.svelte'
  import { describeError, invokeSafe } from '../../lib/ipc'
  import type {
    CollectedEmailAttachment,
    CollectedEmailMessage,
    CollectedEmailReviewDetail,
    CollectedEmailReviewLink,
    EmailCollectionTask,
    EmailCollectionTaskStatus,
  } from '../../lib/types'
  import CollectedAttachmentPreview from './CollectedAttachmentPreview.svelte'
  import {
    collectionProcessResult,
    groupLabel,
    messageGroup,
    messagesForReviewGroup,
    receivedDate,
    sortCollectedMessages,
    type MessageGroup,
    type SortDirection,
  } from './emailCollectionViewModel'

  type PageView = 'tasks' | 'task' | 'review'
  interface SessionCredentialStatus { configured: boolean; email: string | null }
  interface CollectionProgress { taskId: number; current: number; total: number; message: string }
  interface CollectionError { taskId: number; message: string }
  interface QrExtractionResult { detected: boolean; qrDominant: boolean; browserLinkCount: number; review: CollectedEmailReviewDetail }

  const PAGE_SIZE = 25
  let view = $state<PageView>('tasks')
  let tasks = $state<EmailCollectionTask[]>([])
  let selectedTask = $state<EmailCollectionTask | null>(null)
  let messages = $state<CollectedEmailMessage[]>([])
  let selectedMessageId = $state<number | null>(null)
  let activeGroup = $state<MessageGroup>('needs_action')
  let reviewGroup = $state<MessageGroup>('needs_action')
  let sortDirection = $state<SortDirection>('desc')
  let searchText = $state('')
  let currentPage = $state(1)
  let loading = $state(true)
  let detailLoading = $state(false)
  let error = $state<string | null>(null)
  let notice = $state<string | null>(null)
  let showCreate = $state(false)
  let creating = $state(false)
  let taskName = $state('')
  let dateStart = $state('')
  let dateEnd = $state('')
  let session = $state<SessionCredentialStatus>({ configured: false, email: null })
  let progressMessage = $state('')
  let progressCurrent = $state(0)
  let progressTotal = $state(0)
  let reviewDetail = $state<CollectedEmailReviewDetail | null>(null)
  let reviewLoading = $state(false)
  let reviewError = $state<string | null>(null)
  let reviewLoadSequence = 0
  let openingLinkId = $state<number | null>(null)
  let confirmReanalysis = $state(false)
  let reanalyzing = $state(false)
  let previewAttachment = $state<CollectedEmailAttachment | null>(null)
  let attachmentValidityUpdatingId = $state<number | null>(null)
  let qrExtractingAttachmentId = $state<number | null>(null)
  let unlisteners: UnlistenFn[] = []

  const selectedMessage = $derived(messages.find((item) => item.id === selectedMessageId) ?? null)
  const needsActionMessages = $derived(messages.filter((message) => messageGroup(message) === 'needs_action'))
  const pendingMessages = $derived(messages.filter((message) => messageGroup(message) === 'pending'))
  const reviewedMessages = $derived(messages.filter((message) => messageGroup(message) === 'reviewed'))
  const activeMessages = $derived.by(() => {
    const source = activeGroup === 'needs_action' ? needsActionMessages : activeGroup === 'pending' ? pendingMessages : reviewedMessages
    const query = searchText.trim().toLocaleLowerCase()
    const filtered = query ? source.filter((message) => `${message.subject} ${message.sender}`.toLocaleLowerCase().includes(query)) : source
    return sortCollectedMessages(filtered, sortDirection)
  })
  const totalPages = $derived(Math.max(1, Math.ceil(activeMessages.length / PAGE_SIZE)))
  const visibleMessages = $derived(activeMessages.slice((currentPage - 1) * PAGE_SIZE, currentPage * PAGE_SIZE))
  const reviewMessages = $derived.by(() => messagesForReviewGroup(messages, reviewGroup, sortDirection, selectedMessageId))
  const reviewPosition = $derived(selectedMessageId === null ? -1 : reviewMessages.findIndex((message) => message.id === selectedMessageId))

  const statusLabels: Record<EmailCollectionTaskStatus, string> = {
    created: '待开始', collecting: '收集中', review: '待审核', completed: '审核完成', failed: '收集失败', interrupted: '已中断',
  }
  const attachmentLabels: Record<CollectedEmailAttachment['status'], string> = {
    candidate: '待解析文件', supporting_candidate: '配套材料候选', filtered: '已过滤', unsupported: '不支持', failed: '文件损坏或读取失败',
  }
  const roleLabels: Record<CollectedEmailAttachment['role_hint'], string> = {
    invoice: '发票候选', itinerary: '行程材料', detail: '消费明细', supporting: '其他配套材料', unknown: '待解析判断',
  }
  const reasonLabels: Record<string, string> = {
    user_manual_supplement: '用户手工补充', archive_invalid_or_unsafe: '压缩包损坏或不安全', source_classifier_rejected: '附件类型或内容与报销材料无关',
    empty_attachment: '空附件', attachment_too_large: '单个附件超过 25 MiB', collection_size_limit: '本次任务材料超过 500 MiB',
    collection_library_limit: '材料库达到 5 GiB 上限，请先备份并清理', supported_content_candidate: '文件内容符合可处理格式',
    filename_or_subject_candidate: '文件名或邮件主题符合报销材料特征', trusted_sender_candidate: '发件平台符合报销材料来源',
    pdf_structure_invalid: 'PDF 结构校验失败，文件可能不完整或实际并非 PDF', ofd_structure_invalid: 'OFD 压缩结构或 OFD.xml 校验失败',
    xml_structure_invalid: 'XML 结构不完整或无法解析', image_structure_invalid: '图片无法解码或尺寸异常',
    attachment_qr_manual_download: '邮件附件中的二维码用于取得材料，不作为发票文件', supporting_material_keyword: '文件名表明这是行程单、账单或消费明细',
    attachment_contains_qr_link: '材料中包含二维码，已提取可用地址', attachment_qr_no_browser_url: '已识别二维码，但内容不是可由浏览器打开的 HTTP(S) 地址',
  }

  function initializeDates() {
    const now = new Date()
    dateStart = `${now.getFullYear()}-${String(now.getMonth() + 1).padStart(2, '0')}-01`
    const next = new Date(now.getFullYear(), now.getMonth() + 1, 1)
    dateEnd = `${next.getFullYear()}-${String(next.getMonth() + 1).padStart(2, '0')}-01`
  }

  function processResult(message: CollectedEmailMessage): string { return collectionProcessResult(message) }
  function maskEmail(value: string): string {
    const [name, domain] = value.split('@')
    if (!domain) return value || '历史账号'
    return `${name.slice(0, Math.min(3, name.length))}***@${domain}`
  }
  function formatBytes(bytes: number): string { return bytes < 1024 * 1024 ? `${Math.max(1, Math.round(bytes / 1024))} KiB` : `${(bytes / 1024 / 1024).toFixed(1)} MiB` }
  function isImageAttachment(attachment: CollectedEmailAttachment): boolean { return /\.(png|jpe?g|webp|bmp|gif|tiff?)$/i.test(attachment.original_name) }
  function cleanupListeners() { unlisteners.forEach((unlisten) => unlisten()); unlisteners = [] }
  function defaultGroup(source: CollectedEmailMessage[]): MessageGroup {
    if (source.some((message) => messageGroup(message) === 'needs_action')) return 'needs_action'
    if (source.some((message) => messageGroup(message) === 'pending')) return 'pending'
    return 'reviewed'
  }
  function changeGroup(group: MessageGroup) { activeGroup = group; currentPage = 1 }

  async function loadTasks(selectId?: number) {
    loading = true
    const [taskResult, sessionResult] = await Promise.all([invokeSafe<EmailCollectionTask[]>('list_email_collection_tasks'), invokeSafe<SessionCredentialStatus>('get_session_credential_status')])
    loading = false
    if (!taskResult.ok) { error = describeError(taskResult.error); return }
    tasks = taskResult.data
    if (sessionResult.ok) session = sessionResult.data
    const targetId = selectId ?? selectedTask?.id
    if (targetId) selectedTask = tasks.find((item) => item.id === targetId) ?? selectedTask
  }
  async function openTask(task: EmailCollectionTask) {
    selectedTask = task; selectedMessageId = null; searchText = ''; currentPage = 1; sortDirection = 'desc'; view = 'task'
    await loadMessages(true)
  }
  async function loadMessages(selectDefault = false) {
    if (!selectedTask) return
    detailLoading = true
    const result = await invokeSafe<CollectedEmailMessage[]>('list_collected_email_messages', { taskId: selectedTask.id })
    detailLoading = false
    if (!result.ok) { error = describeError(result.error); return }
    messages = result.data
    if (selectDefault) activeGroup = defaultGroup(result.data)
    currentPage = Math.min(currentPage, Math.max(1, Math.ceil(result.data.filter((message) => messageGroup(message) === activeGroup).length / PAGE_SIZE)))
    if (selectedMessageId !== null && !messages.some((message) => message.id === selectedMessageId)) selectedMessageId = null
  }
  async function openReview(message: CollectedEmailMessage) { reviewGroup = messageGroup(message); selectedMessageId = message.id; view = 'review'; previewAttachment = null; await loadReviewDetail() }
  async function loadReviewDetail() {
    if (!selectedMessage) return
    const sequence = ++reviewLoadSequence
    reviewDetail = null; reviewError = null; reviewLoading = true
    const result = await invokeSafe<CollectedEmailReviewDetail>('get_collected_email_review_detail', { messageId: selectedMessage.id })
    if (sequence !== reviewLoadSequence) return
    reviewLoading = false
    if (!result.ok) { reviewError = describeError(result.error); return }
    reviewDetail = result.data
  }
  async function moveReview(offset: number) {
    if (reviewPosition < 0) return
    const target = reviewMessages[reviewPosition + offset]
    if (target) await openReview(target)
  }
  async function createTask() {
    error = null
    if (!taskName.trim()) { error = '请输入收集任务名称'; return }
    if (!session.configured) { error = '请先在“设置与数据”输入邮箱和授权码'; return }
    if (!dateStart || !dateEnd || dateStart >= dateEnd) { error = '结束日期必须晚于开始日期'; return }
    creating = true
    const result = await invokeSafe<number>('create_email_collection_task', { name: taskName.trim(), dateStart, dateEnd })
    creating = false
    if (!result.ok) { error = describeError(result.error); return }
    showCreate = false; taskName = ''
    await loadTasks(result.data)
    const task = tasks.find((item) => item.id === result.data)
    if (task) await openTask(task)
  }
  async function attachTaskListeners(taskId: number) {
    cleanupListeners()
    unlisteners.push(await listen<CollectionProgress>(`email-collection:progress:${taskId}`, (event) => { progressMessage = event.payload.message; progressCurrent = event.payload.current; progressTotal = event.payload.total }))
    unlisteners.push(await listen(`email-collection:complete:${taskId}`, async () => { notice = '只读收集和 FLAGS 复核已完成，请逐封检查材料完整性。'; cleanupListeners(); await loadTasks(taskId); await loadMessages(true) }))
    unlisteners.push(await listen<CollectionError>(`email-collection:error:${taskId}`, async (event) => { error = event.payload.message; cleanupListeners(); await loadTasks(taskId) }))
  }
  async function startTask() {
    if (!selectedTask) return
    error = null; notice = null; progressMessage = '正在准备只读收集…'; progressCurrent = 0; progressTotal = 0
    await attachTaskListeners(selectedTask.id)
    const result = await invokeSafe<string>('start_email_collection_task', { taskId: selectedTask.id })
    if (!result.ok) { cleanupListeners(); error = describeError(result.error); return }
    await loadTasks(selectedTask.id)
  }
  async function resolveMessage(action: 'resolve' | 'ignore' | 'reopen') {
    if (!selectedMessage) return
    const completedMessageId = selectedMessage.id
    const nextMessageId = action === 'reopen' || reviewPosition < 0 ? null : (reviewMessages[reviewPosition + 1]?.id ?? null)
    const result = await invokeSafe<void>('resolve_collected_email_message', { messageId: selectedMessage.id, action })
    if (!result.ok) { error = describeError(result.error); return }
    const completedNotice = action === 'reopen' ? '该邮件已重新打开。' : action === 'ignore' ? '已确认上一封邮件与报销无关。' : '上一封邮件的来源材料已确认齐全。'
    await loadMessages(); await loadTasks(selectedTask?.id)
    if (action === 'reopen') { notice = completedNotice; return }
    const nextMessage = nextMessageId === null ? null : messages.find((message) => message.id === nextMessageId)
    if (nextMessage) {
      await openReview(nextMessage)
      notice = `${completedNotice} 已自动进入下一封。`
    } else {
      notice = messages.some((message) => message.id === completedMessageId) ? `${completedNotice} 这已经是最后一封。` : completedNotice
    }
    window.requestAnimationFrame(() => window.scrollTo({ top: 0, left: 0, behavior: 'auto' }))
  }
  async function setAttachmentExcluded(attachment: CollectedEmailAttachment, excluded: boolean) {
    if (attachmentValidityUpdatingId !== null) return
    attachmentValidityUpdatingId = attachment.id; error = null
    const result = await invokeSafe<void>('set_collected_email_attachment_excluded', { attachmentId: attachment.id, excluded })
    attachmentValidityUpdatingId = null
    if (!result.ok) { error = describeError(result.error); return }
    if (previewAttachment?.id === attachment.id) previewAttachment = null
    notice = excluded
      ? `已将“${attachment.original_name}”标记为无效；文件仍保留用于追溯。`
      : attachment.status === 'filtered'
        ? `已取消“${attachment.original_name}”的无效标记；系统过滤结论不变，可继续预览核对。`
        : `已恢复“${attachment.original_name}”；该文件可以再次进入报销批次。`
    await loadMessages(); await loadTasks(selectedTask?.id)
  }
  async function supplementMessage() {
    if (!selectedMessage) return
    const selected = await open({ multiple: true, directory: false, filters: [{ name: '发票与配套材料', extensions: ['pdf', 'ofd', 'xml', 'png', 'jpg', 'jpeg', 'webp', 'bmp'] }] })
    const paths = Array.isArray(selected) ? selected : selected ? [selected] : []
    if (paths.length === 0) return
    const result = await invokeSafe<number[]>('supplement_collected_email_message', { messageId: selectedMessage.id, paths })
    if (!result.ok) { error = describeError(result.error); return }
    notice = `已把 ${result.data.length} 个本地文件补充到当前邮件材料包。`
    await loadMessages(); await loadTasks(selectedTask?.id)
  }
  async function openEmailLink(link: CollectedEmailReviewLink) {
    if (!selectedMessage || openingLinkId !== null) return
    openingLinkId = link.id
    const result = await invokeSafe<void>('open_collected_email_link', { messageId: selectedMessage.id, linkId: link.id })
    openingLinkId = null
    if (!result.ok) reviewError = describeError(result.error)
  }
  async function extractQrAddress(attachment: CollectedEmailAttachment) {
    if (qrExtractingAttachmentId !== null) return
    qrExtractingAttachmentId = attachment.id; error = null; reviewError = null
    const result = await invokeSafe<QrExtractionResult>('extract_collected_attachment_qr_links', { attachmentId: attachment.id })
    qrExtractingAttachmentId = null
    if (!result.ok) { error = describeError(result.error); return }
    reviewDetail = result.data.review
    if (!result.data.detected) {
      notice = `未能从“${attachment.original_name}”识别二维码；请确认图片清晰且二维码完整。`
      return
    }
    await loadMessages(); await loadTasks(selectedTask?.id)
    notice = result.data.browserLinkCount > 0
      ? `已从“${attachment.original_name}”提取 ${result.data.browserLinkCount} 个二维码地址，请在上方核对域名后用系统浏览器打开。`
      : `已确认“${attachment.original_name}”包含二维码，但二维码内容不是可由浏览器打开的 HTTP(S) 地址。`
  }
  async function reanalyzeMessage() {
    if (!selectedMessage) return
    reanalyzing = true; reviewError = null
    const result = await invokeSafe<CollectedEmailReviewDetail>('reanalyze_collected_email_message', { messageId: selectedMessage.id })
    reanalyzing = false; confirmReanalysis = false
    if (!result.ok) { reviewError = describeError(result.error); return }
    reviewDetail = result.data
    await loadMessages(); await loadTasks(selectedTask?.id)
    notice = '已按用户要求重新读取该邮件，正文、下载链接、附件完整性和材料角色已经更新。'
  }
  async function openAttachment(attachmentId: number) {
    const result = await invokeSafe<void>('open_collected_attachment', { attachmentId, reveal: false })
    if (!result.ok) error = describeError(result.error)
  }
  async function completeReview() {
    if (!selectedTask) return
    const result = await invokeSafe<void>('complete_email_collection_review', { taskId: selectedTask.id })
    if (!result.ok) { error = describeError(result.error); return }
    notice = '来源审核已完成；这些材料现在可以在报销批次中选择导入。'
    await loadTasks(selectedTask.id)
  }
  function returnToTasks() { view = 'tasks'; selectedTask = null; messages = []; selectedMessageId = null; reviewDetail = null; void loadTasks() }

  onMount(() => { initializeDates(); void loadTasks(); return cleanupListeners })
</script>

{#if view === 'review' && selectedTask && selectedMessage}
  <div class="review-page">
    <header class="review-header">
      <div class="review-nav"><button class="back" type="button" onclick={() => { view = 'task'; reviewLoadSequence += 1 }}>← 返回邮件列表</button><span>{groupLabel(reviewGroup)} · {reviewPosition >= 0 ? `${reviewPosition + 1} / ${reviewMessages.length}` : '—'}</span><button type="button" onclick={() => moveReview(-1)} disabled={reviewPosition <= 0}>上一封</button><button type="button" onclick={() => moveReview(1)} disabled={reviewPosition < 0 || reviewPosition >= reviewMessages.length - 1}>下一封</button></div>
      <div class="review-title"><div><span class="eyebrow">{selectedTask.name} · 邮件审核</span><h1>{selectedMessage.subject}</h1></div><span class={`group-badge ${messageGroup(selectedMessage)}`}>{groupLabel(messageGroup(selectedMessage))}</span></div>
    </header>
    <main class="review-content">
      {#if error}<p class="feedback error" role="alert">{error}</p>{/if}{#if notice}<p class="feedback notice" role="status">{notice}</p>{/if}
      <section class="mail-overview"><header><span class="eyebrow">邮件信息</span><div class="mail-overview-actions"><strong>{processResult(selectedMessage)}</strong><button type="button" onclick={() => (confirmReanalysis = true)}>重新分析此邮件</button></div></header><dl><div><dt>发件人</dt><dd>{reviewDetail?.senderName ? `${reviewDetail.senderName} <${reviewDetail.senderAddress ?? selectedMessage.sender}>` : (reviewDetail?.senderAddress ?? selectedMessage.sender)}</dd></div><div><dt>收件日期</dt><dd>{receivedDate(selectedMessage.received_at)}</dd></div><div><dt>主题</dt><dd>{selectedMessage.subject}</dd></div><div><dt>附件数</dt><dd>{selectedMessage.attachments.length}</dd></div></dl></section>
      <section class="body-card"><header><div><span class="eyebrow">邮件正文</span><h2>安全纯文本</h2></div><span>{reviewDetail?.analyzedAt ? `收集时已保存 · ${reviewDetail.analyzedAt}` : '不加载图片、脚本和跟踪资源'}</span></header>{#if reviewLoading}<div class="section-state">正在读取本地邮件审核数据…</div>{:else if reviewError}<div class="section-state error" role="alert"><strong>本地审核数据暂时无法读取</strong><span>{reviewError}</span><button type="button" onclick={loadReviewDetail}>重试读取本地数据</button></div>{:else if reviewDetail && !reviewDetail.available}<div class="section-state"><strong>这封邮件来自旧版收集任务，尚未保存正文和链接</strong><span>只有点击下方按钮时，软件才会重新连接邮箱读取这一封邮件。</span><button type="button" onclick={() => (confirmReanalysis = true)}>重新分析此邮件</button></div>{:else if reviewDetail}<pre>{reviewDetail.bodyText}</pre>{#if reviewDetail.bodyTruncated}<p class="truncate-note">正文较长，当前只显示前 100 KiB。</p>{/if}{/if}</section>
      <section class="link-card"><header><div><span class="eyebrow">需要用户取得的材料</span><h2>邮件与二维码地址</h2></div><span>{reviewDetail?.links.length ?? 0} 个</span></header>{#if reviewLoading}<div class="section-state">正在读取本地链接…</div>{:else if reviewDetail?.available && reviewDetail.links.length > 0}<div class="link-list">{#each reviewDetail.links as link (link.id)}<article><div><strong>{link.label}</strong><span>站点：{link.host}{link.scheme === 'http' ? ' · HTTP 非加密链接' : ''}</span></div><button type="button" disabled={openingLinkId !== null} onclick={() => openEmailLink(link)}>{openingLinkId === link.id ? '正在打开…' : '用系统浏览器打开'}</button></article>{/each}</div>{:else if reviewDetail?.available}<div class="section-state"><strong>尚未识别到可由浏览器打开的材料地址</strong><span>可以在下方对图片附件执行“提取二维码地址”，整个过程只读取本地缓存。</span></div>{/if}</section>
      <section class="attachment-card"><header><div><span class="eyebrow">同一邮件材料包</span><h2>附件与补充文件</h2></div><span>{selectedMessage.attachments.length} 个</span></header><div class="attachment-list">{#each selectedMessage.attachments as attachment (attachment.id)}<article class:unavailable={!attachment.stored_path} class:user-excluded={attachment.user_excluded}><span class="file-kind">{attachment.original_name.split('.').pop()?.toUpperCase() ?? 'FILE'}</span><div><strong>{attachment.original_name}</strong><p>{roleLabels[attachment.role_hint]} · {formatBytes(attachment.byte_len)}{attachment.container_name ? ` · 来自 ${attachment.container_name}` : ''}</p><small>{attachment.user_excluded ? '用户已标记无效 · 文件保留用于追溯' : `${attachmentLabels[attachment.status]} · ${reasonLabels[attachment.reason] ?? '来源规则自动判断'}`}</small>{#if attachment.used_batch_names.length > 0}<em>已用于：{attachment.used_batch_names.join('、')}</em>{/if}</div><div class="attachment-actions">{#if attachment.stored_path}<button type="button" onclick={() => (previewAttachment = attachment)}>预览</button><button type="button" onclick={() => openAttachment(attachment.id)}>系统打开</button>{#if isImageAttachment(attachment)}<button type="button" disabled={qrExtractingAttachmentId !== null} onclick={() => extractQrAddress(attachment)}>{qrExtractingAttachmentId === attachment.id ? '识别中…' : attachment.reason.includes('qr_') ? '重新提取二维码' : '提取二维码地址'}</button>{/if}{:else}<span>未保存</span>{/if}{#if attachment.stored_path && (attachment.status === 'candidate' || attachment.status === 'supporting_candidate' || attachment.status === 'filtered')}<button class:danger={!attachment.user_excluded} type="button" disabled={attachmentValidityUpdatingId !== null} onclick={() => setAttachmentExcluded(attachment, !attachment.user_excluded)}>{attachmentValidityUpdatingId === attachment.id ? '更新中…' : attachment.user_excluded ? '取消无效标记' : '标记无效'}</button>{/if}</div></article>{:else}<div class="section-state"><strong>没有可直接取得的附件</strong><span>可以打开上方下载链接，再将取得的发票文件补充到本邮件。</span></div>{/each}</div></section>
    </main>
    <footer class="review-actions"><div><span>当前处理结果</span><strong>{processResult(selectedMessage)}</strong></div><div><button class="secondary" type="button" onclick={supplementMessage}>＋ 导入已下载文件</button>{#if selectedMessage.resolution_status === 'open'}<button class="secondary danger" type="button" onclick={() => resolveMessage('ignore')}>确认无关</button><button class="primary" type="button" onclick={() => resolveMessage('resolve')}>材料已齐全</button>{:else}<button class="secondary" type="button" onclick={() => resolveMessage('reopen')}>重新打开</button>{/if}</div></footer>
  </div>
{:else if view === 'task' && selectedTask}
  <div class="collection-detail">
    <header class="detail-header"><button class="back" type="button" onclick={returnToTasks}>← 收集任务</button><div class="detail-title"><div><span class="eyebrow">邮件收集 #{selectedTask.id} · INBOX 只读</span><h1>{selectedTask.name}</h1><p>{maskEmail(selectedTask.account_email)} · [{selectedTask.date_start}, {selectedTask.date_end})</p></div><div class="task-actions"><span class:danger={selectedTask.status === 'failed'} class="task-status">{statusLabels[selectedTask.status]}</span>{#if ['created', 'failed', 'interrupted'].includes(selectedTask.status)}<button class="primary" type="button" onclick={startTask}>{selectedTask.status === 'created' ? '开始收集' : '重新收集'}</button>{/if}</div></div>{#if selectedTask.status === 'collecting'}<div class="collection-progress" aria-live="polite"><div><strong>{progressMessage || '正在收集邮件…'}</strong><span>{progressTotal ? `${progressCurrent}/${progressTotal}` : '连接中'}</span></div><div><i style={`width:${progressTotal ? Math.max(3, progressCurrent / progressTotal * 100) : 8}%`}></i></div></div>{/if}</header>
    <main class="ledger-page"><section class="group-tabs" aria-label="邮件审核分组"><button class:active={activeGroup === 'needs_action'} type="button" onclick={() => changeGroup('needs_action')}><span>需要用户处理</span><strong>{needsActionMessages.length}</strong><small>下载、确认或失败项</small></button><button class:active={activeGroup === 'pending'} type="button" onclick={() => changeGroup('pending')}><span>待审核</span><strong>{pendingMessages.length}</strong><small>等待来源确认</small></button><button class:active={activeGroup === 'reviewed'} type="button" onclick={() => changeGroup('reviewed')}><span>已审核</span><strong>{reviewedMessages.length}</strong><small>已完成或已忽略</small></button></section>{#if error}<p class="feedback error" role="alert">{error}</p>{/if}{#if notice}<p class="feedback notice" role="status">{notice}</p>{/if}
      {#if detailLoading}<p class="state">正在读取逐封邮件台账…</p>{:else if messages.length === 0}<section class="empty-detail"><strong>{selectedTask.status === 'created' ? '任务尚未开始' : selectedTask.status === 'collecting' ? '正在建立逐封台账' : '该日期范围内没有邮件记录'}</strong><span>收集阶段只确认来源和附件完整性，不解析票号、金额或费用字段。</span></section>{:else}<section class="mail-ledger"><header><div><span class="eyebrow">{groupLabel(activeGroup)}</span><h2>邮件清单</h2></div><div class="ledger-tools"><label><span class="sr-only">搜索主题或发件人</span><input value={searchText} oninput={(event) => { searchText = event.currentTarget.value; currentPage = 1 }} placeholder="搜索主题或发件人" /></label><button type="button" onclick={() => { sortDirection = sortDirection === 'desc' ? 'asc' : 'desc'; currentPage = 1 }}>收件日期 {sortDirection === 'desc' ? '↓' : '↑'}</button></div></header><div class="table-scroll"><table><thead><tr><th>收件时间</th><th>主题</th><th>发件人</th><th>附件数</th><th>处理结果</th><th>状态</th><th>操作</th></tr></thead><tbody>{#each visibleMessages as message (message.id)}<tr><td class="date-cell">{receivedDate(message.received_at)}</td><td><button class="subject-link" type="button" onclick={() => openReview(message)}>{message.subject}</button></td><td class="sender-cell">{message.sender}</td><td>{message.attachments.length}</td><td class="result-cell">{processResult(message)}</td><td><span class={`group-badge ${messageGroup(message)}`}>{groupLabel(messageGroup(message))}</span></td><td><button class="open-review" type="button" onclick={() => openReview(message)}>{messageGroup(message) === 'reviewed' ? '查看' : messageGroup(message) === 'needs_action' ? '继续处理' : '审核'} →</button></td></tr>{:else}<tr><td class="no-rows" colspan="7">此分组没有匹配邮件。</td></tr>{/each}</tbody></table></div><footer class="pagination"><span>共 {activeMessages.length} 封 · 每页 {PAGE_SIZE} 封</span><div><button type="button" onclick={() => (currentPage -= 1)} disabled={currentPage <= 1}>上一页</button><strong>{currentPage} / {totalPages}</strong><button type="button" onclick={() => (currentPage += 1)} disabled={currentPage >= totalPages}>下一页</button></div></footer></section>{/if}
    </main>{#if selectedTask.status === 'review' || (selectedTask.status === 'completed' && selectedTask.review_status === 'open')}<footer class="task-review-bar"><div><span>{selectedTask.actionable_message_count > 0 ? '来源审核未完成' : '来源审核可以完成'}</span><strong>{selectedTask.actionable_message_count > 0 ? `${selectedTask.actionable_message_count} 封邮件仍需用户处理` : `${selectedTask.candidate_file_count} 个文件可供批次选择`}</strong></div><button type="button" onclick={completeReview} disabled={selectedTask.actionable_message_count > 0}>完成来源审核</button></footer>{/if}
  </div>
{:else}
  <div class="collection-list-page"><header class="page-header"><div><span class="eyebrow">来源材料工作台</span><h1>邮件收集</h1><p>先确认邮件和附件是否收齐；整理完成后，再到报销批次执行解析与归组。</p></div><button class="primary" type="button" onclick={() => { error = null; showCreate = true }}>＋ 新建收集任务</button></header><section class="workflow-note"><b>01</b><div><strong>邮件收集</strong><span>只读搜索、分类和保存附件</span></div><i>→</i><b>02</b><div><strong>来源审核</strong><span>补充下载文件、确认无关邮件</span></div><i>→</i><b>03</b><div><strong>批次导入</strong><span>解析、去重、费用生成和归组</span></div></section><section class="task-summary"><div><span>正在收集</span><strong>{tasks.filter((item) => item.status === 'collecting').length}</strong></div><div><span>待来源审核</span><strong>{tasks.filter((item) => item.status === 'review').length}</strong></div><div><span>审核完成</span><strong>{tasks.filter((item) => item.status === 'completed' && item.review_status === 'completed').length}</strong></div><div><span>失败/中断</span><strong>{tasks.filter((item) => item.status === 'failed' || item.status === 'interrupted').length}</strong></div></section>{#if error}<p class="feedback error" role="alert">{error}</p>{/if}{#if loading}<p class="state">正在读取收集任务…</p>{:else if tasks.length === 0}<section class="empty-list"><span>00</span><h2>先建立一份来源台账</h2><p>例如创建“2026 年 6 月发票邮件”，系统会只读检查指定日期范围内的 INBOX。</p><button class="primary" type="button" onclick={() => (showCreate = true)}>创建第一个收集任务</button></section>{:else}<section class="task-table"><header><div><span class="eyebrow">收集任务</span><h2>按最近更新排序</h2></div><strong>{tasks.length}</strong></header><div class="table-scroll"><table><thead><tr><th>任务</th><th>日期范围</th><th>扫描邮件</th><th>可导入文件</th><th>待操作</th><th>状态</th><th></th></tr></thead><tbody>{#each tasks as task (task.id)}<tr><td><button class="task-link" type="button" onclick={() => openTask(task)}><strong>{task.name}</strong><small>{maskEmail(task.account_email)}</small></button></td><td class="range">{task.date_start}<br />→ {task.date_end}</td><td>{task.scanned_message_count}</td><td>{task.candidate_file_count}</td><td class:warn={task.actionable_message_count > 0}>{task.actionable_message_count}</td><td><span class:danger={task.status === 'failed'} class="task-status">{statusLabels[task.status]}</span></td><td><button class="open-task" type="button" onclick={() => openTask(task)}>{task.status === 'review' ? '继续审核' : '查看'} →</button></td></tr>{/each}</tbody></table></div></section>{/if}</div>
{/if}

{#if showCreate}<div class="modal-backdrop" role="presentation" onclick={(event) => event.target === event.currentTarget && (showCreate = false)}><div class="create-modal" role="dialog" aria-modal="true" aria-labelledby="create-collection-title"><header><div><span class="eyebrow">一次性只读任务</span><h2 id="create-collection-title">新建邮件收集</h2><p>MVP 固定读取 INBOX；结束日期不包含当天。</p></div><button type="button" aria-label="关闭" onclick={() => (showCreate = false)}>×</button></header><form onsubmit={(event) => { event.preventDefault(); void createTask() }}><label><span>任务名称 *</span><input bind:value={taskName} maxlength="100" placeholder="例：2026 年 6 月发票邮件" /></label><label><span>当前邮箱</span><input value={session.email ?? '尚未配置会话授权码'} disabled /></label><div class="date-fields"><label><span>开始日期 *</span><input type="date" bind:value={dateStart} /></label><label><span>结束日期（不含）*</span><input type="date" bind:value={dateEnd} /></label></div><aside><strong>收集阶段不会做什么</strong><span>不解析金额和票号，不判定有效发票，不去重，不归组，不创建费用。</span></aside>{#if error}<p class="feedback error" role="alert">{error}</p>{/if}<footer><button class="secondary" type="button" onclick={() => (showCreate = false)}>取消</button><button class="primary" type="submit" disabled={creating || !session.configured}>{creating ? '创建中…' : '创建任务'}</button></footer></form></div></div>{/if}
{#if confirmReanalysis}<ConfirmDialog title="重新分析这封邮件？" message="该操作会重新连接当前邮箱，只读下载这一封邮件并复核 FLAGS，然后重建正文、下载链接、自动附件和完整性检查结果。用户手工补充的文件会保留；正常查看和打开链接不会访问邮箱。" confirmLabel="重新分析" busy={reanalyzing} onConfirm={reanalyzeMessage} onCancel={() => (confirmReanalysis = false)} />{/if}
{#if previewAttachment}<CollectedAttachmentPreview attachment={previewAttachment} onClose={() => (previewAttachment = null)} />{/if}

<style>
  .collection-list-page,.collection-detail,.review-page{min-height:100vh;color:#17232d}.collection-list-page{max-width:1500px;margin:0 auto;padding:2rem}.page-header,.detail-title,.review-title{display:flex;align-items:flex-end;justify-content:space-between;gap:2rem}.eyebrow{color:#68767d;font-family:var(--font-mono);font-size:.7rem;font-weight:700;letter-spacing:.08em;text-transform:uppercase}h1{margin:.18rem 0 0;font-size:clamp(1.7rem,2.5vw,2.75rem);letter-spacing:-.04em}h2{margin:.2rem 0}.page-header p,.detail-title p{margin:.4rem 0 0;color:#596870}.primary,.secondary{padding:.68rem .9rem;border:1px solid #136b52;background:#136b52;color:#fff;font-weight:700;cursor:pointer}.primary:disabled,.secondary:disabled{opacity:.45;cursor:not-allowed}.secondary{background:#fff;color:#136b52}.secondary.danger{border-color:#b3453e;color:#a53630}.back{padding:0;border:0;background:transparent;color:#136b52;font-weight:700;cursor:pointer}.workflow-note{display:grid;grid-template-columns:auto 1fr auto auto 1fr auto auto 1fr;gap:.65rem;align-items:center;margin:1.5rem 0;padding:.9rem 1rem;border:1px solid #b9c5bf;background:#eaf2ee}.workflow-note b{display:grid;width:30px;height:30px;place-items:center;background:#136b52;color:#fff;font-family:var(--font-mono);font-size:.7rem}.workflow-note div{display:grid}.workflow-note span{color:#637168;font-size:.72rem}.workflow-note i{color:#7c8b83;font-style:normal}.task-summary{display:grid;grid-template-columns:repeat(4,minmax(0,1fr));gap:.7rem;margin:1rem 0}.task-summary>div{display:grid;grid-template-columns:1fr auto;gap:.4rem;padding:.75rem .9rem;border:1px solid #cbd2d6;border-left:4px solid #78888f;background:#fff}.task-summary span{color:#65737a;font-size:.77rem}.task-summary strong{font:1.25rem var(--font-mono);color:#136b52}.task-table,.mail-ledger{margin-top:1rem;border:1px solid #c2cbd0;background:#fff}.task-table>header,.mail-ledger>header{display:flex;justify-content:space-between;align-items:center;gap:1rem;padding:1rem 1.1rem;border-bottom:1px solid #cbd2d6}.task-table>header strong{color:#136b52;font:1.4rem var(--font-mono)}.table-scroll{overflow-x:auto}table{width:100%;border-collapse:collapse}th,td{padding:.75rem 1rem;border-bottom:1px solid #e0e5e7;text-align:left;vertical-align:middle}th{background:#f1f3f4;color:#596870;font-size:.74rem;white-space:nowrap}.task-link,.open-task,.subject-link,.open-review{padding:0;border:0;background:transparent;color:#136b52;text-align:left;cursor:pointer}.task-link{display:grid;gap:.15rem}.task-link small{color:#6c7980}.open-task,.open-review{font-weight:700;white-space:nowrap}.subject-link{max-width:480px;font-weight:700}.range,.date-cell{font:.78rem/1.5 var(--font-mono);white-space:nowrap}.warn{color:#9b5f09;font-weight:700}.task-status{display:inline-block;padding:.25rem .45rem;border-left:3px solid #136b52;background:#edf6f1;color:#24533f;font-size:.74rem;font-weight:700;white-space:nowrap}.task-status.danger{border-color:#b3453e;background:#f8e9e7;color:#862f2a}.empty-list,.empty-detail{display:grid;justify-items:center;gap:.4rem;margin-top:1rem;padding:3rem;border:1px solid #cbd2d6;background:#fff;text-align:center}.empty-list>span{color:#9aa3a0;font-family:var(--font-mono)}.empty-list h2{margin:0}.empty-list p,.empty-detail span{color:#65737a}.state{padding:2rem;color:#65737a}.feedback{margin:1rem 0;padding:.7rem .85rem;border-left:4px solid}.feedback.error{border-color:#b3453e;background:#f8e9e7;color:#862f2a}.feedback.notice{border-color:#136b52;background:#edf6f1;color:#24533f}
  .detail-header,.review-header{padding:1.1rem 2rem;border-bottom:1px solid #cbd2d6;background:#fff}.detail-title,.review-title{margin-top:.7rem}.task-actions{display:flex;gap:.6rem;align-items:center}.collection-progress{margin-top:.8rem}.collection-progress>div:first-child{display:flex;justify-content:space-between;color:#5d6c64;font-size:.78rem}.collection-progress>div:last-child{height:5px;margin-top:.35rem;background:#dce3df}.collection-progress i{display:block;height:100%;background:#136b52;transition:width .15s linear}.ledger-page{max-width:1600px;margin:0 auto;padding:1.2rem 2rem 104px}.group-tabs{display:grid;grid-template-columns:repeat(3,1fr);gap:.7rem}.group-tabs button{display:grid;grid-template-columns:1fr auto;gap:.2rem;padding:.8rem 1rem;border:1px solid #c6ced0;border-left:4px solid #7b898f;background:#fff;color:#17232d;text-align:left;cursor:pointer}.group-tabs button.active{border-left-color:#136b52;background:#eaf3ee}.group-tabs span{font-weight:700}.group-tabs strong{grid-row:1 / span 2;grid-column:2;color:#136b52;font:1.5rem var(--font-mono)}.group-tabs small{color:#68767d}.ledger-tools{display:flex;gap:.45rem}.ledger-tools input{width:min(320px,34vw);padding:.52rem .6rem;border:1px solid #aeb9bf}.ledger-tools button{padding:.52rem .65rem;border:1px solid #9da9ae;background:#fff;color:#344149;font-weight:700;cursor:pointer}.sender-cell{max-width:260px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}.result-cell{max-width:300px;color:#4f5d55}.group-badge{display:inline-block;padding:.25rem .45rem;border-left:3px solid #68787f;background:#eef1f1;color:#4d5c63;font-size:.72rem;font-weight:700;white-space:nowrap}.group-badge.needs_action{border-color:#c47a16;background:#fff0dc;color:#86520a}.group-badge.pending{border-color:#136b52;background:#edf6f1;color:#24533f}.group-badge.reviewed{border-color:#7d8983;background:#eef1ef;color:#4e5c55}.no-rows{padding:2rem;text-align:center;color:#68767d}.pagination{display:flex;align-items:center;justify-content:space-between;padding:.7rem 1rem;border-top:1px solid #cbd2d6;color:#68767d;font-size:.76rem}.pagination div{display:flex;align-items:center;gap:.6rem}.pagination button{padding:.4rem .55rem;border:1px solid #9da9ae;background:#fff;color:#344149;cursor:pointer}.pagination button:disabled{opacity:.4;cursor:not-allowed}.pagination strong{font-family:var(--font-mono)}.task-review-bar,.review-actions{position:fixed;right:0;bottom:0;left:224px;z-index:95;display:flex;justify-content:space-between;align-items:center;gap:1rem;min-height:78px;padding:.8rem 2rem;border-top:1px solid #9eabb1;background:rgba(255,255,255,.98);box-shadow:0 -7px 24px rgba(30,45,38,.1)}.task-review-bar div,.review-actions>div:first-child{display:grid}.task-review-bar span,.review-actions span{color:#68767d;font-size:.72rem}.task-review-bar button{padding:.65rem .9rem;border:1px solid #136b52;background:#136b52;color:#fff;font-weight:700;cursor:pointer}.task-review-bar button:disabled{opacity:.4;cursor:not-allowed}
  .review-nav{display:flex;align-items:center;gap:.5rem}.review-nav span{margin-left:auto;color:#68767d;font:.75rem var(--font-mono)}.review-nav button:not(.back){padding:.35rem .5rem;border:1px solid #a7b0ab;background:#fff;color:#344139;cursor:pointer}.review-nav button:disabled{opacity:.4}.review-title{align-items:center}.review-title h1{max-width:1050px;font-size:clamp(1.45rem,2vw,2.1rem)}.review-content{display:grid;gap:1rem;max-width:1240px;margin:0 auto;padding:1.2rem 2rem 112px}.mail-overview,.body-card,.link-card,.attachment-card{border:1px solid #c4cdca;background:#fff}.mail-overview>header,.body-card>header,.link-card>header,.attachment-card>header{display:flex;align-items:center;justify-content:space-between;gap:1rem;padding:.75rem 1rem;border-bottom:1px solid #d7ddda}.mail-overview>header strong,.body-card>header>span{color:#596860;font-size:.75rem}.mail-overview-actions{display:flex;align-items:center;gap:.7rem}.mail-overview-actions button{padding:.38rem .55rem;border:1px solid #9da9ae;background:#fff;color:#344149;font-weight:700;cursor:pointer}.mail-overview dl{display:grid;grid-template-columns:1fr 1fr;margin:0}.mail-overview dl>div{display:grid;grid-template-columns:92px 1fr;gap:.7rem;padding:.65rem 1rem;border-bottom:1px solid #edf0ee}.mail-overview dt{color:#6d7972;font-size:.74rem}.mail-overview dd{margin:0;overflow-wrap:anywhere}.body-card h2,.link-card h2,.attachment-card h2{font-size:1rem}.body-card pre{max-height:440px;margin:0;padding:1rem;overflow:auto;background:#fbfaf6;color:#26332c;font:.8rem/1.75 var(--font-mono);white-space:pre-wrap;overflow-wrap:anywhere}.truncate-note{margin:0;padding:.55rem 1rem;border-top:1px solid #e1e5e2;background:#fff5e2;color:#7b581d;font-size:.75rem}.section-state{display:grid;justify-items:center;gap:.35rem;padding:1.5rem;color:#68767d;text-align:center}.section-state.error strong{color:#b3453e}.section-state button{padding:.45rem .65rem;border:1px solid #136b52;background:#fff;color:#136b52;font-weight:700;cursor:pointer}.link-list{display:grid}.link-list article{display:flex;align-items:center;justify-content:space-between;gap:1rem;padding:.7rem 1rem;border-bottom:1px solid #e0e5e2}.link-list article>div{display:grid}.link-list span{color:#68767d;font:.72rem var(--font-mono)}.link-list button{padding:.5rem .65rem;border:1px solid #136b52;background:#fff;color:#136b52;font-weight:700;cursor:pointer}.link-list button:disabled{opacity:.5;cursor:wait}.attachment-list{display:grid}.attachment-list article{display:grid;grid-template-columns:52px minmax(0,1fr) auto;gap:.75rem;align-items:center;padding:.7rem 1rem;border-bottom:1px solid #e0e5e2}.attachment-list article.unavailable{background:#f4f5f4;opacity:.75}.attachment-list article.user-excluded{border-left:4px solid #c47a16;background:#fff4df}.attachment-list article.user-excluded small{color:#86520a;font-weight:700}.file-kind{display:grid;height:44px;place-items:center;background:#e8eeeb;color:#315849;font:700 .64rem var(--font-mono)}.attachment-list article>div:nth-child(2){display:grid;gap:.12rem;min-width:0}.attachment-list p,.attachment-list small{margin:0;color:#68767d;font-size:.73rem}.attachment-list em{color:#8a570d;font-size:.72rem;font-style:normal}.attachment-actions{display:flex;flex-wrap:wrap;justify-content:flex-end;gap:.35rem}.attachment-actions button{padding:.4rem .55rem;border:1px solid #9aa69f;background:#fff;color:#344139;font-weight:700;cursor:pointer}.attachment-actions button.danger{border-color:#b3453e;color:#a53630}.attachment-actions button:disabled{opacity:.45;cursor:not-allowed}.attachment-actions span{color:#68767d;font-size:.72rem}.review-actions>div:last-child{display:flex;gap:.5rem}
  .modal-backdrop{position:fixed;inset:0;z-index:200;display:grid;place-items:center;background:rgba(20,31,36,.48)}.create-modal{width:min(620px,calc(100vw - 2rem));background:#fff;box-shadow:0 18px 60px rgba(20,31,36,.25)}.create-modal>header{display:flex;justify-content:space-between;padding:1rem 1.2rem;border-bottom:1px solid #cbd2d6}.create-modal header p{margin:.3rem 0 0;color:#68767d}.create-modal header>button{border:0;background:transparent;font-size:1.5rem;cursor:pointer}.create-modal form{display:grid;gap:.8rem;padding:1.1rem 1.2rem}.create-modal label{display:grid;gap:.3rem}.create-modal label span{font-size:.78rem;font-weight:700}.create-modal input{padding:.55rem .6rem;border:1px solid #aeb9bf;background:#fff}.create-modal input:disabled{background:#eef1f1;color:#637078}.date-fields{display:grid;grid-template-columns:1fr 1fr;gap:.7rem}.create-modal aside{display:grid;gap:.2rem;padding:.7rem .8rem;border-left:4px solid #c47a16;background:#fff4df}.create-modal aside span{color:#76551d;font-size:.77rem}.create-modal form>footer{display:flex;justify-content:flex-end;gap:.5rem}.sr-only{position:absolute;width:1px;height:1px;padding:0;margin:-1px;overflow:hidden;clip:rect(0,0,0,0);white-space:nowrap;border:0}
  @media(max-width:980px){.workflow-note{grid-template-columns:auto 1fr}.workflow-note i{display:none}.task-summary{grid-template-columns:repeat(2,1fr)}.mail-overview dl{grid-template-columns:1fr}.task-review-bar,.review-actions{left:0}.sender-cell{max-width:200px}}@media(max-width:720px){.collection-list-page,.detail-header,.review-header,.ledger-page,.review-content{padding-left:1rem;padding-right:1rem}.page-header,.detail-title,.review-title{display:grid;align-items:start}.group-tabs{grid-template-columns:1fr}.ledger-tools{display:grid}.ledger-tools input{width:100%}.mail-ledger>header{align-items:flex-start}.task-summary,.date-fields{grid-template-columns:1fr}.review-nav{flex-wrap:wrap}.mail-overview dl>div{grid-template-columns:76px 1fr}.attachment-list article{grid-template-columns:44px 1fr}.attachment-actions{grid-column:2}.review-actions{position:sticky;flex-wrap:wrap;padding:.7rem 1rem}.review-actions>div:last-child{flex-wrap:wrap}.subject-link{max-width:280px}}
</style>
