//! Concur 连接与草稿闭环能力测试命令。

use std::sync::Mutex;

use chrono::Utc;
use tauri::{AppHandle, State};
use tauri_plugin_shell::ShellExt;

use crate::concur_api::{
    self, ConcurBrowserOauthConfig, ConcurBrowserOauthInput, ConcurCapabilityTestResult,
    ConcurConnectionInput, ConcurConnectionStatus, ConcurDraftWorkflowTestInput,
};
use crate::error::{AppError, AppResult};
use crate::AppState;

#[tauri::command]
pub fn get_concur_connection_status(
    state: State<Mutex<AppState>>,
) -> AppResult<ConcurConnectionStatus> {
    let app_state = state
        .lock()
        .map_err(|_| AppError::internal("应用状态锁不可用"))?;
    Ok(app_state
        .concur_session()
        .map(ConcurConnectionStatus::from)
        .unwrap_or_else(ConcurConnectionStatus::disconnected))
}

#[tauri::command]
pub fn get_concur_browser_oauth_config() -> AppResult<ConcurBrowserOauthConfig> {
    Ok(concur_api::browser_oauth_config())
}

#[tauri::command]
pub async fn test_concur_browser_oauth(
    input: ConcurBrowserOauthInput,
    app: AppHandle,
    state: State<'_, Mutex<AppState>>,
) -> AppResult<ConcurCapabilityTestResult> {
    {
        let mut app_state = state
            .lock()
            .map_err(|_| AppError::internal("应用状态锁不可用"))?;
        app_state.clear_concur_session();
    }
    let attempt = concur_api::prepare_browser_oauth(input)?;
    let authorize_url = attempt.authorize_url().to_string();
    #[allow(deprecated)]
    app.shell()
        .open(authorize_url, None)
        .map_err(|_| AppError::io("无法打开系统浏览器；请检查默认浏览器或企业安全策略"))?;
    let outcome =
        tauri::async_runtime::spawn_blocking(move || concur_api::complete_browser_oauth(attempt))
            .await
            .map_err(|_| AppError::internal("Concur 浏览器授权测试线程异常"))??;
    if let Some(session) = outcome.session {
        let mut app_state = state
            .lock()
            .map_err(|_| AppError::internal("应用状态锁不可用"))?;
        app_state.set_concur_session(session);
    }
    Ok(outcome.result)
}

#[tauri::command]
pub async fn test_concur_read_access(
    input: ConcurConnectionInput,
    state: State<'_, Mutex<AppState>>,
) -> AppResult<ConcurCapabilityTestResult> {
    {
        let mut app_state = state
            .lock()
            .map_err(|_| AppError::internal("应用状态锁不可用"))?;
        app_state.clear_concur_session();
    }
    let result =
        tauri::async_runtime::spawn_blocking(move || concur_api::read_connection_test(input))
            .await
            .map_err(|_| AppError::internal("Concur 连接测试线程异常"))??;
    let (session, response) = result;
    let mut app_state = state
        .lock()
        .map_err(|_| AppError::internal("应用状态锁不可用"))?;
    app_state.set_concur_session(session);
    Ok(response)
}

#[tauri::command]
pub async fn test_concur_draft_workflow(
    input: ConcurDraftWorkflowTestInput,
    state: State<'_, Mutex<AppState>>,
) -> AppResult<ConcurCapabilityTestResult> {
    let session = {
        let app_state = state
            .lock()
            .map_err(|_| AppError::internal("应用状态锁不可用"))?;
        app_state
            .concur_session_copy()
            .ok_or_else(|| AppError::validation("请先完成 Concur 只读连接测试"))?
    };
    let base_url = session.base_url.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        concur_api::draft_workflow_test(&session, &input)
    })
    .await
    .map_err(|_| AppError::internal("Concur 草稿能力测试线程异常"))??;
    {
        let mut app_state = state
            .lock()
            .map_err(|_| AppError::internal("应用状态锁不可用"))?;
        if let Some(active) = app_state.concur_session_mut() {
            if active.base_url == base_url {
                active.capability_checks = result.steps.clone();
                if result.connected_account.is_some() {
                    active.connected_account = result.connected_account.clone();
                }
                if result.success {
                    active.draft_workflow_verified = true;
                    active.verified_at = Utc::now().to_rfc3339();
                }
            }
        }
    }
    Ok(result)
}

#[tauri::command]
pub fn clear_concur_session(state: State<Mutex<AppState>>) -> AppResult<ConcurConnectionStatus> {
    let mut app_state = state
        .lock()
        .map_err(|_| AppError::internal("应用状态锁不可用"))?;
    app_state.clear_concur_session();
    Ok(ConcurConnectionStatus::disconnected())
}
