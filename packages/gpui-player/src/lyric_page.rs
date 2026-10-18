//! M3 歌词页:TTML → 行布局测量 → div 绝对定位渲染 + 弹簧滚动(纯色高亮)。
//!
//! 布局与滚动的数学移植自上游 `lyric-player/base/layout.ts` 与
//! `group.ts`:焦点行中心对齐到视口 35% 处,每行一个 posY 弹簧
//! (stagger 延迟波浪)与一个 scale 弹簧(100/97)。
//! 渲染用 GPUI 的 div 绝对定位,text_system 做行测量。

use std::time::Instant;

use amll_player_core::{AudioPlayerHandle, AudioThreadEvent, AudioThreadMessage};
use gpui_kit::{
    Context, div, px, rgba, FontWeight, Hsla, InteractiveElement, IntoElement, MouseButton,
    ParentElement, Pixels, Render, SharedString, Size, Styled, TextRun, Window,
};

use crate::spring::{pos_y_policy, Spring};
use crate::timeline::{LyricLine, SeekDetector, Timeline};

/// 帧推进上限(上游由播放器钳制,Spring 本身不钳)
const MAX_FRAME_DELTA_SEC: f64 = 0.1;

/// 主歌词字号(逻辑像素,后续接设置)
const FONT_SIZE: f64 = 38.0;
/// 行内垂直留白(字号的倍数)
const LINE_PADDING_RATIO: f64 = 0.55;
/// 焦点行中心对齐位置(上游 alignPosition 默认 0.35)
const ALIGN_POSITION: f64 = 0.35;
/// 视口外的行缓冲(上游 motionBuffer 0.4H + overscanPx 300)
const VIEWPORT_BUFFER_RATIO: f64 = 0.4;
const VIEWPORT_BUFFER_PX: f64 = 300.0;
/// stagger:每行延迟基数 50ms,焦点行后按 ×1/1.05 递减(上游 baseDelay)
const STAGGER_BASE_DELAY_SEC: f64 = 0.05;
/// 未高亮行文字 alpha(上游 mask dark≈0.2 的视觉近似)
const DIM_ALPHA: f64 = 0.3;
const HIGHLIGHT_ALPHA: f64 = 0.95;

struct LineMeasurement {
    height: f64,
}

pub struct LyricPage {
    handle: AudioPlayerHandle,
    timeline: Timeline,
    detector: SeekDetector,

    /// 播放位置(本地外推,毫秒)
    position_ms: f64,
    event_position_ms: f64,
    event_wall: Instant,
    playing: bool,

    container_size: Option<Size<Pixels>>,
    measurements: Vec<LineMeasurement>,
    measured_font_size: f64,
    content_height: f64,

    pos_y: Vec<Spring>,
    scale: Vec<Spring>,
    last_scroll_to: Option<usize>,
    layout_initialized: bool,
    first_layout: bool,
    wall_origin: Instant,

    last_frame: Option<Instant>,
}

impl LyricPage {
    pub fn new(handle: AudioPlayerHandle, lines: Vec<LyricLine>) -> Self {
        let count = lines.len();
        Self {
            handle,
            timeline: Timeline::new(lines),
            detector: SeekDetector::default(),
            position_ms: 0.0,
            event_position_ms: 0.0,
            event_wall: Instant::now(),
            playing: false,
            container_size: None,
            measurements: Vec::new(),
            measured_font_size: 0.0,
            content_height: 0.0,
            pos_y: (0..count).map(|_| Spring::new(0.0)).collect(),
            scale: (0..count).map(|_| Spring::new(100.0)).collect(),
            last_scroll_to: None,
            layout_initialized: false,
            first_layout: true,
            wall_origin: Instant::now(),
            last_frame: None,
        }
    }

    pub fn on_audio_event(&mut self, event: AudioThreadEvent, cx: &mut Context<Self>) {
        match event {
            AudioThreadEvent::PlayPosition { position } => {
                self.event_position_ms = position * 1000.0;
                self.event_wall = Instant::now();
            }
            AudioThreadEvent::PlayStatus { is_playing } => {
                self.playing = is_playing;
            }
            AudioThreadEvent::AudioPlayFinished { .. } | AudioThreadEvent::TrackEnded => {
                self.playing = false;
            }
            AudioThreadEvent::LoadError { error }
            | AudioThreadEvent::PlayError { error } => {
                tracing::error!("音频错误: {error}");
            }
            _ => {}
        }
        cx.notify();
    }

