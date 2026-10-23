//! 播放列表的查询与增删改(从 Tauri 命令层移植的 SeaORM 主体)。

use std::collections::HashMap;

use sea_orm::{
    ActiveModelTrait as _, ColumnTrait as _, EntityTrait as _, QueryFilter as _, QueryOrder as _,
    Set,
};
use serde::Serialize;

use super::entity::{playlist, playlist_songs};
use super::utils;

/// 播放列表 + 有序曲目 id(上游 PlaylistWithSongs)
#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistWithSongs {
    #[serde(flatten)]
    pub playlist: playlist::Model,
    pub song_ids: Vec<String>,
}

async fn load_song_ids(db: &super::DbConnection, playlist_id: i32) -> anyhow::Result<Vec<String>> {
    let rows = playlist_songs::Entity::find()
        .filter(playlist_songs::Column::PlaylistId.eq(playlist_id))
        .order_by_asc(playlist_songs::Column::AddedAt)
        .all(db)
        .await?;
    Ok(rows.into_iter().map(|r| r.song_id).collect())
}

pub async fn get_all_playlists(db: &super::DbConnection) -> anyhow::Result<Vec<PlaylistWithSongs>> {
    let playlists = playlist::Entity::find()
        .order_by_asc(playlist::Column::Id)
        .all(db)
        .await?;
    let ids: Vec<i32> = playlists.iter().map(|p| p.id).collect();
    let mut map: HashMap<i32, Vec<String>> = HashMap::new();
    if !ids.is_empty() {
        let rows = playlist_songs::Entity::find()
            .filter(playlist_songs::Column::PlaylistId.is_in(ids))
            .order_by_asc(playlist_songs::Column::AddedAt)
            .all(db)
            .await?;
        for r in rows {
            map.entry(r.playlist_id).or_default().push(r.song_id);
        }
    }
    Ok(playlists
        .into_iter()
        .map(|p| {
            let song_ids = map.remove(&p.id).unwrap_or_default();
            PlaylistWithSongs {
                playlist: p,
                song_ids,
            }
        })
        .collect())
}

pub async fn get_playlist(
    db: &super::DbConnection,
    id: i32,
) -> anyhow::Result<Option<PlaylistWithSongs>> {
    let Some(p) = playlist::Entity::find_by_id(id).one(db).await? else {
        return Ok(None);
    };
    let song_ids = load_song_ids(db, p.id).await?;
    Ok(Some(PlaylistWithSongs {
        playlist: p,
        song_ids,
    }))
}

pub async fn create_playlist(db: &super::DbConnection, name: &str) -> anyhow::Result<i32> {
    let now = chrono::Utc::now().timestamp_millis();
    let model = playlist::ActiveModel {
        name: Set(name.to_string()),
        create_time: Set(now),
        update_time: Set(now),
        play_time: Set(0),
        ..Default::default()
    };
    let result = model.insert(db).await?;
    Ok(result.id)
}

pub async fn update_playlist(
    db: &super::DbConnection,
    id: i32,
    name: Option<String>,
    play_time: Option<i64>,
) -> anyhow::Result<bool> {
    let Some(model) = playlist::Entity::find_by_id(id).one(db).await? else {
        return Ok(false);
    };
    let mut active: playlist::ActiveModel = model.into();
    if let Some(name) = name {
        active.name = Set(name);
    }
    if let Some(play_time) = play_time {
        active.play_time = Set(play_time);
    }
    active.update_time = Set(chrono::Utc::now().timestamp_millis());
    active.update(db).await?;
    Ok(true)
}

pub async fn delete_playlist(db: &super::DbConnection, id: i32) -> anyhow::Result<bool> {
    let Some(model) = playlist::Entity::find_by_id(id).one(db).await? else {
        return Ok(false);
    };
    let active: playlist::ActiveModel = model.into();
    active.delete(db).await?;
    playlist_songs::Entity::delete_many()
        .filter(playlist_songs::Column::PlaylistId.eq(id))
        .exec(db)
        .await?;
    Ok(true)
}

pub async fn add_songs(
    db: &super::DbConnection,
    playlist_id: i32,
    song_ids: &[String],
) -> anyhow::Result<()> {
    utils::link_songs_to_playlist(db, playlist_id, song_ids).await
}

pub async fn remove_songs(
    db: &super::DbConnection,
    playlist_id: i32,
    song_ids: &[String],
) -> anyhow::Result<()> {
    utils::unlink_songs_from_playlist(db, playlist_id, song_ids).await
}
