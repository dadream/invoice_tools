<script lang="ts">
import { invokeSafe } from '../../lib/ipc';
import { currentLocalMonth, linkOnlyEmailNotice, monthDateRange } from '../../lib/pipeline';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';

import { open } from '@tauri-apps/plugin-dialog';

type SourceKind = 'local' | 'email';
// 阶段进度状态
interface StageProgress {
  stage: string;
  progress: number;
  current?: number;
  total?: number;
  message: string;
}

interface PipelineError {
  stage: string;
  message: string;
}

interface PipelineCancelled {
  stage: string;
  message: string;
}

interface PipelineComplete {
  batch_id: number;
  invoice_count: number;
  total_amount: string;
  excel_path?: string;
  link_only_email_count: number;
  pending_document_count: number;
  source_file_count: number;
  parsed_document_count: number;
  canonical_invoice_count: number;
  duplicate_document_count: number;
}

interface RecoverablePipeline {
  pipeline_id: string;
  batch_name: string;
  month: string;
  source_kind: 'local' | 'email';
  stage: string;
  status: 'failed' | 'interrupted';
  last_error?: string;
  updated_at: string;
}

// 组件状态
let batchName = $state('');
const initialMonth = currentLocalMonth();
const initialDateRange = monthDateRange(initialMonth)!;
let month = $state(initialMonth);
let dateStart = $state(initialDateRange.start);
let dateEnd = $state(initialDateRange.end);

let sourceKind = $state<SourceKind>('local');
let localPaths = $state<string[]>([]);
let isRunning = $state(false);
let currentStage = $state<string>('');
let progress = $state<number>(0);
let progressMessage = $state<string>('');
let currentCount = $state<number | undefined>(undefined);
let totalCount = $state<number | undefined>(undefined);
let errorMessage = $state<string | null>(null);
let completedResult = $state<PipelineComplete | null>(null);
let isCancelling = $state(false);
let activePipelineId = $state<string | null>(null);
let noticeMessage = $state<string | null>(null);

let recoverablePipelines = $state<RecoverablePipeline[]>([]);
let recoverableError = $state<string | null>(null);
let unlistenFns: UnlistenFn[] = [];

// 阶段中文名映射
const stageNames: Record<string, string> = {
  collect: '收集来源',
  parse: '解析发票',
  dedupe: '去重检查',
  group: '归组行程',
  review: '生成待审核草稿'
};
const checkpointNames: Record<string, string> = { created: '尚未完成采集', collected: '已完成采集', parsed: '已完成解析', deduped: '已完成去重', grouped: '已完成归组' };

function pathName(path: string): string {
  return path.split(/[\\/]/).filter(Boolean).pop() || path;
}

async function chooseFiles() {
  const selected = await open({
    multiple: true,
    directory: false,
    filters: [{
      name: '发票或邮件文件',
      extensions: ['xml', 'ofd', 'pdf', 'png', 'jpg', 'jpeg', 'webp', 'bmp', 'eml']
    }]
  });
  const values = Array.isArray(selected) ? selected : selected ? [selected] : [];
  localPaths = Array.from(new Set([...localPaths, ...values]));
}

async function chooseFolder() {
  const selected = await open({ multiple: false, directory: true });
  if (typeof selected === 'string') {
    localPaths = Array.from(new Set([...localPaths, selected]));
  }
}

function removeLocalPath(path: string) {
  localPaths = localPaths.filter((value) => value !== path);
}

async function loadRecoverablePipelines() {
  const result = await invokeSafe<RecoverablePipeline[]>('list_recoverable_pipelines');
  if (result.ok) {
    recoverablePipelines = result.data;
    recoverableError = null;
  } else {
    recoverableError = result.error.message;
  }
}

