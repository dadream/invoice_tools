<script lang="ts">
  import { onMount } from 'svelte'
  import { invokeSafe } from '../../lib/ipc'

  let homeCity = $state('')
  let loading = $state(true)
  let saving = $state(false)
  let error = $state<string | null>(null)
  let saveSuccess = $state(false)

  const commonCities = [
    '北京', '上海', '广州', '深圳', '杭州', '成都', '重庆', '西安',
    '南京', '武汉', '天津', '苏州', '郑州', '长沙', '东莞', '青岛'
  ]

  async function loadSettings() {
    loading = true
    error = null
    const result = await invokeSafe<string | null>('get_setting', { key: 'home_city' })

    if (result.ok) {
      homeCity = result.data || ''
    } else {
      error = result.error.message
    }
    loading = false
  }

  async function saveSetting() {
    if (!homeCity.trim()) {
      error = '请输入常驻城市'
      return
    }

    saving = true
    error = null
    saveSuccess = false

    const result = await invokeSafe<void>('set_setting', {
      key: 'home_city',
      value: homeCity.trim(),
    })

    if (result.ok) {
      saveSuccess = true
      setTimeout(() => saveSuccess = false, 3000)
    } else {
      error = result.error.message
    }
    saving = false
  }

  function selectCity(city: string) {
    homeCity = city
  }

  onMount(() => {
    loadSettings()
  })
</script>

<div class="general-settings">
  <h2>通用设置</h2>
  <p class="description">
    配置常驻城市，用于归组引擎判断出差行程。
  </p>

  {#if loading}
    <div class="loading">加载中...</div>
  {:else}
    <div class="setting-section">
      <label for="home-city" class="setting-label">
        常驻城市
        <span class="hint">（用于区分出差与本地消费）</span>
      </label>

      <div class="city-input-group">
        <input
          id="home-city"
          type="text"
          bind:value={homeCity}
          placeholder="如：北京"
          class="input"
        />
        <button
          class="btn-save"
          onclick={saveSetting}
          disabled={saving || !homeCity.trim()}
        >
          {saving ? '保存中...' : '保存'}
        </button>
      </div>

      <div class="city-buttons">
        <p class="city-buttons-label">常用城市：</p>
        <div class="city-grid">
          {#each commonCities as city}
            <button
              class="city-btn"
              class:active={homeCity === city}
              onclick={() => selectCity(city)}
            >
              {city}
            </button>
          {/each}
        </div>
      </div>

      {#if saveSuccess}
        <div class="success-message">保存成功</div>
      {/if}

      {#if error}
        <div class="error-message">{error}</div>
      {/if}
    </div>
  {/if}
</div>

<style>
  .general-settings h2 {
    margin: 0 0 0.5rem 0;
    font-size: 1.25rem;
    font-weight: 600;
    color: var(--text-primary);
  }

  .description {
    margin: 0 0 2rem 0;
    color: var(--text-secondary);
    font-size: 0.9rem;
  }

  .loading {
    padding: 1rem;
    background: var(--bg-secondary);
    color: var(--text-secondary);
    border-radius: 6px;
  }

  .setting-section {
    max-width: 600px;
  }

  .setting-label {
    display: block;
    margin-bottom: 0.75rem;
    font-weight: 500;
    color: var(--text-primary);
    font-size: 0.95rem;
  }

  .hint {
    font-weight: 400;
    color: var(--text-secondary);
    font-size: 0.85rem;
  }

  .city-input-group {
    display: flex;
    gap: 0.75rem;
    margin-bottom: 1.5rem;
  }

  .input {
    flex: 1;
    padding: 0.75rem;
    border: 1px solid var(--border-color);
    border-radius: 6px;
    font-size: 0.95rem;
    background: var(--bg-primary);
    color: var(--text-primary);
  }

  .input:focus {
    outline: none;
    border-color: var(--accent-primary);
    box-shadow: 0 0 0 3px rgba(59, 130, 246, 0.1);
  }

  .btn-save {
    padding: 0.75rem 1.5rem;
    border: none;
    border-radius: 6px;
    background: var(--accent-primary);
    color: white;
    font-size: 0.95rem;
    font-weight: 500;
    cursor: pointer;
    transition: all 0.2s;
    white-space: nowrap;
  }

  .btn-save:hover:not(:disabled) {
    background: var(--accent-hover);
  }

  .btn-save:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .city-buttons {
    margin-top: 1.5rem;
  }

  .city-buttons-label {
    margin: 0 0 0.75rem 0;
    color: var(--text-secondary);
    font-size: 0.9rem;
  }

  .city-grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(80px, 1fr));
    gap: 0.5rem;
  }

  .city-btn {
    padding: 0.6rem 0.75rem;
    border: 1px solid var(--border-color);
    background: var(--bg-primary);
    color: var(--text-primary);
    border-radius: 6px;
    font-size: 0.9rem;
    cursor: pointer;
    transition: all 0.2s;
  }

  .city-btn:hover {
    background: var(--bg-hover);
    border-color: var(--accent-primary);
  }

  .city-btn.active {
    background: var(--accent-primary);
    color: white;
    border-color: var(--accent-primary);
  }

  .success-message {
    margin-top: 1rem;
    padding: 0.75rem;
    background: #efe;
    color: #2a2;
    border: 1px solid #cfc;
    border-radius: 6px;
  }

  .error-message {
    margin-top: 1rem;
    padding: 0.75rem;
    background: #fee;
    color: #c33;
    border: 1px solid #fcc;
    border-radius: 6px;
  }
</style>
