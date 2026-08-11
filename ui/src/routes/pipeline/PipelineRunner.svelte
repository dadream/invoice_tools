<script lang="ts">
import { invokeSafe } from '../../lib/ipc';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';

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

interface PipelineComplete {
  batch_id: number;
  invoice_count: number;
  total_amount: string;
  excel_path?: string;
}

// 组件状态
let email = $state('');
let password = $state('');
let batchName = $state('');
let month = $state('2026-08');
let dateStart = $state('2026-08-01');
let dateEnd = $state('2026-08-31');

let isRunning = $state(false);
let currentStage = $state<string>('');
let progress = $state<number>(0);
let progressMessage = $state<string>('');
let currentCount = $state<number | undefined>(undefined);
let totalCount = $state<number | undefined>(undefined);
let errorMessage = $state<string | null>(null);
let completedResult = $state<PipelineComplete | null>(null);

let unlistenFns: UnlistenFn[] = [];

// 阶段中文名映射
const stageNames: Record<string, string> = {
  collect: '采集邮件',
  parse: '解析发票',
  dedupe: '去重检查',
  group: '归组行程',
  review: '审核归组',
  export: '导出报表'
};

// 启动流水线
async function startPipeline(e: Event) {
  e.preventDefault();

  if (!email || !password || !batchName) {
    errorMessage = '请填写所有必填字段';
    return;
  }

  isRunning = true;
  errorMessage = null;
  completedResult = null;
  currentStage = '';
  progress = 0;
  progressMessage = '正在启动流水线...';

  const result = await invokeSafe<string>('start_pipeline', {
    config: {
      email,
      password,
      batch_name: batchName,
      month,
      date_range: {
        start: dateStart,
        end: dateEnd
      }
    }
  });

  if (!result.ok) {
    errorMessage = result.error.message;
    isRunning = false;
    return;
  }

  const pipelineId = result.data;

  // 监听进度事件
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

  // 监听错误事件
  const errorUnlisten = await listen<PipelineError>(
    `pipeline:error:${pipelineId}`,
    (event) => {
      const data = event.payload;
      errorMessage = `[${stageNames[data.stage] || data.stage}] ${data.message}`;
      isRunning = false;
      cleanup();
    }
  );
  unlistenFns.push(errorUnlisten);

  // 监听完成事件
  const completeUnlisten = await listen<PipelineComplete>(
    `pipeline:complete:${pipelineId}`,
    (event) => {
      completedResult = event.payload;
      isRunning = false;
      currentStage = 'complete';
      progress = 1.0;
      progressMessage = '流水线完成！';
      cleanup();
    }
  );
  unlistenFns.push(completeUnlisten);
}

// 清理事件监听器
function cleanup() {
  unlistenFns.forEach(fn => fn());
  unlistenFns = [];
}

// 组件销毁时清理
$effect(() => {
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
  currentCount = undefined;
  totalCount = undefined;
  cleanup();
}
</script>

<div class="pipeline-runner">
  <h1>发票流水线</h1>
  <p class="subtitle">端到端自动化：从邮箱采集到最终导出</p>

  {#if !isRunning && !completedResult}
    <!-- 配置表单 -->
    <form onsubmit={startPipeline}>
      <div class="form-group">
        <label for="email">邮箱地址 *</label>
        <input
          type="email"
          id="email"
          bind:value={email}
          placeholder="your-email@example.com"
          required
        />
      </div>

      <div class="form-group">
        <label for="password">邮箱密码/授权码 *</label>
        <input
          type="password"
          id="password"
          bind:value={password}
          placeholder="邮箱密码或 IMAP 授权码"
          required
        />
      </div>

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
          type="text"
          id="month"
          bind:value={month}
          placeholder="2026-08"
        />
      </div>

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
          <label for="dateEnd">结束日期</label>
          <input
            type="date"
            id="dateEnd"
            bind:value={dateEnd}
          />
        </div>
      </div>

      {#if errorMessage}
        <div class="error-box">
          {errorMessage}
        </div>
      {/if}

      <button type="submit" class="btn-primary">
        启动流水线
      </button>
    </form>
  {:else if isRunning}
    <!-- 进度显示 -->
    <div class="progress-container">
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
      <h2>流水线完成</h2>

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

      {#if completedResult.excel_path}
        <div class="excel-path">
          <strong>Excel 文件:</strong> {completedResult.excel_path}
        </div>
      {/if}

      <div class="actions">
        <a href="/batches/{completedResult.batch_id}" class="btn-primary">
          查看批次详情
        </a>
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
  margin-bottom: 2rem;
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

.actions {
  margin-top: 2rem;
}
</style>
