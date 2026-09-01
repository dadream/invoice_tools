<script lang="ts">
  import { onMount } from 'svelte'
  import { invokeSafe } from '../../lib/ipc'
  import ConfirmDialog from '../../lib/ConfirmDialog.svelte'

  interface AccountInfo {
    id: number
    email: string
  }

  interface SessionCredentialStatus {
    configured: boolean
    email?: string
  }

  let accounts = $state<AccountInfo[]>([])
  let sessionStatus = $state<SessionCredentialStatus>({ configured: false })
  let loading = $state(true)
  let error = $state<string | null>(null)

  // 添加表单
  let showAddForm = $state(false)
  let newEmail = $state('')
  let newPassword = $state('')
  let addLoading = $state(false)
  let addError = $state<string | null>(null)
  let testResult = $state<string | null>(null)
  let deleteCandidate = $state<AccountInfo | null>(null)

  async function loadAccounts() {
    loading = true
    error = null
    const accountsResult = await invokeSafe<AccountInfo[]>('list_accounts', {})
    const statusResult = await invokeSafe<SessionCredentialStatus>('get_session_credential_status', {})
    if (!accountsResult.ok) {
      error = accountsResult.error.message
    } else if (!statusResult.ok) {
      error = statusResult.error.message
    } else {
      accounts = accountsResult.data
      sessionStatus = statusResult.data
    }
    loading = false
  }

  function useSavedAddress(email: string) {
    newEmail = email
    newPassword = ''
    showAddForm = true
    testResult = null
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
      newPassword = ''
      testResult = '本次会话已配置；应用退出后授权码自动失效。'
      showAddForm = false
      await loadAccounts()
    } else {
      addError = result.error.message
    }
    addLoading = false
  }

  async function deleteAccount(id: number) {
    deleteCandidate = null
    const result = await invokeSafe<void>('delete_account', { id })
    if (result.ok) {
      await loadAccounts()
    } else {
      error = result.error.message
    }
  }

  async function clearSession() {
    const result = await invokeSafe<void>('clear_session_credential', {})
    if (result.ok) {
      newPassword = ''
      testResult = null
      await loadAccounts()
    } else {
      error = result.error.message
    }
  }

  function executeDeleteAccount() {
    if (deleteCandidate) void deleteAccount(deleteCandidate.id)
  }

  onMount(() => {
    loadAccounts()
  })
</script>

<div class="accounts-settings">
  <h2>邮箱账号</h2>
  <p class="description">
    邮箱地址保存在本机；授权码只存在于当前应用会话内存，不写入数据库，退出后需重新输入。
  </p>

  <div class:session-active={sessionStatus.configured} class="session-status">
    <strong>{sessionStatus.configured ? '本次邮箱会话可用' : '本次邮箱会话未配置'}</strong>
    {#if sessionStatus.configured}
      <span>{sessionStatus.email}</span>
      <button class="btn-secondary" onclick={clearSession}>清除本次授权码</button>
    {:else}
      <span>本地文件功能不受影响；使用邮箱前请输入授权码。</span>
    {/if}
  </div>

  {#if loading}
    <div class="loading">加载中...</div>
  {:else if error}
    <div class="error-message">{error}</div>
  {:else}
    <div class="accounts-list">
      {#if accounts.length === 0}
        <div class="empty-state">
          <p>暂无保存的邮箱地址</p>
          <button class="btn-primary" onclick={() => showAddForm = true}>
            输入邮箱和本次授权码
          </button>
        </div>
      {:else}
        {#each accounts as account (account.id)}
          <div class="account-card">
            <div class="account-info">
              <span class="account-icon" aria-hidden="true"><svg viewBox="0 0 24 24"><path d="M3 6h18v12H3zM3 7l9 7 9-7" /></svg></span>
              <span class="account-email">{account.email}</span>
            </div>
            <button class="btn-secondary" onclick={() => useSavedAddress(account.email)}>
              本次使用
            </button>
            <button
              class="btn-delete"
              onclick={() => (deleteCandidate = account)}
              title="删除"
            >
              ✕
            </button>
          </div>
        {/each}

        {#if !showAddForm}
          <button class="btn-add" onclick={() => showAddForm = true}>
            + 输入新的邮箱会话
          </button>
        {/if}
      {/if}
    </div>
  {/if}

  {#if showAddForm}
    <div class="add-form">
      <h3>配置本次邮箱会话</h3>

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
          {addLoading ? '配置中...' : '用于本次会话'}
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

{#if deleteCandidate}
  <ConfirmDialog title="删除邮箱地址配置" message={`将删除 ${deleteCandidate.email} 的本机地址记录，并清除当前会话中的邮箱授权码。历史发票和批次数据不会删除。`} confirmLabel="确认删除" tone="danger" onConfirm={executeDeleteAccount} onCancel={() => (deleteCandidate = null)} />
{/if}

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

  .session-status {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    margin-bottom: 1.25rem;
    padding: 0.9rem 1rem;
    border: 1px solid var(--border-color);
    border-radius: 6px;
    background: var(--bg-secondary);
    color: var(--text-secondary);
  }

  .session-status.session-active {
    border-color: var(--accent-primary);
  }

  .session-status .btn-secondary {
    margin-left: auto;
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
    display: grid;
    width: 30px;
    height: 30px;
    place-items: center;
    border: 1px solid var(--border-color);
    color: var(--accent-primary);
  }
  .account-icon svg { width: 18px; height: 18px; fill: none; stroke: currentColor; stroke-width: 1.7; stroke-linecap: round; stroke-linejoin: round; }

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
