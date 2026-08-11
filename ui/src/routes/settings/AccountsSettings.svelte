<script lang="ts">
  import { onMount } from 'svelte'
  import { invokeSafe } from '../../lib/ipc'

  interface AccountInfo {
    id: number
    email: string
  }

  let accounts = $state<AccountInfo[]>([])
  let loading = $state(true)
  let error = $state<string | null>(null)

  // 添加表单
  let showAddForm = $state(false)
  let newEmail = $state('')
  let newPassword = $state('')
  let addLoading = $state(false)
  let addError = $state<string | null>(null)
  let testResult = $state<string | null>(null)

  async function loadAccounts() {
    loading = true
    error = null
    const result = await invokeSafe<AccountInfo[]>('list_accounts', {})
    if (result.ok) {
      accounts = result.data
    } else {
      error = result.error.message
    }
    loading = false
  }

  async function testConnection() {
    if (!newEmail || !newPassword) return

    testResult = null
    addError = null
    const result = await invokeSafe<string>('test_account_connection', {
      email: newEmail,
      password: newPassword,
    })

    if (result.ok) {
      testResult = result.data
    } else {
      addError = result.error.message
    }
  }

  async function addAccount() {
    if (!newEmail || !newPassword) return

    addLoading = true
    addError = null
    const result = await invokeSafe<number>('add_account', {
      email: newEmail,
      password: newPassword,
    })

    if (result.ok) {
      newEmail = ''
      newPassword = ''
      testResult = null
      showAddForm = false
      await loadAccounts()
    } else {
      addError = result.error.message
    }
    addLoading = false
  }

  async function deleteAccount(id: number) {
    if (!confirm('确定删除此账号？')) return

    const result = await invokeSafe<void>('delete_account', { id })
    if (result.ok) {
      await loadAccounts()
    } else {
      error = result.error.message
    }
  }

  onMount(() => {
    loadAccounts()
  })
</script>

