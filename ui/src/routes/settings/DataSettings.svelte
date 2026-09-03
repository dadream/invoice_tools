<script lang="ts">
  import { open, save } from '@tauri-apps/plugin-dialog'
  import { describeError, invokeSafe } from '../../lib/ipc'
  import ConfirmDialog from '../../lib/ConfirmDialog.svelte'

  interface BackupExportResult {
    fileCount: number
    totalBytes: number
    archiveSha256: string
  }

  interface BackupImportPreview {
    formatVersion: number
    createdAtUtc: string
    fileCount: number
    totalBytes: number
    warning: string
  }
  interface CleanupCategory {
    name: string
    fileCount: number
    bytes: number
  }

  interface CleanupPreview {
    programDirectory: string
    dataDirectory: string
    programCleanupAvailable: boolean
    programFileCount: number
    dataFileCount: number
    totalBytes: number
    confirmationPhrase: string
    categories: CleanupCategory[]
    warning: string
  }


  let busy = $state(false)
  let message = $state<string | null>(null)
  let error = $state<string | null>(null)
  let cleanupFinalConfirmation = $state(false)
  let selectedBackup = $state<string | null>(null)
  let preview = $state<BackupImportPreview | null>(null)
  let confirmed = $state(false)
  let cleanupPreview = $state<CleanupPreview | null>(null)
  let includeProgram = $state(false)
  let includeData = $state(true)
  let cleanupConfirmation = $state('')
  let cleanupStarting = $state(false)

  function formatBytes(bytes: number) {
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KiB`
    return `${(bytes / 1024 / 1024).toFixed(1)} MiB`
  }

  async function exportBackup() {
    error = null
    message = null
    const date = new Date().toISOString().slice(0, 10)
    const destination = await save({
      title: '导出未加密备份',
      defaultPath: `发票报销助手备份-${date}.zip`,
      filters: [{ name: '备份 ZIP', extensions: ['zip'] }],
    })
    if (!destination) return

    busy = true
    const result = await invokeSafe<BackupExportResult>('export_backup', {
      destinationPath: destination,
    })
    busy = false
    if (!result.ok) {
      error = describeError(result.error)
      return
    }
    message = `备份完成：${result.data.fileCount} 个文件，${formatBytes(result.data.totalBytes)}。SHA-256：${result.data.archiveSha256}`
  }

  async function chooseBackup() {
    error = null
    message = null
    preview = null
    confirmed = false
    const path = await open({
      title: '选择发票报销助手备份',
      multiple: false,
      directory: false,
      filters: [{ name: '备份 ZIP', extensions: ['zip'] }],
    })
    if (!path || Array.isArray(path)) return
    selectedBackup = path
    busy = true
    const result = await invokeSafe<BackupImportPreview>('preview_backup_import', {
      backupPath: path,
    })
    busy = false
    if (!result.ok) {
      selectedBackup = null
      error = describeError(result.error)
      return
    }
    preview = result.data
  }

  async function stageImport() {
    if (!selectedBackup || !preview || !confirmed) return
    busy = true
    error = null
    message = null
    const result = await invokeSafe<BackupImportPreview>('stage_backup_import', {
      backupPath: selectedBackup,
    })
    busy = false
    if (!result.ok) {
      error = describeError(result.error)
      return
    }
    message = '备份已安全暂存。请关闭并重新打开应用；导入会在数据库打开前完成，失败时保留原数据。'
    preview = null
    selectedBackup = null
    confirmed = false
  }
  async function loadCleanupPreview() {
    busy = true
    error = null
    message = null
    cleanupConfirmation = ''
    const result = await invokeSafe<CleanupPreview>('preview_cleanup')
    busy = false
    if (!result.ok) {
      cleanupPreview = null
      error = describeError(result.error)
      return
    }
    cleanupPreview = result.data
    includeProgram = result.data.programCleanupAvailable
    includeData = true
  }

  async function confirmCleanup() {
    if (!cleanupPreview || cleanupStarting) return
    if (!includeData && !includeProgram) {
      error = '至少选择清除程序或本机数据'
      return
    }
    if (cleanupConfirmation !== cleanupPreview.confirmationPhrase) {
      error = `请输入确认短语“${cleanupPreview.confirmationPhrase}”`
      return
    }
    cleanupFinalConfirmation = false
    cleanupStarting = true
    error = null
    const result = await invokeSafe<void>('start_cleanup', {
      includeProgram,
      includeData,
      confirmation: cleanupConfirmation,
    })
    if (!result.ok) {
      cleanupStarting = false
      error = describeError(result.error)
    }
  }
</script>

<section class="settings-section">
  <h2>数据备份与迁移</h2>
  <p class="description">
    备份属于你，可以复制到另一台电脑导入。当前备份未加密，请像保护原始发票一样保管。
  </p>

  <div class="warning" role="note">
    <strong>不会备份：</strong>邮箱授权码、会话秘密、日志、临时缓存和本机邮箱账户配置。
  </div>

  <div class="card">
    <h3>导出未加密备份</h3>
    <p>包含发票台账的一致性快照和应用保存的发票原件；不会覆盖已有备份文件。</p>
    <button class="primary" onclick={exportBackup} disabled={busy}>
      {busy ? '处理中…' : '选择位置并导出'}
    </button>
  </div>

  <div class="card">
    <h3>从另一台电脑导入</h3>
    <p>预览先检查 ZIP 结构和清单；确认后再逐文件校验 SHA-256 与 SQLite 完整性，全部通过才进入待导入状态。</p>
    <button onclick={chooseBackup} disabled={busy}>选择并预览备份</button>

    {#if preview}
      <dl class="preview">
        <dt>格式版本</dt><dd>{preview.formatVersion}</dd>
        <dt>创建时间</dt><dd>{new Date(preview.createdAtUtc).toLocaleString()}</dd>
        <dt>文件数量</dt><dd>{preview.fileCount}</dd>
        <dt>数据大小</dt><dd>{formatBytes(preview.totalBytes)}</dd>
      </dl>
      <p class="warning">{preview.warning}</p>
      <label class="confirm-row">
        <input type="checkbox" bind:checked={confirmed} />
        我已确认：重启后将用此备份替换本机发票台账和原件，并已保留需要的数据副本。
      </label>
      <button class="danger" onclick={stageImport} disabled={busy || !confirmed}>
        准备导入并等待重启
      </button>
    {/if}
  </div>

  <div class="card danger-zone">
    <h3>退出并清除程序与数据</h3>
    <p>先生成只读预览，不会立即删除。清除由临时产品副本在主程序退出后执行，不申请管理员权限。</p>
    <button onclick={loadCleanupPreview} disabled={busy || cleanupStarting}>查看清除预览</button>

    {#if cleanupPreview}
      <div class="warning cleanup-warning" role="alert">
        <strong>不可恢复：</strong>{cleanupPreview.warning}
      </div>
      <dl class="preview paths">
        <dt>程序目录</dt><dd>{cleanupPreview.programDirectory}</dd>
        <dt>数据目录</dt><dd>{cleanupPreview.dataDirectory}</dd>
        <dt>总大小</dt><dd>{formatBytes(cleanupPreview.totalBytes)}</dd>
      </dl>
      <table class="category-table">
        <thead><tr><th>数据类别</th><th>文件</th><th>大小</th></tr></thead>
        <tbody>
          {#each cleanupPreview.categories as category}
            <tr>
              <td>{category.name}</td>
              <td>{category.fileCount}</td>
              <td>{formatBytes(category.bytes)}</td>
            </tr>
          {/each}
        </tbody>
      </table>
      <label class="confirm-row">
        <input type="checkbox" bind:checked={includeData} />
        清除本机数据（{cleanupPreview.dataFileCount} 个文件）
      </label>
      <label class="confirm-row" class:disabled={!cleanupPreview.programCleanupAvailable}>
        <input
          type="checkbox"
          bind:checked={includeProgram}
          disabled={!cleanupPreview.programCleanupAvailable}
        />
        清除标准 portable 包中的产品文件（{cleanupPreview.programFileCount} 个文件）
      </label>
      {#if !cleanupPreview.programCleanupAvailable}
        <p class="inline-hint">当前不是标准 portable 包，程序文件请手动删除；本机数据仍可安全清除。</p>
      {/if}
      <label class="phrase-field">
        输入“{cleanupPreview.confirmationPhrase}”确认
        <input type="text" bind:value={cleanupConfirmation} autocomplete="off" />
      </label>
      <button
        class="danger"
        onclick={() => {
          error = null
          if (!cleanupPreview || (!includeData && !includeProgram)) { error = '至少选择清除程序或本机数据'; return }
          if (cleanupConfirmation !== cleanupPreview.confirmationPhrase) { error = `请输入确认短语“${cleanupPreview.confirmationPhrase}”`; return }
          cleanupFinalConfirmation = true
        }}
        disabled={cleanupStarting || (!includeData && !includeProgram)}
      >
        {cleanupStarting ? '正在退出并启动清理…' : '永久清除所选内容'}
      </button>
    {/if}
  </div>

  {#if error}<p class="error" role="alert">{error}</p>{/if}
  {#if message}<p class="success" role="status">{message}</p>{/if}
</section>

{#if cleanupFinalConfirmation && cleanupPreview}
  <ConfirmDialog title="永久清除所选内容" message={`将永久清除 ${includeData ? `${cleanupPreview.dataFileCount} 个本机数据文件` : '不清除本机数据'}${includeProgram ? `及 ${cleanupPreview.programFileCount} 个 portable 程序文件` : ''}。主程序会退出，此操作不能撤销。`} confirmLabel="永久清除并退出" tone="danger" busy={cleanupStarting} onConfirm={() => void confirmCleanup()} onCancel={() => (cleanupFinalConfirmation = false)} />
{/if}

<style>
  .settings-section { max-width: 760px; }
  h2 { margin: 0 0 0.5rem; }
  h3 { margin: 0 0 0.5rem; font-size: 1rem; }
  .description, .card p { color: var(--text-secondary); line-height: 1.6; }
  .card { margin-top: 1rem; padding: 1rem; border: 1px solid var(--border-color); border-radius: 8px; }
  .warning { padding: 0.75rem; background: #fff8e6; border: 1px solid #e8c46a; border-radius: 6px; color: #704d00; }
  button { padding: 0.55rem 0.9rem; border: 1px solid var(--border-color); border-radius: 6px; cursor: pointer; }
  button:disabled { cursor: not-allowed; opacity: 0.55; }
  .primary { color: white; background: var(--accent-primary); border-color: var(--accent-primary); }
  .danger { margin-top: 0.75rem; color: white; background: #b42318; border-color: #b42318; }
  .preview { display: grid; grid-template-columns: 100px 1fr; gap: 0.4rem 1rem; margin: 1rem 0; }
  .preview dt { color: var(--text-secondary); }
  .preview dd { margin: 0; overflow-wrap: anywhere; }
  .confirm-row { display: flex; align-items: flex-start; gap: 0.5rem; margin-top: 0.75rem; line-height: 1.5; }
  .confirm-row input { margin-top: 0.25rem; }
  .error { color: #b42318; }
  .success { padding: 0.75rem; background: #ecfdf3; border-radius: 6px; color: #027a48; overflow-wrap: anywhere; }
  .danger-zone { border-color: #d92d20; }
  .cleanup-warning { margin-top: 1rem; }
  .paths dd { font-family: Consolas, monospace; font-size: 0.85rem; }
  .category-table { width: 100%; margin: 0.75rem 0; border-collapse: collapse; font-size: 0.9rem; }
  .category-table th, .category-table td { padding: 0.45rem; border-bottom: 1px solid var(--border-color); text-align: left; }
  .category-table th:nth-child(n+2), .category-table td:nth-child(n+2) { text-align: right; }
  .disabled { color: var(--text-secondary); }
  .phrase-field { display: grid; gap: 0.4rem; margin-top: 1rem; font-weight: 600; }
  .phrase-field input { max-width: 280px; padding: 0.55rem; border: 1px solid var(--border-color); border-radius: 6px; }
  .inline-hint { margin: 0.5rem 0; font-size: 0.85rem; color: var(--text-secondary); }
</style>
