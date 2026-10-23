//! 服务事件总线:音频线程事件 → 订阅者(tokio broadcast)。
//!
//! Tauri 侧把事件桥到 emit;GPUI 侧订阅后投递到 UI 线程。

use amll_player_core::{AudioThreadEvent, AudioThreadEventMessage};

/// 订阅者拿到的事件包装
#[derive(Debug, Clone)]
pub enum ServiceEvent {
    /// 音频线程事件透传(PlayPosition/PlayStatus/FFTData/VolumeChanged/…)
    Audio(AudioThreadEvent),
    /// 队列当前曲目切换(播放了新曲目)
    TrackChanged { song_id: String, file_path: String },
    /// 曲目自然播完(队列已决策出下一步,或已按 Off 停止)
    TrackEnded,
}

pub type ServiceEventSender = tokio::sync::broadcast::Sender<ServiceEvent>;

/// 把音频线程的发射通道适配为事件总线:消费 AudioPlayer 的
/// evt_sender 队列并转发。返回订阅句柄。
pub fn spawn_audio_bridge(
    runtime: &tokio::runtime::Runtime,
    mut evt_rx: tokio::sync::mpsc::UnboundedReceiver<AudioThreadEventMessage<AudioThreadEvent>>,
    bus: ServiceEventSender,
) -> tokio::task::JoinHandle<()> {
    runtime.spawn(async move {
        while let Some(msg) = evt_rx.recv().await {
            if let Some(event) = msg.data {
                let _ = bus.send(ServiceEvent::Audio(event));
            }
        }
    })
}