<div class="accounts-settings">
  <h2>邮箱账号</h2>
  <p class="description">
    管理用于采集发票的邮箱账号。密码将加密存储，主密钥保存在系统 Keychain。
  </p>

  {#if loading}
    <div class="loading">加载中...</div>
  {:else if error}
    <div class="error-message">{error}</div>
  {:else}
    <div class="accounts-list">
      {#if accounts.length === 0}
        <div class="empty-state">
          <p>暂无邮箱账号</p>
          <button class="btn-primary" onclick={() => showAddForm = true}>
            添加第一个账号
          </button>
        </div>
      {:else}
        {#each accounts as account (account.id)}
          <div class="account-card">
            <div class="account-info">
              <span class="account-icon">📧</span>
              <span class="account-email">{account.email}</span>
            </div>
            <button
              class="btn-delete"
              onclick={() => deleteAccount(account.id)}
              title="删除"
            >
              ✕
            </button>
          </div>
        {/each}

        {#if !showAddForm}
          <button class="btn-add" onclick={() => showAddForm = true}>
            + 添加账号
          </button>
        {/if}
      {/if}
    </div>
  {/if}

  {#if showAddForm}
    <div class="add-form">
      <h3>添加邮箱账号</h3>

      <div class="form-group">
        <label for="email">邮箱地址</label>
        <input
          id="email"
          type="email"
          bind:value={newEmail}
          placeholder="example@qq.com"
          class="input"
        />
      </div>

      <div class="form-group">
        <label for="password">
          授权码
          <span class="hint">（QQ邮箱：设置 → 账号 → POP3/IMAP → 生成授权码）</span>
        </label>
        <input
          id="password"
          type="password"
          bind:value={newPassword}
          placeholder="16位授权码"
          class="input"
        />
      </div>

      {#if testResult}
        <div class="success-message">{testResult}</div>
      {/if}

      {#if addError}
        <div class="error-message">{addError}</div>
      {/if}

      <div class="form-actions">
        <button
          class="btn-secondary"
          onclick={testConnection}
          disabled={!newEmail || !newPassword}
        >
          测试连接
        </button>
        <button
          class="btn-primary"
          onclick={addAccount}
          disabled={addLoading || !newEmail || !newPassword}
        >
          {addLoading ? '保存中...' : '保存'}
        </button>
        <button
          class="btn-secondary"
          onclick={() => {
            showAddForm = false
            newEmail = ''
            newPassword = ''
            testResult = null
            addError = null
          }}
        >
          取消
        </button>
      </div>
    </div>
  {/if}
</div>

<style>
  .accounts-settings h2 {
    margin: 0 0 0.5rem 0;
    font-size: 1.25rem;
    font-weight: 600;
    color: var(--text-primary);
  }

  .description {
    margin: 0 0 1.5rem 0;
    color: var(--text-secondary);
    font-size: 0.9rem;
  }

  .loading,
  .error-message {
    padding: 1rem;
    border-radius: 6px;
  }

  .loading {
    background: var(--bg-secondary);
    color: var(--text-secondary);
  }

  .error-message {
    background: #fee;
    color: #c33;
    border: 1px solid #fcc;
  }

  .success-message {
    padding: 0.75rem;
    background: #efe;
    color: #2a2;
    border: 1px solid #cfc;
    border-radius: 6px;
    margin-bottom: 1rem;
  }

  .accounts-list {
    display: flex;
    flex-direction: column;
    gap: 0.75rem;
  }

  .empty-state {
    text-align: center;
    padding: 3rem 1rem;
    color: var(--text-secondary);
  }

  .account-card {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 1rem;
    background: var(--bg-secondary);
    border: 1px solid var(--border-color);
    border-radius: 8px;
    transition: box-shadow 0.2s;
  }

  .account-card:hover {
    box-shadow: 0 2px 8px rgba(0, 0, 0, 0.1);
  }

  .account-info {
    display: flex;
    align-items: center;
    gap: 0.75rem;
  }

  .account-icon {
    font-size: 1.5rem;
  }

  .account-email {
    font-weight: 500;
    color: var(--text-primary);
  }

  .btn-delete {
    padding: 0.5rem 0.75rem;
    border: none;
    background: transparent;
    color: var(--text-secondary);
    cursor: pointer;
    border-radius: 4px;
    font-size: 1.25rem;
    transition: all 0.2s;
  }

  .btn-delete:hover {
    background: #fee;
    color: #c33;
  }

  .btn-add {
    padding: 1rem;
    border: 2px dashed var(--border-color);
    background: transparent;
    color: var(--text-secondary);
    cursor: pointer;
    border-radius: 8px;
    font-size: 0.95rem;
    transition: all 0.2s;
  }

  .btn-add:hover {
    border-color: var(--accent-primary);
    color: var(--accent-primary);
    background: var(--bg-hover);
  }

  .add-form {
    margin-top: 2rem;
    padding: 1.5rem;
    background: var(--bg-secondary);
    border: 1px solid var(--border-color);
    border-radius: 8px;
  }

  .add-form h3 {
    margin: 0 0 1.5rem 0;
    font-size: 1.1rem;
    font-weight: 600;
    color: var(--text-primary);
  }

  .form-group {
    margin-bottom: 1.25rem;
  }

  .form-group label {
    display: block;
    margin-bottom: 0.5rem;
    font-weight: 500;
    color: var(--text-primary);
    font-size: 0.9rem;
  }

  .hint {
    font-weight: 400;
    color: var(--text-secondary);
    font-size: 0.85rem;
  }

  .input {
    width: 100%;
    padding: 0.75rem;
    border: 1px solid var(--border-color);
    border-radius: 6px;
    font-size: 0.95rem;
    background: var(--bg-primary);
    color: var(--text-primary);
    box-sizing: border-box;
  }

  .input:focus {
    outline: none;
    border-color: var(--accent-primary);
    box-shadow: 0 0 0 3px rgba(59, 130, 246, 0.1);
  }

  .form-actions {
    display: flex;
    gap: 0.75rem;
    margin-top: 1.5rem;
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
