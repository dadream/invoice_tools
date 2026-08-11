<script lang="ts">
  import { onMount } from 'svelte'
  import { invokeSafe } from '../../lib/ipc'

  interface GroupingRules {
    homeCities: string[]
    weekendDays: number[]
    airportKeywords: string[]
    hotelKeywords: string[]
  }

  let rules = $state<GroupingRules>({
    homeCities: [],
    weekendDays: [0, 6],
    airportKeywords: [],
    hotelKeywords: [],
  })

  let loading = $state(true)
  let saving = $state(false)
  let error = $state<string | null>(null)
  let saveSuccess = $state(false)

  // 临时编辑状态
  let airportText = $state('')
  let hotelText = $state('')

  const weekDays = [
    { value: 1, label: '周一' },
    { value: 2, label: '周二' },
    { value: 3, label: '周三' },
    { value: 4, label: '周四' },
    { value: 5, label: '周五' },
    { value: 6, label: '周六' },
    { value: 0, label: '周日' },
  ]

  async function loadRules() {
    loading = true
    error = null
    const result = await invokeSafe<GroupingRules>('get_grouping_rules', {})

    if (result.ok) {
      rules = result.data
      airportText = rules.airportKeywords.join('\n')
      hotelText = rules.hotelKeywords.join('\n')
    } else {
      error = result.error.message
    }
    loading = false
  }

  async function saveRules() {
    saving = true
    error = null
    saveSuccess = false

    // 从文本框解析回数组
    const updatedRules: GroupingRules = {
      ...rules,
      airportKeywords: airportText.split('\n').map(s => s.trim()).filter(Boolean),
      hotelKeywords: hotelText.split('\n').map(s => s.trim()).filter(Boolean),
    }

    const result = await invokeSafe<void>('save_grouping_rules', { rules: updatedRules })

    if (result.ok) {
      rules = updatedRules
      saveSuccess = true
      setTimeout(() => saveSuccess = false, 3000)
    } else {
      error = result.error.message
    }
    saving = false
  }

  function toggleWeekday(day: number) {
    if (rules.weekendDays.includes(day)) {
      rules.weekendDays = rules.weekendDays.filter(d => d !== day)
    } else {
      rules.weekendDays = [...rules.weekendDays, day]
    }
  }

  onMount(() => {
    loadRules()
  })
</script>

<div class="grouping-settings">
  <h2>归组规则</h2>
  <p class="description">
    配置行程归组引擎的规则，用于自动识别出差、住宿和交通。
  </p>

  {#if loading}
    <div class="loading">加载中...</div>
  {:else}
    <div class="rules-form">
      <!-- 休息日定义 -->
      <div class="form-section">
        <label class="section-label">
          休息日定义
          <span class="hint">（勾选的日期视为休息日）</span>
        </label>
        <div class="weekday-buttons">
          {#each weekDays as day}
            <button
              class="weekday-btn"
              class:active={rules.weekendDays.includes(day.value)}
              onclick={() => toggleWeekday(day.value)}
            >
              {day.label}
            </button>
          {/each}
        </div>
      </div>

      <!-- 机场关键词 -->
      <div class="form-section">
        <label for="airport-keywords" class="section-label">
          机场关键词
          <span class="hint">（每行一个，用于识别机票和打车到机场）</span>
        </label>
        <textarea
          id="airport-keywords"
          bind:value={airportText}
          placeholder="机场&#10;航站楼&#10;Airport"
          rows="6"
          class="textarea"
        ></textarea>
      </div>

      <!-- 酒店关键词 -->
      <div class="form-section">
        <label for="hotel-keywords" class="section-label">
          酒店关键词
          <span class="hint">（每行一个，用于识别住宿发票）</span>
        </label>
        <textarea
          id="hotel-keywords"
          bind:value={hotelText}
          placeholder="酒店&#10;宾馆&#10;Hotel"
          rows="6"
          class="textarea"
        ></textarea>
      </div>

      {#if saveSuccess}
        <div class="success-message">保存成功</div>
      {/if}

      {#if error}
        <div class="error-message">{error}</div>
      {/if}

      <div class="form-actions">
        <button
          class="btn-primary"
          onclick={saveRules}
          disabled={saving}
        >
          {saving ? '保存中...' : '保存设置'}
        </button>
        <button
          class="btn-secondary"
          onclick={loadRules}
          disabled={saving}
        >
          重置
        </button>
      </div>
    </div>
  {/if}
</div>

<style>
  .grouping-settings h2 {
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

  .rules-form {
    max-width: 700px;
  }

  .form-section {
    margin-bottom: 2rem;
  }

  .section-label {
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

  .weekday-buttons {
    display: flex;
    gap: 0.5rem;
    flex-wrap: wrap;
  }

  .weekday-btn {
    padding: 0.6rem 1rem;
    border: 1px solid var(--border-color);
    background: var(--bg-primary);
    color: var(--text-primary);
    border-radius: 6px;
    font-size: 0.9rem;
    cursor: pointer;
    transition: all 0.2s;
  }

  .weekday-btn:hover {
    background: var(--bg-hover);
    border-color: var(--accent-primary);
  }

  .weekday-btn.active {
    background: var(--accent-primary);
    color: white;
    border-color: var(--accent-primary);
  }

  .textarea {
    width: 100%;
    padding: 0.75rem;
    border: 1px solid var(--border-color);
    border-radius: 6px;
    font-size: 0.95rem;
    font-family: 'Monaco', 'Menlo', monospace;
    background: var(--bg-primary);
    color: var(--text-primary);
    resize: vertical;
    box-sizing: border-box;
  }

  .textarea:focus {
    outline: none;
    border-color: var(--accent-primary);
    box-shadow: 0 0 0 3px rgba(59, 130, 246, 0.1);
  }

  .success-message {
    padding: 0.75rem;
    background: #efe;
    color: #2a2;
    border: 1px solid #cfc;
    border-radius: 6px;
    margin-bottom: 1rem;
  }

  .error-message {
    padding: 0.75rem;
    background: #fee;
    color: #c33;
    border: 1px solid #fcc;
    border-radius: 6px;
    margin-bottom: 1rem;
  }

  .form-actions {
    display: flex;
    gap: 0.75rem;
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

  .btn-primary {
    background: var(--accent-primary);
    color: white;
  }

  .btn-primary:hover:not(:disabled) {
    background: var(--accent-hover);
  }

  .btn-primary:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .btn-secondary {
    background: var(--bg-primary);
    color: var(--text-primary);
    border: 1px solid var(--border-color);
  }

  .btn-secondary:hover:not(:disabled) {
    background: var(--bg-hover);
  }

  .btn-secondary:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }
</style>
