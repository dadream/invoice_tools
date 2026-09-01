<script lang="ts">
  import { tick, untrack } from 'svelte'
  import { open } from '@tauri-apps/plugin-dialog'
  import { isPngBytes, normalizeIpcBytes } from '../../lib/binary'
  import { describeError, invokeSafe } from '../../lib/ipc'
  import type { Invoice, InvoiceDocument, InvoicePreviewMetadata, OfdPreviewPage, PdfTextPreviewPage, PendingInvoiceDocument } from '../../lib/types'

  interface Props {
    invoice: Invoice | null
    document?: InvoiceDocument | null
    pendingDocument?: PendingInvoiceDocument | null
  }

  let { invoice, document = null, pendingDocument = null }: Props = $props()
  let metadata = $state<InvoicePreviewMetadata | null>(null)
  let objectUrl = $state<string | null>(null)
  let textPreview = $state<string | null>(null)
  let layoutPreview = $state<OfdPreviewPage | PdfTextPreviewPage | null>(null)
  let layoutKind = $state<'ofd' | 'pdf-fallback' | null>(null)
  let layoutCanvas = $state<HTMLCanvasElement | null>(null)
  let loading = $state(false)
  let externalAction = $state<string | null>(null)
  let error = $state<string | null>(null)
  let zoom = $state(1)
  let pdfPage = $state(1)
  let pdfRotation = $state(0)
  let loadSequence = 0
  let pdfRenderSequence = 0

  function releaseObjectUrl() {
    if (objectUrl) URL.revokeObjectURL(objectUrl)
    objectUrl = null
  }

  function resetPreview() {
    releaseObjectUrl()
    metadata = null
    textPreview = null
    layoutPreview = null
    layoutKind = null
    error = null
    loading = false
    zoom = 1
    pdfPage = 1
    pdfRotation = 0
    pdfRenderSequence += 1
  }

  async function loadPreview(invoiceId: number | null, documentId: number | null, pendingDocumentId: number | null) {
    const sequence = ++loadSequence
    resetPreview()
    if (invoiceId === null && documentId === null && pendingDocumentId === null) return

    loading = true
    const metadataResult = await invokeSafe<InvoicePreviewMetadata>(
      pendingDocumentId !== null
        ? 'get_pending_document_preview_metadata'
        : documentId === null ? 'get_invoice_preview_metadata' : 'get_expense_document_preview_metadata',
      pendingDocumentId !== null ? { pendingDocumentId } : documentId === null ? { invoiceId } : { documentId },
    )
    if (sequence !== loadSequence) return
    if (!metadataResult.ok) {
      loading = false
      error = describeError(metadataResult.error)
      return
    }
    metadata = metadataResult.data
    if (!['image', 'pdf', 'ofd', 'text'].includes(metadata.preview_kind)) {
      loading = false
      return
    }
    if (metadata.preview_kind === 'pdf') {
      await renderPdfPage(sequence, 1)
      return
    }
    if (metadata.preview_kind === 'ofd') {
      await renderOfdPage(sequence, 1)
      return
    }

    const dataResult = await invokeSafe<ArrayBuffer>(
      pendingDocumentId !== null
        ? 'read_pending_document_preview'
        : documentId === null ? 'read_invoice_preview' : 'read_expense_document_preview',
      pendingDocumentId !== null ? { pendingDocumentId } : documentId === null ? { invoiceId } : { documentId },
    )
    if (sequence !== loadSequence) return
    loading = false
    if (!dataResult.ok) {
      error = describeError(dataResult.error)
      return
    }

    const bytes = normalizeIpcBytes(dataResult.data)
    if (!bytes) {
      error = '原件数据格式异常，请重新加载或使用系统打开'
      return
    }
    if (metadata.preview_kind === 'text') {
      textPreview = new TextDecoder('utf-8').decode(bytes)
      return
    }
    const blob = new Blob([Uint8Array.from(bytes).buffer], {
      type: metadata.mime_type ?? 'application/octet-stream',
    })
    objectUrl = URL.createObjectURL(blob)
  }

  async function renderPdfPage(invoiceSequence: number, requestedPage: number) {
    const pageCount = metadata?.page_count ?? 1
    const page = Math.min(pageCount, Math.max(1, Math.round(requestedPage)))
    const renderSequence = ++pdfRenderSequence
    loading = true
    error = null
    const result = await invokeSafe<ArrayBuffer>('render_pdf_preview_page', {
      ...currentIds(),
      page,
    })
    if (invoiceSequence !== loadSequence || renderSequence !== pdfRenderSequence) return
    const renderedBytes = result.ok ? normalizeIpcBytes(result.data) : null
    if (!result.ok || !renderedBytes || !isPngBytes(renderedBytes)) {
      const fallback = await invokeSafe<PdfTextPreviewPage>('render_pdf_text_preview_page', {
        ...currentIds(),
        page,
      })
      if (invoiceSequence !== loadSequence || renderSequence !== pdfRenderSequence) return
      loading = false
      if (!fallback.ok) {
        const rasterError = result.ok
          ? 'Windows PDF 渲染返回了无效图像数据'
          : describeError(result.error)
        error = `${rasterError}；兼容版式也无法生成：${describeError(fallback.error)}`
        return
      }
      releaseObjectUrl()
      layoutPreview = fallback.data
      layoutKind = 'pdf-fallback'
      pdfPage = page
      await tick()
      drawLayoutPreview()
      return
    }
    loading = false
    releaseObjectUrl()
    layoutPreview = null
    layoutKind = null
    objectUrl = URL.createObjectURL(new Blob([Uint8Array.from(renderedBytes).buffer], { type: 'image/png' }))
    pdfPage = page
  }

  async function renderOfdPage(invoiceSequence: number, requestedPage: number) {
    const pageCount = metadata?.page_count ?? 1
    const page = Math.min(pageCount, Math.max(1, Math.round(requestedPage)))
    const renderSequence = ++pdfRenderSequence
    loading = true
    error = null
    const result = await invokeSafe<OfdPreviewPage>('render_ofd_preview_page', {
      ...currentIds(),
      page,
    })
    if (invoiceSequence !== loadSequence || renderSequence !== pdfRenderSequence) return
    loading = false
    if (!result.ok) {
      error = describeError(result.error)
      return
    }
    releaseObjectUrl()
    layoutPreview = result.data
    layoutKind = 'ofd'
    pdfPage = page
    await tick()
    drawLayoutPreview()
  }

  function drawLayoutPreview() {
    if (!layoutCanvas || !layoutPreview) return
    const isOfd = layoutKind === 'ofd'
    const logicalWidth = isOfd ? (layoutPreview as OfdPreviewPage).width_mm * 3.45 : (layoutPreview as PdfTextPreviewPage).width
    const logicalHeight = isOfd ? (layoutPreview as OfdPreviewPage).height_mm * 3.45 : (layoutPreview as PdfTextPreviewPage).height
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
    context.fillStyle = '#ffffff'
    context.fillRect(0, 0, width, height)
    context.fillStyle = '#252b28'
    context.textBaseline = 'alphabetic'
    if (isOfd) {
      const page = layoutPreview as OfdPreviewPage
      const unit = 3.45 * factor
      for (const item of page.texts) {
        const fontSize = Math.max(8, Math.min(34, item.font_size_mm * unit))
        context.font = `${fontSize}px "Source Han Sans SC"`
        context.fillText(item.text, item.x_mm * unit, (item.y_mm + Math.max(item.height_mm * 0.82, item.font_size_mm)) * unit, Math.max(8, item.width_mm * unit * 1.25))
      }
    } else {
      const page = layoutPreview as PdfTextPreviewPage
      for (const item of page.texts) {
        const fontSize = Math.max(7, Math.min(30, item.height * factor || 10))
        context.font = `${fontSize}px "Source Han Sans SC"`
        context.fillText(item.text, item.x * factor, (item.y + Math.max(item.height * 0.82, 7)) * factor, Math.max(8, item.width * factor * 1.35))
      }
    }
  }

  function changePdfPage(page: number) {
    if (metadata?.preview_kind === 'ofd') void renderOfdPage(loadSequence, page)
    else void renderPdfPage(loadSequence, page)
  }

  function updateZoom(delta: number) {
    zoom = Math.min(2.5, Math.max(0.5, Math.round((zoom + delta) * 10) / 10))
  }

  function currentIds() {
    return {
      invoiceId: document || pendingDocument ? null : invoice?.id ?? null,
      documentId: pendingDocument ? null : document?.id ?? null,
      pendingDocumentId: pendingDocument?.id ?? null,
    }
  }

  function reloadPreview() {
    const ids = currentIds()
    void loadPreview(ids.invoiceId, ids.documentId, ids.pendingDocumentId)
  }

  async function openExternal(reveal: boolean) {
    if (externalAction) return
    externalAction = reveal ? 'reveal' : 'open'
    error = null
    const result = await invokeSafe<void>('open_preview_path', { ...currentIds(), reveal })
    externalAction = null
    if (!result.ok) error = describeError(result.error)
  }

  async function repairMissingOriginal() {
    if (externalAction) return
    const replacement = await open({
      multiple: false,
      directory: false,
      title: '重新选择缺失的原件',
      filters: [{ name: '发票与配套材料', extensions: ['xml', 'ofd', 'pdf', 'png', 'jpg', 'jpeg', 'webp', 'bmp'] }],
    })
    if (typeof replacement !== 'string') return
    externalAction = 'repair'
    const result = await invokeSafe<void>('repair_missing_preview_file', {
      ...currentIds(),
      replacementPath: replacement,
    })
    externalAction = null
    if (!result.ok) {
      error = describeError(result.error)
      return
    }
    reloadPreview()
  }

  $effect(() => {
    const invoiceId = invoice?.id ?? null
    const documentId = document?.id ?? null
    const pendingDocumentId = pendingDocument?.id ?? null
    // loadPreview 在第一次 await 前会重置并读取 objectUrl。若直接在 effect 的
    // 跟踪区内调用，异步创建 Blob URL 后会把 effect 自己再次触发，随即撤销 URL，
    // 造成原件区域持续闪烁且永远无法稳定显示。这里只跟踪发票 id，预览状态不参与依赖。
    untrack(() => void loadPreview(invoiceId, documentId, pendingDocumentId))
    return () => {
      loadSequence += 1
      releaseObjectUrl()
    }
  })
