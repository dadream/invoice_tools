<script lang="ts">
  import { invokeSafe } from '../../lib/ipc'

  interface WizardState {
    step: number
    email: string
    password: string
    homeCity: string
  }

  let wizardState = $state<WizardState>({
    step: 1,
    email: '',
    password: '',
    homeCity: '',
  })

  let loading = $state(false)
  let error = $state<string | null>(null)
  let testResult = $state<string | null>(null)

  const commonCities = [
    '北京', '上海', '广州', '深圳', '杭州', '成都', '重庆', '西安',
    '南京', '武汉', '天津', '苏州', '郑州', '长沙', '东莞', '青岛'
  ]

  function nextStep() {
    if (wizardState.step < 4) {
      wizardState.step++
      error = null
      testResult = null
    }
  }

  function prevStep() {
    if (wizardState.step > 1) {
      wizardState.step--
      error = null
    }
  }

  async function testConnection() {
    if (!wizardState.email || !wizardState.password) return

    loading = true
    error = null
    testResult = null

    const result = await invokeSafe<string>('test_account_connection', {
      email: wizardState.email,
      password: wizardState.password,
    })

    if (result.ok) {
      testResult = result.data
    } else {
      error = result.error.message
    }
    loading = false
  }

  async function completeWizard() {
    loading = true
    error = null

    // 1. 添加邮箱账号
    const accountResult = await invokeSafe<number>('add_account', {
      email: wizardState.email,
      password: wizardState.password,
    })

    if (!accountResult.ok) {
      error = accountResult.error.message
      loading = false
      return
    }

    // 2. 保存常驻城市
    if (wizardState.homeCity.trim()) {
      const cityResult = await invokeSafe<void>('set_setting', {
        key: 'home_city',
        value: wizardState.homeCity.trim(),
      })

      if (!cityResult.ok) {
        error = cityResult.error.message
        loading = false
        return
      }
    }

    // 3. 完成，跳转到主界面
    loading = false
    // 触发 App.svelte 重新检查 first-run 状态
    window.location.reload()
  }

  function selectCity(city: string) {
    wizardState.homeCity = city
  }
</script>

