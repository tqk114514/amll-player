//! AMLL Player 进程内服务层(Phase 3)。
//!
//! 把 Tauri 后端的可移植核心抽成共用 crate:曲库 SQLite(实体/迁移/
//! 扫描/查询)、播放列表 CRUD、播放队列(上游 PlayQueueManager 的
//! 移植)、音频控制(包装 player-core 的 AudioPlayer)与事件总线。
//! Tauri 应用与 GPUI 应用都以进程内方式消费本层。
//!
//! 用法:`PlayerService::open(data_dir)` → `scan_folder` 入库 →
//! `play_queue`/`play_at` 播放;`subscribe` 拿事件广播。

pub mod db;
pub mod db_events;
pub mod events;
pub mod queue;

use amll_player_core::{
    AudioPlayer, AudioPlayerConfig, AudioPlayerHandle, AudioThreadEvent, AudioThreadMessage,
    NowPlayingOptions, SongData,
};
use anyhow::Context as _;
use events::{ServiceEvent, ServiceEventSender};
use queue::PlayQueue;
use std::sync::{Arc, Mutex};

/// 服务句柄:线程安全,克隆共享。
/// 音频线程在独立的 tokio runtime 上运行(与现状一致)。
#[derive(Clone)]
pub struct PlayerService {
    inner: Arc<ServiceInner>,
    runtime: Arc<tokio::runtime::Runtime>,
}

struct ServiceInner {
    db: db::DbConnection,
    covers_dir: std::path::PathBuf,
    handle: AudioPlayerHandle,
    queue: Mutex<PlayQueue>,
    bus: ServiceEventSender,
}

impl PlayerService {
    /// 打开服务:连接 `<data_dir>/amll-player.db`(自动迁移),
    /// 启动音频线程与事件总线。
    pub fn open(data_dir: &std::path::Path) -> anyhow::Result<Self> {
        Self::open_with(data_dir, NowPlayingOptions::default())
    }

    pub fn open_with(
        data_dir: &std::path::Path,
        media_controls: NowPlayingOptions,
    ) -> anyhow::Result<Self> {
        let runtime = Arc::new(
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .context("创建音频运行时失败")?,
        );
        let db = runtime.block_on(db::open_at(&data_dir.join("amll-player.db")))?;
        let covers_dir = data_dir.join("covers");
        std::fs::create_dir_all(&covers_dir)?;

        let (bus, _) = tokio::sync::broadcast::channel(256);
        let (evt_tx, evt_rx) = tokio::sync::mpsc::unbounded_channel::<
            amll_player_core::AudioThreadEventMessage<AudioThreadEvent>,
        >();
        let config = AudioPlayerConfig {
            media_controls_options: media_controls,
        };
        // AudioPlayer::new 内部 spawn tokio 任务,须在 runtime 上下文构造
        let player = runtime
            .block_on(async { AudioPlayer::new(config, evt_tx) })
            .context("初始化音频设备失败")?;
        let handle = player.handler();

        // 音频线程常驻:run() 驱动消息循环
        let player_task = player.run();
        runtime.spawn(player_task);
        events::spawn_audio_bridge(&runtime, evt_rx, bus.clone());

        Ok(Self {
            inner: Arc::new(ServiceInner {
                db,
                covers_dir,
                handle,
                queue: Mutex::new(PlayQueue::new()),
                bus,
            }),
            runtime,
        })
    }

    // ==== 曲库 ====

    /// 扫描文件夹入库(并行;返回入库曲目 id 列表)
    pub fn scan_folder(&self, folder: &std::path::Path) -> anyhow::Result<Vec<String>> {
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let scanned = db::scanner::scan_folder(folder, &cancel, &|_| {});
        let db_ref = self.inner.db.clone();
        let covers = self.inner.covers_dir.clone();
        self.runtime
            .block_on(db::library::upsert_scanned(&db_ref, &covers, scanned))
    }

    pub async fn get_song(&self, id: &str) -> anyhow::Result<Option<db::entity::song::Model>> {
        db::library::get_song(&self.inner.db, id).await
    }

    pub async fn get_songs_by_ids(
        &self,
        ids: &[String],
    ) -> anyhow::Result<Vec<db::entity::song::Model>> {
        db::library::get_songs_by_ids(&self.inner.db, ids).await
    }

    // ==== 播放列表 ====

    pub async fn get_all_playlists(&self) -> anyhow::Result<Vec<db::playlist::PlaylistWithSongs>> {
        db::playlist::get_all_playlists(&self.inner.db).await
    }

    pub async fn get_playlist(
        &self,
        id: i32,
    ) -> anyhow::Result<Option<db::playlist::PlaylistWithSongs>> {
        db::playlist::get_playlist(&self.inner.db, id).await
    }

    pub async fn create_playlist(&self, name: &str) -> anyhow::Result<i32> {
        db::playlist::create_playlist(&self.inner.db, name).await
    }

    pub async fn delete_playlist(&self, id: i32) -> anyhow::Result<bool> {
        db::playlist::delete_playlist(&self.inner.db, id).await
    }

    pub async fn add_songs_to_playlist(
        &self,
        playlist_id: i32,
        song_ids: &[String],
    ) -> anyhow::Result<()> {
        db::playlist::add_songs(&self.inner.db, playlist_id, song_ids).await
    }

    // ==== 队列 ====

    /// 设置队列(曲目 id 顺序表)并从头播放
    pub async fn play_queue(
        &self,
        song_ids: Vec<String>,
        playlist_id: Option<i32>,
    ) -> anyhow::Result<()> {
        let pending = {
            let mut queue = self.inner.queue.lock().expect("队列锁中毒");
            queue.set_queue(song_ids, playlist_id);
            queue.take_pending_play()
        };
        self.flush_pending_play(pending).await
    }

