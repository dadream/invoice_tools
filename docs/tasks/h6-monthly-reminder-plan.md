# H6 月度提醒 - 实现方案

**任务编号**: H6  
**优先级**: ⭐  
**依赖**: S0.2（Tauri 应用）  
**预计工期**: 0.5 天

---

## 1. 目标

每月固定时间（如月底前 3 天）提醒用户整理本月发票并提交报销。

---

## 2. 功能需求

### 2.1 核心功能

- 每月 28 日上午 9:00 发送系统通知
- 通知内容：提醒用户本月还有未处理的发票
- 用户可在设置中启用/禁用提醒
- 用户可自定义提醒日期和时间

### 2.2 提醒条件

仅在以下情况发送提醒：
1. 用户已启用月度提醒功能
2. 到达设定的提醒时间
3. 存在未归入批次的发票，或存在草稿状态的批次

---

## 3. 技术实现

### 3.1 定时任务

#### Rust 后端（使用 tokio 定时器）

```rust
// src-tauri/src/scheduler.rs

use chrono::{Datelike, Local, Timelike};
use std::time::Duration;
use tokio::time;

pub struct ReminderScheduler {
    settings: Arc<Mutex<ReminderSettings>>,
    app_handle: tauri::AppHandle,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct ReminderSettings {
    pub enabled: bool,
    pub day_of_month: u8,  // 1-31
    pub hour: u8,          // 0-23
    pub minute: u8,        // 0-59
}

impl ReminderScheduler {
    pub fn new(app_handle: tauri::AppHandle) -> Self {
        let settings = Arc::new(Mutex::new(ReminderSettings {
            enabled: false,
            day_of_month: 28,
            hour: 9,
            minute: 0,
        }));
        
        Self { settings, app_handle }
    }
    
    pub async fn start(&self) {
        let mut interval = time::interval(Duration::from_secs(3600)); // 每小时检查一次
        
        loop {
            interval.tick().await;
            
            let settings = self.settings.lock().await.clone();
            if !settings.enabled {
                continue;
            }
            
            let now = Local::now();
            if self.should_send_reminder(&now, &settings) {
                self.send_reminder().await;
            }
        }
    }
    
    fn should_send_reminder(&self, now: &chrono::DateTime<Local>, settings: &ReminderSettings) -> bool {
        // 检查日期和时间是否匹配
        now.day() == settings.day_of_month as u32
            && now.hour() == settings.hour as u32
            && now.minute() >= settings.minute as u32
            && now.minute() < settings.minute as u32 + 1
    }
    
    async fn send_reminder(&self) {
        // 检查是否有未处理的发票
        let state = self.app_handle.state::<AppState>();
        
        let has_pending = match self.check_pending_invoices(&state.ledger_db).await {
            Ok(pending) => pending,
            Err(e) => {
                eprintln!("Failed to check pending invoices: {}", e);
                return;
            }
        };
        
        if !has_pending {
            return; // 没有待处理发票，不发送提醒
        }
        
        // 发送系统通知
        self.app_handle
            .emit_all("monthly-reminder", ())
            .ok();
        
        // 使用系统通知 API
        #[cfg(target_os = "macos")]
        self.send_macos_notification();
        
        #[cfg(target_os = "windows")]
        self.send_windows_notification();
        
        #[cfg(target_os = "linux")]
        self.send_linux_notification();
    }
    
    async fn check_pending_invoices(&self, ledger_db: &LedgerDb) -> StoreResult<bool> {
        // 检查是否有草稿批次或未归入批次的发票
        let draft_batches = ledger_db.count_batches_by_status(BatchStatus::Draft)?;
        Ok(draft_batches > 0)
    }
    
    #[cfg(target_os = "macos")]
    fn send_macos_notification(&self) {
        use notify_rust::Notification;
        
        Notification::new()
            .summary("发票助手 - 月度提醒")
            .body("本月还有未完成的报销批次，记得及时提交哦！")
            .icon("invoice-assistant")
            .show()
            .ok();
    }
    
    #[cfg(target_os = "windows")]
    fn send_windows_notification(&self) {
        use notify_rust::Notification;
        
        Notification::new()
            .summary("发票助手 - 月度提醒")
            .body("本月还有未完成的报销批次，记得及时提交哦！")
            .show()
            .ok();
    }
    
    #[cfg(target_os = "linux")]
    fn send_linux_notification(&self) {
        use notify_rust::Notification;
        
        Notification::new()
            .summary("发票助手 - 月度提醒")
            .body("本月还有未完成的报销批次，记得及时提交哦！")
            .timeout(5000)
            .show()
            .ok();
    }
}
```

