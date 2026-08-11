<script lang="ts">
  import { onMount } from 'svelte'
  import AccountsSettings from './AccountsSettings.svelte'
  import GeneralSettings from './GeneralSettings.svelte'
  import GroupingSettings from './GroupingSettings.svelte'

  let activeTab = $state<'accounts' | 'general' | 'grouping'>('accounts')

  const tabs = [
    { id: 'accounts' as const, label: '邮箱账号', icon: '📧' },
    { id: 'general' as const, label: '通用设置', icon: '⚙️' },
    { id: 'grouping' as const, label: '归组规则', icon: '📋' },
  ]
</script>

<div class="settings-page">
  <header class="page-header">
    <h1>设置</h1>
  </header>

  <div class="settings-layout">
    <!-- 左侧标签页导航 -->
    <nav class="tabs-nav">
      {#each tabs as tab}
        <button
          class="tab-button"
          class:active={activeTab === tab.id}
          onclick={() => activeTab = tab.id}
        >
          <span class="tab-icon">{tab.icon}</span>
          <span class="tab-label">{tab.label}</span>
        </button>
      {/each}
    </nav>

    <!-- 右侧内容区 -->
    <main class="content-area">
      {#if activeTab === 'accounts'}
        <AccountsSettings />
      {:else if activeTab === 'general'}
        <GeneralSettings />
      {:else if activeTab === 'grouping'}
        <GroupingSettings />
      {/if}
    </main>
  </div>
</div>

<style>
  .settings-page {
    height: 100vh;
    display: flex;
    flex-direction: column;
    background: var(--bg-primary);
  }

  .page-header {
    padding: 1.5rem 2rem;
    border-bottom: 1px solid var(--border-color);
  }

  .page-header h1 {
    margin: 0;
    font-size: 1.5rem;
    font-weight: 600;
    color: var(--text-primary);
  }

  .settings-layout {
    display: flex;
    flex: 1;
    overflow: hidden;
  }

  .tabs-nav {
    width: 200px;
    border-right: 1px solid var(--border-color);
    padding: 1rem 0;
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
  }

  .tab-button {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    padding: 0.75rem 1.5rem;
    border: none;
    background: transparent;
    color: var(--text-secondary);
    cursor: pointer;
    transition: all 0.2s;
    text-align: left;
    font-size: 0.95rem;
  }

  .tab-button:hover {
    background: var(--bg-hover);
    color: var(--text-primary);
  }

  .tab-button.active {
    background: var(--bg-selected);
    color: var(--accent-primary);
    font-weight: 500;
    border-left: 3px solid var(--accent-primary);
  }

  .tab-icon {
    font-size: 1.25rem;
  }

  .content-area {
    flex: 1;
    overflow-y: auto;
    padding: 2rem;
  }
</style>
