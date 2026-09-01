import { describe, expect, it, vi } from 'vitest'

vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn() }))
vi.mock('@tauri-apps/api/webview', () => ({ getCurrentWebview: vi.fn() }))

const { fileExtension, fileName, isSupportedInvoiceFile, SUPPORTED_EXTENSIONS } =
  await import('./invoice')

describe('invoice file selection', () => {
  it('accepts every backend-supported image format', () => {
    for (const extension of ['png', 'jpg', 'jpeg', 'webp', 'bmp']) {
      expect(isSupportedInvoiceFile(String.raw`C:\发票\票据.${extension.toUpperCase()}`)).toBe(true)
    }
  })

  it('keeps structured and PDF formats available', () => {
    expect(SUPPORTED_EXTENSIONS).toEqual([
      'xml',
      'ofd',
      'pdf',
      'png',
      'jpg',
      'jpeg',
      'webp',
      'bmp',
    ])
  })

  it('rejects unsupported and executable files', () => {
    expect(isSupportedInvoiceFile('invoice.gif')).toBe(false)
    expect(isSupportedInvoiceFile('invoice.exe')).toBe(false)
    expect(isSupportedInvoiceFile('invoice.pdf.exe')).toBe(false)
    expect(isSupportedInvoiceFile('README')).toBe(false)
  })

  it('handles Windows and Unix paths without exposing the full path as a name', () => {
    const windowsPath = String.raw`C:\发票\六月.扫描.JPEG`
    expect(fileExtension('/tmp/INVOICE.PDF')).toBe('pdf')
    expect(fileExtension(windowsPath)).toBe('jpeg')
    expect(fileName('/tmp/INVOICE.PDF')).toBe('INVOICE.PDF')
    expect(fileName(windowsPath)).toBe('六月.扫描.JPEG')
  })
})
