//! 曲目(song)的查询与写入。

use sea_orm::{ActiveModelTrait as _, ColumnTrait as _, EntityTrait as _, QueryFilter as _, Set};

use super::entity::song;
use super::utils;

/// 批量入库(存在则更新,保持封面文件同步)
pub async fn upsert_songs(
    db: &super::DbConnection,
    covers_dir: &std::path::Path,
    songs: Vec<song::Model>,
) -> anyhow::Result<Vec<String>> {
    std::fs::create_dir_all(covers_dir)?;
    let mut ids = Vec::with_capacity(songs.len());
    for s in songs {
        let song_id = s.id.clone();
        utils::upsert_song(db, &s).await?;
        ids.push(song_id);
    }
    Ok(ids)
}

/// 带封面入库:封面字节写入 covers 目录,cover_path 回填
pub async fn upsert_scanned(
    db: &super::DbConnection,
    covers_dir: &std::path::Path,
    scanned: Vec<Result<super::scanner::ScannedSong, String>>,
) -> anyhow::Result<Vec<String>> {
    let models: Vec<song::Model> = scanned
        .into_iter()
        .filter_map(|entry| {
            let Ok(mut scanned) = entry else {
                return None;
            };
            scanned.model.cover_path =
                utils::save_cover(covers_dir, &scanned.model.id, scanned.cover_bytes.as_deref());
            Some(scanned.model)
        })
        .collect();
    upsert_songs(db, covers_dir, models).await
}

pub async fn get_song(
    db: &super::DbConnection,
    id: &str,
) -> anyhow::Result<Option<song::Model>> {
    Ok(song::Entity::find_by_id(id).one(db).await?)
}

pub async fn get_songs_by_ids(
    db: &super::DbConnection,
    ids: &[String],
) -> anyhow::Result<Vec<song::Model>> {
    Ok(song::Entity::find()
        .filter(song::Column::Id.is_in(ids.to_vec()))
        .all(db)
        .await?)
}

/// 修改曲目元数据(仅更新显式提供的字段)
pub struct SongPatch {
    pub song_name: Option<String>,
    pub song_artists: Option<String>,
    pub song_album: Option<String>,
    pub lyric_format: Option<String>,
    pub lyric: Option<String>,
    pub translated_lrc: Option<Option<String>>,
    pub roman_lrc: Option<Option<String>>,
    pub cover_path: Option<Option<String>>,
}

pub async fn update_song(
    db: &super::DbConnection,
    id: &str,
    patch: SongPatch,
) -> anyhow::Result<Option<song::Model>> {
    let Some(existing) = song::Entity::find_by_id(id).one(db).await? else {
        return Ok(None);
    };
    let mut active: song::ActiveModel = existing.into();
    if let Some(v) = patch.song_name {
        active.song_name = Set(v);
    }
    if let Some(v) = patch.song_artists {
        active.song_artists = Set(v);
    }
    if let Some(v) = patch.song_album {
        active.song_album = Set(v);
    }
    if let Some(v) = patch.lyric_format {
        active.lyric_format = Set(v);
    }
    if let Some(v) = patch.lyric {
        active.lyric = Set(v);
    }
    if let Some(v) = patch.translated_lrc {
        active.translated_lrc = Set(v);
    }
    if let Some(v) = patch.roman_lrc {
        active.roman_lrc = Set(v);
    }
    if let Some(v) = patch.cover_path {
        active.cover_path = Set(v);
    }
    let updated = active.update(db).await?;
    Ok(Some(updated))
}

/// 清理零引用曲目(连带封面文件);返回被删除的曲目 id
pub async fn cleanup_orphaned_songs(
    db: &super::DbConnection,
    song_ids: &[String],
) -> anyhow::Result<Vec<String>> {
    utils::cleanup_orphaned_songs(db, song_ids).await
}
