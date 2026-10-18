//! GPUI 歌词页原型(第一阶段 M3)。
//!
//! 用法: gpui-player <音频文件路径> <TTML 歌词路径>
//! 音频经 player-core 播放,TTML 经 ttml_processor 解析,
//! 歌词以 AMLL 手感(弹簧滚动 + stagger + 行缩放)渲染。

mod lyric_page;
mod spring;
mod timeline;

use std::sync::mpsc;

use amll_player_core::{
    AudioPlayer, AudioPlayerConfig, AudioPlayerHandle, AudioThreadEvent, AudioThreadEventMessage,
    AudioThreadMessage, NowPlayingOptions, SongData,
};
use gpui_kit::{point, px, size, AppContext, Bounds, TitlebarOptions, WindowOptions, WindowBounds};

use crate::lyric_page::LyricPage;
use crate::timeline::{LyricLine, LyricWord};

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let args: Vec<String> = std::env::args().collect();
    let paths: Vec<&String> = args
        .iter()
        .skip(1)
        .filter(|arg| !arg.starts_with('-') && std::path::Path::new(arg.as_str()).exists())
        .collect();
    if paths.len() < 2 {
        eprintln!("用法: gpui-player <音频文件路径> <TTML 歌词路径>");
        eprintln!("示例: cargo run -p gpui-player -- \"歌曲.flac\" \"歌词.ttml\"");
        eprintln!(
            "收到的参数: {:?}",
            args.iter().skip(1).collect::<Vec<_>>()
        );
        std::process::exit(1);
    }
    let audio_path = paths[0].clone();
    let ttml_bytes = std::fs::read(paths[1])?;
    // TTML 官方是 UTF-8;对非 UTF-8 的历史文件做有损兜底而非直接崩溃
    let ttml_content = String::from_utf8_lossy(&ttml_bytes).into_owned();

    // TTML → 原型行结构(词粒度,供 M4 逐词渐变)
    let parsed = ttml_processor::parse_ttml(&ttml_content)?;
    let lines: Vec<LyricLine> = parsed
        .lines
        .iter()
        .map(|line| LyricLine {
            start_time_ms: line.start_time as f64,
            end_time_ms: line.end_time as f64,
            words: line
                .words
                .as_ref()
                .map(|words| {
                    words
                        .iter()
                        .map(|word| LyricWord {
                            start_time_ms: word.start_time as f64,
                            end_time_ms: word.end_time as f64,
                            text: format!(
                                "{}{}",
                                word.text,
                                if word.ends_with_space.unwrap_or(false) {
                                    " "
                                } else {
                                    ""
                                }
                            ),
                        })
                        .collect()
                })
                .unwrap_or_else(|| {
                    vec![LyricWord {
                        start_time_ms: line.start_time as f64,
                        end_time_ms: line.end_time as f64,
                        text: line.text.clone(),
                    }]
                }),
        })
        .collect();
    tracing::info!("解析 TTML: {} 行", lines.len());

    let (evt_tx, mut evt_rx) =
        tokio::sync::mpsc::unbounded_channel::<AudioThreadEventMessage<AudioThreadEvent>>();
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

    // 发起播放
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

        let view = cx.new(|_| LyricPage::new(handle, lines));

        gpui_kit::open_window(
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

        // 事件桥:tokio unbounded 的 recv 可在任意执行器上 await
        cx.spawn(async move |cx| {
            while let Some(evt) = evt_rx.recv().await {
                let Some(event) = evt.data else { continue };
                let _ = view.update(cx, |page: &mut LyricPage, cx| {
                    page.on_audio_event(event, cx);
                });
            }
        })
        .detach();
    });

    Ok(())
}
