//! GPUI 歌词页原型(第一阶段)。
//!
//! 用法: gpui-player [旗标] <音频文件路径> <TTML 歌词路径>
//!       gpui-player --trace <out.jsonl> [--score <score.json>] [--quit-on-done] <TTML 歌词路径>
//!
//! 音频经 player-core 播放,TTML 经 ttml_processor 解析,
//! 歌词按上游 AMLL core 的布局与动画语义渲染(见 lyric_page.rs 文档)。
//! `--trace` 启用确定性虚拟时钟并逐帧记录,供与上游 DOM 版数值对比。

mod layout;
mod line_break;
mod lyric_page;
mod spring;
mod dots;
mod ease;
mod mesh_bg;
mod timeline;
mod transform;
mod words;
mod scroll;
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
    eprintln!("      gpui-player --library <音乐文件夹>(扫描入库并按队列播放)");
    eprintln!("      gpui-player --trace <out.jsonl> [--score <score.json>] [--quit-on-done] <TTML>");
    eprintln!("示例: cargo run -p gpui-player -- \"歌曲.flac\" \"歌词.ttml\"");
    eprintln!("      可选: --cover 封面.png|jpg(指定 mesh 背景取色图)");
    std::process::exit(1);
}

/// 解析器行 → 原型行结构(词粒度,供 M4 逐词渐变)。
/// 翻译/音译取首条非空文本(上游 toAmllLyrics 无语言偏好时的回退语义)
fn map_lyric_lines(parsed: &[ttml_processor::model::LyricLine]) -> Vec<LyricLine> {
    parsed
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
                            // [ruby] 注音(RubyTag 拼接)
                            ruby: word.ruby.as_ref().map(|tags| {
                                tags.iter()
                                    .map(|t| t.text.as_str())
                                    .collect::<Vec<_>>()
                                    .join("")
                            }),
                        })
                        .collect()
                })
                .unwrap_or_else(|| {
                    vec![LyricWord {
                        start_time_ms: line.start_time as f64,
                        end_time_ms: line.end_time as f64,
                        text: line.text.clone(),
                        ruby: None,
                    }]
                }),
            translation: line
                .translations
                .as_ref()
                .and_then(|t| t.first())
                .map(|t| t.text.clone())
                .filter(|t| !t.trim().is_empty()),
            romanization: line
                .romanizations
                .as_ref()
                .and_then(|r| r.first())
                .map(|r| r.text.clone())
                .filter(|t| !t.trim().is_empty()),
            // [roman] 逐词音译音节(首条 romanizations 的 words;
            // 行级文本之外还有逐词数据时,按 IoU 对齐挂词)
            roman_words: line
                .romanizations
                .as_ref()
                .and_then(|r| r.first())
                .and_then(|r| r.words.as_ref())
                .filter(|words| !words.is_empty())
                .map(|words| {
                    words
                        .iter()
                        .map(|w| LyricWord {
                            start_time_ms: w.start_time as f64,
                            end_time_ms: w.end_time as f64,
                            text: w.text.clone(),
                            ruby: None,
                        })
                        .collect()
                }),
            // [BG] 背景人声(解析器 background_vocal,行内 x-bg span)
            bg_vocal: line
                .background_vocal
                .as_ref()
                .map(|bg| bg.text.clone())
                .filter(|t| !t.trim().is_empty()),
        })
        .collect()
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
    let mut cover_path: Option<PathBuf> = None;
    let mut library_dir: Option<PathBuf> = None;
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
            "--cover" => {
                index += 1;
                cover_path = Some(PathBuf::from(
                    args.get(index).unwrap_or_else(|| usage()),
                ));
            }
            // [Phase 3] 实库模式:扫描文件夹入库,经 PlayerService
            // 按队列播放(曲目顺序即文件枚举序)
            "--library" => {
                index += 1;
                library_dir = Some(PathBuf::from(
                    args.get(index).unwrap_or_else(|| usage()),
                ));
            }
            _ if !arg.starts_with('-') && std::path::Path::new(arg).exists() => {
                paths.push(PathBuf::from(arg));
            }
            _ => {}
        }
        index += 1;
    }
    // [Phase 3] 实库模式:经 PlayerService 扫描入库并按队列播放。
    // 当前歌曲即队列首曲——音频路径取 DB 的 file_path,歌词取扫描器
    // 存入的 TTML 内容(同名 .ttml 伴生文件);曲库数据与播放链路
    // 全部走服务层,Tauri 前端走的是同一份代码。
    if let Some(library) = &library_dir {
        let data_dir = std::env::temp_dir().join("amll-gpui-service");
        let service = player_service::PlayerService::open(&data_dir)?;
        let song_ids = service.scan_folder(library)?;
        if song_ids.is_empty() {
            anyhow::bail!("文件夹中未找到音频: {}", library.display());
        }
        tracing::info!("[library] 入库 {} 曲", song_ids.len());
        // 播放队列(异步操作在服务自带的 runtime 上驱动)
        let service_task = service.clone();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("创建运行时失败");
            rt.block_on(async move {
                if let Err(e) = service_task.play_queue(song_ids, None).await {
                    tracing::error!("[library] 启动队列失败: {e:?}");
                }
            });
        });
        // 等队列首曲开播,读取当前曲目构建歌词页
        let first_id = loop {
            if let Some(id) = service.current_song_id() {
                break id;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        };
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let first = rt
            .block_on(service.get_song(&first_id))?
            .ok_or_else(|| anyhow::anyhow!("首曲查询失败"))?;
        tracing::info!(
            "[library] 当前曲目: {} - {}",
            first.song_artists,
            first.song_name
        );
        // 事件桥:服务的音频事件 → 现有事件总线形态
        let (evt_tx, evt_rx) =
            tokio::sync::mpsc::unbounded_channel::<amll_player_core::AudioThreadEventMessage<
                amll_player_core::AudioThreadEvent,
            >>();
        let service_bus = service.clone();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("创建运行时失败");
            rt.block_on(async move {
                let mut bus = service_bus.subscribe();
                while let Ok(event) = bus.recv().await {
                    match event {
                        player_service::events::ServiceEvent::Audio(audio) => {
                            let _ = evt_tx.send(
                                amll_player_core::AudioThreadEventMessage::new(
                                    "".into(),
                                    Some(audio),
                                ),
                            );
                        }
                        player_service::events::ServiceEvent::TrackChanged { song_id, .. } => {
                            // 曲目切换:Phase 4 外壳接管歌词页重建
                            tracing::info!("[library] 切曲: {song_id}");
                        }
                        player_service::events::ServiceEvent::TrackEnded => {
                            tracing::info!("[library] 曲目播完");
                        }
                    }
                }
            });
        });
        // 歌词:扫描器存入的 TTML 内容(无则空歌词页)
        let ttml_content = first.lyric.clone();
        let audio_path_lib = first.file_path.clone();
        return run_app(Some(audio_path_lib), ttml_content, cover_path, trace_path, pixel_dir, score_path, quit_on_done, Some(service.audio_handle()), Some(evt_rx), true);
    }

    // 直连/采集模式:位置参数 = (音频, TTML) 或仅 TTML(配合 --trace)
    if paths.is_empty() || paths.len() > 2 {
        usage();
    }
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
    run_app(audio_path.map(|p| p.display().to_string()), ttml_content, cover_path, trace_path, pixel_dir, score_path, quit_on_done, None, None, false)
}