### 3.2 启动时初始化

```rust
// src-tauri/src/main.rs

#[tokio::main]
async fn main() {
    let app = tauri::Builder::default()
        .setup(|app| {
            let app_handle = app.handle();
            
            // 启动提醒调度器
            tokio::spawn(async move {
                let scheduler = ReminderScheduler::new(app_handle.clone());
                scheduler.start().await;
            });
            
            Ok(())
        })
        // ... 其他配置
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
```

### 3.3 IPC 命令

```rust
// src-tauri/src/commands/settings.rs

#[tauri::command]
pub async fn get_reminder_settings(
    state: tauri::State<'_, AppState>,
) -> AppResult<ReminderSettings> {
    let settings = state.reminder_settings.lock().await.clone();
    Ok(settings)
}

#[tauri::command]
pub async fn update_reminder_settings(
    settings: ReminderSettings,
    state: tauri::State<'_, AppState>,
) -> AppResult<()> {
    *state.reminder_settings.lock().await = settings;
    
    // 持久化到数据库
    state.ledger_db.set_reminder_settings(&settings)?;
    
    Ok(())
}

#[tauri::command]
pub async fn test_reminder_notification(
    state: tauri::State<'_, AppState>,
) -> AppResult<()> {
    // 手动触发一次提醒（用于测试）
    let scheduler = ReminderScheduler::new(state.app_handle.clone());
    scheduler.send_reminder().await;
    Ok(())
}
```

---

## 4. 前端 UI

### 4.1 设置界面

