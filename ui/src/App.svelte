<script lang="ts">
  import BatchList from './routes/batches/BatchList.svelte'
  import PipelineRunner from './routes/pipeline/PipelineRunner.svelte'

  let currentRoute = $state<'batches' | 'pipeline'>('batches');

  function navigateTo(route: 'batches' | 'pipeline') {
    currentRoute = route;
  }
</script>

<nav class="navbar">
  <div class="nav-container">
    <h1 class="nav-title">发票助手</h1>
    <div class="nav-links">
      <button
        class="nav-link"
        class:active={currentRoute === 'batches'}
        onclick={() => navigateTo('batches')}
      >
        批次管理
      </button>
      <button
        class="nav-link"
        class:active={currentRoute === 'pipeline'}
        onclick={() => navigateTo('pipeline')}
      >
        流水线
      </button>
    </div>
  </div>
</nav>

<main class="container">
  {#if currentRoute === 'batches'}
    <BatchList />
  {:else if currentRoute === 'pipeline'}
    <PipelineRunner />
  {/if}
</main>

<style>
  .navbar {
    background: white;
    border-bottom: 1px solid #e0e0e0;
    padding: 1rem 0;
    margin-bottom: 2rem;
  }

  .nav-container {
    max-width: 1200px;
    margin: 0 auto;
    padding: 0 2rem;
    display: flex;
    justify-content: space-between;
    align-items: center;
  }

  .nav-title {
    font-size: 1.5rem;
    margin: 0;
  }

  .nav-links {
    display: flex;
    gap: 1rem;
  }

  .nav-link {
    background: none;
    border: none;
    padding: 0.5rem 1rem;
    font-size: 1rem;
    cursor: pointer;
    border-radius: 4px;
    transition: background 0.2s;
  }

  .nav-link:hover {
    background: #f0f0f0;
  }

  .nav-link.active {
    background: #007bff;
    color: white;
  }

  .container {
    min-height: 100vh;
    background: #f5f5f5;
  }
</style>
