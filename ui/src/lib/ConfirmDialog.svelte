<script lang="ts">
  import { onMount } from 'svelte'

  interface Props {
    title: string
    message: string
    confirmLabel?: string
    cancelLabel?: string
    tone?: 'primary' | 'danger'
    busy?: boolean
    onConfirm: () => void
    onCancel: () => void
  }

  let {
    title,
    message,
    confirmLabel = '确认',
    cancelLabel = '取消',
    tone = 'primary',
    busy = false,
    onConfirm,
    onCancel,
  }: Props = $props()
  let confirmButton: HTMLButtonElement

  onMount(() => queueMicrotask(() => confirmButton?.focus()))

  function handleKeydown(event: KeyboardEvent) {
    if (event.key === 'Escape' && !busy) {
      event.preventDefault()
      onCancel()
    }
  }
</script>

<svelte:window onkeydown={handleKeydown} />

<!-- svelte-ignore a11y_click_events_have_key_events -->
<div class="backdrop" role="presentation" onclick={(event) => event.target === event.currentTarget && !busy && onCancel()}>
  <div class="dialog" role="alertdialog" aria-modal="true" aria-labelledby="confirm-title" aria-describedby="confirm-message">
    <span class="eyebrow">需要确认</span>
    <h2 id="confirm-title">{title}</h2>
    <p id="confirm-message">{message}</p>
    <footer>
      <button type="button" class="cancel" onclick={onCancel} disabled={busy}>{cancelLabel}</button>
      <button bind:this={confirmButton} type="button" class:danger={tone === 'danger'} class="confirm" onclick={onConfirm} disabled={busy}>{busy ? '正在处理…' : confirmLabel}</button>
    </footer>
  </div>
</div>

<style>
  .backdrop{position:fixed;inset:0;z-index:1200;display:grid;place-items:center;padding:1rem;background:rgb(15 24 19 / 48%)}.dialog{width:min(440px,100%);padding:1.25rem;border:1px solid #8c968f;background:#fbfaf6;box-shadow:0 18px 60px rgb(10 20 15 / 26%);color:#17211c}.eyebrow{color:#6c756f;font-family:var(--font-mono,'IBM Plex Mono',Consolas,monospace);font-size:.66rem;font-weight:700;letter-spacing:.09em}h2{margin:.25rem 0 0;font-size:1.18rem}p{margin:.75rem 0 1.1rem;color:#59645e;line-height:1.6}footer{display:flex;justify-content:flex-end;gap:.55rem}button{padding:.58rem .8rem;border:1px solid #8d968f;background:#fff;color:#344139;font-weight:700;cursor:pointer}.confirm{border-color:#136b52;background:#136b52;color:#fff}.confirm.danger{border-color:#a83932;background:#a83932}.cancel:disabled,.confirm:disabled{opacity:.5;cursor:not-allowed}
</style>
