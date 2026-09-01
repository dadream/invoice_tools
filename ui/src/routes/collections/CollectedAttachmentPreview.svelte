<script lang="ts">
  import { tick, untrack } from 'svelte'
  import { isPngBytes, normalizeIpcBytes } from '../../lib/binary'
  import { describeError, invokeSafe } from '../../lib/ipc'
  import type {
    CollectedEmailAttachment,
    InvoicePreviewMetadata,
    OfdPreviewPage,
    PdfTextPreviewPage,
  } from '../../lib/types'

  interface Props {
    attachment: CollectedEmailAttachment
    onClose: () => void
  }

  let { attachment, onClose }: Props = $props()
  let metadata = $state<InvoicePreviewMetadata | null>(null)
  let objectUrl = $state<string | null>(null)
  let textPreview = $state<string | null>(null)
  let layoutPreview = $state<OfdPreviewPage | PdfTextPreviewPage | null>(null)
  let layoutKind = $state<'ofd' | 'pdf-fallback' | null>(null)
  let layoutCanvas = $state<HTMLCanvasElement | null>(null)
  let loading = $state(false)
  let externalAction = $state<string | null>(null)
  let error = $state<string | null>(null)
  let page = $state(1)
  let zoom = $state(1)
  let rotation = $state(0)
  let loadSequence = 0
  let renderSequence = 0

  function releaseObjectUrl() {
    if (objectUrl) URL.revokeObjectURL(objectUrl)
    objectUrl = null
  }

  function reset() {
    releaseObjectUrl()
    metadata = null
    textPreview = null
    layoutPreview = null
    layoutKind = null
    error = null
    page = 1
    zoom = 1
    rotation = 0
    renderSequence += 1
  }

  async function loadPreview(attachmentId: number) {
    const sequence = ++loadSequence
    reset()
    loading = true
    const metadataResult = await invokeSafe<InvoicePreviewMetadata>('get_collected_attachment_preview_metadata', { attachmentId })
    if (sequence !== loadSequence) return
    if (!metadataResult.ok) {
      loading = false
      error = describeError(metadataResult.error)
      return
    }
    metadata = metadataResult.data
    if (metadata.preview_kind === 'pdf') return renderPdf(sequence, 1)
    if (metadata.preview_kind === 'ofd') return renderOfd(sequence, 1)
    if (!['image', 'text'].includes(metadata.preview_kind)) {
      loading = false
      return
    }
    const bytesResult = await invokeSafe<ArrayBuffer>('read_collected_attachment_preview', { attachmentId })
    if (sequence !== loadSequence) return
    loading = false
    if (!bytesResult.ok) {
      error = describeError(bytesResult.error)
      return
    }
    const bytes = normalizeIpcBytes(bytesResult.data)
    if (!bytes) {
      error = '附件数据格式异常，请使用系统程序打开'
      return
    }
    if (metadata.preview_kind === 'text') {
      textPreview = new TextDecoder('utf-8').decode(bytes)
      return
    }
    objectUrl = URL.createObjectURL(new Blob([Uint8Array.from(bytes).buffer], { type: metadata.mime_type ?? 'application/octet-stream' }))
  }

  async function renderPdf(sequence: number, requestedPage: number) {
    const targetPage = Math.min(metadata?.page_count ?? 1, Math.max(1, Math.round(requestedPage)))
    const currentRender = ++renderSequence
    loading = true
    error = null
    const result = await invokeSafe<ArrayBuffer>('render_collected_pdf_preview_page', { attachmentId: attachment.id, page: targetPage })
    if (sequence !== loadSequence || currentRender !== renderSequence) return
    const bytes = result.ok ? normalizeIpcBytes(result.data) : null
    if (!result.ok || !bytes || !isPngBytes(bytes)) {
      const fallback = await invokeSafe<PdfTextPreviewPage>('render_collected_pdf_text_preview_page', { attachmentId: attachment.id, page: targetPage })
      if (sequence !== loadSequence || currentRender !== renderSequence) return
      loading = false
      if (!fallback.ok) {
        const rasterError = result.ok ? 'Windows PDF 渲染返回了无效图像数据' : describeError(result.error)
        error = `${rasterError}；兼容版式也无法生成：${describeError(fallback.error)}`
        return
      }
      releaseObjectUrl()
      layoutPreview = fallback.data
      layoutKind = 'pdf-fallback'
      page = targetPage
      await tick()
      drawLayout()
      return
    }
    loading = false
    releaseObjectUrl()
    layoutPreview = null
    layoutKind = null
    objectUrl = URL.createObjectURL(new Blob([Uint8Array.from(bytes).buffer], { type: 'image/png' }))
    page = targetPage
  }

  async function renderOfd(sequence: number, requestedPage: number) {
    const targetPage = Math.min(metadata?.page_count ?? 1, Math.max(1, Math.round(requestedPage)))
    const currentRender = ++renderSequence
    loading = true
    error = null
    const result = await invokeSafe<OfdPreviewPage>('render_collected_ofd_preview_page', { attachmentId: attachment.id, page: targetPage })
    if (sequence !== loadSequence || currentRender !== renderSequence) return
    loading = false
    if (!result.ok) {
      error = describeError(result.error)
      return
    }
    releaseObjectUrl()
    layoutPreview = result.data
    layoutKind = 'ofd'
    page = targetPage
    await tick()
    drawLayout()
  }

  function drawLayout() {
    if (!layoutCanvas || !layoutPreview) return
    const ofd = layoutKind === 'ofd'
    const logicalWidth = ofd ? (layoutPreview as OfdPreviewPage).width_mm * 3.45 : (layoutPreview as PdfTextPreviewPage).width
    const logicalHeight = ofd ? (layoutPreview as OfdPreviewPage).height_mm * 3.45 : (layoutPreview as PdfTextPreviewPage).height
    const width = Math.max(320, Math.min(1_400, Math.round(logicalWidth)))
    const factor = width / Math.max(1, logicalWidth)
    const height = Math.max(420, Math.min(2_000, Math.round(logicalHeight * factor)))
    const density = Math.min(2, window.devicePixelRatio || 1)
    layoutCanvas.width = Math.round(width * density)
    layoutCanvas.height = Math.round(height * density)
    layoutCanvas.style.width = `${width}px`
    layoutCanvas.style.height = `${height}px`
    const context = layoutCanvas.getContext('2d')
    if (!context) return
    context.setTransform(density, 0, 0, density, 0, 0)
    context.fillStyle = '#fff'
    context.fillRect(0, 0, width, height)
    context.fillStyle = '#252b28'
    context.textBaseline = 'alphabetic'
    if (ofd) {
      const content = layoutPreview as OfdPreviewPage
      const unit = 3.45 * factor
      for (const item of content.texts) {
        context.font = `${Math.max(8, Math.min(34, item.font_size_mm * unit))}px "Source Han Sans SC"`
        context.fillText(item.text, item.x_mm * unit, (item.y_mm + Math.max(item.height_mm * .82, item.font_size_mm)) * unit, Math.max(8, item.width_mm * unit * 1.25))
      }
    } else {
      const content = layoutPreview as PdfTextPreviewPage
      for (const item of content.texts) {
        context.font = `${Math.max(7, Math.min(30, item.height * factor || 10))}px "Source Han Sans SC"`
        context.fillText(item.text, item.x * factor, (item.y + Math.max(item.height * .82, 7)) * factor, Math.max(8, item.width * factor * 1.35))
      }
    }
  }

  function changePage(nextPage: number) {
    if (metadata?.preview_kind === 'ofd') void renderOfd(loadSequence, nextPage)
    else void renderPdf(loadSequence, nextPage)
  }

  async function openExternal(reveal: boolean) {
    if (externalAction) return
    externalAction = reveal ? 'reveal' : 'open'
    const result = await invokeSafe<void>('open_collected_attachment', { attachmentId: attachment.id, reveal })
    externalAction = null
    if (!result.ok) error = describeError(result.error)
  }

  function handleKeydown(event: KeyboardEvent) {
    if (event.key === 'Escape') onClose()
  }

  $effect(() => {
    const attachmentId = attachment.id
    untrack(() => void loadPreview(attachmentId))
    return () => {
      loadSequence += 1
      releaseObjectUrl()
    }
  })
