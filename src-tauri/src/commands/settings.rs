//! H2 设置界面后端命令
//!
//! 提供三类配置接口：
//! 1. 邮箱账号管理（复用 invoice-store::accounts_db）
//! 2. 应用设置（常驻城市等，存储在 ledger_db settings 表）
//! 3. 归组规则配置（JSON 序列化存储）

use crate::error::{AppError, AppResult};
use crate::AppState;
use invoice_grouping::types::StationCityAlias;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
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

/// 保存邮箱地址，并把授权码放入当前进程会话；授权码不写入数据库。
#[tauri::command]
pub fn add_account(
    email: String,
    password: String,
    state: State<Mutex<AppState>>,
) -> AppResult<i64> {
    use invoice_collect::config::ImapConfig;

    let password = zeroize::Zeroizing::new(password);
    let config = ImapConfig::from_credentials(&email, password.as_str())
        .map_err(|e| AppError::validation(format!("不支持的邮箱域名: {}", e)))?;

    let mut app_state = state.lock().unwrap();
    let account_id = {
        let db = app_state.accounts_db()?;
        if let Some(existing) = db
            .list_accounts()
            .map_err(|e| AppError::database(format!("读取邮箱配置失败: {}", e)))?
            .into_iter()
            .find(|account| account.email.eq_ignore_ascii_case(email.trim()))
        {
            existing.id
        } else {
            db.create_account(email.trim(), &config.host, config.port)
                .map_err(|e| AppError::database(format!("保存邮箱地址失败: {}", e)))?
        }
    };
    app_state.set_session_credential(email, password);

    tracing::info!(
        account_id,
        imap_server = %config.host,
        "邮箱会话凭据已配置（授权码未持久化）"
    );

    Ok(account_id)
}

/// 删除邮箱账号
#[tauri::command]
pub fn delete_account(id: i64, state: State<Mutex<AppState>>) -> AppResult<()> {
    let mut app_state = state.lock().unwrap();
    let deleted_email = {
        let db = app_state.accounts_db()?;
        let account = db
            .get_account(id)
            .map_err(|e| AppError::database(format!("读取邮箱配置失败: {}", e)))?;
        db.delete_account(id)
            .map_err(|e| AppError::database(format!("删除邮箱配置失败: {}", e)))?;
        account.email
    };
    if app_state
        .session_email()
        .is_some_and(|email| email.eq_ignore_ascii_case(&deleted_email))
    {
        app_state.clear_session_credential();
    }

    tracing::info!(account_id = id, "删除邮箱地址配置成功");

    Ok(())
}