```svelte
<!-- ui/src/routes/settings/ReminderSettings.svelte -->
<script lang="ts">
  import { onMount } from 'svelte'
  import { invokeSafe } from '../../lib/ipc'
  
  interface ReminderSettings {
    enabled: boolean
    day_of_month: number
    hour: number
    minute: number
  }
  
  let settings = $state<ReminderSettings>({
    enabled: false,
    day_of_month: 28,
    hour: 9,
    minute: 0,
  })
  
  let loading = $state(true)
  let saving = $state(false)
  let testResult = $state<string | null>(null)
  
  async function loadSettings() {
    const result = await invokeSafe<ReminderSettings>('get_reminder_settings', {})
    if (result.ok) {
      settings = result.data
    }
    loading = false
  }
  
  async function saveSettings() {
    saving = true
    const result = await invokeSafe('update_reminder_settings', { settings })
    if (result.ok) {
      alert('保存成功！')
    } else {
      alert(`保存失败：${result.error.message}`)
    }
    saving = false
  }
  
  async function testNotification() {
    testResult = null
    const result = await invokeSafe('test_reminder_notification', {})
    if (result.ok) {
      testResult = '✅ 测试通知已发送，请查看系统通知'
    } else {
      testResult = `❌ 发送失败：${result.error.message}`
    }
  }
  
  onMount(() => {
    loadSettings()
  })
</script>

<div class="reminder-settings">
  <h2>月度提醒</h2>
  <p class="description">
    定期提醒您整理本月发票并提交报销，避免错过报销时间。
  </p>
  
  {#if loading}
    <div class="loading">加载中...</div>
  {:else}
    <div class="form">
      <label class="switch-label">
        <input 
          type="checkbox" 
          bind:checked={settings.enabled}
          onchange={saveSettings}
        />
        <span>启用月度提醒</span>
      </label>
      
      {#if settings.enabled}
        <div class="time-picker">
          <label>
            提醒日期：每月
            <input 
              type="number" 
              min="1" 
              max="31"
              bind:value={settings.day_of_month}
            />
            日
          </label>
          
          <label>
            提醒时间：
            <input 
              type="number" 
              min="0" 
              max="23"
              bind:value={settings.hour}
            />
            :
            <input 
              type="number" 
              min="0" 
              max="59"
              bind:value={settings.minute}
            />
          </label>
          
          <button onclick={saveSettings} disabled={saving} class="btn-primary">
            {saving ? '保存中...' : '保存设置'}
          </button>
          
          <button onclick={testNotification} class="btn-secondary">
            测试通知
          </button>
          
          {#if testResult}
            <div class="test-result">{testResult}</div>
          {/if}
        </div>
        
        <div class="info-box">
          <h4>💡 提示</h4>
          <ul>
            <li>提醒仅在存在未完成批次时发送</li>
            <li>请确保允许应用发送系统通知</li>
            <li>如果当月没有该日期（如 30 日），将在最后一天提醒</li>
          </ul>
        </div>
      {/if}
    </div>
  {/if}
</div>

<style>
  .reminder-settings {
    max-width: 600px;
  }
  
  .description {
    color: #666;
    margin-bottom: 1.5rem;
  }
  
  .switch-label {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    font-size: 1.125rem;
    margin-bottom: 1.5rem;
  }
  
  .time-picker {
    background: #f5f5f5;
    padding: 1.5rem;
    border-radius: 8px;
    margin-top: 1rem;
  }
  
  .time-picker label {
    display: block;
    margin-bottom: 1rem;
  }
  
  .time-picker input[type="number"] {
    width: 60px;
    padding: 0.5rem;
    text-align: center;
  }
  
  .info-box {
    background: #e3f2fd;
    border-left: 4px solid #2196f3;
    padding: 1rem;
    margin-top: 1rem;
  }
  
  .info-box h4 {
    margin-top: 0;
  }
  
  .info-box ul {
    margin-bottom: 0;
  }
  
  .test-result {
    margin-top: 1rem;
    padding: 0.75rem;
    background: white;
    border-radius: 4px;
  }
</style>
```

### 4.2 通知监听

```typescript
// ui/src/lib/notifications.ts

import { listen } from '@tauri-apps/api/event'

export function setupReminderListener() {
  listen('monthly-reminder', () => {
    // 显示应用内通知
    showInAppNotification({
      title: '月度提醒',
      message: '本月还有未完成的报销批次，记得及时提交哦！',
      type: 'reminder',
      action: {
        label: '查看批次',
        onClick: () => {
          window.location.href = '/batches'
        },
      },
    })
  })
}
```

---

## 5. 数据持久化

### 5.1 数据库表

```sql
-- 添加到 ledger.db

CREATE TABLE IF NOT EXISTS app_settings (
    key TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL,  -- JSON serialized
    updated_at TEXT NOT NULL
);

-- 存储提醒设置
INSERT OR REPLACE INTO app_settings (key, value, updated_at)
VALUES (
    'reminder_settings',
    '{"enabled":false,"day_of_month":28,"hour":9,"minute":0}',
    datetime('now')
);
```

### 5.2 LedgerDb 扩展

