<script lang="ts">
  interface Props {
    onSubmit: (name: string, month: string) => Promise<void>
    onCancel: () => void
  }

  let { onSubmit, onCancel }: Props = $props()

  let name = $state('')
  let month = $state('')
  let submitting = $state(false)

  // 默认月份为当前月
  $effect(() => {
    const now = new Date()
    month = `${now.getFullYear()}-${String(now.getMonth() + 1).padStart(2, '0')}`
  })

  async function handleSubmit(e: Event) {
    e.preventDefault()

    if (!name.trim()) {
      alert('请输入批次名称')
      return
    }

    if (!/^\d{4}-\d{2}$/.test(month)) {
      alert('月份格式错误，应为 YYYY-MM')
      return
    }

    submitting = true
    await onSubmit(name.trim(), month)
    submitting = false
  }
</script>

<!-- 只在点到遮罩本身时关闭，省掉内层 stopPropagation（那会要求内层再挂键盘处理器） -->
<div
  class="modal-overlay"
  onclick={(e) => e.target === e.currentTarget && onCancel()}
  role="button"
  tabindex="0"
  onkeydown={(e) => e.key === 'Escape' && onCancel()}
>
  <div class="modal" role="dialog" aria-modal="true" aria-labelledby="modal-title" tabindex="-1">
    <h2 id="modal-title">创建批次</h2>

    <form onsubmit={handleSubmit}>
      <div class="form-group">
        <label for="name">批次名称</label>
        <input
          id="name"
          type="text"
          bind:value={name}
          placeholder="例：2026年7月出差"
          maxlength="100"
          required
        />
      </div>

      <div class="form-group">
        <label for="month">归属月份</label>
        <input
          id="month"
          type="month"
          bind:value={month}
          required
        />
      </div>

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

  .form-group { margin-bottom: 1.5rem; }
  label { display: block; margin-bottom: 0.5rem; font-weight: 500; }
  input {
    width: 100%;
    padding: 0.5rem;
    border: 1px solid #ccc;
    border-radius: 4px;
    font-size: 1rem;
  }
  input:focus { outline: none; border-color: #0070f3; }

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
    background: #0070f3;
    color: #fff;
  }
  .btn-primary:hover:not(:disabled) { background: #0058c4; }
  .btn-primary:disabled { opacity: 0.5; cursor: not-allowed; }

  .btn-secondary {
    background: #eee;
    color: #333;
  }
  .btn-secondary:hover { background: #ddd; }
</style>
