// Prevents additional console window on Windows in release
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::Mutex;

mod commands;
mod error;
mod logger;

/// 应用共享状态。当前为空，后续任务（S0.5 批次状态机）在此挂载。
#[derive(Default)]
pub struct AppState {}

fn main() {
    // guard 必须绑定到具名变量：绑到 `_` 会立即 drop，退出时丢日志。
    let _log_guard = logger::init().expect("日志系统初始化失败");

    tauri::Builder::default()
        .manage(Mutex::new(AppState::default()))
        .plugin(tauri_plugin_shell::init())
        .invoke_handler(tauri::generate_handler![
            commands::greet,
            commands::get_version,
            commands::health_check,
            commands::trigger_error,
        ])
        .run(tauri::generate_context!())
        .expect("Tauri 应用运行失败");
}
