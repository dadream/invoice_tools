// @vitest-environment jsdom

import { mount, tick, unmount } from 'svelte'
import { afterEach, describe, expect, it, vi } from 'vitest'
import CreateBatchModal from './CreateBatchModal.svelte'

let mounted: ReturnType<typeof mount> | null = null

afterEach(async () => {
  if (mounted) await unmount(mounted)
  mounted = null
  document.body.innerHTML = ''
})

async function renderModal(onCancel = vi.fn()) {
  mounted = mount(CreateBatchModal, {
    target: document.body,
    props: {
      onSubmit: vi.fn(async () => undefined),
      onCancel,
    },
  })
  await tick()
  await Promise.resolve()
  return onCancel
}

describe('CreateBatchModal keyboard behavior', () => {
  it('moves initial focus into the batch name field', async () => {
    await renderModal()

    expect(document.activeElement).toBe(document.querySelector('#name'))
  })

  it('closes through Escape regardless of the currently focused control', async () => {
    const onCancel = await renderModal()
    const cancel = Array.from(document.querySelectorAll<HTMLButtonElement>('button'))
      .find((button) => button.textContent?.includes('取消'))
    cancel?.focus()

    window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape' }))

    expect(onCancel).toHaveBeenCalledOnce()
  })

  it('keeps Tab focus inside the dialog', async () => {
    await renderModal()
    const dialog = document.querySelector<HTMLElement>('[role="dialog"]')
    const first = document.querySelector<HTMLInputElement>('#name')
    const buttons = Array.from(document.querySelectorAll<HTMLButtonElement>('button'))
    const last = buttons.at(-1)
    last?.focus()

    window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Tab', cancelable: true }))

    expect(dialog?.contains(document.activeElement)).toBe(true)
    expect(document.activeElement).toBe(first)
  })
})