</script>

<section class="preview preview-shell" aria-labelledby="original-preview-title">
  <header>
    <div>
      <span class="eyebrow">原件</span>
      <h3 id="original-preview-title">{metadata?.file_name ?? '选择一张发票'}</h3>
    </div>
    {#if metadata?.preview_kind === 'image'}
      <div class="zoom-controls" aria-label="原件缩放">
        <button type="button" onclick={() => updateZoom(-0.1)} aria-label="缩小原件">−</button>
        <output>{Math.round(zoom * 100)}%</output>
        <button type="button" onclick={() => updateZoom(0.1)} aria-label="放大原件">+</button>
        <button type="button" class="reset" onclick={() => (zoom = 1)}>重置</button>
      </div>
    {/if}
    {#if invoice || document || pendingDocument}
      <div class="file-actions" aria-label="原件恢复操作">
        <button type="button" onclick={reloadPreview} disabled={loading || externalAction !== null}>重新加载</button>
        <button type="button" onclick={() => openExternal(false)} disabled={externalAction !== null}>{externalAction === 'open' ? '正在打开…' : '系统打开'}</button>
        <button type="button" onclick={() => openExternal(true)} disabled={externalAction !== null}>{externalAction === 'reveal' ? '正在定位…' : '打开所在文件夹'}</button>
        {#if error}<button type="button" class="repair" onclick={repairMissingOriginal} disabled={externalAction !== null}>{externalAction === 'repair' ? '正在校验…' : '重新关联原件'}</button>{/if}
      </div>
    {/if}
    {#if metadata?.preview_kind === 'pdf' || metadata?.preview_kind === 'ofd'}
      <div class="pdf-controls" aria-label="分页原件查看工具">
        <button type="button" onclick={() => changePdfPage(pdfPage - 1)} disabled={loading || pdfPage <= 1} aria-label="上一页">‹</button>
        <input type="number" min="1" max={metadata.page_count ?? 1} value={pdfPage} onchange={(event) => changePdfPage(Number(event.currentTarget.value))} aria-label="原件页码" />
        <span>/ {metadata.page_count ?? '—'}</span>
        <button type="button" onclick={() => changePdfPage(pdfPage + 1)} disabled={loading || pdfPage >= (metadata.page_count ?? 1)} aria-label="下一页">›</button>
        <button type="button" onclick={() => updateZoom(-0.1)} aria-label="缩小原件">−</button>
        <output>{Math.round(zoom * 100)}%</output>
        <button type="button" onclick={() => updateZoom(0.1)} aria-label="放大原件">+</button>
        <button type="button" onclick={() => (zoom = 1)}>适宽</button>
        <button type="button" onclick={() => (pdfRotation = (pdfRotation + 90) % 360)}>旋转</button>
      </div>
    {/if}
  </header>

  <div class="canvas" aria-live="polite">
    {#if invoice === null && document === null && pendingDocument === null}
      <div class="empty">
        <strong>尚未选择发票</strong>
        <span>从左侧分组中选择一张发票，原件会在此处显示。</span>
      </div>
    {:else if loading}
      <div class="empty"><strong>正在读取原件…</strong></div>
    {:else if error}
      <div class="empty error" role="alert">
        <strong>原件读取失败</strong>
        <span>{error}</span>
      </div>
    {:else if metadata?.preview_kind === 'too_large'}
      <div class="empty warning">
        <strong>原件超过应用内预览上限</strong>
        <span>{(metadata.bytes / 1024 / 1024).toFixed(1)} MiB；字段仍可审核，原件不会被修改。</span>
      </div>
    {:else if metadata?.preview_kind === 'unsupported'}
      <div class="empty warning">
        <strong>{metadata.extension.toUpperCase() || '该格式'} 暂不支持应用内可视化</strong>
        <span>原件仍保留并进入原始文件 ZIP；请依据右侧字段和解析证据审核。</span>
      </div>
    {:else if metadata?.preview_kind === 'image' && objectUrl}
      <div class="image-stage">
        <img
          src={objectUrl}
          alt={'发票原件：' + metadata.file_name}
          style:transform={'scale(' + zoom + ')'}
        />
      </div>
    {:else if metadata?.preview_kind === 'pdf' && objectUrl}
      <div class="image-stage pdf-stage">
        <img src={objectUrl} alt={`PDF 原件第 ${pdfPage} 页：${metadata.file_name}`} style:transform={`rotate(${pdfRotation}deg) scale(${zoom})`} />
      </div>
    {:else if (metadata?.preview_kind === 'ofd' || layoutKind === 'pdf-fallback') && layoutPreview}
      <div class="image-stage pdf-stage layout-stage">
        <div class="layout-notice">{layoutKind === 'ofd' ? 'OFD 应用内只读版式预览' : 'Windows PDF 渲染不可用，正在显示兼容文本版式'}</div>
        <canvas bind:this={layoutCanvas} aria-label={`${metadata?.file_name ?? '原件'} 第 ${pdfPage} 页版式预览`} style:transform={`rotate(${pdfRotation}deg) scale(${zoom})`}></canvas>
      </div>
    {:else if metadata?.preview_kind === 'text' && textPreview !== null}
      <pre>{textPreview}</pre>
    {:else}
      <div class="empty"><strong>没有可显示的原件内容</strong></div>
    {/if}
  </div>

  {#if metadata}
    <footer>
      <span>{metadata.extension.toUpperCase() || '未知格式'}</span>
      <span>{(metadata.bytes / 1024).toFixed(1)} KiB</span>
      <span>只读预览</span>
    </footer>
  {/if}
</section>

<style>
  .preview {
    display: grid;
    grid-template-rows: auto minmax(0, 1fr) auto;
    height: 100%;
    min-width: 0;
    min-height: 0;
    overflow: hidden;
    background: #ebe7dc;
  }
  header {
    display: flex;
    flex-wrap: wrap;
    min-height: 62px;
    align-items: center;
    justify-content: space-between;
    gap: 1rem;
    padding: 0.65rem 0.85rem;
    border-bottom: 1px solid var(--line, #d6d1c5);
    background: #f8f6ef;
  }
  .eyebrow {
    display: block;
    color: #637067;
    font-size: 0.68rem;
    font-weight: 700;
    letter-spacing: 0.12em;
  }
  h3 {
    max-width: 28rem;
    margin: 0.1rem 0 0;
    overflow: hidden;
    color: var(--ink, #17211c);
    font-family: var(--font-mono, 'IBM Plex Mono', Consolas, monospace);
    font-size: 0.82rem;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .zoom-controls,.pdf-controls { display: flex; align-items: center; gap: 0.3rem; }
  .zoom-controls button,
  .pdf-controls button,
  .file-actions button {
    min-width: 30px;
    min-height: 30px;
    border: 1px solid #a8afa9;
    border-radius: 3px;
    background: #fff;
    color: #17211c;
    cursor: pointer;
  }
  .zoom-controls .reset { padding-inline: 0.5rem; font-size: 0.72rem; }
  .pdf-controls input { width: 3rem; min-height: 30px; padding: .2rem; border: 1px solid #a8afa9; text-align: center; }
  .pdf-controls span { color: #536159; font-size: .7rem; }
  .pdf-controls button { padding-inline: .45rem; font-size: .7rem; }
  .file-actions { display: flex; flex-wrap: wrap; justify-content: flex-end; gap: 0.3rem; }
  .file-actions button { padding-inline: 0.5rem; font-size: 0.7rem; }
  .file-actions button.repair { border-color: #c47a16; color: #7c5314; }
  .file-actions button:disabled { opacity: 0.5; cursor: wait; }
  output { min-width: 3rem; color: #435048; font-size: 0.75rem; text-align: center; }
  .canvas { min-width: 0; min-height: 0; overflow: auto; overscroll-behavior: contain; scrollbar-gutter: stable; padding: 1rem; }
  .image-stage {
    display: grid;
    min-width: 100%;
    min-height: 100%;
    place-items: start center;
    overflow: visible;
  }
  img {
    max-width: 100%;
    height: auto;
    transform-origin: top center;
    border: 1px solid #c5c0b5;
    background: #fff;
    box-shadow: 0 8px 24px rgb(23 33 28 / 14%);
    transition: transform 120ms ease;
  }
  canvas {
    max-width: 100%;
    height: auto;
    transform-origin: top center;
    border: 1px solid #c5c0b5;
    background: #fff;
    box-shadow: 0 8px 24px rgb(23 33 28 / 14%);
    transition: transform 120ms ease;
  }
  .pdf-stage { padding-bottom: 2rem; }
  .layout-stage { position: relative; gap: .55rem; }
  .layout-notice { position: sticky; top: 0; z-index: 2; padding: .35rem .55rem; border-left: 3px solid var(--amber,#a65d00); background: #fff7e6; color: #76501a; font-size: .7rem; }
  pre {
    min-height: 100%;
    margin: 0;
    padding: 1rem;
    overflow: auto;
    border: 1px solid #c5c0b5;
    background: #fff;
    color: #243029;
    font-family: var(--font-mono, 'IBM Plex Mono', Consolas, monospace);
    font-size: 0.74rem;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .empty {
    display: grid;
    min-height: 360px;
    place-content: center;
    gap: 0.4rem;
    padding: 2rem;
    color: #69736d;
    text-align: center;
  }
  .empty strong { color: #354039; }
  .empty span { max-width: 32rem; font-size: 0.82rem; }
  .empty.error strong { color: var(--risk, #b33a32); }
  .empty.warning strong { color: #8a570d; }
  footer {
    display: flex;
    gap: 0.8rem;
    padding: 0.45rem 0.75rem;
    border-top: 1px solid var(--line, #d6d1c5);
    background: #f8f6ef;
    color: #657068;
    font-size: 0.7rem;
  }
  @media (prefers-reduced-motion: reduce) {
    img { transition: none; }
  }
</style>
