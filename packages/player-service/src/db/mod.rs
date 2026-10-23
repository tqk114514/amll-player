//! 曲库数据库层:SQLite(SeaORM)+ 迁移 + 扫描 + 曲目/播放列表查询。
//!
//! 从 Tauri 后端(`packages/player/src-tauri/src/db`)抽出的可移植部分,
//! 不依赖任何窗口/应用句柄:调用方给路径,本层负责连接、迁移与查询。
//! 事件不在此发射——返回值即结果,通知由上层(事件总线)转发。

pub mod entity;
pub mod library;
pub mod migration;
pub mod playlist;
pub mod scanner;
pub mod utils;

use sea_orm::{Database, DatabaseConnection};

pub type DbConnection = DatabaseConnection;

/// 连接 SQLite(不存在则创建)并跑到最新迁移。
/// `db_path`:数据库文件路径(如 `<appdata>/amll-player.db`)
pub async fn open_at(db_path: &std::path::Path) -> anyhow::Result<DatabaseConnection> {
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let db_url = format!("sqlite:{}?mode=rwc", db_path.display());
    let db = Database::connect(&db_url)
        .await
        .map_err(|e| anyhow::anyhow!("连接数据库失败: {e}"))?;
    use sea_orm_migration::MigratorTrait as _;
    migration::Migrator::up(&db, None)
        .await
        .map_err(|e| anyhow::anyhow!("数据库迁移失败: {e}"))?;
    Ok(db)
}
