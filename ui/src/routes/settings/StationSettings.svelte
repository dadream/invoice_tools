<script lang="ts">
  import { onMount } from 'svelte'
  import { invokeSafe } from '../../lib/ipc'

  interface HomeStationEntry {
    stationName: string
    aliases: string[]
  }

  interface HomeStationLibrary {
    city: string
    stations: HomeStationEntry[]
  }

  interface EditableStation {
    stationName: string
    aliasesText: string
  }

  let city = $state('')
  let stations = $state<EditableStation[]>([])
  let loading = $state(true)
  let saving = $state(false)
  let error = $state<string | null>(null)
  let success = $state(false)

  function toEditable(entry: HomeStationEntry): EditableStation {
    return { stationName: entry.stationName, aliasesText: entry.aliases.join('、') }
  }

  async function loadLibrary() {
    loading = true
    error = null
    success = false
    const result = await invokeSafe<HomeStationLibrary>('get_home_station_library', {})
    if (result.ok) {
      city = result.data.city
      stations = result.data.stations.map(toEditable)
    } else {
      error = result.error.message
    }
    loading = false
  }

  function parseAliases(value: string): string[] {
    return [...new Set(value.split(/[、,，;；\n]/).map(alias => alias.trim()).filter(Boolean))]
  }

  function addStation() {
    stations = [...stations, { stationName: '', aliasesText: '' }]
  }

  function removeStation(index: number) {
    stations = stations.filter((_, stationIndex) => stationIndex !== index)
  }

  async function saveLibrary() {
    const normalized = stations.map(station => ({
      stationName: station.stationName.trim(),
      aliases: parseAliases(station.aliasesText),
    }))
    if (!city || normalized.length === 0 || normalized.some(station => !station.stationName)) {
      error = '请至少保留一个填写完整的标准站名'
      return
    }
    saving = true
    error = null
    success = false
    const result = await invokeSafe<void>('save_home_station_library', {
      library: { city, stations: normalized },
    })
    if (result.ok) {
      stations = normalized.map(toEditable)
      success = true
      setTimeout(() => (success = false), 4000)
    } else {
      error = result.error.message
    }
    saving = false
  }

  onMount(loadLibrary)
</script>

<section class="station-settings">
  <header>
    <div>
      <h2>常驻城市车站库</h2>
      <p>用于判断铁路行程是否从常驻地出发或返回常驻地。这里只维护当前城市，不需要保存全国车站。</p>
    </div>
    {#if city}<span class="city-badge">当前城市：{city}</span>{/if}
  </header>

  {#if loading}
    <div class="notice">正在加载车站库…</div>
  {:else if error && !city}
    <div class="error" role="alert">{error}</div>
  {:else}
    <div class="guidance">
      <strong>填写方式</strong>
      <span>标准站名填写铁路票面名称；别名可用顿号或逗号分隔。例如“北京朝阳”，别名“北京朝阳站、朝阳站”。</span>
      <span>同名简称只会在当前常驻城市上下文使用。保存后需对已有批次执行“重新分析归组”。</span>
    </div>

    <div class="station-list" aria-label={`${city}常驻车站`}>
      <div class="list-header"><span>标准站名</span><span>可识别别名</span><span></span></div>
      {#each stations as station, index}
        <div class="station-row">
          <input bind:value={station.stationName} aria-label={`第 ${index + 1} 个标准站名`} placeholder="例如：清河" />
          <input bind:value={station.aliasesText} aria-label={`${station.stationName || `第 ${index + 1} 个车站`}的别名`} placeholder="清河站、清河火车站" />
          <button class="remove" type="button" onclick={() => removeStation(index)} disabled={stations.length <= 1}>移除</button>
        </div>
      {/each}
    </div>

    <button class="add" type="button" onclick={addStation}>＋ 添加车站</button>

    {#if success}<div class="success" role="status">车站库已保存。新导入与重新分析归组会使用这份配置。</div>{/if}
    {#if error}<div class="error" role="alert">{error}</div>{/if}

    <footer>
      <button class="secondary" type="button" onclick={loadLibrary} disabled={saving}>放弃修改</button>
      <button class="primary" type="button" onclick={saveLibrary} disabled={saving}>{saving ? '保存中…' : '保存车站库'}</button>
    </footer>
  {/if}
</section>

<style>
  .station-settings { max-width: 860px; }
  header { display: flex; justify-content: space-between; align-items: flex-start; gap: 1rem; margin-bottom: 1.5rem; }
  h2 { margin: 0 0 .45rem; font-size: 1.25rem; }
  header p { margin: 0; color: var(--text-secondary); line-height: 1.6; }
  .city-badge { flex: 0 0 auto; padding: .45rem .7rem; border-radius: 999px; background: var(--bg-selected); color: var(--accent-primary); font-weight: 600; }
  .guidance { display: grid; gap: .35rem; padding: 1rem; margin-bottom: 1.25rem; border: 1px solid var(--border-color); border-radius: 8px; background: var(--bg-secondary); }
  .guidance span { color: var(--text-secondary); font-size: .88rem; line-height: 1.5; }
  .station-list { border: 1px solid var(--border-color); border-radius: 8px; overflow: hidden; }
  .list-header, .station-row { display: grid; grid-template-columns: minmax(150px, .7fr) minmax(260px, 1.5fr) 68px; gap: .75rem; align-items: center; padding: .75rem; }
  .list-header { background: var(--bg-secondary); color: var(--text-secondary); font-size: .82rem; font-weight: 600; }
  .station-row + .station-row { border-top: 1px solid var(--border-color); }
  input { min-width: 0; width: 100%; box-sizing: border-box; padding: .65rem .7rem; border: 1px solid var(--border-color); border-radius: 6px; background: var(--bg-primary); color: var(--text-primary); }
  input:focus { outline: 2px solid color-mix(in srgb, var(--accent-primary) 25%, transparent); border-color: var(--accent-primary); }
  button { cursor: pointer; border-radius: 6px; font-weight: 600; }
  button:disabled { opacity: .45; cursor: not-allowed; }
  .remove { border: 0; background: transparent; color: #a33; padding: .55rem .25rem; }
  .add { margin-top: .8rem; padding: .6rem .85rem; border: 1px solid var(--border-color); background: var(--bg-primary); color: var(--accent-primary); }
  footer { display: flex; justify-content: flex-end; gap: .75rem; margin-top: 1.5rem; }
  .primary, .secondary { padding: .7rem 1.15rem; }
  .primary { border: 1px solid var(--accent-primary); background: var(--accent-primary); color: white; }
  .secondary { border: 1px solid var(--border-color); background: var(--bg-primary); color: var(--text-primary); }
  .notice, .success, .error { padding: .85rem 1rem; border-radius: 7px; margin: 1rem 0; }
  .notice { background: var(--bg-secondary); color: var(--text-secondary); }
  .success { background: #edf8f3; color: #136b52; border: 1px solid #b9ddcf; }
  .error { background: #fff1f1; color: #9d2c2c; border: 1px solid #efc4c4; }
  @media (max-width: 720px) {
    header { display: grid; }
    .city-badge { justify-self: start; }
    .list-header { display: none; }
    .station-row { grid-template-columns: 1fr auto; }
    .station-row input:nth-child(2) { grid-column: 1 / -1; grid-row: 2; }
    .remove { grid-column: 2; grid-row: 1; }
  }
</style>
