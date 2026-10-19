//! GPUI 歌词页原型(第一阶段)。
//!
//! 用法: gpui-player [旗标] <音频文件路径> <TTML 歌词路径>
//!       gpui-player --trace <out.jsonl> [--score <score.json>] [--quit-on-done] <TTML 歌词路径>
//!
//! 音频经 player-core 播放,TTML 经 ttml_processor 解析,
//! 歌词按上游 AMLL core 的布局与动画语义渲染(见 lyric_page.rs 文档)。
//! `--trace` 启用确定性虚拟时钟并逐帧记录,供与上游 DOM 版数值对比。

mod layout;
mod lyric_page;
mod spring;
mod dots;
mod ease;
mod timeline;
mod transform;
mod words;
mod trace;

use std::path::PathBuf;
use std::sync::mpsc;

use amll_player_core::{
    AudioPlayer, AudioPlayerConfig, AudioPlayerHandle, AudioThreadEvent, AudioThreadEventMessage,
    AudioThreadMessage, NowPlayingOptions, SongData,
};
use gpui_kit::{
    point, px, size, AppContext, Bounds, TitlebarOptions, WindowBounds, WindowOptions,
};

use crate::lyric_page::LyricPage;
use crate::timeline::{LyricLine, LyricWord};
use crate::trace::{TraceScore, TraceState};

fn usage() -> ! {
    eprintln!("用法: gpui-player <音频文件路径> <TTML 歌词路径>");
    eprintln!("      gpui-player --trace <out.jsonl> [--score <score.json>] [--quit-on-done] <TTML>");
    eprintln!("示例: cargo run -p gpui-player -- \"歌曲.flac\" \"歌词.ttml\"");
    std::process::exit(1);
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let args: Vec<String> = std::env::args().collect();
    let mut trace_path: Option<PathBuf> = None;
    let mut pixel_dir: Option<PathBuf> = None;
    let mut score_path: Option<PathBuf> = None;
    let mut quit_on_done = false;
    let mut paths: Vec<PathBuf> = Vec::new();
    let mut index = 1;
    while index < args.len() {
        let arg = args[index].as_str();
        match arg {
            "--trace" => {
                index += 1;
                trace_path = Some(PathBuf::from(
                    args.get(index).unwrap_or_else(|| usage()),
                ));
            }
            "--trace-pixels" => {
                index += 1;
                pixel_dir = Some(PathBuf::from(
                    args.get(index).unwrap_or_else(|| usage()),
                ));
            }
            "--score" => {
                index += 1;
                score_path = Some(PathBuf::from(
                    args.get(index).unwrap_or_else(|| usage()),
                ));
            }
            "--quit-on-done" => quit_on_done = true,
            _ if !arg.starts_with('-') && std::path::Path::new(arg).exists() => {
                paths.push(PathBuf::from(arg));
            }
            _ => {}
        }
        index += 1;
    }
    if paths.is_empty() || paths.len() > 2 {
        usage();
    }
    // 位置参数:两个 = (音频, TTML);一个 = 仅 TTML(配合 --trace 做数值采集)
    let (audio_path, ttml_path) = if paths.len() == 2 {
        (Some(paths[0].clone()), paths[1].clone())
    } else if trace_path.is_some() {
        (None, paths[0].clone())
    } else {
        usage();
    };

    let ttml_bytes = std::fs::read(&ttml_path)?;
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

    // 采集状态:--trace 开启虚拟时钟;剧本缺省时按歌词全长推导
    let max_end_ms = lines
        .iter()
        .map(|line| line.end_time_ms)
        .fold(0.0_f64, f64::max);
    let trace_state = match trace_path {
        Some(path) => {
            let score = match score_path {
                Some(score_file) => TraceScore::parse(&std::fs::read_to_string(score_file)?)?,
                None => TraceScore {
                    duration_ms: max_end_ms + 2000.0,
                    seeks: Vec::new(),
                },
            };
            Some(TraceState::new(score, path, quit_on_done)?)
        }
        None => None,
    };

    let (evt_tx, mut evt_rx) =
        tokio::sync::mpsc::unbounded_channel::<AudioThreadEventMessage<AudioThreadEvent>>();
    let (handle_tx, handle_rx) = mpsc::channel::<AudioPlayerHandle>();

    // 音频线程:独立线程 + current-thread 运行时,与 src-tauri 的接线一致
    if audio_path.is_some() {
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
                    Ok(player) => player,
                    Err(e) => {
                        tracing::error!("初始化音频设备失败: {e:?}");
                        return;
                    }
                };
                let _ = handle_tx.send(player.handler());
                player.run().await;
            });
        });
    }

    // 无音频(--trace 仅 TTML)时不启动音频线程,此处不能等待空通道
    let handle = if audio_path.is_some() {
        handle_rx.recv().ok()
    } else {
        None
    };

    // 发起播放
    if let (Some(handle), Some(audio)) = (handle.clone(), audio_path.as_ref()) {
        let audio = audio.display().to_string();
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
                            file_path: audio,
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

        let view = cx.new(|_| LyricPage::new(handle, lines, trace_state, pixel_dir));

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
                view.update(cx, |page: &mut LyricPage, cx| {
                    page.on_audio_event(event, cx);
                });
            }
        })
        .detach();
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 数值对比的输入一致性:Rust 侧(ttml_processor 解析 demo.ttml)必须与
    /// 浏览器 harness 直接消费的 demo-lines.json(生成器同源产出)完全一致。
    #[test]
    fn demo_ttml_matches_reference_lines_json() {
        let ttml = std::fs::read_to_string("testdata/demo.ttml").unwrap();
        let parsed = ttml_processor::parse_ttml(&ttml).unwrap();
        let reference: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string("testdata/demo-lines.json").unwrap())
                .unwrap();
        let ref_lines = reference.as_array().unwrap();
        assert_eq!(
            parsed.lines.len(),
            ref_lines.len(),
            "两侧行数应一致"
        );
        for (index, (line, ref_line)) in parsed.lines.iter().zip(ref_lines).enumerate() {
            assert!(
                (line.start_time as f64 - ref_line["startTime"].as_f64().unwrap()).abs() <= 1.0,
                "行 {index} startTime 不一致"
            );
            assert!(
                (line.end_time as f64 - ref_line["endTime"].as_f64().unwrap()).abs() <= 1.0,
                "行 {index} endTime 不一致"
            );
            let words = line.words.as_ref().unwrap();
            let ref_words = ref_line["words"].as_array().unwrap();
            assert_eq!(words.len(), ref_words.len(), "行 {index} 词数不一致");
            for (word, ref_word) in words.iter().zip(ref_words) {
                let text = format!(
                    "{}{}",
                    word.text,
                    if word.ends_with_space.unwrap_or(false) {
                        " "
                    } else {
                        ""
                    }
                );
                assert_eq!(
                    text,
                    ref_word["word"].as_str().unwrap(),
                    "行 {index} 词文本不一致"
                );
                assert!(
                    (word.start_time as f64 - ref_word["startTime"].as_f64().unwrap()).abs() <= 1.0,
                    "行 {index} 词 startTime 不一致"
                );
                assert!(
                    (word.end_time as f64 - ref_word["endTime"].as_f64().unwrap()).abs() <= 1.0,
                    "行 {index} 词 endTime 不一致"
                );
            }
        }
    }
}