    fn send_seek(&self, position_sec: f64) {
        let handle = self.handle.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("创建 Tokio 运行时失败");
            runtime.block_on(async move {
                if let Err(e) = handle
                    .send_anonymous(AudioThreadMessage::SeekAudio {
                        position: position_sec,
                    })
                    .await
                {
                    tracing::warn!("seek 失败: {e:?}");
                }
            });
        });
    }

    /// 用文本系统实测各行的(换行)高度并建立前缀和。
    /// M3 行不做软换行(单行),行高 = (ascent+descent) + 留白;
    /// 多行长行在 M4 引入换行布局。
    fn measure_lines(&mut self, window: &Window) {
        let font_size = FONT_SIZE;
        if self.measured_font_size == font_size && !self.measurements.is_empty() {
            return;
        }
        let mut prefix = 0.0;
        let mut measurements = Vec::with_capacity(self.timeline.lines().len());
        for line in self.timeline.lines() {
            let text: SharedString = line.full_line_text().into();
            let height = if text.trim().is_empty() {
                font_size * (1.0 + LINE_PADDING_RATIO)
            } else {
                let runs = [TextRun {
                    len: text.len(),
                    font: lyric_font(),
                    color: Hsla::default(),
                    ..Default::default()
                }];
                let shaped = window
                    .text_system()
                    .shape_line(text, px(font_size as f32), &runs, None);
                // ShapedLine 经 Deref 暴露 LineLayout 字段;Pixels/Pixels -> f32
                let ascent = shaped.ascent / px(1.0);
                let descent = shaped.descent / px(1.0);
                (ascent + descent) as f64 + font_size * LINE_PADDING_RATIO
            };
            measurements.push(LineMeasurement { height: prefix + height });
            prefix += height;
        }
        self.measurements = measurements;
        self.content_height = prefix;
        self.measured_font_size = font_size;
    }

    /// 前缀和缓存的二次包装:第 index 行的顶部 Y
    fn prefix_top(&self, index: usize) -> f64 {
        match index {
            0 => 0.0,
            _ => self.measurements[index - 1].height,
        }
    }

    fn line_height(&self, index: usize) -> f64 {
        let top = self.prefix_top(index);
        let bottom = self.measurements[index].height;
        bottom - top
    }

    /// 焦点行(scrollToIndex)变化或 seek 时重定全部滚动弹簧目标。
    /// 刚度按行间隔动态计算(上游 getPosYSpringPolicy),stagger 波浪
    /// 从焦点行向两侧扩散。间奏/前奏期锚定在第一行(上游 interlude 语义)。
    fn retarget_springs(&mut self, is_seek: bool) {
        // 间奏/前奏无焦点时锚定第一行,避免初始所有行堆叠在 y=0
        let scroll_to = self.timeline.scroll_to_index().unwrap_or(0);
        let changed = self.last_scroll_to != Some(scroll_to);
        if !changed && !is_seek && self.layout_initialized {
            return;
        }
        let prev_scroll_to = self.last_scroll_to;
        self.last_scroll_to = Some(scroll_to);

        let lines = self.timeline.lines();
        let params = if is_seek || self.timeline.in_interlude() {
            pos_y_policy::slow()
        } else if let Some(prev) = prev_scroll_to
            && prev != scroll_to
            && lines[scroll_to].start_time_ms >= lines[prev].start_time_ms
        {
            let interval = lines[scroll_to].start_time_ms - lines[prev].start_time_ms;
            pos_y_policy::by_interval_ms(interval)
        } else {
            pos_y_policy::slow()
        };
        let patch = pos_y_policy::patch(params);
        for spring in &mut self.pos_y {
            spring.update_params(patch, 0.0);
        }

        let Some(size) = self.container_size else {
            return;
        };
        let container_height = (size.height / px(1.0)) as f64;
        let focal_top = self.prefix_top(scroll_to);
        let focal_height = self.line_height(scroll_to);
        // 上游 commit():viewportStartY = H·align − anchorOffset − scroll − focalTop
        // (scroll 为用户滚动偏移,M3 恒 0;anchorOffset = 焦点行高/2)
        let viewport_start = container_height * ALIGN_POSITION - focal_height / 2.0 - focal_top;

        // stagger:焦点行 0 延迟,向两侧各 50ms 递增,焦点行之后基数 ×1/1.05
        let mut base_delay = STAGGER_BASE_DELAY_SEC;
        let mut delay_up = 0.0;
        let mut delay_down = 0.0;
        let line_count = self.timeline.lines().len();
        let line_tops: Vec<f64> = (0..line_count)
            .map(|index| self.prefix_top(index))
            .collect();
        for index in 0..line_count {
            let delay = if index < scroll_to {
                delay_up += base_delay;
                delay_up
            } else if index > scroll_to {
                delay_down += base_delay;
                base_delay /= 1.05;
                delay_down
            } else {
                0.0
            };
            let target_y = viewport_start + line_tops[index];
            if self.first_layout {
                // 首次布局直接钉位,不做从 0 散开的入场动画
                self.pos_y[index].set_position(target_y);
            } else {
                self.pos_y[index].set_target_position(target_y, delay);
            }
        }

        // scale 目标:高亮行 100,其余 97
        let highlighted: &[usize] = self.timeline.highlighted();
        for (index, spring) in self.scale.iter_mut().enumerate() {
            let target = if highlighted.contains(&index) { 100.0 } else { 97.0 };
            spring.set_target_position(target, 0.0);
        }
        self.layout_initialized = true;
        self.first_layout = false;
    }

    fn animating(&self) -> bool {
        self.playing
            || self.pos_y.iter().any(|s| !s.arrived())
            || self.scale.iter().any(|s| !s.arrived())
    }
}