/// 测试邮箱连接（验证账号有效性）
#[tauri::command]
pub fn test_account_connection(email: String, password: String) -> AppResult<String> {
    use invoice_collect::config::ImapConfig;

    let password = zeroize::Zeroizing::new(password);
    // IMAP 库可能 panic，用 catch_unwind 包裹
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> AppResult<String> {
        let config = ImapConfig::from_credentials(&email, password.as_str())
            .map_err(|e| AppError::network(format!("配置错误: {}", e)))?;

        // 尝试连接（Session 类型，不是 ImapClient）
        let _session = invoice_collect::imap_client::Session::connect(&config)
            .map_err(|e| AppError::network(format!("连接失败: {}", e)))?;

        // 简单检查：能连上就认为有效
        Ok("连接成功；授权码仅用于本次应用会话，退出后需重新输入".to_string())
    }));

    match result {
        Ok(Ok(msg)) => Ok(msg),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(AppError::internal("IMAP 库内部错误")),
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionCredentialStatus {
    pub configured: bool,
    pub email: Option<String>,
}

#[tauri::command]
pub fn get_session_credential_status(
    state: State<Mutex<AppState>>,
) -> AppResult<SessionCredentialStatus> {
    let app_state = state.lock().unwrap();
    Ok(SessionCredentialStatus {
        configured: app_state.session_email().is_some(),
        email: app_state.session_email().map(str::to_string),
    })
}

#[tauri::command]
pub fn clear_session_credential(state: State<Mutex<AppState>>) -> AppResult<()> {
    state.lock().unwrap().clear_session_credential();
    tracing::info!("邮箱会话凭据已清除");
    Ok(())
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

// ========== 常驻城市车站库 ==========

const HOME_STATION_LIBRARIES_KEY: &str = "home_station_libraries";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HomeStationEntry {
    pub station_name: String,
    pub aliases: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HomeStationLibrary {
    pub city: String,
    pub stations: Vec<HomeStationEntry>,
}

type StoredHomeStationLibraries = BTreeMap<String, Vec<HomeStationEntry>>;

fn normalize_station_value(value: &str) -> String {
    value
        .trim()
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect()
}

fn default_home_station_library(city: &str) -> HomeStationLibrary {
    HomeStationLibrary {
        city: city.to_string(),
        stations: invoice_parse::station_city::station_city_records_for_city(city)
            .into_iter()
            .map(|record| HomeStationEntry {
                station_name: record.station_name,
                aliases: record.aliases,
            })
            .collect(),
    }
}

fn read_stored_home_station_libraries(
    db: &invoice_store::LedgerDb,
) -> AppResult<StoredHomeStationLibraries> {
    let Some(json) = db
        .get_setting(HOME_STATION_LIBRARIES_KEY)
        .map_err(|error| AppError::database(format!("读取常驻车站库失败: {error}")))?
    else {
        return Ok(BTreeMap::new());
    };
    serde_json::from_str(&json)
        .map_err(|error| AppError::internal(format!("解析常驻车站库失败: {error}")))
}

fn validate_home_station_library(library: &mut HomeStationLibrary) -> AppResult<()> {
    library.city = library.city.trim().to_string();
    if library.city.is_empty() || library.city.chars().count() > 20 {
        return Err(AppError::validation("常驻城市不能为空且不能超过 20 个字符"));
    }
    if library.stations.is_empty() || library.stations.len() > 100 {
        return Err(AppError::validation("常驻车站至少保留 1 个，最多 100 个"));
    }

    let mut seen_names = HashSet::new();
    for station in &mut library.stations {
        station.station_name = station.station_name.trim().to_string();
        if station.station_name.is_empty() || station.station_name.chars().count() > 40 {
            return Err(AppError::validation("标准站名不能为空且不能超过 40 个字符"));
        }
        if station.aliases.len() > 12 {
            return Err(AppError::validation("每个车站最多设置 12 个别名"));
        }
        station.aliases = station
            .aliases
            .iter()
            .map(|alias| alias.trim().to_string())
            .filter(|alias| !alias.is_empty())
            .collect();
        for value in std::iter::once(&station.station_name).chain(station.aliases.iter()) {
            if value.chars().count() > 40 || value.contains(['→', '\n', '\r']) {
                return Err(AppError::validation("站名或别名格式无效"));
            }
            let key = normalize_station_value(value);
            if !seen_names.insert(key) {
                return Err(AppError::validation(format!("车站名称或别名重复：{value}")));
            }
        }
    }
    Ok(())
}

pub(crate) fn load_effective_home_station_aliases(
    db: &invoice_store::LedgerDb,
    home_city: &str,
) -> AppResult<Vec<StationCityAlias>> {
    let stored = read_stored_home_station_libraries(db)?;
    let stations = stored
        .get(home_city)
        .cloned()
        .unwrap_or_else(|| default_home_station_library(home_city).stations);
    Ok(stations
        .into_iter()
        .flat_map(|station| {
            std::iter::once(station.station_name)
                .chain(station.aliases)
                .map(|station_name| StationCityAlias {
                    station_name,
                    city_name: home_city.to_string(),
                })
                .collect::<Vec<_>>()
        })
        .collect())
}

#[tauri::command]
pub fn get_home_station_library(state: State<Mutex<AppState>>) -> AppResult<HomeStationLibrary> {
    let app_state = state.lock().unwrap();
    let db = app_state.ledger_db()?;
    let home_city = db
        .get_setting("home_city")
        .map_err(|error| AppError::database(format!("读取常驻城市失败: {error}")))?
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| AppError::validation("请先在通用设置中填写常驻城市"))?;
    let stored = read_stored_home_station_libraries(db)?;
    Ok(stored
        .get(&home_city)
        .cloned()
        .map(|stations| HomeStationLibrary {
            city: home_city.clone(),
            stations,
        })
        .unwrap_or_else(|| default_home_station_library(&home_city)))
}

#[tauri::command]
pub fn save_home_station_library(
    mut library: HomeStationLibrary,
    state: State<Mutex<AppState>>,
) -> AppResult<()> {
    validate_home_station_library(&mut library)?;
    let app_state = state.lock().unwrap();
    let db = app_state.ledger_db()?;
    let home_city = db
        .get_setting("home_city")
        .map_err(|error| AppError::database(format!("读取常驻城市失败: {error}")))?
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| AppError::validation("请先在通用设置中填写常驻城市"))?;
    if library.city != home_city {
        return Err(AppError::validation(
            "常驻城市已变化，请重新加载车站库后再保存",
        ));
    }
    let mut stored = read_stored_home_station_libraries(db)?;
    stored.insert(home_city.clone(), library.stations);
    let json = serde_json::to_string(&stored)
        .map_err(|error| AppError::internal(format!("序列化常驻车站库失败: {error}")))?;
    db.set_setting(HOME_STATION_LIBRARIES_KEY, &json)
        .map_err(|error| AppError::database(format!("保存常驻车站库失败: {error}")))?;
    tracing::info!(city = %home_city, "保存常驻城市车站库成功");
    Ok(())
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
            hotel_keywords: vec!["酒店".to_string(), "宾馆".to_string(), "Hotel".to_string()],
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
pub fn save_grouping_rules(rules: GroupingRules, state: State<Mutex<AppState>>) -> AppResult<()> {
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

#[cfg(test)]
mod home_station_library_tests {
    use super::*;

    #[test]
    fn bundled_beijing_library_contains_qinghe() {
        let library = default_home_station_library("北京");
        assert!(library.stations.iter().any(|station| {
            station.station_name == "清河" && station.aliases.iter().any(|alias| alias == "清河站")
        }));
    }

    #[test]
    fn rejects_duplicate_station_aliases() {
        let mut library = HomeStationLibrary {
            city: "北京".to_string(),
            stations: vec![
                HomeStationEntry {
                    station_name: "清河站".to_string(),
                    aliases: vec!["清河".to_string()],
                },
                HomeStationEntry {
                    station_name: "北京南站".to_string(),
                    aliases: vec!["清河".to_string()],
                },
            ],
        };

        assert!(validate_home_station_library(&mut library).is_err());
    }

    #[test]
    fn trims_valid_station_library() {
        let mut library = HomeStationLibrary {
            city: " 北京 ".to_string(),
            stations: vec![HomeStationEntry {
                station_name: " 清河站 ".to_string(),
                aliases: vec![" 清河 ".to_string(), "".to_string()],
            }],
        };

        validate_home_station_library(&mut library).unwrap();

        assert_eq!(library.city, "北京");
        assert_eq!(library.stations[0].station_name, "清河站");
        assert_eq!(library.stations[0].aliases, vec!["清河"]);
    }
}
