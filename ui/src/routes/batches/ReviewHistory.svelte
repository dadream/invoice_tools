<script lang="ts">
  import { describeError, invokeSafe } from '../../lib/ipc'
  import type { ReviewAction } from '../../lib/types'
  import ConfirmDialog from '../../lib/ConfirmDialog.svelte'

  interface Props {
    batchId: number
    actions: ReviewAction[]
    reviewError: string | null
    canEdit: boolean
    onChanged: () => Promise<void>
  }

  let { batchId, actions, reviewError, canEdit, onChanged }: Props = $props()
  let undoing = $state(false)
  let actionError = $state<string | null>(null)
  let confirmingUndo = $state(false)
  const nextUndo = $derived(actions.find((action) => action.undone_at === null) ?? null)

  async function undo() {
    if (!nextUndo) return
    confirmingUndo = false
    undoing = true
    actionError = null
    const result = await invokeSafe<ReviewAction>('undo_last_review_action', { batchId })
    undoing = false
    if (!result.ok) {
      actionError = describeError(result.error)
      return
    }
    await onChanged()
  }
</script>

<section class="history-section">
  <div class="section-heading">
    <div>
      <h3>审核历史</h3>
      <p>字段修改、重复项判断和归组调整按时间记录；撤销严格按相反顺序执行。</p>
    </div>
    {#if canEdit}
      <button class="undo" onclick={() => (confirmingUndo = true)} disabled={undoing || nextUndo === null}>
        {undoing ? '撤销中…' : '撤销上一步'}
      </button>
    {/if}
  </div>

  {#if reviewError}
    <p class="error" role="alert">{reviewError}</p>
  {:else if actionError}
    <p class="error" role="alert">{actionError}</p>
  {:else if actions.length === 0}
    <p class="empty">尚无人工审核操作。</p>
  {:else}
    <ol>
      {#each actions.slice(0, 20) as action}
        <li class:undone={action.undone_at !== null}>
          <div>
            <strong>{action.summary}</strong>
            <span>{action.created_at}</span>
          </div>
          {#if action.undone_at}
            <span class="badge">已撤销</span>
          {/if}
        </li>
      {/each}
    </ol>
    {#if actions.length > 20}
      <p class="footnote">仅显示最近 20 条，共 {actions.length} 条。</p>
    {/if}
  {/if}
</section>

{#if confirmingUndo && nextUndo}
  <ConfirmDialog title="撤销上一步审核操作" message={`将撤销“${nextUndo.summary}”并恢复操作前的数据。审核历史会保留这次撤销记录。`} confirmLabel="确认撤销" busy={undoing} onConfirm={() => void undo()} onCancel={() => (confirmingUndo = false)} />
{/if}

<style>
  .history-section { margin-bottom: 2rem; padding-top: 1rem; border-top: 1px solid #eee; }
  .section-heading { display: flex; justify-content: space-between; align-items: flex-start; gap: 1rem; }
  h3 { margin: 0; font-size: 1rem; font-weight: 600; }
  .section-heading p { margin: 0.35rem 0 0; color: #64748b; font-size: 0.82rem; }
  .undo { flex: none; padding: 0.4rem 0.7rem; border: 1px solid var(--pine,#136b52); border-radius: 3px; background: #fff; color: var(--pine,#136b52); cursor: pointer; }
  .undo:disabled { opacity: 0.45; cursor: not-allowed; }
  ol { display: grid; gap: 0.45rem; margin: 0.9rem 0 0; padding: 0; list-style: none; }
  li { display: flex; align-items: center; justify-content: space-between; gap: 1rem; padding: 0.55rem 0.7rem; border: 1px solid #e2e8f0; border-radius: 5px; background: #f8fafc; }
  li div { display: grid; gap: 0.2rem; }
  li strong { color: #334155; font-size: 0.85rem; }
  li span { color: #64748b; font-size: 0.75rem; }
  li.undone { opacity: 0.62; }
  .badge { padding: 0.18rem 0.4rem; border-radius: 999px; background: #e2e8f0; color: #475569; white-space: nowrap; }
  .empty, .footnote { color: #777; font-size: 0.85rem; }
  .error { color: #c33; font-size: 0.85rem; }
</style>
