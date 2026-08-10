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
    // 初始化日志系统
    logger::init().expect("Failed to initialize logger");

    tauri::Builder::default()
        .manage(Mutex::new(AppState::default()))
        .plugin(tauri_plugin_shell::init())
        .invoke_handler(tauri::generate_handler![
            commands::greet,
            commands::get_version,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
