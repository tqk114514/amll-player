//! M1 工具链冒烟:GPUI 窗口 + 真实音频播放接线。
//!
//! 用法: gpui-player <音频文件路径>
//! 验证:Windows D3D11 工具链、CJK 文本渲染、player-core 播放链路、事件桥。

mod spring;
mod timeline;

use std::sync::mpsc;

use amll_player_core::{
    AudioPlayer, AudioPlayerConfig, AudioPlayerHandle, AudioThreadEvent, AudioThreadEventMessage,
    AudioThreadMessage, NowPlayingOptions, SongData,
};
use gpui_kit::{
    div, open_window, point, rgba, size, AppContext, Bounds, Context, IntoElement, ParentElement,
    Render, Styled, TitlebarOptions, Window, WindowOptions, WindowBounds, px,
};
use gpui_kit::component::button::{Button, ButtonVariants};

struct PlayerView {
    handle: AudioPlayerHandle,
    title: String,
    position: f64,
    playing: bool,
    volume: f64,
}

impl PlayerView {
    fn format_position(&self) -> String {
        let total = self.position.max(0.0) as u64;
        format!("{:02}:{:02}", total / 60, total % 60)
    }

    fn send_message(&self, cx: &mut Context<Self>, msg: AudioThreadMessage) {
        let handle = self.handle.clone();
        cx.spawn(async move |_, _| {
            if let Err(e) = handle.send_anonymous(msg).await {
                tracing::warn!("发送播放控制消息失败: {e:?}");
            }
        })
        .detach();
    }
}

impl Render for PlayerView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_3()
            .size_full()
            .items_center()
            .justify_center()
            .bg(rgba(0x14141aff))
            .child(
                div()
                    .text_size(px(22.0))
                    .text_color(rgba(0xffffffff))
                    .child(format!("♪ {}", self.title)),
            )
            .child(
                div()
                    .text_size(px(14.0))
                    .text_color(rgba(0xa0a0a8ff))
                    .child(format!(
                        "{} / {}",
                        self.format_position(),
                        if self.playing { "播放中" } else { "已暂停" }
                    )),
            )
            .child(
                div().flex().gap_2().child(
                    Button::new("play-pause")
                        .primary()
                        .label(if self.playing { "暂停" } else { "播放" })
                        .on_click(cx.listener(
                            |this: &mut PlayerView, _event, _window, cx| {
                                this.send_message(cx, AudioThreadMessage::ResumeOrPauseAudio);
                            },
                        )),
                ),
            )
    }
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("用法: gpui-player <音频文件路径>");
        std::process::exit(1);
    }
    let audio_path = args[1].clone();
    let title = std::path::Path::new(&audio_path)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "未知曲目".into());

    let (evt_tx, mut evt_rx) = tokio::sync::mpsc::unbounded_channel::<
        AudioThreadEventMessage<AudioThreadEvent>,
    >();
    let (handle_tx, handle_rx) = mpsc::channel::<AudioPlayerHandle>();

    // 音频线程:独立线程 + current-thread 运行时,与 src-tauri 的接线一致
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("创建 Tokio 运行时失败");
        runtime.block_on(async move {
            let config = AudioPlayerConfig {
                media_controls_options: NowPlayingOptions {
                    hwnd: None,
                    discord: None,
                    app_name: Some("AMLL GPUI 原型".into()),
                },
            };
            let player = match AudioPlayer::new(config, evt_tx) {
                Ok(p) => p,
                Err(e) => {
                    tracing::error!("初始化音频设备失败: {e:?}");
                    return;
                }
            };
            let _ = handle_tx.send(player.handler());
            player.run().await;
        });
    });

    let handle = handle_rx
        .recv()
        .map_err(|_| anyhow::anyhow!("音频线程初始化失败"))?;

    // 发起播放:send_anonymous 是 async,借一个一次性小运行时执行
    {
        let handle = handle.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("创建 Tokio 运行时失败");
            runtime.block_on(async move {
                if let Err(e) = handle
                    .send_anonymous(AudioThreadMessage::PlayAudio {
                        song: SongData {
                            song_id: Some("gpui-proto".into()),
                            file_path: audio_path,
                        },
                    })
                    .await
                {
                    tracing::error!("发起播放失败: {e:?}");
                }
            });
        });
    }

    gpui_kit::application().run(move |cx| {
        gpui_kit::init(cx);

        let view = cx.new(|_| PlayerView {
            handle,
            title,
            position: 0.0,
            playing: false,
            volume: 1.0,
        });

        open_window(
            WindowOptions {
                titlebar: Some(TitlebarOptions {
                    title: Some("AMLL GPUI 原型".into()),
                    ..Default::default()
                }),
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin: point(px(100.0), px(100.0)),
                    size: size(px(1024.0), px(700.0)),
                })),
                ..Default::default()
            },
            cx,
            |_, _cx| view.clone(),
        )
        .expect("打开窗口失败");

        // 事件桥:tokio unbounded 的 recv 可在任意执行器上 await,
        // 把音频线程事件转发进 GPUI 的视图实体
        cx.spawn(async move |cx| {
            while let Some(evt) = evt_rx.recv().await {
                let data = evt.data;
                let Some(event) = data else { continue };
                let _ = view.update(cx, |this: &mut PlayerView, cx| match event {
                    AudioThreadEvent::PlayPosition { position } => {
                        this.position = position;
                        cx.notify();
                    }
                    AudioThreadEvent::PlayStatus { is_playing } => {
                        this.playing = is_playing;
                        cx.notify();
                    }
                    AudioThreadEvent::VolumeChanged { volume } => {
                        this.volume = volume;
                        cx.notify();
                    }
                    AudioThreadEvent::AudioPlayFinished { .. }
                    | AudioThreadEvent::TrackEnded => {
                        this.playing = false;
                        cx.notify();
                    }
                    AudioThreadEvent::LoadError { error }
                    | AudioThreadEvent::PlayError { error } => {
                        tracing::error!("音频错误: {error}");
                    }
                    _ => {}
                });
            }
        })
        .detach();
    });

    Ok(())
}