```rust
// crates/invoice-store/src/ledger_db.rs

impl LedgerDb {
    pub fn get_reminder_settings(&self) -> StoreResult<ReminderSettings> {
        let json = self.conn
            .query_row(
                "SELECT value FROM app_settings WHERE key = 'reminder_settings'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .unwrap_or_else(|| {
                // 默认设置
                r#"{"enabled":false,"day_of_month":28,"hour":9,"minute":0}"#.to_string()
            });
        
        serde_json::from_str(&json)
            .map_err(|e| StoreError::Internal(format!("Failed to parse settings: {}", e)))
    }
    
    pub fn set_reminder_settings(&self, settings: &ReminderSettings) -> StoreResult<()> {
        let json = serde_json::to_string(settings)
            .map_err(|e| StoreError::Internal(format!("Failed to serialize settings: {}", e)))?;
        
        let now = chrono::Utc::now().to_rfc3339();
        
        self.conn.execute(
            "INSERT OR REPLACE INTO app_settings (key, value, updated_at) VALUES (?1, ?2, ?3)",
            params!["reminder_settings", json, now],
        )?;
        
        Ok(())
    }
    
    pub fn count_batches_by_status(&self, status: BatchStatus) -> StoreResult<usize> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM batches WHERE status = ?1",
            [status.to_str()],
            |row| row.get(0),
        )?;
        Ok(count as usize)
    }
}
```

---

## 6. 依赖项

### 6.1 Cargo.toml

```toml
[dependencies]
tokio = { version = "1", features = ["full"] }
chrono = "0.4"
notify-rust = "4.11"  # 跨平台系统通知
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
```

### 6.2 Tauri 权限

```json
// src-tauri/capabilities/default.json
{
  "permissions": [
    "notification:default",
    "notification:allow-is-permission-granted",
    "notification:allow-request-permission",
    "notification:allow-notify"
  ]
}
```

---

## 7. 测试场景

### 7.1 单元测试

```rust
#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_should_send_reminder() {
        let settings = ReminderSettings {
            enabled: true,
            day_of_month: 28,
            hour: 9,
            minute: 0,
        };
        
        // 匹配的时间
        let now = Local.ymd(2026, 8, 28).and_hms(9, 0, 30);
        assert!(should_send_reminder(&now, &settings));
        
        // 不匹配的日期
        let now = Local.ymd(2026, 8, 27).and_hms(9, 0, 30);
        assert!(!should_send_reminder(&now, &settings));
        
        // 不匹配的小时
        let now = Local.ymd(2026, 8, 28).and_hms(10, 0, 30);
        assert!(!should_send_reminder(&now, &settings));
    }
}
```

### 7.2 集成测试

1. 设置提醒时间为当前时间 + 2 分钟
2. 等待 2 分钟
3. 验证收到系统通知
4. 验证应用内事件触发

---

## 8. 可选增强

### 8.1 智能提醒

根据用户历史行为调整提醒时间：

```rust
// 如果用户通常在月初采集发票，则在月初提醒
// 如果用户通常在月末导出，则在月末前几天提醒
pub fn calculate_optimal_reminder_time(history: &[BatchCreatedAt]) -> u8 {
    let avg_day = history.iter()
        .map(|b| b.created_at.day())
        .sum::<u32>() / history.len() as u32;
    
    // 提前 3 天提醒
    avg_day.saturating_sub(3).max(1) as u8
}
```

### 8.2 多种提醒类型

- 每月固定日期提醒
- 每周五下午提醒
- 季度报销截止日提醒

### 8.3 提醒历史

记录所有发送的提醒及用户响应：

```sql
CREATE TABLE reminder_history (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    sent_at TEXT NOT NULL,
    type TEXT NOT NULL,
    opened BOOLEAN DEFAULT FALSE,
    acted BOOLEAN DEFAULT FALSE
);
```

---

## 9. 验收标准

- [ ] 用户可在设置中启用/禁用月度提醒
- [ ] 用户可自定义提醒日期和时间
- [ ] 到达提醒时间时发送系统通知
- [ ] 仅在有未完成批次时发送提醒
- [ ] 测试通知功能正常工作
- [ ] 设置持久化到数据库
- [ ] 跨平台通知正常（Windows/macOS/Linux）

---

**估计工作量**: 4 小时  
**依赖模块**: invoice-store（设置存储）, Tauri 通知 API, tokio 定时器
