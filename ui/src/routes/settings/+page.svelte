<script lang="ts">
  import AboutSettings from './AboutSettings.svelte'
  import AccountsSettings from './AccountsSettings.svelte'
  import DataSettings from './DataSettings.svelte'
  import GeneralSettings from './GeneralSettings.svelte'
  import GroupingSettings from './GroupingSettings.svelte'
  import StationSettings from './StationSettings.svelte'

  let activeTab = $state<'accounts' | 'general' | 'stations' | 'grouping' | 'data' | 'about'>('accounts')

  const tabs = [
    { id: 'accounts' as const, label: '邮箱账号', icon: '01' },
    { id: 'general' as const, label: '通用设置', icon: '02' },
    { id: 'stations' as const, label: '常驻车站', icon: '03' },
    { id: 'grouping' as const, label: '归组规则', icon: '04' },
    { id: 'data' as const, label: '数据与备份', icon: '05' },
    { id: 'about' as const, label: '版本与支持', icon: '06' },
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
      {:else if activeTab === 'stations'}
        <StationSettings />
      {:else if activeTab === 'grouping'}
        <GroupingSettings />
      {:else if activeTab === 'data'}
        <DataSettings />
      {:else if activeTab === 'about'}
        <AboutSettings />
      {/if}
    </main>
  </div>
</div>

<style>
  .settings-page {
    min-height: 100vh;
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
    display: grid;
    grid-template-columns: 210px minmax(0, 1fr);
    flex: 1;
    align-items: start;
  }

  .tabs-nav {
    min-height: 100%;
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
    width: 1.5rem;
    font-family: var(--font-mono);
    font-size: 0.68rem;
  }

  .content-area {
    min-width: 0;
    padding: 2rem;
  }

  @media (max-width: 760px) {
    .settings-page {
      min-height: auto;
    }

    .page-header {
      padding: 1.1rem 1rem;
    }

    .settings-layout {
      grid-template-columns: minmax(0, 1fr);
    }

    .tabs-nav {
      min-height: auto;
      grid-auto-flow: column;
      grid-auto-columns: minmax(132px, 1fr);
      overflow-x: auto;
      border-right: 0;
      border-bottom: 1px solid var(--border-color);
      padding: 0;
    }

    .tab-button {
      justify-content: center;
      padding: 0.75rem 0.85rem;
      border-bottom: 3px solid transparent;
      white-space: nowrap;
    }

    .tab-button.active {
      border-left: 0;
      border-bottom-color: var(--accent-primary);
    }

    .tab-icon {
      width: auto;
    }

    .content-area {
      padding: 1.25rem 1rem 2rem;
    }
  }
</style>