<div class="wizard-container">
  <div class="wizard-card">
    <!-- 进度指示器 -->
    <div class="progress-bar">
      {#each [1, 2, 3, 4] as stepNum}
        <div
          class="progress-dot"
          class:active={wizardState.step >= stepNum}
          class:current={wizardState.step === stepNum}
        >
          {stepNum}
        </div>
        {#if stepNum < 4}
          <div class="progress-line" class:active={wizardState.step > stepNum}></div>
        {/if}
      {/each}
    </div>

    <!-- 步骤 1: 欢迎 -->
    {#if wizardState.step === 1}
      <div class="step-content">
        <div class="welcome-icon">📬</div>
        <h1>欢迎使用发票助手</h1>
        <p class="subtitle">让报销变得更简单</p>

        <div class="feature-list">
          <div class="feature-item">
            <span class="feature-icon">📧</span>
            <div>
              <h3>自动采集</h3>
              <p>从邮箱中自动提取发票附件</p>
            </div>
          </div>
          <div class="feature-item">
            <span class="feature-icon">🔍</span>
            <div>
              <h3>智能解析</h3>
              <p>支持 XML、OFD、PDF 多种格式</p>
            </div>
          </div>
          <div class="feature-item">
            <span class="feature-icon">📊</span>
            <div>
              <h3>批量导出</h3>
              <p>生成报销台账和明细表</p>
            </div>
          </div>
        </div>

        <button class="btn-primary btn-large" onclick={nextStep}>
          开始设置
        </button>
      </div>
    {/if}

    <!-- 步骤 2: 邮箱配置 -->
    {#if wizardState.step === 2}
      <div class="step-content">
        <h2>配置邮箱账号</h2>
        <p class="step-description">
          发票助手将从邮箱中采集发票附件。密码加密存储，仅用于 IMAP 连接。
        </p>

        <div class="form-group">
          <label for="email">邮箱地址</label>
          <input
            id="email"
            type="email"
            bind:value={wizardState.email}
            placeholder="example@qq.com"
            class="input"
          />
          <span class="hint">目前支持 QQ 邮箱、网易邮箱等主流服务商</span>
        </div>

        <div class="form-group">
          <label for="password">
            授权码
            <span class="hint-inline">（IMAP 专用密码）</span>
          </label>
          <input
            id="password"
            type="password"
            bind:value={wizardState.password}
            placeholder="QQ邮箱：设置 → 账号 → POP3/IMAP → 生成授权码"
            class="input"
          />
        </div>

        {#if testResult}
          <div class="success-message">{testResult}</div>
        {/if}

        {#if error}
          <div class="error-message">{error}</div>
        {/if}

        <div class="button-group">
          <button class="btn-secondary" onclick={prevStep}>
            上一步
          </button>
          <button
            class="btn-secondary"
            onclick={testConnection}
            disabled={loading || !wizardState.email || !wizardState.password}
          >
            {loading ? '测试中...' : '测试连接'}
          </button>
          <button
            class="btn-primary"
            onclick={nextStep}
            disabled={!wizardState.email || !wizardState.password}
          >
            下一步
          </button>
        </div>
      </div>
    {/if}

    <!-- 步骤 3: 常驻城市 -->
    {#if wizardState.step === 3}
      <div class="step-content">
        <h2>设置常驻城市</h2>
        <p class="step-description">
          用于归组引擎判断出差行程。在常驻城市的消费将标记为"本地"，其他城市标记为"出差"。
        </p>

        <div class="form-group">
          <label for="home-city">常驻城市</label>
          <input
            id="home-city"
            type="text"
            bind:value={wizardState.homeCity}
            placeholder="如：北京"
            class="input"
          />
        </div>

        <div class="city-selection">
          <p class="city-label">常用城市：</p>
          <div class="city-grid">
            {#each commonCities as city}
              <button
                class="city-btn"
                class:active={wizardState.homeCity === city}
                onclick={() => selectCity(city)}
              >
                {city}
              </button>
            {/each}
          </div>
        </div>

        <div class="button-group">
          <button class="btn-secondary" onclick={prevStep}>
            上一步
          </button>
          <button class="btn-primary" onclick={nextStep}>
            下一步
          </button>
        </div>
      </div>
    {/if}

    <!-- 步骤 4: 完成 -->
    {#if wizardState.step === 4}
      <div class="step-content">
        <div class="complete-icon">✅</div>
        <h2>准备就绪</h2>
        <p class="step-description">
          所有配置已完成，点击下方按钮开始使用发票助手。
        </p>

        <div class="summary-card">
          <div class="summary-item">
            <span class="summary-label">邮箱账号：</span>
            <span class="summary-value">{wizardState.email}</span>
          </div>
          <div class="summary-item">
            <span class="summary-label">常驻城市：</span>
            <span class="summary-value">{wizardState.homeCity || '未设置'}</span>
          </div>
        </div>

        {#if error}
          <div class="error-message">{error}</div>
        {/if}

        <div class="button-group">
          <button class="btn-secondary" onclick={prevStep}>
            上一步
          </button>
          <button
            class="btn-primary btn-large"
            onclick={completeWizard}
            disabled={loading}
          >
            {loading ? '保存中...' : '开始使用'}
          </button>
        </div>
      </div>
    {/if}
  </div>
</div>

<style>
  .wizard-container {
    min-height: 100vh;
    background: linear-gradient(135deg, #667eea 0%, #764ba2 100%);
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 2rem;
  }

  .wizard-card {
    background: white;
    border-radius: 12px;
    box-shadow: 0 20px 60px rgba(0, 0, 0, 0.3);
    max-width: 600px;
    width: 100%;
    padding: 2.5rem;
  }

  .progress-bar {
    display: flex;
    align-items: center;
    justify-content: center;
    margin-bottom: 3rem;
  }

  .progress-dot {
    width: 36px;
    height: 36px;
    border-radius: 50%;
    background: #e0e0e0;
    color: #666;
    display: flex;
    align-items: center;
    justify-content: center;
    font-weight: 600;
    transition: all 0.3s;
  }

  .progress-dot.active {
    background: #667eea;
    color: white;
  }

  .progress-dot.current {
    transform: scale(1.2);
    box-shadow: 0 0 0 4px rgba(102, 126, 234, 0.2);
  }

  .progress-line {
    width: 60px;
    height: 3px;
    background: #e0e0e0;
    transition: all 0.3s;
  }

  .progress-line.active {
    background: #667eea;
  }

  .step-content {
    text-align: center;
  }

  .welcome-icon {
    font-size: 5rem;
    margin-bottom: 1rem;
  }

  .complete-icon {
    font-size: 5rem;
    margin-bottom: 1rem;
  }

  h1 {
    margin: 0 0 0.5rem 0;
    font-size: 2rem;
    color: #213547;
  }

  h2 {
    margin: 0 0 1rem 0;
    font-size: 1.5rem;
    color: #213547;
  }

  .subtitle {
    margin: 0 0 2rem 0;
    color: #666;
    font-size: 1.1rem;
  }

  .step-description {
    margin: 0 0 2rem 0;
    color: #666;
    font-size: 0.95rem;
    line-height: 1.6;
  }

  .feature-list {
    display: flex;
    flex-direction: column;
    gap: 1.5rem;
    margin: 2rem 0 3rem 0;
    text-align: left;
  }

  .feature-item {
    display: flex;
    align-items: flex-start;
    gap: 1rem;
  }

  .feature-icon {
    font-size: 2rem;
    flex-shrink: 0;
  }

  .feature-item h3 {
    margin: 0 0 0.25rem 0;
    font-size: 1.1rem;
    color: #213547;
  }

  .feature-item p {
    margin: 0;
    color: #666;
    font-size: 0.9rem;
  }

  .form-group {
    margin-bottom: 1.5rem;
    text-align: left;
  }

  .form-group label {
    display: block;
    margin-bottom: 0.5rem;
    font-weight: 500;
    color: #213547;
    font-size: 0.95rem;
  }

  .hint {
    display: block;
    margin-top: 0.25rem;
    color: #666;
    font-size: 0.85rem;
    font-weight: 400;
  }

  .hint-inline {
    font-weight: 400;
    color: #666;
    font-size: 0.85rem;
  }

  .input {
    width: 100%;
    padding: 0.75rem;
    border: 1px solid #e0e0e0;
    border-radius: 6px;
    font-size: 0.95rem;
    box-sizing: border-box;
    transition: all 0.2s;
  }

  .input:focus {
    outline: none;
    border-color: #667eea;
    box-shadow: 0 0 0 3px rgba(102, 126, 234, 0.1);
  }

  .city-selection {
    margin-top: 2rem;
    text-align: left;
  }

  .city-label {
    margin: 0 0 0.75rem 0;
    color: #666;
    font-size: 0.9rem;
  }

  .city-grid {
    display: grid;
    grid-template-columns: repeat(4, 1fr);
    gap: 0.5rem;
  }

  .city-btn {
    padding: 0.6rem 0.75rem;
    border: 1px solid #e0e0e0;
    background: white;
    color: #213547;
    border-radius: 6px;
    font-size: 0.9rem;
    cursor: pointer;
    transition: all 0.2s;
  }

  .city-btn:hover {
    background: #f5f5f5;
    border-color: #667eea;
  }

  .city-btn.active {
    background: #667eea;
    color: white;
    border-color: #667eea;
  }

  .summary-card {
    background: #f5f5f5;
    border-radius: 8px;
    padding: 1.5rem;
    margin: 2rem 0;
    text-align: left;
  }

  .summary-item {
    display: flex;
    margin-bottom: 0.75rem;
  }

  .summary-item:last-child {
    margin-bottom: 0;
  }

  .summary-label {
    font-weight: 500;
    color: #666;
    min-width: 100px;
  }

  .summary-value {
    color: #213547;
  }

  .button-group {
    display: flex;
    gap: 0.75rem;
    justify-content: center;
    margin-top: 2rem;
  }

  .btn-primary,
  .btn-secondary {
    padding: 0.75rem 1.5rem;
    border: none;
    border-radius: 6px;
    font-size: 0.95rem;
    font-weight: 500;
    cursor: pointer;
    transition: all 0.2s;
  }

  .btn-large {
    padding: 1rem 2.5rem;
    font-size: 1.05rem;
  }

  .btn-primary {
    background: #667eea;
    color: white;
  }

  .btn-primary:hover:not(:disabled) {
    background: #5568d3;
    transform: translateY(-1px);
    box-shadow: 0 4px 12px rgba(102, 126, 234, 0.4);
  }

  .btn-primary:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .btn-secondary {
    background: white;
    color: #213547;
    border: 1px solid #e0e0e0;
  }

  .btn-secondary:hover:not(:disabled) {
    background: #f5f5f5;
  }

  .btn-secondary:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .success-message {
    padding: 0.75rem;
    background: #efe;
    color: #2a2;
    border: 1px solid #cfc;
    border-radius: 6px;
    margin-bottom: 1rem;
    text-align: left;
  }

  .error-message {
    padding: 0.75rem;
    background: #fee;
    color: #c33;
    border: 1px solid #fcc;
    border-radius: 6px;
    margin-bottom: 1rem;
    text-align: left;
  }
</style>
