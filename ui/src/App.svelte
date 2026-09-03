<script lang="ts">
  import { onMount } from 'svelte'
  import { invokeSafe } from './lib/ipc'
  import BatchList from './routes/batches/BatchList.svelte'
  import EmailCollectionWorkbench from './routes/collections/EmailCollectionWorkbench.svelte'
  import SettingsPage from './routes/settings/+page.svelte'
  import WelcomeWizard from './routes/welcome/+page.svelte'

  let currentRoute = $state<'collections' | 'batches' | 'settings' | 'welcome'>('collections')
  let checkingFirstRun = $state(true)
  let sidebarCollapsed = $state(false)

  function navigateTo(route: 'collections' | 'batches' | 'settings') {
    currentRoute = route;
  }

  async function checkFirstRun() {
    const result = await invokeSafe<boolean>('is_first_run', {})
    if (result.ok && result.data) {
      currentRoute = 'welcome'
    }
    checkingFirstRun = false
  }

  function toggleSidebar() {
    sidebarCollapsed = !sidebarCollapsed
    try {
      window.localStorage.setItem('invoice-assistant.sidebar-collapsed', String(sidebarCollapsed))
    } catch {
      // 导航状态持久化失败不影响本次使用。
    }
  }

  onMount(() => {
    try {
      sidebarCollapsed = window.localStorage.getItem('invoice-assistant.sidebar-collapsed') === 'true'
    } catch {
      sidebarCollapsed = false
    }
    checkFirstRun()
  })
</script>

