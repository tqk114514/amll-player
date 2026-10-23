//! 曲目与播放列表的底层工具(从 Tauri 后端移植,去依赖化)。

use sea_orm::{
    ActiveModelTrait as _, ActiveValue::Set, ColumnTrait as _, ConnectionTrait,
    EntityTrait as _, PaginatorTrait as _, QueryFilter as _,
};

use super::entity::{playlist_songs, song};

/// 封面字节写入 covers 目录;返回封面文件路径(无字节返回 None)
pub fn save_cover(
    covers_dir: &std::path::Path,
    song_id: &str,
    cover_bytes: Option<&[u8]>,
) -> Option<String> {
    let bytes = cover_bytes?;
    if bytes.is_empty() {
        return None;
    }
    let cover_file = covers_dir.join(format!("{song_id}.jpg"));
    match std::fs::write(&cover_file, bytes) {
        Ok(()) => Some(cover_file.to_string_lossy().to_string()),
        Err(_) => None,
    }
}

/// 清理零引用曲目(连带封面文件);返回被删除的曲目 id
pub async fn cleanup_orphaned_songs(
    db: &impl ConnectionTrait,
    song_ids: &[String],
) -> anyhow::Result<Vec<String>> {
    let mut deleted = Vec::new();
    for song_id in song_ids {
        let ref_count = playlist_songs::Entity::find()
            .filter(playlist_songs::Column::SongId.eq(song_id))
            .count(db)
            .await?;
        if ref_count > 0 {
            continue;
        }
        if let Some(s) = song::Entity::find_by_id(song_id).one(db).await? {
            if let Some(ref cover_path) = s.cover_path
                && !cover_path.is_empty()
            {
                let _ = std::fs::remove_file(cover_path);
            }
            let active: song::ActiveModel = s.into();
            active.delete(db).await?;
            deleted.push(song_id.clone());
        }
    }
    Ok(deleted)
}

/// 曲目入库(存在则按列更新)
pub async fn upsert_song(
    db: &impl ConnectionTrait,
    model: &song::Model,
) -> anyhow::Result<()> {
    let active: song::ActiveModel = model.clone().into();
    song::Entity::insert(active)
        .on_conflict(
            sea_orm::sea_query::OnConflict::column(song::Column::Id)
                .update_columns([
                    song::Column::FilePath,
                    song::Column::SongName,
                    song::Column::SongArtists,
                    song::Column::SongAlbum,
                    song::Column::Duration,
                    song::Column::LyricFormat,
                    song::Column::Lyric,
                    song::Column::TranslatedLrc,
                    song::Column::RomanLrc,
                    song::Column::CoverPath,
                    song::Column::ModifiedAt,
                ])
                .to_owned(),
        )
        .exec(db)
        .await?;
    Ok(())
}

/// 把曲目挂到播放列表(added_at 按传入顺序递增,保持插入顺序)
pub async fn link_songs_to_playlist(
    db: &impl ConnectionTrait,
    playlist_id: i32,
    song_ids: &[String],
) -> anyhow::Result<()> {
    if song_ids.is_empty() {
        return Ok(());
    }
    let now = chrono::Utc::now().timestamp_millis();
    let entries: Vec<playlist_songs::ActiveModel> = song_ids
        .iter()
        .enumerate()
        .map(|(i, song_id)| playlist_songs::ActiveModel {
            playlist_id: Set(playlist_id),
            song_id: Set(song_id.clone()),
            added_at: Set(now + i as i64),
            ..Default::default()
        })
        .collect();
    playlist_songs::Entity::insert_many(entries)
        .exec(db)
        .await?;
    Ok(())
}

/// 从播放列表移除曲目
pub async fn unlink_songs_from_playlist(
    db: &impl ConnectionTrait,
    playlist_id: i32,
    song_ids: &[String],
) -> anyhow::Result<()> {
    if song_ids.is_empty() {
        return Ok(());
    }
    playlist_songs::Entity::delete_many()
        .filter(playlist_songs::Column::PlaylistId.eq(playlist_id))
        .filter(playlist_songs::Column::SongId.is_in(song_ids.to_vec()))
        .exec(db)
        .await?;
    Ok(())
}