#[allow(clippy::too_many_arguments)]
fn run_app(
    audio_path: Option<String>,
    ttml_content: String,
    cover_path: Option<PathBuf>,
    trace_path: Option<PathBuf>,
    pixel_dir: Option<PathBuf>,
    score_path: Option<PathBuf>,
    quit_on_done: bool,
    // 已就绪的音频句柄(库模式由 PlayerService 提供)
    existing_handle: Option<AudioPlayerHandle>,
    // 已有的音频事件流(库模式由服务事件桥提供)
    existing_evt_rx: Option<tokio::sync::mpsc::UnboundedReceiver<AudioThreadEventMessage<AudioThreadEvent>>>,
    // 库模式标志(来自 PlayerService:既有句柄+事件流)
    from_service: bool,
) -> anyhow::Result<()> {

    // TTML → 原型行结构(词粒度,供 M4 逐词渐变)
    let parsed = ttml_processor::parse_ttml(&ttml_content)?;
    let lines: Vec<LyricLine> = map_lyric_lines(&parsed.lines);
    tracing::info!("解析 TTML: {} 行", lines.len());

    // mesh 渐变背景:封面 = --cover 指定图(png/jpg) > FLAC 内嵌 PICTURE
    let mesh = (|| -> Option<crate::mesh_bg::MeshBackgroundResource> {
        let cover_bytes = if let Some(path) = &cover_path {
            std::fs::read(path)
                .map_err(|e| tracing::warn!("读取封面失败 {path:?}: {e}"))
                .ok()?
        } else if let Some(audio) = &audio_path {
            let bytes = std::fs::read(audio)
                .map_err(|e| tracing::warn!("读取音频失败 {audio:?}: {e}"))
                .ok()?;
            let cover = crate::mesh_bg::extract_flac_cover(&bytes);
            if cover.is_none() {
                tracing::info!("音频无内嵌封面,跳过 mesh 背景(可用 --cover 指定)");
            }
            cover?
        } else {
            return None;
        };
        // id 取内容哈希:同专辑跨启动稳定(控制点摇号一致)
        let id = cover_bytes
            .iter()
            .fold(0xC0FFEE_u64, |acc, byte| acc.rotate_left(8) ^ *byte as u64);
        crate::mesh_bg::build_resource(&cover_bytes, id)
    })();

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

    // 音频接线三分支:
    // [库模式] PlayerService 已在播放:直接用它的句柄与事件桥,
    //   不再自建音频线程、不重复发起播放
    // [直连模式] 自建音频线程(独立线程 + current-thread 运行时,
    //   与 src-tauri 的接线一致)并发起播放
    // [采集模式] 无音频,不启动线程
    let (handle, mut evt_rx): (
        Option<AudioPlayerHandle>,
        tokio::sync::mpsc::UnboundedReceiver<AudioThreadEventMessage<AudioThreadEvent>>,
    ) = match (existing_handle, existing_evt_rx) {
        (Some(handle), Some(rx)) => (Some(handle), rx),
        (fallback_handle, fallback_rx) => {
            let (evt_tx, evt_rx) =
                tokio::sync::mpsc::unbounded_channel::<AudioThreadEventMessage<AudioThreadEvent>>();
            let (handle_tx, handle_rx) = mpsc::channel::<AudioPlayerHandle>();
            let has_audio = audio_path.is_some();
            if has_audio {
                let evt_tx = evt_tx.clone();
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
            let handle = if has_audio {
                handle_rx.recv().ok()
            } else {
                None
            };
            if let Some(rx) = fallback_rx {
                // 既有事件流但无句柄(理论不可达):桥到内部通道
                std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .expect("创建运行时失败");
                    rt.block_on(async move {
                        let mut rx = rx;
                        while let Some(msg) = rx.recv().await {
                            let _ = evt_tx.send(msg);
                        }
                    });
                });
            }
            let _ = fallback_handle;
            (handle, evt_rx)
        }
    };

    // 发起播放(直连模式;库模式由服务队列驱动,跳过)
    let start_play_path = if from_service { None } else { audio_path.clone() };
    if let (Some(handle), Some(audio)) = (handle.clone(), start_play_path.as_ref()) {
        let audio = audio.to_string();
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

        // [AMLL] Root(gpui-component RootPlugin)会给根表面刷一层不透明的
        // 主题背景色(Light=纯白),它位于歌词页之下、mesh 之上,会把场景级
        // mesh 渐变背景整个盖住(画面全白的根因)。与上游一致——web 版的
        // body 是透明的,mesh 是页面自己的最底层——这里把主题背景 token
        // 置为全透明:无封面时由歌词页画自己的暗色兜底,有封面时 mesh 直接可见。
        gpui_kit::component::Theme::global_mut(cx).tokens.background =
            gpui::transparent_black().into();

        let view = cx.new(|_| LyricPage::new(handle, lines, trace_state, pixel_dir, mesh));

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

    /// x-bg 容器 span(带嵌套逐词 span)→ background_vocal
    #[test]
    fn bg_fixture_parses_xbg_container() {
        let ttml = std::fs::read_to_string("testdata/wrap-bg.ttml").unwrap();
        let parsed = ttml_processor::parse_ttml(&ttml).unwrap();
        assert_eq!(parsed.lines.len(), 5, "行数");
        let bg = parsed.lines[4]
            .background_vocal
            .as_ref()
            .expect("L5 应有 background_vocal");
        assert!(bg.text.contains("背景人声第一句"), "文本: {}", bg.text);
        assert!(bg.text.contains("背景人声第二句"), "文本: {}", bg.text);
        let lines = map_lyric_lines(&parsed.lines);
        assert_eq!(
            lines[4].bg_vocal.as_deref(),
            Some("背景人声第一句) (背景人声第二句")
        );
    }

    /// inline role 格式(ttm:role="x-translation"/"x-roman",AMLL 库
    /// TTML 常见形态)→ 翻译/音译进子行字段、不混入主行 words;
    /// 无 role 的行两个字段为 None
    #[test]
    fn inline_role_ttml_maps_translation_romanization() {
        let ttml = r#"<tt xmlns="http://www.w3.org/ns/ttml" xmlns:ttm="http://www.w3.org/ns/ttml#metadata"><body dur="00:10.000"><div><p begin="00:00.100" end="00:04.000" itunes:key="L1" xmlns:itunes="http://music.apple.com/lyric-ttml-internal"><span begin="00:00.100" end="00:01.000">海</span><span begin="00:01.000" end="00:04.000">边</span><span ttm:role="x-translation" xml:lang="zh-CN">海边测试译文</span><span ttm:role="x-roman">hái-pinn</span></p><p begin="00:05.000" end="00:09.000" itunes:key="L2"><span begin="00:05.000" end="00:09.000">纯词行</span></p></div></body></tt>"#;
        let parsed = ttml_processor::parse_ttml(ttml).unwrap();
        let lines = map_lyric_lines(&parsed.lines);

        assert_eq!(lines.len(), 2);
        // 主行 words 不含翻译/音译(解析器对 role span 提前返回)
        assert_eq!(lines[0].full_line_text(), "海边");
        assert_eq!(lines[0].translation.as_deref(), Some("海边测试译文"));
        assert_eq!(lines[0].romanization.as_deref(), Some("hái-pinn"));
        assert_eq!(lines[0].sub_line_count(), 2.0);
        // 无 role 行:字段 None、子行数 0
        assert_eq!(lines[1].full_line_text(), "纯词行");
        assert_eq!(lines[1].translation, None);
        assert_eq!(lines[1].romanization, None);
        assert_eq!(lines[1].sub_line_count(), 0.0);
    }

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

#[cfg(test)]
mod quad_size_check {
    /// 补丁不变量:Quad 追加变换矩阵后为 184 字节——精灵/quad 结构体经
    /// 原始拷贝进入 StructuredBuffer,Rust 与 HLSL 两侧布局必须逐字节
    /// 一致(HLSL 侧 float2x2@160 + float2@176 = 184),此值变更须同步
    /// shaders.hlsl 的 Quad 结构
    #[test]
    fn quad_struct_size() {
        assert_eq!(std::mem::size_of::<gpui::Quad>(), 184);
        assert_eq!(std::mem::size_of::<gpui::TransformationMatrix>(), 24);
        assert_eq!(std::mem::size_of::<gpui::MonochromeSprite>(), 128);
    }
}
