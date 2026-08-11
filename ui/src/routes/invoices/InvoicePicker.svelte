<script lang="ts">
  import { invokeSafe, describeError } from '../../lib/ipc'
  import type { ParsedInvoice, TicketType } from '../../lib/types'
  import { TICKET_TYPES, TICKET_TYPE_LABELS } from '../../lib/types'
  import {
    pickInvoiceFile,
    onInvoiceDrop,
    fileName,
    SUPPORTED_EXTENSIONS,
  } from '../../lib/invoice'

  interface Props {
    /** 解析成功后回传结果与所选票种，票种要一路带到入库命令 */
    onParsed: (parsed: ParsedInvoice, ticketType: TicketType) => void
  }

  let { onParsed }: Props = $props()

  // 票种必须由用户选择：解析器不推断票种，只把入参原样写进结果
  let ticketType = $state<TicketType>('other')
  let parsing = $state(false)
  let error = $state<string | null>(null)
  let hovering = $state(false)
  let pendingName = $state<string | null>(null)

  async function parsePath(path: string) {
    parsing = true
    error = null
    pendingName = fileName(path)

    // 参数名是 camelCase：#[tauri::command] 默认把 ticket_type 转成 ticketType
    const result = await invokeSafe<ParsedInvoice>('parse_invoice', {
      path,
      ticketType,
    })

    parsing = false
    pendingName = null

    if (result.ok) {
      onParsed(result.data, ticketType)
    } else {
      error = describeError(result.error)
    }
  }

  async function handlePick() {
    error = null
    let path: string | null
    try {
      path = await pickInvoiceFile()
    } catch (e) {
      // 对话框本身失败（例如权限未放开）不该冒泡成未捕获 rejection
      error = `打开文件选择器失败: ${e instanceof Error ? e.message : String(e)}`
      return
    }
    if (path === null) return
    await parsePath(path)
  }

  // 拖拽监听必须在 cleanup 里 unlisten，否则每次重建 effect 都会叠加一层
  $effect(() => {
    let unlisten: (() => void) | null = null
    let disposed = false

    onInvoiceDrop({
      onHover: (h) => {
        if (!parsing) hovering = h
      },
      onUnsupported: () => {
        error = `只支持 ${SUPPORTED_EXTENSIONS.join(' / ')} 格式的发票文件`
      },
      onDrop: (paths) => {
        // 一次只处理一张，避免并发解析把 parsing 状态搅乱
        if (parsing) return
        void parsePath(paths[0])
      },
    })
      .then((fn) => {
        if (disposed) {
          fn()
        } else {
          unlisten = fn
        }
      })
      .catch(() => {
        error = '拖拽功能不可用，请使用「选择文件」按钮'
      })

    return () => {
      disposed = true
      unlisten?.()
      unlisten = null
    }
  })
</script>

<section class="picker">
  <h3>添加发票</h3>

  <div class="form-row">
    <label for="ticket-type">票种</label>
    <select id="ticket-type" bind:value={ticketType} disabled={parsing}>
      {#each TICKET_TYPES as type}
        <option value={type}>{TICKET_TYPE_LABELS[type]}</option>
      {/each}
    </select>
  </div>
  <p class="hint">解析器不会自动判断票种，请先选好再选文件。</p>

  <div class="dropzone" class:hovering class:busy={parsing}>
    {#if parsing}
      <p class="status">正在解析 {pendingName ?? '文件'}...</p>
    {:else}
      <p class="status">把发票文件拖到这里</p>
      <p class="formats">支持 {SUPPORTED_EXTENSIONS.join(' / ')}</p>
      <button class="btn-primary" onclick={handlePick} disabled={parsing}>
        选择文件
      </button>
    {/if}
  </div>

  {#if error}
    <p class="error" role="alert">{error}</p>
  {/if}
</section>

<style>
  .picker { margin-bottom: 2rem; }
  h3 { margin: 0 0 1rem; font-size: 1rem; font-weight: 600; }

  .form-row { display: flex; align-items: center; gap: 0.75rem; margin-bottom: 0.5rem; }
  label { font-weight: 500; color: #666; }
  select {
    flex: 1;
    padding: 0.4rem 0.5rem;
    border: 1px solid #ccc;
    border-radius: 4px;
    font-size: 0.9rem;
    background: #fff;
  }
  select:focus { outline: none; border-color: #0070f3; }
  select:disabled { opacity: 0.5; }

  .hint { margin: 0 0 1rem; font-size: 0.8rem; color: #999; }

  .dropzone {
    border: 2px dashed #ccc;
    border-radius: 8px;
    padding: 1.5rem 1rem;
    text-align: center;
    background: #fafafa;
    transition: border-color 0.15s, background 0.15s;
  }
  .dropzone.hovering { border-color: #0070f3; background: #e3f2fd; }
  .dropzone.busy { border-style: solid; background: #f5f5f5; }

  .status { margin: 0 0 0.25rem; color: #666; font-size: 0.9rem; }
  .formats { margin: 0 0 0.75rem; color: #999; font-size: 0.8rem; }

  .btn-primary {
    padding: 0.5rem 1rem;
    background: #0070f3;
    color: #fff;
    border: none;
    border-radius: 4px;
    cursor: pointer;
    font-size: 0.9rem;
  }
  .btn-primary:hover:not(:disabled) { background: #0058c4; }
  .btn-primary:disabled { opacity: 0.5; cursor: not-allowed; }

  .error {
    margin: 0.75rem 0 0;
    padding: 0.5rem 0.75rem;
    background: #fdecec;
    border-radius: 4px;
    color: #c33;
    font-size: 0.85rem;
  }
</style>