{#if checkingFirstRun}
  <div class="loading-screen">
    <div class="loading-spinner">加载中...</div>
  </div>
{:else if currentRoute === 'welcome'}
  <WelcomeWizard />
{:else}
  <div class="app-shell" class:sidebar-collapsed={sidebarCollapsed}>
    <aside class="sidebar" aria-label="主导航">
      <div class="brand">
        <span class="brand-mark" aria-hidden="true">票</span>
        <div>
          <strong>发票报销助手</strong>
          <span>本地工作台</span>
        </div>
      </div>

      <button class="sidebar-toggle" type="button" aria-expanded={!sidebarCollapsed} aria-controls="primary-navigation" aria-label={sidebarCollapsed ? '展开左侧导航' : '折叠左侧导航'} title={sidebarCollapsed ? '展开导航' : '折叠导航'} onclick={toggleSidebar}>
        <span aria-hidden="true">{sidebarCollapsed ? '›' : '‹'}</span>
        <strong>{sidebarCollapsed ? '展开导航' : '折叠导航'}</strong>
      </button>

      <nav id="primary-navigation" class="nav-links">
        <button
          class="nav-link"
          class:active={currentRoute === 'collections'}
          aria-current={currentRoute === 'collections' ? 'page' : undefined}
          title="邮件收集"
          onclick={() => navigateTo('collections')}
        >
          <span>01</span>
          <strong>邮件收集</strong>
        </button>
        <button
          class="nav-link"
          class:active={currentRoute === 'batches'}
          aria-current={currentRoute === 'batches' ? 'page' : undefined}
          title="报销批次"
          onclick={() => navigateTo('batches')}
        >
          <span>02</span>
          <strong>报销批次</strong>
        </button>
        <button
          class="nav-link"
          class:active={currentRoute === 'settings'}
          aria-current={currentRoute === 'settings' ? 'page' : undefined}
          title="设置与数据"
          onclick={() => navigateTo('settings')}
        >
          <span>03</span>
          <strong>设置与数据</strong>
        </button>
      </nav>

      <footer>
        <strong>免安装 · 本地处理</strong>
        <span>授权码仅在当前会话使用</span>
      </footer>
    </aside>

    <main class="workspace-root">
      {#if currentRoute === 'collections'}
        <EmailCollectionWorkbench />
      {:else if currentRoute === 'batches'}
        <BatchList />
      {:else if currentRoute === 'settings'}
        <SettingsPage />
      {/if}
    </main>
  </div>
{/if}

<style>
  .app-shell {
    --app-sidebar-width: 224px;
    --app-sidebar-center-offset: 112px;
    display: grid;
    grid-template-columns: var(--app-sidebar-width) minmax(0, 1fr);
    min-height: 100vh;
    background: var(--paper, #f3f0e8);
    transition: grid-template-columns 160ms ease;
  }
  .app-shell.sidebar-collapsed {
    --app-sidebar-width: 72px;
    --app-sidebar-center-offset: 36px;
  }
  .sidebar {
    position: sticky;
    top: 0;
    z-index: 120;
    display: flex;
    height: 100vh;
    flex-direction: column;
    border-right: 1px solid #30473c;
    background: #17211c;
    color: #f3f0e8;
  }
  .brand {
    display: grid;
    grid-template-columns: 42px minmax(0, 1fr);
    gap: 0.65rem;
    align-items: center;
    min-height: 82px;
    padding: 0 1rem;
    border-bottom: 1px solid #35443c;
  }
  .brand-mark {
    display: grid;
    width: 38px;
    height: 38px;
    place-items: center;
    border: 1px solid #739687;
    color: #dfece5;
    font-family: var(--font-mono, 'IBM Plex Mono', Consolas, monospace);
  }
  .brand div { display: grid; gap: 0.12rem; }
  .brand strong { font-size: 0.9rem; }
  .brand div span { color: #9eada5; font-size: 0.67rem; letter-spacing: 0.08em; }
  .sidebar-toggle {
    display: grid;
    grid-template-columns: 2rem minmax(0, 1fr);
    gap: .4rem;
    align-items: center;
    min-height: 38px;
    margin: .55rem .7rem 0;
    padding: .35rem .6rem;
    border: 1px solid #40544a;
    background: #1d2a24;
    color: #bdc9c2;
    text-align: left;
    cursor: pointer;
  }
  .sidebar-toggle:hover { border-color: #739687; color: #fff; }
  .sidebar-toggle span { color: #9ad2bc; font-size: 1.2rem; line-height: 1; }
  .sidebar-toggle strong { font-size: .72rem; }
  .nav-links { display: grid; gap: 0.25rem; padding: 1rem 0.7rem; }
  .nav-link {
    display: grid;
    grid-template-columns: 2rem minmax(0, 1fr);
    gap: 0.4rem;
    align-items: center;
    width: 100%;
    padding: 0.7rem 0.6rem;
    border: 0;
    border-left: 3px solid transparent;
    background: transparent;
    color: #bdc9c2;
    text-align: left;
    cursor: pointer;
    transition: background 120ms ease, color 120ms ease;
  }
  .nav-link > span {
    color: #7f9188;
    font-family: var(--font-mono, 'IBM Plex Mono', Consolas, monospace);
    font-size: 0.68rem;
  }
  .nav-link strong { font-size: 0.82rem; font-weight: 600; }
  .nav-link:hover { background: #223129; color: #fff; }
  .nav-link.active {
    border-left-color: #70b89c;
    background: #253a31;
    color: #fff;
  }
  .nav-link.active > span { color: #9ad2bc; }
  .sidebar footer {
    display: grid;
    gap: 0.2rem;
    margin-top: auto;
    padding: 1rem;
    border-top: 1px solid #35443c;
  }
  .sidebar footer strong { color: #cbd8d1; font-size: 0.7rem; }
  .sidebar footer span { color: #87988f; font-size: 0.62rem; }
  .workspace-root { min-width: 0; min-height: 100vh; background: var(--paper, #f3f0e8); }
  .loading-screen {
    display: grid;
    min-height: 100vh;
    place-items: center;
    background: var(--paper, #f3f0e8);
  }
  .loading-spinner { color: #536159; font-size: 0.9rem; }
  @media (prefers-reduced-motion: reduce) {
    .app-shell, .nav-link { transition: none; }
  }
  @media (min-width: 821px) {
    .sidebar-collapsed .brand { grid-template-columns: 1fr; justify-items: center; padding: 0 .5rem; }
    .sidebar-collapsed .brand > div,
    .sidebar-collapsed .nav-link strong,
    .sidebar-collapsed .sidebar-toggle strong,
    .sidebar-collapsed .sidebar footer { display: none; }
    .sidebar-collapsed .sidebar-toggle { grid-template-columns: 1fr; justify-items: center; margin-inline: .55rem; padding-inline: .3rem; }
    .sidebar-collapsed .nav-links { padding-inline: .55rem; }
    .sidebar-collapsed .nav-link { grid-template-columns: 1fr; justify-items: center; padding-inline: .3rem; text-align: center; }
  }
  @media (max-width: 820px) {
    .app-shell { grid-template-columns: minmax(0, 1fr); }
    .sidebar { position: sticky; height: auto; }
    .brand { min-height: 58px; }
    .sidebar-toggle { display: none; }
    .nav-links { grid-template-columns: repeat(3, minmax(0, 1fr)); padding: .45rem .7rem; }
    .nav-link { border-left: 0; border-bottom: 3px solid transparent; }
    .nav-link.active { border-bottom-color: #70b89c; }
    .sidebar footer { display: none; }
  }
</style>