async function attachPipelineListeners(pipelineId: string) {
  cleanup();
  const progressUnlisten = await listen<StageProgress>(
    `pipeline:progress:${pipelineId}`,
    (event) => {
      const data = event.payload;
      currentStage = data.stage;
      progress = data.progress;
      progressMessage = data.message;
      currentCount = data.current;
      totalCount = data.total;
    }
  );
  unlistenFns.push(progressUnlisten);

  const errorUnlisten = await listen<PipelineError>(
    `pipeline:error:${pipelineId}`,
    (event) => {
      const data = event.payload;
      errorMessage = `[${stageNames[data.stage] || data.stage}] ${data.message}`;
      isRunning = false;
      isCancelling = false;
      activePipelineId = null;
      cleanup();
      void loadRecoverablePipelines();
    }
  );
  unlistenFns.push(errorUnlisten);

  const cancelledUnlisten = await listen<PipelineCancelled>(
    `pipeline:cancelled:${pipelineId}`,
    (event) => {
      currentStage = event.payload.stage;
      noticeMessage = event.payload.message;
      errorMessage = null;
      isRunning = false;
      isCancelling = false;
      activePipelineId = null;
      cleanup();
      void loadRecoverablePipelines();
    }
  );
  unlistenFns.push(cancelledUnlisten);

  const completeUnlisten = await listen<PipelineComplete>(
    `pipeline:complete:${pipelineId}`,
    (event) => {
      completedResult = event.payload;
      isRunning = false;
      isCancelling = false;
      activePipelineId = null;
      noticeMessage = null;
      currentStage = 'complete';
      progress = 1.0;
      progressMessage = '流水线完成！';
      cleanup();
      void loadRecoverablePipelines();
    }
  );
  unlistenFns.push(completeUnlisten);
}

async function resumePipeline(item: RecoverablePipeline) {
  isRunning = true;
  isCancelling = false;
  activePipelineId = item.pipeline_id;
  noticeMessage = null;
  errorMessage = null;
  completedResult = null;
  currentStage = '';
  progress = 0;
  progressMessage = `正在从“${checkpointNames[item.stage] || item.stage}”恢复...`;
  await attachPipelineListeners(item.pipeline_id);
  const result = await invokeSafe<void>('resume_pipeline', { pipelineId: item.pipeline_id });
  if (!result.ok) {
    errorMessage = result.error.message;
    isRunning = false;
    isCancelling = false;
    activePipelineId = null;
    cleanup();
    await loadRecoverablePipelines();
  }
}

// 启动流水线
async function startPipeline(e: Event) {
  e.preventDefault();

  if (!batchName.trim()) {
    errorMessage = '请填写批次名称';
    return;
  }
  if (!monthDateRange(month)) {
    errorMessage = '月份格式必须为 YYYY-MM';
    return;
  }
  if (dateEnd < dateStart) {
    errorMessage = '结束日期不能早于开始日期';
    return;
  }
  if (sourceKind === 'local' && localPaths.length === 0) {
    errorMessage = '请选择至少一个发票文件、EML 或文件夹';
    return;
  }

  isRunning = true;
  isCancelling = false;
  noticeMessage = null;
  errorMessage = null;
  completedResult = null;
  currentStage = '';
  progress = 0;
  progressMessage = '正在启动流水线...';


  const pipelineId = crypto.randomUUID();
  activePipelineId = pipelineId;
  await attachPipelineListeners(pipelineId);

  const result = await invokeSafe<void>('start_pipeline', {
    pipelineId,
    config: {
      batch_name: batchName,
      month,
      source: sourceKind === 'email'
        ? { kind: 'email' }
        : { kind: 'local', paths: localPaths },
      date_range: {
        start: dateStart,
        end: dateEnd
      }
    }
  });

  if (!result.ok) {
    errorMessage = result.error.message;
    isRunning = false;
    isCancelling = false;
    activePipelineId = null;
    cleanup();
  }
}

function syncDateRangeToMonth() {
  const range = monthDateRange(month);
  if (!range) return;
  dateStart = range.start;
  dateEnd = range.end;
}

async function cancelPipeline() {
  if (!activePipelineId || isCancelling) return;

  isCancelling = true;
  const result = await invokeSafe<void>('cancel_pipeline', { pipelineId: activePipelineId });
  if (result.ok) {
    progressMessage = '正在安全停止；当前文件处理完成后会保留最近检查点...';
  } else {
    errorMessage = result.error.message;
    isCancelling = false;
  }
}

// 清理事件监听器
function cleanup() {
  unlistenFns.forEach(fn => fn());
  unlistenFns = [];
}

