// @vitest-environment jsdom

import { mount, tick, unmount } from 'svelte'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { Invoice } from '../../lib/types'
import { invokeSafe } from '../../lib/ipc'
import OriginalPreview from './OriginalPreview.svelte'

vi.mock('../../lib/ipc', () => ({
  invokeSafe: vi.fn(),
  describeError: vi.fn(() => '读取失败'),
}))

const invokeMock = vi.mocked(invokeSafe)
let mounted: ReturnType<typeof mount> | null = null
let createObjectUrl: ReturnType<typeof vi.fn>
let revokeObjectUrl: ReturnType<typeof vi.fn>
let canvasContext: Record<string, unknown>
let canvasFillText: ReturnType<typeof vi.fn>

const invoice: Invoice = {
  id: 7,
  batch_id: 4,
  invoice_number: '26112000000000000007',
  issue_date: '2026-06-18',
  amount: '1200.00',
  tax_amount: '67.92',
  buyer_name: '合成购方',
  seller_name: '合成销方',
  ticket_type: 'other',
  city: '北京',
  departure_time: null,
  checkin_date: null,
  file_path: 'C:/synthetic/invoice.png',
  created_at: '2026-06-18 12:00:00',
  verification_result: 'not_applicable',
  is_duplicate: false,
  duplicate_reason: null,
  is_excluded: false,
}

async function settlePreview() {
  for (let index = 0; index < 6; index += 1) {
    await Promise.resolve()
    await tick()
  }
}

beforeEach(() => {
  createObjectUrl = vi.fn(() => 'blob:synthetic-preview')
  revokeObjectUrl = vi.fn()
  Object.defineProperty(URL, 'createObjectURL', { configurable: true, value: createObjectUrl })
  Object.defineProperty(URL, 'revokeObjectURL', { configurable: true, value: revokeObjectUrl })
  canvasFillText = vi.fn()
  canvasContext = { setTransform: vi.fn(), fillRect: vi.fn(), fillText: canvasFillText, fillStyle: '', textBaseline: '', font: '' }
  Object.defineProperty(HTMLCanvasElement.prototype, 'getContext', { configurable: true, value: vi.fn(() => canvasContext) })
  invokeMock.mockImplementation(async (command) => {
    if (command === 'get_invoice_preview_metadata') {
      return {
        ok: true,
        data: {
          file_name: 'invoice.png',
          extension: 'png',
          mime_type: 'image/png',
          preview_kind: 'image',
          bytes: 4,
        },
      } as never
    }
    if (command === 'read_invoice_preview') {
      return { ok: true, data: new Uint8Array([137, 80, 78, 71]).buffer } as never
    }
    throw new Error(`unexpected command: ${command}`)
  })
})

afterEach(async () => {
  if (mounted) await unmount(mounted)
  mounted = null
  invokeMock.mockReset()
  document.body.innerHTML = ''
})