    pub async fn play_at(&self, index: usize) -> anyhow::Result<()> {
        let pending = {
            let mut queue = self.inner.queue.lock().expect("队列锁中毒");
            queue.play_at_index(index);
            queue.take_pending_play()
        };
        self.flush_pending_play(pending).await
    }

    pub async fn next(&self) -> anyhow::Result<()> {
        let pending = {
            let mut queue = self.inner.queue.lock().expect("队列锁中毒");
            queue.advance_for_user();
            queue.take_pending_play()
        };
        self.flush_pending_play(pending).await
    }

    pub async fn previous(&self) -> anyhow::Result<()> {
        let pending = {
            let mut queue = self.inner.queue.lock().expect("队列锁中毒");
            queue.retreat_for_user();
            queue.take_pending_play()
        };
        self.flush_pending_play(pending).await
    }

    pub fn cycle_repeat_mode(&self) {
        self.inner
            .queue
            .lock()
            .expect("队列锁中毒")
            .cycle_repeat_mode();
    }

    pub fn toggle_shuffle(&self) {
        self.inner.queue.lock().expect("队列锁中毒").toggle_shuffle();
    }

    pub fn queue_snapshot(&self, position_sec: f64) -> queue::PersistedQueueState {
        self.inner
            .queue
            .lock()
            .expect("队列锁中毒")
            .persisted_state(position_sec)
    }

    pub async fn restore_queue(&self, state: queue::PersistedQueueState) -> anyhow::Result<()> {
        let mut queue = self.inner.queue.lock().expect("队列锁中毒");
        queue.restore(state);
        drop(queue);
        Ok(())
    }

    pub fn current_song_id(&self) -> Option<String> {
        self.inner
            .queue
            .lock()
            .expect("队列锁中毒")
            .current_song_id()
            .map(|s| s.to_string())
    }

    // ==== 播放控制 ====

    pub fn pause(&self) {
        let handle = self.inner.handle.clone();
        self.runtime.spawn(async move {
            let _ = handle.send_anonymous(AudioThreadMessage::PauseAudio).await;
        });
    }

    pub fn resume(&self) {
        let handle = self.inner.handle.clone();
        self.runtime.spawn(async move {
            let _ = handle
                .send_anonymous(AudioThreadMessage::ResumeAudio)
                .await;
        });
    }

    pub fn seek(&self, position_sec: f64) {
        let handle = self.inner.handle.clone();
        self.runtime.spawn(async move {
            let _ = handle
                .send_anonymous(AudioThreadMessage::SeekAudio {
                    position: position_sec,
                })
                .await;
        });
    }

    pub fn set_volume(&self, volume: f64) {
        let handle = self.inner.handle.clone();
        self.runtime.spawn(async move {
            let _ = handle
                .send_anonymous(AudioThreadMessage::SetVolume { volume })
                .await;
        });
    }

    // ==== 事件 ====

    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<ServiceEvent> {
        self.inner.bus.subscribe()
    }

    /// 底层音频句柄(供 UI 直接 seek/暂停等;与 Service 方法同源)
    pub fn audio_handle(&self) -> AudioPlayerHandle {
        self.inner.handle.clone()
    }

    /// 队列决策出的播放指令落地:查库拿路径 → 发曲 → 广播 TrackChanged
    async fn flush_pending_play(&self, pending: Option<usize>) -> anyhow::Result<()> {
        let Some(index) = pending else {
            return Ok(());
        };
        let song_id = {
            let queue = self.inner.queue.lock().expect("队列锁中毒");
            queue.play_list().get(index).cloned()
        };
        let Some(song_id) = song_id else {
            return Ok(());
        };
        let song = db::library::get_song(&self.inner.db, &song_id)
            .await?
            .with_context(|| format!("曲目不存在: {song_id}"))?;
        let handle = self.inner.handle.clone();
        let _ = handle
            .send_anonymous(AudioThreadMessage::PlayAudio {
                song: SongData {
                    file_path: song.file_path.clone(),
                    song_id: Some(song.id.clone()),
                },
            })
            .await;
        let _ = self.inner.bus.send(ServiceEvent::TrackChanged {
            song_id: song.id,
            file_path: song.file_path,
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 服务装配烟囱:临时目录开库 → 建列表 → 查询。
    /// 音频设备在无声卡环境可能初始化失败,该情况仅记录跳过——
    /// 播放链路依赖实机验收
    #[tokio::test(flavor = "multi_thread")]
    async fn service_opens_db_and_cruds_playlist() {
        let dir = tempfile::tempdir().expect("临时目录");
        // open 内部自带 block_on:异步上下文中经阻塞线程构造
        let opened = tokio::task::spawn_blocking(move || PlayerService::open(dir.path()))
            .await
            .expect("join");
        let service = match opened {
            Ok(s) => s,
            Err(e) => {
                eprintln!("音频设备不可用,跳过服务测试: {e}");
                return;
            }
        };
        let playlist_id = service.create_playlist("测试列表").await.expect("建列表");
        let lists = service.get_all_playlists().await.expect("查询列表");
        assert_eq!(lists.len(), 1);
        assert_eq!(lists[0].playlist.name, "测试列表");
        let _ = playlist_id;
        // 服务自持 runtime:其 drop 需阻塞上下文,显式在阻塞线程析构
        tokio::task::spawn_blocking(move || drop(service))
            .await
            .expect("join");
    }
}