fn lyric_font() -> gpui_kit::Font {
    gpui_kit::Font {
        family: SharedString::from(".SystemUIFont"),
        weight: FontWeight::from(600.0),
        ..Default::default()
    }
}

impl Render for LyricPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = Instant::now();
        let delta = self
            .last_frame
            .map(|last| now.duration_since(last).as_secs_f64())
            .unwrap_or(0.0)
            .clamp(0.0, MAX_FRAME_DELTA_SEC);
        self.last_frame = Some(now);

        if self.playing {
            self.position_ms =
                self.event_position_ms + now.duration_since(self.event_wall).as_millis() as f64;
        } else {
            self.position_ms = self.event_position_ms;
        }

        // 墙钟:取距进程内固定起点的流逝时间(Instant::now().elapsed() 恒为 0,是 bug)
        let wall_ms = self.wall_origin.elapsed().as_millis() as f64;
        let is_seek = self.detector.detect(self.position_ms, wall_ms, self.playing);
        self.timeline.sync(self.position_ms, is_seek);

        // 布局测量需要 window(text_system);容器尺寸取窗口尺寸(M3 全屏歌词页)
        self.container_size = Some(window.bounds().size);
        self.measure_lines(window);
        self.retarget_springs(is_seek);

        for spring in &mut self.pos_y {
            spring.update(delta);
        }
        for spring in &mut self.scale {
            spring.update(delta);
        }

        if self.animating() {
            window.request_animation_frame();
        }

        // 渲染可见行(div 绝对定位)
        let container_width = self
            .container_size
            .map(|size| (size.width / px(1.0)) as f64)
            .unwrap_or(0.0);
        let container_height = self
            .container_size
            .map(|size| (size.height / px(1.0)) as f64)
            .unwrap_or(0.0);

        let mut children = Vec::new();
        let highlighted: Vec<usize> = self.timeline.highlighted().to_vec();
        let line_hit: Vec<(usize, f64, f64)> = self
            .timeline
            .lines()
            .iter()
            .enumerate()
            .map(|(index, line)| {
                let y = self.pos_y[index].current_position();
                (index, y, line.start_time_ms)
            })
            .filter(|(index, y, _)| {
                let height = self.line_height(*index);
                *y + height >= -(container_height * VIEWPORT_BUFFER_RATIO + VIEWPORT_BUFFER_PX)
                    && *y <= container_height + container_height * VIEWPORT_BUFFER_RATIO
                        + VIEWPORT_BUFFER_PX
            })
            .collect();

        for (index, y, _) in &line_hit {
            let index = *index;
            let y = *y;
            let line = &self.timeline.lines()[index];
            let scale = self.scale[index].current_position();
            // 字号离散到 100%/97% 两档:连续的分数字号会让文本每帧重新
            // 排版,宽度持续微变,表现为肉眼可见的"呼吸/微抖";
            // 上游 DOM 用 transform:scale() 不触发重排版,GPUI 的 div
            // 没有 transform,故只在弹簧跨越阈值时切档
            let effective_font = FONT_SIZE * if scale > 98.5 { 1.0 } else { 0.97 };
            let is_highlighted = highlighted.contains(&index);
            let text: String = line.full_line_text();
            if text.trim().is_empty() {
                continue;
            }
            let alpha = if is_highlighted {
                HIGHLIGHT_ALPHA
            } else {
                DIM_ALPHA
            };
            // 行绘制:测量宽度以居中
            let shared: SharedString = text.clone().into();
            let runs = [TextRun {
                len: shared.len(),
                font: lyric_font(),
                color: Hsla {
                    h: 0.0,
                    s: 0.0,
                    l: 1.0,
                    a: alpha as f32,
                },
                ..Default::default()
            }];
            let shaped = window
                .text_system()
                .shape_line(shared, px(effective_font as f32), &runs, None);
            let width = (shaped.width() / px(1.0)) as f64;
            let origin_x = (container_width - width) / 2.0;
            let height = self.line_height(index);
            let text_padding_top = (height - effective_font * 1.2) / 2.0;
            children.push(
                div()
                    .id(("lyric-line", index))
                    .absolute()
                    .left(px(origin_x as f32))
                    .top(px(y as f32 + text_padding_top as f32))
                    .text_size(px(effective_font as f32))
                    .line_height(px(effective_font as f32 * 1.2))
                    .font_weight(FontWeight::from(600.0))
                    .text_color(Hsla {
                        h: 0.0,
                        s: 0.0,
                        l: 1.0,
                        a: alpha as f32,
                    })
                    .on_mouse_down(MouseButton::Left, cx.listener({
                        let start_ms = line.start_time_ms;
                        move |this: &mut LyricPage, _event, _window, _cx| {
                            this.send_seek(start_ms / 1000.0);
                        }
                    }))
                    .child(text.clone()),
            );
        }

        div()
            .id("lyric-page")
            .relative()
            .size_full()
            .bg(rgba(0x0a0a0cff))
            .children(children)
    }
}