describe('OriginalPreview', () => {
  it('exposes the bounded preview shell used by the parent viewer scroll layout', async () => {
    mounted = mount(OriginalPreview, { target: document.body, props: { invoice } })
    await settlePreview()

    expect(document.querySelector('.preview-shell')).not.toBeNull()
    expect(document.querySelector('.preview-shell > .canvas')).not.toBeNull()
  })

  it('loads one stable Blob URL without retriggering the effect from preview state', async () => {
    mounted = mount(OriginalPreview, {
      target: document.body,
      props: { invoice },
    })
    await settlePreview()

    expect(invokeMock.mock.calls.map(([command]) => command)).toEqual([
      'get_invoice_preview_metadata',
      'read_invoice_preview',
    ])
    expect(createObjectUrl).toHaveBeenCalledOnce()
    expect(revokeObjectUrl).not.toHaveBeenCalled()
    expect(document.querySelector<HTMLImageElement>('img')?.src).toBe('blob:synthetic-preview')

    await settlePreview()
    expect(invokeMock).toHaveBeenCalledTimes(2)
  })

  it('revokes the Blob URL when the preview is destroyed', async () => {
    mounted = mount(OriginalPreview, {
      target: document.body,
      props: { invoice },
    })
    await settlePreview()

    await unmount(mounted)
    mounted = null

    expect(revokeObjectUrl).toHaveBeenCalledWith('blob:synthetic-preview')
  })

  it('renders OFD through the passive page-layout command without reading raw bytes', async () => {
    invokeMock.mockImplementation(async (command) => {
      if (command === 'get_invoice_preview_metadata') return { ok: true, data: { file_name: 'invoice.ofd', extension: 'ofd', mime_type: 'application/ofd', preview_kind: 'ofd', bytes: 2048, page_count: 1 } } as never
      if (command === 'render_ofd_preview_page') return { ok: true, data: { page: 1, width_mm: 210, height_mm: 297, texts: [{ text: '电子发票', x_mm: 20, y_mm: 20, width_mm: 40, height_mm: 5, font_size_mm: 4 }] } } as never
      throw new Error(`unexpected command: ${command}`)
    })

    mounted = mount(OriginalPreview, { target: document.body, props: { invoice } })
    await settlePreview()

    expect(invokeMock.mock.calls.map(([command]) => command)).toEqual(['get_invoice_preview_metadata', 'render_ofd_preview_page'])
    expect(document.body.textContent).toContain('OFD 应用内只读版式预览')
    expect(canvasFillText).toHaveBeenCalled()
  })

  it('falls back to a PDF text layout when Windows raster rendering fails', async () => {
    invokeMock.mockImplementation(async (command) => {
      if (command === 'get_invoice_preview_metadata') return { ok: true, data: { file_name: 'invoice.pdf', extension: 'pdf', mime_type: 'application/pdf', preview_kind: 'pdf', bytes: 2048, page_count: 1 } } as never
      if (command === 'render_pdf_preview_page') return { ok: false, error: { message: 'renderer unavailable' } } as never
      if (command === 'render_pdf_text_preview_page') return { ok: true, data: { page: 1, width: 595, height: 842, texts: [{ text: '发票金额 126.00', x: 20, y: 20, width: 120, height: 12 }] } } as never
      throw new Error(`unexpected command: ${command}`)
    })

    mounted = mount(OriginalPreview, { target: document.body, props: { invoice } })
    await settlePreview()

    expect(invokeMock.mock.calls.map(([command]) => command)).toEqual(['get_invoice_preview_metadata', 'render_pdf_preview_page', 'render_pdf_text_preview_page'])
    expect(document.body.textContent).toContain('Windows PDF 渲染不可用')
    expect(canvasFillText).toHaveBeenCalled()
  })

  it('accepts a serialized byte array from Tauri and shows the rendered PDF page', async () => {
    invokeMock.mockImplementation(async (command) => {
      if (command === 'get_invoice_preview_metadata') return { ok: true, data: { file_name: 'invoice.pdf', extension: 'pdf', mime_type: 'application/pdf', preview_kind: 'pdf', bytes: 2048, page_count: 1 } } as never
      if (command === 'render_pdf_preview_page') return { ok: true, data: [137, 80, 78, 71, 13, 10, 26, 10, 0] } as never
      throw new Error(`unexpected command: ${command}`)
    })

    mounted = mount(OriginalPreview, { target: document.body, props: { invoice } })
    await settlePreview()

    expect(invokeMock.mock.calls.map(([command]) => command)).toEqual(['get_invoice_preview_metadata', 'render_pdf_preview_page'])
    expect(createObjectUrl).toHaveBeenCalledOnce()
    expect(document.querySelector<HTMLImageElement>('img')?.alt).toContain('PDF 原件第 1 页')
  })

  it('uses the text-layout fallback when the IPC response is not a PNG', async () => {
    invokeMock.mockImplementation(async (command) => {
      if (command === 'get_invoice_preview_metadata') return { ok: true, data: { file_name: 'invoice.pdf', extension: 'pdf', mime_type: 'application/pdf', preview_kind: 'pdf', bytes: 2048, page_count: 1 } } as never
      if (command === 'render_pdf_preview_page') return { ok: true, data: [1, 2, 3, 4] } as never
      if (command === 'render_pdf_text_preview_page') return { ok: true, data: { page: 1, width: 595, height: 842, texts: [{ text: '铁路电子客票', x: 20, y: 20, width: 120, height: 12 }] } } as never
      throw new Error(`unexpected command: ${command}`)
    })

    mounted = mount(OriginalPreview, { target: document.body, props: { invoice } })
    await settlePreview()

    expect(invokeMock.mock.calls.map(([command]) => command)).toEqual(['get_invoice_preview_metadata', 'render_pdf_preview_page', 'render_pdf_text_preview_page'])
    expect(document.body.textContent).toContain('Windows PDF 渲染不可用')
    expect(canvasFillText).toHaveBeenCalled()
  })
})