</script>

<svelte:window onkeydown={handleKeydown} />
<!-- svelte-ignore a11y_click_events_have_key_events -->
<div class="backdrop" role="presentation" onclick={(event) => event.target === event.currentTarget && onClose()}>
  <div class="dialog" role="dialog" aria-modal="true" aria-labelledby="attachment-preview-title">
    <header>
      <div><span class="eyebrow">邮件附件 · 只读预览</span><h2 id="attachment-preview-title">{metadata?.file_name ?? attachment.original_name}</h2></div>
      <div class="header-actions"><button type="button" onclick={() => openExternal(false)} disabled={externalAction !== null}>{externalAction === 'open' ? '正在打开…' : '系统打开'}</button><button type="button" onclick={() => openExternal(true)} disabled={externalAction !== null}>{externalAction === 'reveal' ? '正在定位…' : '所在文件夹'}</button><button class="close" type="button" aria-label="关闭预览" onclick={onClose}>×</button></div>
    </header>
    {#if metadata?.preview_kind === 'pdf' || metadata?.preview_kind === 'ofd'}
      <nav class="preview-tools" aria-label="分页预览工具"><button type="button" onclick={() => changePage(page - 1)} disabled={loading || page <= 1}>‹</button><input type="number" min="1" max={metadata.page_count ?? 1} value={page} onchange={(event) => changePage(Number(event.currentTarget.value))} aria-label="页码" /><span>/ {metadata.page_count ?? '—'}</span><button type="button" onclick={() => changePage(page + 1)} disabled={loading || page >= (metadata.page_count ?? 1)}>›</button><i></i><button type="button" onclick={() => (zoom = Math.max(.5, zoom - .1))}>−</button><output>{Math.round(zoom * 100)}%</output><button type="button" onclick={() => (zoom = Math.min(2.5, zoom + .1))}>＋</button><button type="button" onclick={() => (rotation = (rotation + 90) % 360)}>旋转</button></nav>
    {/if}
    <main>
      {#if loading}<div class="empty"><strong>正在读取附件…</strong></div>
      {:else if error}<div class="empty error" role="alert"><strong>应用内预览失败</strong><span>{error}</span><button type="button" onclick={() => openExternal(false)}>使用系统程序打开</button></div>
      {:else if metadata?.preview_kind === 'too_large'}<div class="empty warning"><strong>附件超过 20 MiB 预览上限</strong><span>原件未被修改，可使用系统程序打开。</span></div>
      {:else if metadata?.preview_kind === 'unsupported'}<div class="empty warning"><strong>该格式暂不支持应用内预览</strong><span>原件仍保留在同一邮件材料包中。</span></div>
      {:else if metadata?.preview_kind === 'image' && objectUrl}<div class="stage"><img src={objectUrl} alt={`附件预览：${metadata.file_name}`} style:transform={`rotate(${rotation}deg) scale(${zoom})`} /></div>
      {:else if metadata?.preview_kind === 'pdf' && objectUrl}<div class="stage"><img src={objectUrl} alt={`PDF 第 ${page} 页：${metadata.file_name}`} style:transform={`rotate(${rotation}deg) scale(${zoom})`} /></div>
      {:else if (metadata?.preview_kind === 'ofd' || layoutKind === 'pdf-fallback') && layoutPreview}<div class="stage layout"><span>{layoutKind === 'ofd' ? 'OFD 只读版式预览' : 'PDF 兼容文本版式预览'}</span><canvas bind:this={layoutCanvas} aria-label={`第 ${page} 页版式预览`} style:transform={`rotate(${rotation}deg) scale(${zoom})`}></canvas></div>
      {:else if metadata?.preview_kind === 'text' && textPreview !== null}<pre>{textPreview}</pre>
      {:else}<div class="empty"><strong>没有可显示的附件内容</strong></div>{/if}
    </main>
    {#if metadata}<footer><span>{metadata.extension.toUpperCase() || '未知格式'}</span><span>{(metadata.bytes / 1024).toFixed(1)} KiB</span><span>只读预览</span></footer>{/if}
  </div>
</div>

<style>
  .backdrop{position:fixed;inset:0;z-index:1000;display:grid;place-items:center;padding:1rem;background:rgb(15 24 19 / 52%)}.dialog{display:grid;grid-template-rows:auto auto minmax(0,1fr) auto;width:min(1180px,calc(100vw - 2rem));height:min(880px,calc(100vh - 2rem));border:1px solid #78847d;background:#f8f6ef;box-shadow:0 20px 70px rgb(10 20 15 / 30%)}header{display:flex;align-items:center;justify-content:space-between;gap:1rem;padding:.7rem 1rem;border-bottom:1px solid #c9cec9;background:#fff}.eyebrow{color:#67736c;font-family:var(--font-mono);font-size:.66rem;font-weight:700;letter-spacing:.1em}h2{max-width:650px;margin:.12rem 0 0;overflow:hidden;font:700 .9rem var(--font-mono);text-overflow:ellipsis;white-space:nowrap}.header-actions{display:flex;gap:.35rem}.header-actions button,.preview-tools button,.empty button{min-height:32px;padding:.35rem .55rem;border:1px solid #9ba59f;background:#fff;color:#26332c;font-weight:700;cursor:pointer}.header-actions .close{min-width:34px;border:0;font-size:1.25rem}.preview-tools{display:flex;align-items:center;gap:.3rem;padding:.45rem .8rem;border-bottom:1px solid #c9cec9;background:#f1efe7}.preview-tools input{width:3rem;min-height:30px;border:1px solid #9ba59f;text-align:center}.preview-tools i{flex:1}.preview-tools span,.preview-tools output{color:#57645d;font-size:.72rem}.preview-tools output{min-width:3rem;text-align:center}main{min-width:0;min-height:0;overflow:auto;padding:1rem;background:#e9e5da}.stage{display:grid;min-width:100%;min-height:100%;place-items:start center;padding-bottom:2rem;overflow:auto}.stage img,.stage canvas{max-width:100%;height:auto;transform-origin:top center;border:1px solid #bdb8ae;background:#fff;box-shadow:0 8px 24px rgb(23 33 28 / 14%)}.layout{gap:.5rem}.layout>span{position:sticky;top:0;z-index:2;padding:.3rem .5rem;border-left:3px solid #c47a16;background:#fff7e6;color:#76501a;font-size:.7rem}pre{min-height:100%;margin:0;padding:1rem;overflow:auto;border:1px solid #c5c0b5;background:#fff;font: .74rem/1.65 var(--font-mono);white-space:pre-wrap;overflow-wrap:anywhere}.empty{display:grid;min-height:360px;place-content:center;justify-items:center;gap:.45rem;color:#657068;text-align:center}.empty span{max-width:560px}.empty.error strong{color:#b3453e}.empty.warning strong{color:#8a570d}footer{display:flex;gap:.8rem;padding:.45rem .8rem;border-top:1px solid #c9cec9;background:#fff;color:#657068;font-size:.7rem}@media(max-width:720px){.backdrop{padding:0}.dialog{width:100vw;height:100vh}.dialog header{align-items:flex-start}.header-actions button:not(.close){display:none}.preview-tools{overflow-x:auto}h2{max-width:55vw}}
</style>
