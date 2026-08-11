//! H2 设置界面后端命令
//!
//! 提供三类配置接口：
//! 1. 邮箱账号管理（复用 invoice-store::accounts_db）
//! 2. 应用设置（常驻城市等，存储在 ledger_db settings 表）
//! 3. 归组规则配置（JSON 序列化存储）

use crate::error::{AppError, AppResult};
use crate::AppState;
use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use tauri::State;

// ========== 邮箱账号管理 ==========

/// 列出所有邮箱账号（不返回密码）
#[tauri::command]
pub fn list_accounts(state: State<Mutex<AppState>>) -> AppResult<Vec<AccountInfo>> {
    let app_state = state.lock().unwrap();
    let db = app_state.accounts_db()?;

    let accounts = db
        .list_accounts()
        .map_err(|e| AppError::database(format!("列出账号失败: {}", e)))?;

    Ok(accounts
        .into_iter()
        .map(|acc| AccountInfo {
            id: acc.id,
            email: acc.email,
        })
        .collect())
}

/// 邮箱账号信息（不含密码）
#[derive(Debug, Serialize, Deserialize)]
pub struct AccountInfo {
    pub id: i64,
    pub email: String,
}

/// 添加邮箱账号
#[tauri::command]
pub fn add_account(
    email: String,
    password: String,
    state: State<Mutex<AppState>>,
) -> AppResult<i64> {
    use invoice_collect::config::ImapConfig;

    let app_state = state.lock().unwrap();
    let db = app_state.accounts_db()?;

    // 先从 email 推断 IMAP 服务器配置
    std::env::set_var("INVOICE_IMAP_PASSWORD", &password);
    let config = ImapConfig::from_env(&email)
        .map_err(|e| AppError::validation(format!("不支持的邮箱域名: {}", e)))?;

    // 创建账号记录
    let account_id = db
        .create_account(&email, &config.host, config.port)
        .map_err(|e| AppError::database(format!("添加账号失败: {}", e)))?;

    // 设置加密凭证
    db.set_credential(account_id, &password)
        .map_err(|e| AppError::database(format!("保存凭证失败: {}", e)))?;

    tracing::info!(
        account_id,
        email = %email,
        imap_server = %config.host,
        "添加邮箱账号成功"
    );

    Ok(account_id)
}

/// 删除邮箱账号
#[tauri::command]
pub fn delete_account(id: i64, state: State<Mutex<AppState>>) -> AppResult<()> {
    let app_state = state.lock().unwrap();
    let db = app_state.accounts_db()?;

    db.delete_account(id)
        .map_err(|e| AppError::database(format!("删除账号失败: {}", e)))?;

    tracing::info!(account_id = id, "删除邮箱账号成功");

    Ok(())
}

/// 测试邮箱连接（验证账号有效性）
#[tauri::command]
pub fn test_account_connection(email: String, password: String) -> AppResult<String> {
    use invoice_collect::config::ImapConfig;

    // IMAP 库可能 panic，用 catch_unwind 包裹
    let result = std::panic::catch_unwind(|| -> AppResult<String> {
        // 临时设置环境变量（invoice-collect::ImapConfig::from_env 依赖它）
        std::env::set_var("INVOICE_IMAP_PASSWORD", &password);

        let config = ImapConfig::from_env(&email)
            .map_err(|e| AppError::network(format!("配置错误: {}", e)))?;

        // 尝试连接（Session 类型，不是 ImapClient）
        let _session = invoice_collect::imap_client::Session::connect(&config)
            .map_err(|e| AppError::network(format!("连接失败: {}", e)))?;

        // 简单检查：能连上就认为有效
        Ok("连接成功".to_string())
    });

    match result {
        Ok(Ok(msg)) => Ok(msg),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(AppError::internal("IMAP 库内部错误")),
    }
}

// ========== 应用设置 ==========

/// 获取单个设置项
#[tauri::command]
pub fn get_setting(key: String, state: State<Mutex<AppState>>) -> AppResult<Option<String>> {
    let app_state = state.lock().unwrap();
    let db = app_state.ledger_db()?;

    db.get_setting(&key)
        .map_err(|e| AppError::database(format!("读取设置失败: {}", e)))
}

/// 设置单个配置项
#[tauri::command]
pub fn set_setting(key: String, value: String, state: State<Mutex<AppState>>) -> AppResult<()> {
    let app_state = state.lock().unwrap();
    let db = app_state.ledger_db()?;

    db.set_setting(&key, &value)
        .map_err(|e| AppError::database(format!("保存设置失败: {}", e)))?;

    tracing::info!(key = %key, "保存设置成功");

    Ok(())
}

/// 获取所有设置
#[tauri::command]
pub fn get_all_settings(
    state: State<Mutex<AppState>>,
) -> AppResult<std::collections::HashMap<String, String>> {
    let app_state = state.lock().unwrap();
    let db = app_state.ledger_db()?;

    db.get_all_settings()
        .map_err(|e| AppError::database(format!("读取设置失败: {}", e)))
}

// ========== 归组规则配置 ==========

/// 归组规则（可序列化的部分）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupingRules {
    /// 常驻城市列表
    pub home_cities: Vec<String>,
    /// 休息日（0=周日, 6=周六）
    pub weekend_days: Vec<u8>,
    /// 机场关键词
    pub airport_keywords: Vec<String>,
    /// 酒店关键词
    pub hotel_keywords: Vec<String>,
}

impl Default for GroupingRules {
    fn default() -> Self {
        Self {
            home_cities: vec!["北京".to_string()],
            weekend_days: vec![0, 6], // 周六日
            airport_keywords: vec![
                "机场".to_string(),
                "航站楼".to_string(),
                "Airport".to_string(),
            ],
            hotel_keywords: vec![
                "酒店".to_string(),
                "宾馆".to_string(),
                "Hotel".to_string(),
            ],
        }
    }
}

/// 获取归组规则
#[tauri::command]
pub fn get_grouping_rules(state: State<Mutex<AppState>>) -> AppResult<GroupingRules> {
    let app_state = state.lock().unwrap();
    let db = app_state.ledger_db()?;

    match db
        .get_setting("grouping_rules")
        .map_err(|e| AppError::database(format!("读取归组规则失败: {}", e)))?
    {
        Some(json) => serde_json::from_str(&json)
            .map_err(|e| AppError::internal(format!("解析归组规则失败: {}", e))),
        None => Ok(GroupingRules::default()),
    }
}

/// 保存归组规则
#[tauri::command]
pub fn save_grouping_rules(
    rules: GroupingRules,
    state: State<Mutex<AppState>>,
) -> AppResult<()> {
    let app_state = state.lock().unwrap();
    let db = app_state.ledger_db()?;

    let json = serde_json::to_string(&rules)
        .map_err(|e| AppError::internal(format!("序列化归组规则失败: {}", e)))?;

    db.set_setting("grouping_rules", &json)
        .map_err(|e| AppError::database(format!("保存归组规则失败: {}", e)))?;

    tracing::info!(
        home_cities = ?rules.home_cities,
        weekend_days = ?rules.weekend_days,
        "保存归组规则成功"
    );

    Ok(())
}