// 组件销毁时清理
$effect(() => {
  void loadRecoverablePipelines();
  return () => {
    cleanup();
  };
});

// 格式化进度文本
function formatProgress(): string {
  if (currentCount !== undefined && totalCount !== undefined) {
    return `${currentCount}/${totalCount}`;
  }
  return `${Math.round(progress * 100)}%`;
}

// 重置表单
function reset() {
  isRunning = false;
  currentStage = '';
  progress = 0;
  progressMessage = '';
  errorMessage = null;
  completedResult = null;
  isCancelling = false;
  activePipelineId = null;
  noticeMessage = null;
  currentCount = undefined;
  totalCount = undefined;
  cleanup();
}
</script>

<div class="pipeline-runner">
  <h1>新建发票批次</h1>
  <p class="subtitle">从本地文件、文件夹、EML 或只读邮箱收集发票，生成待人工审核草稿。</p>
  <p class="hint">不会自动批准或导出；邮箱授权码只保留在本次应用会话。</p>

  {#if !isRunning && !completedResult && recoverablePipelines.length > 0}
    <section class="recovery-panel" aria-labelledby="recovery-title">
      <h2 id="recovery-title">可恢复任务</h2>
      <p>以下任务已失败或在应用退出时中断。恢复时会先校验文件与 SHA-256，不会重复创建已完成批次。</p>
      <div class="recovery-list">
        {#each recoverablePipelines as item}
          <article class="recovery-item">
            <div>
              <strong>{item.batch_name}</strong>
              <span>{item.month} · {item.source_kind === 'email' ? '只读邮箱' : '本地来源'} · {checkpointNames[item.stage] || item.stage}</span>
              {#if item.last_error}<small>{item.last_error}</small>{/if}
            </div>
            <button type="button" class="btn-recover" onclick={() => resumePipeline(item)}>校验并恢复</button>
          </article>
        {/each}
      </div>
    </section>
  {/if}
  {#if recoverableError}
    <div class="error-box" role="alert">无法读取可恢复任务：{recoverableError}</div>
  {/if}
  {#if noticeMessage}
    <div class="notice-box" role="status">{noticeMessage}</div>
  {/if}

  {#if !isRunning && !completedResult}
    <!-- 配置表单 -->
    <form onsubmit={startPipeline}>
      <div class="form-group">
        <label for="batchName">批次名称 *</label>
        <input
          type="text"
          id="batchName"
          bind:value={batchName}
          placeholder="例如：2026年8月出差"
          required
        />
      </div>

      <div class="form-group">
        <label for="month">月份</label>
        <input
          type="month"
          id="month"
          bind:value={month}
          onchange={syncDateRangeToMonth}
          required
        />
      </div>

      <fieldset class="source-section">
        <legend>发票来源 *</legend>
        <div class="source-options">
          <label class="source-option" class:active={sourceKind === 'local'}>
            <input type="radio" name="source" value="local" bind:group={sourceKind} />
            <span><strong>本地文件</strong><small>文件、文件夹或 .eml，无需邮箱</small></span>
          </label>
          <label class="source-option" class:active={sourceKind === 'email'}>
            <input type="radio" name="source" value="email" bind:group={sourceKind} />
            <span><strong>只读邮箱</strong><small>使用本次会话授权码按日期获取</small></span>
          </label>
        </div>

        {#if sourceKind === 'local'}
          <div class="local-picker">
            <div class="picker-actions">
              <button type="button" class="btn-secondary" onclick={chooseFiles}>选择文件或 EML</button>
              <button type="button" class="btn-secondary" onclick={chooseFolder}>选择文件夹</button>
            </div>
            <p class="source-help">
              当前可解析 XML / OFD / PDF / PNG / JPG / JPEG / WebP / BMP；EML 会提取其中的这些附件。
            </p>
            {#if localPaths.length > 0}
              <ul class="selected-paths" aria-label="已选择的本地来源">
                {#each localPaths as path (path)}
                  <li>
                    <span title={path}>{pathName(path)}</span>
                    <button type="button" onclick={() => removeLocalPath(path)} aria-label="移除 {pathName(path)}">移除</button>
                  </li>
                {/each}
              </ul>
            {:else}
              <p class="empty-source">尚未选择本地来源</p>
            {/if}
          </div>
        {:else}
          <p class="source-help">
            如首次设置时尚未连接邮箱，请先在“设置 → 邮箱来源”输入本次会话授权码；邮箱读取采用只读方式。
          </p>
        {/if}
      </fieldset>

      {#if sourceKind === 'email'}
        <div class="form-row">
        <div class="form-group">
          <label for="dateStart">开始日期</label>
          <input
            type="date"
            id="dateStart"
            bind:value={dateStart}
          />
        </div>

        <div class="form-group">
          <label for="dateEnd">结束日期（包含当天）</label>
          <input
            type="date"
            id="dateEnd"
            bind:value={dateEnd}
          />
        </div>
      </div>

      {/if}
      {#if errorMessage}
        <div class="error-box" role="alert">
          {errorMessage}
        </div>
      {/if}
      <button type="submit" class="btn-primary">
        收集并生成审核草稿
      </button>
    </form>
  {:else if isRunning}
    <!-- 进度显示 -->
    <div class="progress-container" aria-live="polite">
      <div class="stage-indicator">
        <div class="stage-label">
          当前阶段: <strong>{stageNames[currentStage] || currentStage}</strong>
        </div>
        <div class="progress-percent">{formatProgress()}</div>
      </div>

      <div class="progress-bar-container">
        <div class="progress-bar" style="width: {progress * 100}%"></div>
      </div>

      <div class="progress-message">{progressMessage}</div>

      <div class="stop-actions">
        <button type="button" class="btn-stop" onclick={cancelPipeline} disabled={isCancelling}>
          {isCancelling ? '正在安全停止...' : '安全停止'}
        </button>
        <small>安全停止会等待当前文件结束，不会删除已完成检查点。</small>
      </div>

      {#if errorMessage}
        <div class="error-box">
          {errorMessage}
        </div>
      {/if}

      <!-- 阶段列表 -->
      <div class="stages">
        {#each Object.entries(stageNames) as [stage, name]}
          <div class="stage-item" class:active={currentStage === stage} class:done={progress === 1 && currentStage === 'complete'}>
            <div class="stage-icon">
              {#if currentStage === stage}
                <span class="spinner">⏳</span>
              {:else if progress === 1 && currentStage === 'complete'}
                <span class="check">✓</span>
              {:else}
                <span class="dot">•</span>
              {/if}
            </div>
            <div class="stage-name">{name}</div>
          </div>
        {/each}
      </div>
    </div>
  {:else if completedResult}
    <!-- 完成结果 -->
    <div class="result-container">
      <div class="success-icon">✓</div>
      <h2>待审核草稿已生成</h2>
      <p class="review-notice">请进入批次管理核对原件、字段和重复标记；审核完成前不能导出。</p>

      <div class="result-stats">
        <div class="stat">
          <div class="stat-label">批次 ID</div>
          <div class="stat-value">{completedResult.batch_id}</div>
        </div>
        <div class="stat">
          <div class="stat-label">发票数量</div>
          <div class="stat-value">{completedResult.invoice_count}</div>
        </div>
        <div class="stat">
          <div class="stat-label">总金额</div>
          <div class="stat-value">¥{completedResult.total_amount}</div>
        </div>
      </div>

      {#if completedResult.source_file_count > 0}
        <div class="reconciliation" role="status">
          <strong>导入对账：</strong>
          {completedResult.source_file_count} 个唯一文件 →
          {completedResult.parsed_document_count} 份发票文档 →
          {completedResult.canonical_invoice_count} 张唯一发票；
          {completedResult.duplicate_document_count} 份同票副本已合并，
          {completedResult.pending_document_count} 份材料待处理。
        </div>
      {/if}

      {#if completedResult.excel_path}
        <div class="excel-path">
          <strong>Excel 文件:</strong> {completedResult.excel_path}
        </div>
      {/if}

      {#if completedResult.link_only_email_count > 0}
        <div class="link-warning" role="status">
          {linkOnlyEmailNotice(completedResult.link_only_email_count)}
        </div>
      {/if}

      <div class="actions">
        <button type="button" class="btn-primary" onclick={() => window.location.reload()}>
          返回批次管理
        </button>
        <button onclick={reset} class="btn-secondary">
          运行新流水线
        </button>
      </div>
    </div>
  {/if}
</div>

<style>
.pipeline-runner {
  max-width: 800px;
  margin: 0 auto;
  padding: 2rem;
}

h1 {
  font-size: 2rem;
  margin-bottom: 0.5rem;
}

.subtitle {
  color: #666;
  margin-bottom: 0.5rem;
}

.hint {
  color: #999;
  font-size: 0.9rem;
  margin-bottom: 2rem;
}

.form-group {
  margin-bottom: 1.5rem;
}

.form-group label {
  display: block;
  margin-bottom: 0.5rem;
  font-weight: 500;
}

.form-group input {
  width: 100%;
  padding: 0.75rem;
  border: 1px solid #ddd;
  border-radius: 4px;
  font-size: 1rem;
}

.form-row {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 1rem;
}

.btn-primary {
  background: #007bff;
  color: white;
  padding: 0.75rem 1.5rem;
  border: none;
  border-radius: 4px;
  font-size: 1rem;
  cursor: pointer;
  text-decoration: none;
  display: inline-block;
}

.btn-primary:hover {
  background: #0056b3;
}

.btn-secondary {
  background: #6c757d;
  color: white;
  padding: 0.75rem 1.5rem;
  border: none;
  border-radius: 4px;
  font-size: 1rem;
  cursor: pointer;
  margin-left: 1rem;
}

.btn-secondary:hover {
  background: #545b62;
}

.error-box {
  background: #f8d7da;
  border: 1px solid #f5c2c7;
  color: #842029;
  padding: 1rem;
  border-radius: 4px;
  margin: 1rem 0;
}

.notice-box {
  background: #e7f3ff;
  border: 1px solid #9ec5fe;
  color: #084298;
  padding: 1rem;
  border-radius: 4px;
  margin: 1rem 0;
}

.progress-container {
  margin-top: 2rem;
}

.stage-indicator {
  display: flex;
  justify-content: space-between;
  align-items: center;
  margin-bottom: 1rem;
}

.stage-label {
  font-size: 1.1rem;
}

.progress-percent {
  font-size: 1.2rem;
  font-weight: bold;
  color: #007bff;
}

.progress-bar-container {
  width: 100%;
  height: 24px;
  background: #e9ecef;
  border-radius: 12px;
  overflow: hidden;
  margin-bottom: 1rem;
}

.progress-bar {
  height: 100%;
  background: linear-gradient(90deg, #007bff, #0056b3);
  transition: width 0.3s ease;
}

.progress-message {
  color: #666;
  margin-bottom: 1rem;
}

.stop-actions {
  display: flex;
  align-items: center;
  gap: 0.75rem;
  margin-bottom: 2rem;
}

.stop-actions small {
  color: #666;
}

.btn-stop {
  flex: 0 0 auto;
  border: 1px solid #b42318;
  border-radius: 4px;
  background: white;
  color: #b42318;
  padding: 0.6rem 0.9rem;
  cursor: pointer;
}

.btn-stop:hover:not(:disabled) {
  background: #fff1f0;
}

.btn-stop:disabled {
  opacity: 0.6;
  cursor: wait;
}

.stages {
  display: grid;
  grid-template-columns: repeat(3, 1fr);
  gap: 1rem;
  margin-top: 2rem;
}

.stage-item {
  display: flex;
  align-items: center;
  padding: 0.75rem;
  border: 1px solid #ddd;
  border-radius: 4px;
  background: #f8f9fa;
}

.stage-item.active {
  border-color: #007bff;
  background: #e7f3ff;
}

.stage-item.done {
  border-color: #28a745;
  background: #d4edda;
}

.stage-icon {
  margin-right: 0.5rem;
  font-size: 1.2rem;
}

.spinner {
  animation: spin 1s linear infinite;
}

@keyframes spin {
  from { transform: rotate(0deg); }
  to { transform: rotate(360deg); }
}

.check {
  color: #28a745;
}

.dot {
  color: #999;
}

.result-container {
  text-align: center;
  margin-top: 2rem;
}

.success-icon {
  font-size: 4rem;
  color: #28a745;
  margin-bottom: 1rem;
}

.result-stats {
  display: grid;
  grid-template-columns: repeat(3, 1fr);
  gap: 1.5rem;
  margin: 2rem 0;
}

.stat {
  padding: 1.5rem;
  border: 1px solid #ddd;
  border-radius: 8px;
  background: #f8f9fa;
}

.stat-label {
  color: #666;
  font-size: 0.9rem;
  margin-bottom: 0.5rem;
}

.stat-value {
  font-size: 1.5rem;
  font-weight: bold;
  color: #333;
}

.excel-path {
  background: #e7f3ff;
  border: 1px solid #b3d9ff;
  padding: 1rem;
  border-radius: 4px;
  margin: 1.5rem 0;
  text-align: left;
  word-break: break-all;
}

.reconciliation {
  background: #eef7f1;
  border: 1px solid #9bc7aa;
  color: #214f31;
  padding: 1rem;
  border-radius: 6px;
  margin: 1.5rem 0;
  text-align: left;
  line-height: 1.55;
}

.link-warning {
  background: #fff4df;
  border: 1px solid #e4b35c;
  color: #6b4a12;
  padding: 1rem;
  border-radius: 6px;
  margin: 1.5rem 0;
  text-align: left;
  line-height: 1.55;
}

.actions {
  margin-top: 2rem;
}

.source-section {
  border: 1px solid #d6d1c5;
  border-radius: 8px;
  padding: 1rem;
  margin: 0 0 1.5rem;
}

.source-section legend { font-weight: 600; padding: 0 0.35rem; }
.source-options { display: grid; grid-template-columns: 1fr 1fr; gap: 0.75rem; }
.source-option {
  display: flex;
  gap: 0.65rem;
  align-items: flex-start;
  border: 1px solid #d6d1c5;
  border-radius: 6px;
  padding: 0.8rem;
  cursor: pointer;
}
.source-option.active { border-color: #136b52; background: #edf7f3; }
.source-option span { display: grid; gap: 0.2rem; }
.source-option small { color: #666; }
.local-picker { margin-top: 1rem; }
.picker-actions { display: flex; gap: 0.5rem; }
.picker-actions .btn-secondary { margin-left: 0; }
.source-help, .empty-source { color: #666; font-size: 0.88rem; margin: 0.75rem 0 0; }
.selected-paths {
  list-style: none;
  padding: 0;
  margin: 0.75rem 0 0;
  display: grid;
  gap: 0.4rem;
}
.selected-paths li {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 0.75rem;
  background: #f3f0e8;
  border-radius: 4px;
  padding: 0.5rem 0.65rem;
}
.selected-paths span { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.selected-paths button {
  border: 0;
  background: transparent;
  color: #b33a32;
  cursor: pointer;
}
.review-notice {
  max-width: 620px;
  margin: 0.5rem auto 1.5rem;
  padding: 0.75rem;
  background: #fff4df;
  color: #6b4a12;
  border-radius: 6px;
}
.recovery-panel { margin: 1rem 0 1.5rem; padding: 1rem; border: 1px solid #e4b35c; border-radius: 8px; background: #fffaf0; }
.recovery-panel h2 { margin: 0 0 0.4rem; font-size: 1.1rem; }
.recovery-panel p { margin: 0 0 0.8rem; color: #6b4a12; font-size: 0.88rem; }
.recovery-list { display: grid; gap: 0.6rem; }
.recovery-item { display: flex; align-items: center; justify-content: space-between; gap: 1rem; padding: 0.7rem; background: white; border: 1px solid #ead9b8; border-radius: 6px; }
.recovery-item div { display: grid; gap: 0.2rem; min-width: 0; }
.recovery-item span, .recovery-item small { color: #6b7280; font-size: 0.8rem; overflow-wrap: anywhere; }
.btn-recover { flex: 0 0 auto; padding: 0.55rem 0.8rem; border: 0; border-radius: 4px; background: #9a5b00; color: white; cursor: pointer; }
.btn-recover:hover { background: #784600; }
</style>
