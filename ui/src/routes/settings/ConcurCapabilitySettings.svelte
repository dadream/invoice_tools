<script lang="ts">
  import { onMount } from 'svelte'
  import { describeError, invokeSafe } from '../../lib/ipc'
  import type {
    ConcurBrowserOauthConfig,
    ConcurCapabilityTestStep,
    ConcurCapabilityTestResult,
    ConcurConnectionStatus,
  } from '../../lib/types'

  let status = $state<ConcurConnectionStatus>({
    configured: false,
    base_url: null,
    read_verified: false,
    draft_workflow_verified: false,
    verified_at: null,
    authorization_method: null,
    granted_scopes: [],
    connected_account: null,
    capability_checks: [],
    reason: '正在读取连接状态…',
  })
  let browserOauthConfig = $state<ConcurBrowserOauthConfig>({
    redirect_uri: '正在读取本机回调地址…',
    scopes: 'EXPRPT IMAGE',
    timeout_seconds: 300,
  })
  let baseUrl = $state('https://cn.api.concurcdc.cn')
  let clientId = $state('')
  let clientSecret = $state('')
  let browserConfirmed = $state(false)
  let accessToken = $state('')
  let expenseTypeCode = $state('')
  let paymentTypeId = $state('')
  let locationId = $state('')
  let confirmed = $state(false)
  let browserAuthorizing = $state(false)
  let connecting = $state(false)
  let testing = $state(false)
  let error = $state<string | null>(null)
  let result = $state<ConcurCapabilityTestResult | null>(null)

  async function loadStatus() {
    const response = await invokeSafe<ConcurConnectionStatus>('get_concur_connection_status')
    if (!response.ok) {
      error = describeError(response.error)
      return
    }
    status = response.data
    if (status.base_url) baseUrl = status.base_url
  }

  async function loadBrowserOauthConfig() {
    const response = await invokeSafe<ConcurBrowserOauthConfig>('get_concur_browser_oauth_config')
    if (!response.ok) {
      error = describeError(response.error)
      return
    }
    browserOauthConfig = response.data
  }

  async function testBrowserOauth() {
    if (!clientId.trim() || !clientSecret.trim() || !browserConfirmed || browserAuthorizing) return
    browserAuthorizing = true
    error = null
    result = null
    const request = invokeSafe<ConcurCapabilityTestResult>('test_concur_browser_oauth', {
      input: {
        base_url: baseUrl,
        client_id: clientId,
        client_secret: clientSecret,
        confirmed: browserConfirmed,
      },
    })
    clientSecret = ''
    browserConfirmed = false
    const response = await request
    browserAuthorizing = false
    if (!response.ok) {
      error = describeError(response.error)
      await loadStatus()
      return
    }
    result = response.data
    await loadStatus()
  }

  async function testReadAccess() {
    if (!accessToken.trim() || connecting) return
    connecting = true
    error = null
    result = null
    const response = await invokeSafe<ConcurCapabilityTestResult>('test_concur_read_access', {
      input: { base_url: baseUrl, access_token: accessToken },
    })
    connecting = false
    accessToken = ''
    if (!response.ok) {
      error = describeError(response.error)
      return
    }
    result = response.data
    await loadStatus()
  }

  async function testDraftWorkflow() {
    if (!canRunWorkflow || testing) return
    testing = true
    error = null
    result = null
    const response = await invokeSafe<ConcurCapabilityTestResult>('test_concur_draft_workflow', {
      input: {
        expense_type_code: expenseTypeCode,
        payment_type_id: paymentTypeId,
        location_id: locationId.trim() || null,
        confirmed,
      },
    })
    testing = false
    if (!response.ok) {
      error = describeError(response.error)
      return
    }
    result = response.data
    await loadStatus()
  }

  async function clearSession() {
    const response = await invokeSafe<ConcurConnectionStatus>('clear_concur_session')
    if (!response.ok) {
      error = describeError(response.error)
      return
    }
    status = response.data
    result = null
    confirmed = false
  }

  const canRunWorkflow = $derived(
    status.read_verified && expenseTypeCode.trim().length > 0 && paymentTypeId.trim().length > 0 && confirmed,
  )
  const latestSteps = $derived(result?.steps ?? status.capability_checks)
  const connectedAccount = $derived(result?.connected_account ?? status.connected_account)

  const capabilityDefinitions = [
    { key: 'report_read', label: '报销单读取', description: '读取当前账号自己的报销单', steps: ['report_read'] },
    { key: 'report_create', label: '草稿创建', description: '创建并回读未提交报销单', steps: ['report_create', 'report_readback'] },
    { key: 'expense_create', label: '费用创建', description: '创建并回读 0.01 元测试费用', steps: ['expense_create', 'expense_readback'] },
    { key: 'attachment_upload', label: '发票上传', description: '上传并回读一页测试 PDF', steps: ['attachment_upload', 'attachment_readback'] },
  ] as const

  function capabilityResult(keys: readonly string[]): ConcurCapabilityTestStep {
    const matches = keys.map((key) => latestSteps.find((step) => step.key === key)).filter(Boolean) as ConcurCapabilityTestStep[]
    const failure = matches.find((step) => step.status === 'failed')
    if (failure) return failure
    if (keys.every((key) => matches.some((step) => step.key === key && step.status === 'passed'))) {
      return matches[matches.length - 1] ?? { key: keys[0], label: '', status: 'passed', message: '已通过' }
    }
    return { key: keys[0], label: '', status: 'not_tested', message: '尚未执行此项测试' }
  }

  function capabilityLabel(statusValue: string): string {
    return statusValue === 'passed' ? '通过' : statusValue === 'failed' ? '失败' : '未测试'
  }

  function accountPrimary(): string {
    return connectedAccount?.display_name || connectedAccount?.login_id || (status.read_verified ? '当前授权账号' : '尚未连接账号')
  }

  function accountSecondary(): string {
    if (connectedAccount?.display_name && connectedAccount.login_id) return connectedAccount.login_id
    if (status.read_verified) return '报销单接口未返回账号名称；创建测试草稿后将再次识别'
    return '完成浏览器授权并通过报销单读取后显示'
  }

  function stepStatusLabel(value: string): string {
    return value === 'passed' ? '通过' : value === 'failed' ? '未通过' : '未测试'
  }

  function authorizationMethodLabel(value: string | null): string {
    if (value === 'browser_oauth') return '浏览器 OAuth'
    if (value === 'manual_access_token') return '手工令牌'
    return '尚未连接'
  }

  onMount(() => { void Promise.all([loadStatus(), loadBrowserOauthConfig()]) })
