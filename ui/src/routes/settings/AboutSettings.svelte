<script lang="ts">
  import { onMount } from 'svelte'
  import { open } from '@tauri-apps/plugin-shell'
  import { describeError, invokeSafe } from '../../lib/ipc'

  interface VersionInfo {
    version: string
    name: string
  }

  type UpdateStatus = 'not_configured' | 'up_to_date' | 'update_available' | 'current_version_newer'

  interface UpdateCheckResult {
    configured: boolean
    status: UpdateStatus
    currentVersion: string
    latestVersion?: string
    releaseSummary?: string
    sha256?: string
    downloadPageUrl?: string
    checkedAtUtc?: string
    message: string
  }

  let currentVersion = $state('读取中…')
  let checking = $state(false)
  let result = $state<UpdateCheckResult | null>(null)
  let error = $state<string | null>(null)

  async function loadVersion() {
    const response = await invokeSafe<VersionInfo>('get_version')
    currentVersion = response.ok ? response.data.version : '无法读取'
  }

  async function checkForUpdates() {
    checking = true
    result = null
    error = null
    const response = await invokeSafe<UpdateCheckResult>('check_for_updates')
    checking = false
    if (response.ok) {
      result = response.data
      currentVersion = response.data.currentVersion
    } else {
      error = describeError(response.error)
    }
  }

  async function openDownloadPage() {
    if (!result?.downloadPageUrl) return
    error = null
    try {
      await open(result.downloadPageUrl)
    } catch {
      error = '无法打开系统浏览器；请复制下方下载页地址。'
    }
  }

  onMount(() => {
    void loadVersion()
  })
</script>

<section class="settings-section">
  <h2>版本与支持</h2>
  <p class="description">查看本机版本，并在需要时手动检查官方版本清单。</p>

  <div class="card">
    <div class="version-row">
      <div>
        <span class="eyebrow">当前版本</span>
        <strong>{currentVersion}</strong>
      </div>
      <span class="channel">内部 Alpha · Windows x64 portable</span>
    </div>

    <div class="network-note" role="note">
      <strong>联网说明：</strong>应用不会自动检查、下载或替换程序。只有点击下方按钮时，才会读取固定的 HTTPS 版本清单。
    </div>

    <button class="primary" onclick={checkForUpdates} disabled={checking}>
      {checking ? '正在检查…' : '手动检查更新（会联网）'}
    </button>

    {#if result}
      <div class:available={result.status === 'update_available'} class="result" role="status">
        <strong>{result.message}</strong>
        {#if result.configured}
          <dl>
            <dt>发布版本</dt><dd>{result.latestVersion}</dd>
            <dt>检查时间</dt><dd>{result.checkedAtUtc ? new Date(result.checkedAtUtc).toLocaleString() : '—'}</dd>
            <dt>版本说明</dt><dd>{result.releaseSummary}</dd>
            <dt>ZIP SHA-256</dt><dd class="mono">{result.sha256}</dd>
            <dt>下载页</dt><dd class="mono">{result.downloadPageUrl}</dd>
          </dl>
          {#if result.downloadPageUrl}
            <button onclick={openDownloadPage}>在系统浏览器打开下载页</button>
          {/if}
        {:else}
          <p>公开发布地址尚未由产品负责人配置，因此本次没有发起网络请求。</p>
        {/if}
      </div>
    {/if}

    {#if error}<p class="error" role="alert">{error}</p>{/if}
  </div>

  <div class="card support-card">
    <h3>支持与反馈</h3>
    <p>内部验证期间请通过指定测试负责人反馈，不要在公开平台上传发票、邮箱地址、日志或备份。</p>
    <p class="muted">正式支持、隐私和安全邮箱将在公开 Beta 前由发布主体配置。</p>
  </div>
</section>

<style>
  .settings-section { max-width: 760px; }
  h2 { margin: 0 0 0.5rem; }
  h3 { margin: 0 0 0.5rem; font-size: 1rem; }
  .description, .card p { color: var(--text-secondary); line-height: 1.6; }
  .card { margin-top: 1rem; padding: 1rem; border: 1px solid var(--border-color); border-radius: 8px; }
  .version-row { display: flex; align-items: center; justify-content: space-between; gap: 1rem; }
  .version-row div { display: grid; gap: 0.2rem; }
  .version-row strong { font-size: 1.35rem; }
  .eyebrow { color: var(--text-secondary); font-size: 0.78rem; }
  .channel { padding: 0.3rem 0.55rem; border-radius: 999px; background: #fff4df; color: #6b4a12; font-size: 0.78rem; }
  .network-note { margin: 1rem 0; padding: 0.75rem; border: 1px solid #b6d7c9; border-radius: 6px; background: #edf7f3; color: #184f3d; line-height: 1.55; }
  button { padding: 0.55rem 0.9rem; border: 1px solid var(--border-color); border-radius: 6px; cursor: pointer; }
  button:disabled { cursor: not-allowed; opacity: 0.55; }
  .primary { color: white; background: var(--accent-primary); border-color: var(--accent-primary); }
  .result { margin-top: 1rem; padding: 0.85rem; border-radius: 6px; background: #f2f4f7; color: #344054; }
  .result.available { border: 1px solid #70b89c; background: #edf7f3; }
  dl { display: grid; grid-template-columns: 110px minmax(0, 1fr); gap: 0.45rem 0.75rem; margin: 0.85rem 0; }
  dt { color: var(--text-secondary); }
  dd { margin: 0; overflow-wrap: anywhere; }
  .mono { font-family: var(--font-mono); font-size: 0.82rem; }
  .error { padding: 0.75rem; border-radius: 6px; background: #fee; color: #b42318 !important; }
  .muted { font-size: 0.88rem; }
  @media (max-width: 720px) {
    .version-row { align-items: flex-start; flex-direction: column; }
    dl { grid-template-columns: 1fr; }
  }
</style>
