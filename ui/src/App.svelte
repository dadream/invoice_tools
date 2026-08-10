<script lang="ts">
  import { invokeSafe, describeError, type AppError, type ErrorKind } from './lib/ipc'

  interface VersionInfo { version: string; name: string }
  interface HealthInfo { log_dir: string; log_file: string; ok: boolean }

  let name = $state('用户')
  let greetMsg = $state('')
  let version = $state('')
  let health = $state<HealthInfo | null>(null)
  let lastError = $state<AppError | null>(null)

  async function greet() {
    lastError = null
    greetMsg = ''
    const result = await invokeSafe<string>('greet', { name })
    if (result.ok) greetMsg = result.data
    else lastError = result.error
  }

  async function demoError(kind: ErrorKind) {
    lastError = null
    greetMsg = ''
    const result = await invokeSafe<never>('trigger_error', { kind })
    if (!result.ok) lastError = result.error
  }

  $effect(() => {
    void (async () => {
      const v = await invokeSafe<VersionInfo>('get_version')
      if (v.ok) version = `${v.data.name} v${v.data.version}`
      const h = await invokeSafe<HealthInfo>('health_check')
      if (h.ok) health = h.data
    })()
  })
</script>

<main class="container">
  <h1>发票报销助手</h1>
  <p class="version">{version || '加载中…'}</p>

  <section class="card">
    <div class="row">
      <input placeholder="输入姓名…" bind:value={name} aria-label="姓名" />
      <button onclick={greet}>打招呼</button>
    </div>

    {#if greetMsg}<p class="result" role="status">{greetMsg}</p>{/if}

    {#if lastError}
      <p class="error" role="alert">
        [{lastError.kind}] {describeError(lastError)}
      </p>
    {/if}
  </section>

  <section class="card">
    <h2>错误处理演示</h2>
    <div class="row">
      <button onclick={() => greet()} data-testid="empty-name">
        触发验证错误（先清空姓名）
      </button>
      <button onclick={() => demoError('network')}>可恢复错误</button>
      <button onclick={() => demoError('internal')}>不可恢复错误</button>
    </div>
  </section>

  {#if health}
    <section class="info">
      <p>日志目录: <code>{health.log_dir}</code></p>
      <p>当前日志: <code>{health.log_file}</code></p>
    </section>
  {/if}
</main>

<style>
  .container { max-width: 820px; margin: 0 auto; padding: 2rem; }
  h1 { font-size: 2rem; margin-bottom: .25rem; }
  h2 { font-size: 1rem; margin: 0 0 .75rem; opacity: .8; }
  .version { opacity: .65; font-size: .85rem; margin-top: 0; }
  .card {
    background: rgba(127, 127, 127, .08);
    border: 1px solid rgba(127, 127, 127, .2);
    border-radius: 8px; padding: 1.25rem; margin-bottom: 1rem;
  }
  .row { display: flex; gap: .5rem; flex-wrap: wrap; }
  input { padding: .5rem .75rem; border: 1px solid rgba(127,127,127,.4); border-radius: 4px; font: inherit; }
  button { padding: .5rem 1rem; border: 0; border-radius: 4px; background: #0070f3; color: #fff; font: inherit; cursor: pointer; }
  button:hover { background: #0058c4; }
  .result { margin: 1rem 0 0; color: #0a7; }
  .error { margin: 1rem 0 0; color: #c33; }
  .info { font-size: .85rem; opacity: .75; }
  code { word-break: break-all; }
</style>