</script>

<section class="concur-settings" aria-labelledby="concur-settings-title">
  <header>
    <div>
      <h2 id="concur-settings-title">Concur 能力测试</h2>
      <p>先验证企业账号是否开放草稿、费用和附件接口，再允许报销批次执行真实交付。</p>
    </div>
    {#if status.configured}
      <button class="secondary" type="button" onclick={clearSession}>断开本次连接</button>
    {/if}
  </header>

  <section class:ready={status.draft_workflow_verified} class="account-card" aria-label="当前 Concur 连接账号">
    <div class="account-identity">
      <span class="eyebrow">已连接的账号</span>
      <strong>{accountPrimary()}</strong>
      <p>{accountSecondary()}</p>
    </div>
    <dl>
      <div><dt>连接状态</dt><dd>{status.read_verified ? '已连接' : '未连接'}</dd></div>
      <div><dt>授权方式</dt><dd>{authorizationMethodLabel(status.authorization_method)}</dd></div>
      <div><dt>数据中心</dt><dd>{status.base_url ?? '连接后自动确认'}</dd></div>
    </dl>
  </section>

  <section class="capability-overview" aria-labelledby="capability-overview-title">
    <header>
      <div><span class="eyebrow">测试结果</span><h3 id="capability-overview-title">本软件依赖的 Concur 能力</h3></div>
      <p>{status.reason}</p>
    </header>
    <div class="capability-grid">
      {#each capabilityDefinitions as capability}
        {@const check = capabilityResult(capability.steps)}
        <article class:passed={check.status === 'passed'} class:failed={check.status === 'failed'}>
          <span class="capability-status">{capabilityLabel(check.status)}</span>
          <strong>{capability.label}</strong>
          <p>{check.status === 'not_tested' ? capability.description : check.message}</p>
        </article>
      {/each}
    </div>
  </section>

  {#if error}<p class="message error" role="alert">{error}</p>{/if}

  <section class="test-section">
    <div class="section-heading">
      <span>01</span>
      <div><h3>连接 Concur 账号</h3><p>打开系统浏览器，选择用于报销的公司账号并完成官方 OAuth 授权。</p></div>
    </div>
    <div class="alpha-note">
      <strong>开始前准备</strong>
      <p>Client ID 和 Client Secret 由 Concur 管理员或产品测试负责人提供，不是用户的账号密码。普通用户只需在打开的浏览器中选择正确账号。</p>
    </div>
    <div class="form-grid three">
      <label>
        <span>Concur 数据中心地址</span>
        <input bind:value={baseUrl} placeholder="https://cn.api.concurcdc.cn" disabled={browserAuthorizing || connecting || testing} />
      </label>
      <label>
        <span>Client ID *</span>
        <input bind:value={clientId} autocomplete="off" placeholder="企业测试 OAuth 应用 ID" disabled={browserAuthorizing || connecting || testing} />
      </label>
      <label>
        <span>Client Secret *</span>
        <input type="password" bind:value={clientSecret} autocomplete="off" placeholder="只用于本次授权码交换" disabled={browserAuthorizing || connecting || testing} />
      </label>
    </div>
    <details class="technical-details">
      <summary>管理员接入信息</summary>
      <div class="callback-detail">
        <div><span>需登记的回调地址</span><code>{browserOauthConfig.redirect_uri}</code></div>
        <div><span>申请权限</span><code>{browserOauthConfig.scopes}</code></div>
      </div>
    </details>
    <label class="confirmation">
      <input type="checkbox" bind:checked={browserConfirmed} disabled={browserAuthorizing || connecting || testing} />
      <span>我确认使用企业批准的测试应用，并会在浏览器中选择用于报销的正确账号。</span>
    </label>
    <button class="primary" type="button" onclick={testBrowserOauth} disabled={!clientId.trim() || !clientSecret.trim() || !browserConfirmed || browserAuthorizing || connecting || testing}>
      {browserAuthorizing ? `等待浏览器授权…（最长 ${Math.round(browserOauthConfig.timeout_seconds / 60)} 分钟）` : '打开系统浏览器并测试授权'}
    </button>
  </section>

  <section class="test-section">
    <div class="section-heading">
      <span>02</span>
      <div><h3>连接与只读权限</h3><p>读取一条当前用户的未提交报销单摘要，不创建或修改任何 Concur 数据。</p></div>
    </div>
    <div class="form-grid">
      <label>
        <span>Concur 数据中心地址</span>
        <input bind:value={baseUrl} placeholder="https://cn.api.concurcdc.cn" disabled={browserAuthorizing || connecting || testing} />
        <small>中国区使用 https://cn.api.concurcdc.cn；只接受 SAP Concur 官方 HTTPS API 地址。</small>
      </label>
      <label>
        <span>OAuth 访问令牌</span>
        <input type="password" bind:value={accessToken} autocomplete="off" placeholder="只用于本次程序运行" disabled={browserAuthorizing || connecting || testing} />
        <small>令牌不写入数据库和备份，程序退出后失效。</small>
      </label>
    </div>
    <button class="primary" type="button" onclick={testReadAccess} disabled={!accessToken.trim() || browserAuthorizing || connecting || testing}>
      {connecting ? '正在测试连接…' : '执行只读连接测试'}
    </button>
  </section>

  <section class:disabled={!status.read_verified} class="test-section">
    <div class="section-heading">
      <span>03</span>
      <div><h3>草稿、费用与附件闭环</h3><p>创建一份明确标识的未提交测试草稿、0.01 元测试费用和一页测试 PDF，并逐步回读。</p></div>
    </div>
    <div class="form-grid three">
      <label>
        <span>费用类型代码 *</span>
        <input bind:value={expenseTypeCode} placeholder="例如租户中的餐费代码" disabled={!status.read_verified || testing} />
      </label>
      <label>
        <span>付款类型 ID *</span>
        <input bind:value={paymentTypeId} placeholder="租户付款类型稳定 ID" disabled={!status.read_verified || testing} />
      </label>
      <label>
        <span>地点 ID</span>
        <input bind:value={locationId} placeholder="可选；用于同时验证地点映射" disabled={!status.read_verified || testing} />
      </label>
    </div>
    <label class="confirmation">
      <input type="checkbox" bind:checked={confirmed} disabled={!status.read_verified || testing} />
      <span>我确认本次测试会在 Concur 创建一份未提交测试草稿；测试后由我在 Concur 中检查并删除。</span>
    </label>
    <button class="primary" type="button" onclick={testDraftWorkflow} disabled={!canRunWorkflow || testing}>
      {testing ? '正在逐步验证…' : '创建测试草稿并验证完整能力'}
    </button>
  </section>

  {#if result}
    <section class:success={result.success} class="result" aria-live="polite">
      <header>
        <div><span class="eyebrow">最近一次结果</span><h3>{result.success ? '本次测试通过' : '部分能力未通过'}</h3></div>
        <time>{new Date(result.checked_at).toLocaleString('zh-CN')}</time>
      </header>
      {#if result.draft_report_name}
        <div class="draft-reference">
          <span>测试草稿</span>
          <strong>{result.draft_report_name}</strong>
          <code>{result.draft_report_id ?? '外部 ID 待核对'}</code>
        </div>
      {/if}
      <ol>
        {#each result.steps as step}
          <li class:failed={step.status === 'failed'}>
            <span>{stepStatusLabel(step.status)}</span>
            <div><strong>{step.label}</strong><p>{step.message}</p></div>
          </li>
        {/each}
      </ol>
      <p class="next-action"><strong>下一步：</strong>{result.next_action}</p>
    </section>
  {/if}

  <aside class="boundary">
    <strong>功能边界</strong>
    <p>软件只创建未提交草稿，不点击最终提交；发票金额按票面实际金额写入，公司限额和审批规则仍由用户在 Concur 中处理。</p>
  </aside>
</section>

<style>
  .concur-settings{max-width:1100px;color:var(--text-primary)}
  .concur-settings>header,.result>header{display:flex;justify-content:space-between;gap:1rem;align-items:flex-start}
  h2{margin:0 0 .45rem;font-size:1.25rem}h3{margin:0;font-size:1rem}
  header p,.section-heading p,.account-card p,.capability-overview p{margin:.3rem 0 0;color:var(--text-secondary);font-size:.86rem;line-height:1.55}
  button{font:inherit;cursor:pointer}button:disabled{opacity:.45;cursor:not-allowed}
  .primary,.secondary{padding:.65rem .85rem;border:1px solid var(--accent-primary);font-weight:700}
  .primary{margin-top:1rem;background:var(--accent-primary);color:#fff}.secondary{background:var(--bg-primary);color:var(--accent-primary)}
  .account-card{display:flex;justify-content:space-between;gap:1rem;margin-top:1.4rem;padding:1rem;border:1px solid var(--border-color);border-left:4px solid #8a928c;background:var(--bg-secondary)}
  .account-card.ready{border-left-color:var(--accent-primary)}.account-identity strong{display:block;margin-top:.2rem;font-size:1.05rem}.account-card dl{display:grid;grid-template-columns:repeat(3,minmax(120px,1fr));gap:.5rem;margin:0}.account-card dl>div{display:grid;gap:.2rem;padding:.55rem .7rem;border:1px solid var(--border-color);background:var(--bg-primary)}.account-card dt{color:var(--text-secondary);font-size:.7rem}.account-card dd{margin:0;color:var(--text-primary);font-size:.8rem;font-weight:700;overflow-wrap:anywhere}
  .eyebrow{color:var(--text-secondary);font-family:var(--font-mono);font-size:.68rem;font-weight:700;letter-spacing:.06em}
  .capability-overview{margin-top:1rem;padding:1rem;border:1px solid var(--border-color);background:var(--bg-primary)}.capability-overview>header{display:flex;justify-content:space-between;gap:1rem;align-items:flex-end}.capability-overview>header>p{max-width:520px;text-align:right}.capability-grid{display:grid;grid-template-columns:repeat(4,minmax(0,1fr));gap:.65rem;margin-top:.85rem}.capability-grid article{min-height:105px;padding:.7rem;border:1px solid var(--border-color);border-top:4px solid #8a928c;background:var(--bg-secondary)}.capability-grid article.passed{border-top-color:var(--accent-primary)}.capability-grid article.failed{border-top-color:#b3453e;background:#fff8f7}.capability-grid article strong{display:block;margin-top:.45rem;font-size:.9rem}.capability-grid article p{font-size:.74rem;line-height:1.45}.capability-status{padding:.15rem .35rem;background:#e8ebea;color:#596870;font-size:.67rem;font-weight:700}.passed .capability-status{background:#e7f1eb;color:#24533f}.failed .capability-status{background:#f8e9e7;color:#862f2a}
  .message{padding:.75rem .9rem;border-left:4px solid;margin:1rem 0}.message.error{border-color:#b3453e;background:#f8e9e7;color:#862f2a}
  .test-section{margin-top:1rem;padding:1rem;border:1px solid var(--border-color);background:var(--bg-primary)}.test-section.disabled{background:var(--bg-secondary)}
  .section-heading{display:flex;gap:.75rem}.section-heading>span{display:grid;width:34px;height:34px;place-items:center;background:var(--accent-primary);color:#fff;font-family:var(--font-mono);font-size:.72rem;font-weight:700}
  .alpha-note{margin-top:.85rem;padding:.7rem .8rem;border-left:4px solid #315f8a;background:#edf3f8;color:#294d6c}.alpha-note p{margin:.2rem 0 0;font-size:.8rem;line-height:1.55}
  .technical-details{margin-top:.8rem;border:1px solid var(--border-color);background:var(--bg-secondary)}.technical-details summary{padding:.65rem .75rem;color:var(--accent-primary);font-size:.76rem;font-weight:700;cursor:pointer}.technical-details .callback-detail{margin:0;padding:0 .7rem .7rem}
  .callback-detail{display:grid;grid-template-columns:minmax(0,2fr) minmax(180px,1fr);gap:.65rem;margin-top:.8rem}.callback-detail>div{display:grid;gap:.3rem;padding:.65rem .75rem;border:1px solid var(--border-color);background:var(--bg-secondary)}.callback-detail span{color:var(--text-secondary);font-size:.72rem;font-weight:700}.callback-detail code{overflow-wrap:anywhere;color:var(--text-primary);font-size:.76rem}
  .form-grid{display:grid;grid-template-columns:repeat(2,minmax(0,1fr));gap:.75rem;margin-top:1rem}.form-grid.three{grid-template-columns:repeat(3,minmax(0,1fr))}
  label{display:grid;gap:.35rem;font-size:.8rem;font-weight:700}label small{color:var(--text-secondary);font-weight:400;line-height:1.4}
  input{box-sizing:border-box;width:100%;padding:.65rem .7rem;border:1px solid var(--border-color);background:var(--bg-primary);color:var(--text-primary);font:inherit}input:focus{outline:2px solid color-mix(in srgb,var(--accent-primary) 25%,transparent);border-color:var(--accent-primary)}
  .confirmation{display:flex;align-items:flex-start;gap:.55rem;margin-top:1rem;padding:.75rem;background:#fff4d9;color:#694d18;line-height:1.5}.confirmation input{width:auto;margin-top:.15rem}
  .result{margin-top:1rem;padding:1rem;border:1px solid #d7a09b;border-left:4px solid #b3453e;background:#fff}.result.success{border-color:#78a08d;border-left-color:var(--accent-primary)}.result time{color:var(--text-secondary);font-size:.72rem}
  .draft-reference{display:grid;grid-template-columns:auto 1fr auto;gap:.75rem;align-items:center;margin-top:.8rem;padding:.65rem;background:var(--bg-secondary);font-size:.76rem}.draft-reference span{color:var(--text-secondary)}.draft-reference code{font-size:.68rem;color:var(--text-secondary)}
  .result ol{display:grid;gap:.45rem;margin:.8rem 0 0;padding:0;list-style:none}.result li{display:grid;grid-template-columns:58px 1fr;gap:.65rem;padding:.6rem;border:1px solid var(--border-color)}.result li>span{align-self:start;padding:.2rem .3rem;background:#e7f1eb;color:#24533f;text-align:center;font-size:.67rem;font-weight:700}.result li.failed>span{background:#f8e9e7;color:#862f2a}.result li p{margin:.18rem 0 0;color:var(--text-secondary);font-size:.76rem}.next-action{margin:.8rem 0 0;padding:.7rem;background:var(--bg-secondary);font-size:.8rem}
  .boundary{margin-top:1rem;padding:.85rem;border-left:4px solid #8a928c;background:var(--bg-secondary)}.boundary p{margin:.25rem 0 0;color:var(--text-secondary);font-size:.8rem;line-height:1.55}
  @media(max-width:900px){.form-grid,.form-grid.three,.callback-detail,.capability-grid{grid-template-columns:1fr}.account-card,.concur-settings>header,.result>header,.capability-overview>header{display:grid}.account-card dl{grid-template-columns:1fr}.capability-overview>header>p{text-align:left}.draft-reference{grid-template-columns:1fr}.concur-settings>header .secondary{justify-self:start}}
</style>
