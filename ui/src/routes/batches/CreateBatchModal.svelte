<script lang="ts">
  import { onMount } from 'svelte'

  interface Props {
    onSubmit: (name: string) => Promise<string | null | void>
    onCancel: () => void
  }

  let { onSubmit, onCancel }: Props = $props()

  let name = $state('')
  let submitting = $state(false)
  let formError = $state<string | null>(null)
  let dialogElement: HTMLDivElement
  let nameInput: HTMLInputElement

  onMount(() => {
    const previouslyFocused = document.activeElement instanceof HTMLElement
      ? document.activeElement
      : null

    queueMicrotask(() => nameInput?.focus())

    return () => previouslyFocused?.focus()
  })

  async function handleSubmit(e: Event) {
    e.preventDefault()

    if (!name.trim()) {
      formError = '请输入批次名称'
      nameInput?.focus()
      return
    }

    submitting = true
    try {
      formError = (await onSubmit(name.trim())) ?? null
    } finally {
      submitting = false
    }
  }

  function handleWindowKeydown(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.preventDefault()
      onCancel()
      return
    }

    if (e.key !== 'Tab') return

    const focusable = Array.from(
      dialogElement.querySelectorAll<HTMLElement>(
        'input:not([disabled]), button:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])',
      ),
    )
    if (focusable.length === 0) {
      e.preventDefault()
      dialogElement.focus()
      return
    }

    const first = focusable[0]
    const last = focusable[focusable.length - 1]
    const active = document.activeElement

    if (!dialogElement.contains(active)) {
      e.preventDefault()
      ;(e.shiftKey ? last : first).focus()
    } else if (e.shiftKey && active === first) {
      e.preventDefault()
      last.focus()
    } else if (!e.shiftKey && active === last) {
      e.preventDefault()
      first.focus()
    }
  }
</script>

<svelte:window onkeydown={handleWindowKeydown} />

<!-- svelte-ignore a11y_click_events_have_key_events -->
<div
  class="modal-overlay"
  role="presentation"
  onclick={(e) => e.target === e.currentTarget && onCancel()}
>
  <div
    bind:this={dialogElement}
    class="modal"
    role="dialog"
    aria-modal="true"
    aria-labelledby="modal-title"
    tabindex="-1"
  >
    <h2 id="modal-title">创建批次</h2>
    <p class="intro">批次只是本次整理工作的容器。创建后，再选择已收集的邮件附件或本地发票文件。</p>

    <form onsubmit={handleSubmit}>
      <div class="form-group">
        <label for="name">批次名称</label>
        <input
          bind:this={nameInput}
          id="name"
          type="text"
          bind:value={name}
          placeholder="例：2026年5-6月差旅报销"
          maxlength="100"
          required
          oninput={() => (formError = null)}
        />
      </div>

      {#if formError}<p class="form-error" role="alert">{formError}</p>{/if}

      <div class="form-actions">
        <button type="button" class="btn-secondary" onclick={onCancel}>
          取消
        </button>
        <button type="submit" class="btn-primary" disabled={submitting}>
          {submitting ? '创建中...' : '创建'}
        </button>
      </div>
    </form>
  </div>
</div>

<style>
  .modal-overlay {
    position: fixed;
    top: 0;
    left: 0;
    right: 0;
    bottom: 0;
    background: rgba(0, 0, 0, 0.5);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 1000;
  }

  .modal {
    background: #fff;
    border-radius: 8px;
    padding: 2rem;
    width: 90%;
    max-width: 500px;
    box-shadow: 0 4px 16px rgba(0, 0, 0, 0.2);
  }

  h2 { margin: 0 0 1.5rem; }
  .intro { margin: -0.8rem 0 1.5rem; color: #59645e; font-size: 0.88rem; line-height: 1.55; }

  .form-group { margin-bottom: 1.5rem; }
  .form-error { margin: -1rem 0 1rem; padding: .55rem .65rem; border-left: 4px solid var(--risk,#b33a32); background: #f8e9e7; color: #862f2a; font-size: .8rem; }
  label { display: block; margin-bottom: 0.5rem; font-weight: 500; }
  input {
    width: 100%;
    padding: 0.5rem;
    border: 1px solid #ccc;
    border-radius: 4px;
    font-size: 1rem;
  }
  input:focus { border-color: var(--pine); outline: 2px solid #a8c8ba; }

  .form-actions {
    display: flex;
    gap: 0.5rem;
    justify-content: flex-end;
    margin-top: 2rem;
  }

  .btn-primary,
  .btn-secondary {
    padding: 0.5rem 1rem;
    border: none;
    border-radius: 4px;
    cursor: pointer;
    font-size: 1rem;
  }

  .btn-primary {
    background: var(--pine);
    color: #fff;
  }
  .btn-primary:hover:not(:disabled) { background: #0f5844; }
  .btn-primary:disabled { opacity: 0.5; cursor: not-allowed; }

  .btn-secondary {
    background: #eee;
    color: #333;
  }
  .btn-secondary:hover { background: #ddd; }
</style>
