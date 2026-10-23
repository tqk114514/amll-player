pub mod cleanup;
pub mod commands;
pub mod migrate;
pub mod refresh;
pub mod scanner;
pub mod utils;
// 实体与迁移已抽到 player-service crate(Tauri 与 GPUI 共用);
// 迁移经 player_service::db::open_at 应用,此处不再直接引用
pub use player_service::db::entity;

use sea_orm::DatabaseConnection;
use tauri::AppHandle;
use tauri::Manager;
use tauri::path::BaseDirectory;

pub type DbConnection = DatabaseConnection;

pub async fn init_database(app: &AppHandle) -> Result<DatabaseConnection, String> {
    let db_path = app
        .path()
        .resolve("amll-player.db", BaseDirectory::AppData)
        .map_err(|e| format!("Failed to resolve database path: {e}"))?;

    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create database directory: {e}"))?;
    }

    player_service::db::open_at(&db_path)
        .await
        .map_err(|e| format!("Failed to open database: {e}"))
}
