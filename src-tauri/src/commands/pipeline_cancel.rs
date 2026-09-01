use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use crate::error::{AppError, AppResult};

pub(crate) const CANCELLATION_MESSAGE: &str =
    "用户已安全停止；已完成检查点会保留，可在“可恢复任务”中继续";

pub(crate) type CancellationToken = Arc<AtomicBool>;

fn active_pipelines() -> &'static Mutex<HashMap<String, CancellationToken>> {
    static ACTIVE: OnceLock<Mutex<HashMap<String, CancellationToken>>> = OnceLock::new();
    ACTIVE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(crate) struct ActivePipelineRegistration {
    pipeline_id: String,
    token: CancellationToken,
}

impl ActivePipelineRegistration {
    pub(crate) fn token(&self) -> CancellationToken {
        Arc::clone(&self.token)
    }
}

impl Drop for ActivePipelineRegistration {
    fn drop(&mut self) {
        if let Ok(mut active) = active_pipelines().lock() {
            active.remove(&self.pipeline_id);
        }
    }
}

pub(crate) fn register_active_pipeline(pipeline_id: &str) -> AppResult<ActivePipelineRegistration> {
    let mut active = active_pipelines()
        .lock()
        .map_err(|_| AppError::internal("流水线取消状态锁不可用"))?;
    if active.contains_key(pipeline_id) {
        return Err(AppError::validation("该流水线已在运行，请勿重复启动"));
    }
    let token = Arc::new(AtomicBool::new(false));
    active.insert(pipeline_id.to_string(), Arc::clone(&token));
    Ok(ActivePipelineRegistration {
        pipeline_id: pipeline_id.to_string(),
        token,
    })
}

pub(crate) fn request_cancel(pipeline_id: &str) -> AppResult<()> {
    let active = active_pipelines()
        .lock()
        .map_err(|_| AppError::internal("流水线取消状态锁不可用"))?;
    let token = active
        .get(pipeline_id)
        .ok_or_else(|| AppError::validation("该流水线当前未运行或已完成"))?;
    token.store(true, Ordering::Release);
    Ok(())
}

pub(crate) fn cancellation_requested(token: &CancellationToken) -> bool {
    token.load(Ordering::Acquire)
}

pub(crate) fn ensure_not_cancelled(token: &CancellationToken) -> AppResult<()> {
    if cancellation_requested(token) {
        Err(AppError::validation(CANCELLATION_MESSAGE))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_registry_rejects_duplicates_and_cleans_up() {
        let pipeline_id = uuid::Uuid::new_v4().to_string();
        let registration = register_active_pipeline(&pipeline_id).unwrap();
        let token = registration.token();
        assert!(register_active_pipeline(&pipeline_id).is_err());
        request_cancel(&pipeline_id).unwrap();
        assert!(cancellation_requested(&token));
        assert!(ensure_not_cancelled(&token).is_err());

        drop(registration);
        assert!(request_cancel(&pipeline_id).is_err());
        assert!(register_active_pipeline(&pipeline_id).is_ok());
    }
}
