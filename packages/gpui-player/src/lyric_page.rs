//! 歌词页:上游 AMLL core(DomLyricPlayer)布局与动画语义的 GPUI 移植。
//!
//! 按 App 实际配置(react-full `PrebuiltLyricPlayer`)对齐:
//! - 焦点对齐:`alignAnchor=Top`、`alignPosition=0.25`(react-full 预置,
//!   与 core 默认的 center/0.35 不同)
//! - 行高 2.0em(wrapper 0.4em 上下 + line-height 1.2em),文字左缘 1em,左对齐
//! - 字号 = max(5vh, 2.5vw, 14px),随窗口尺寸变化(medium 预置)
//! - posY 弹簧按"焦点行与索引上一行"的间隔动态刚度;仅正常播放(PlaybackTick)
//!   开启 stagger——自视口顶部向下累积,焦点行也带延迟;seek/重建禁用
//! - 缩放 97/100 两档:上游用 `transform: scale()`(transform-origin 为行元素
//!   左缘垂直居中;左对齐文字下缩放前后的文字共享左缘与垂直中心)。
//!   GPUI 无文字变换 API,用两层静态排版(100%/97% 字号)交叉淡化等价近似,
//!   无逐帧重排版抖动、无档位跳变
//! - 文本 alpha:未激活 0.2(≈0.45s ease-out 过渡)、激活 0.85
//!   (上游 = 行 opacity × mask alpha:1.0×0.2 / 0.85×1.0;blur 是 Phase 2)
//! - 间奏:圆点行占位 spacer 已按上游参与布局(圆点视觉呈现属 Phase 2),
//!   焦点可为圆点行(圆点顶部对齐视口 25%)
//! - 初始入场:全部行 posY 起于 2×H(屏幕下方),以 slow 参数整块滑入(上游行为)

use std::time::Instant;

use amll_player_core::{AudioPlayerHandle, AudioThreadEvent, AudioThreadMessage};
use gpui_kit::{
    Context, Hsla, InteractiveElement, IntoElement, MouseButton, ParentElement, Pixels,
    Render, SharedString, Size, Styled, FontWeight, Window, div, px, rgba,
};

use crate::layout::{AlignAnchor, FrameContext, InterludeLayout, LayoutCalculator, LayoutTarget};
use crate::spring::{pos_y_policy, scale_policy, Spring, SpringParams};
use crate::timeline::{LyricLine, SeekDetector, Timeline, TimelineDiff};
use crate::trace::TraceState;
use crate::transform::ScaledElement;

/// 焦点行顶部对齐位置(react-full 预置 alignPosition=0.25)
const ALIGN_POSITION: f64 = 0.25;
/// 视口缓冲(上游 overscanPx;motionBuffer = 0.4H)
const OVERSCAN_PX: f64 = 300.0;
const MOTION_BUFFER_RATIO: f64 = 0.4;
/// stagger:每行延迟基数 50ms,焦点行之后 ×1/1.05 递减
const STAGGER_BASE_DELAY_SEC: f64 = 0.05;
const STAGGER_DECAY: f64 = 1.05;
/// 未激活行缩放(SCALE_ASPECT,enableScale 时)
const SCALE_ASPECT: f64 = 97.0;
/// wrapper 上下 padding(0.4em)
const WRAPPER_PAD_EM: f64 = 0.4;
/// 行内 line-height(1.2em)
const LINE_HEIGHT_EM: f64 = 1.2;
/// 单行总高:0.4 + 1.2 + 0.4 = 2.0em
const LINE_TOTAL_EM: f64 = WRAPPER_PAD_EM * 2.0 + LINE_HEIGHT_EM;
/// 文字左缘内缩(--lyric-line-padding-x = 1em)
const TEXT_INSET_X_EM: f64 = 1.0;
/// 文本 alpha(上游 = 行 opacity × 词 mask alpha;blur 是 Phase 2):
/// 激活 0.85×1.0;未激活 1.0×0.2
const TEXT_ALPHA_ACTIVE: f64 = 0.85;
const TEXT_ALPHA_DIM: f64 = 0.2;
/// alpha 过渡时长(gradientMask 0.3s / solid 0.45s,ease-out)
const ALPHA_TO_ACTIVE_SEC: f64 = 0.3;
const ALPHA_TO_DIM_SEC: f64 = 0.45;
/// 帧推进上限(上游 MAX_FRAME_DELTA)
const MAX_FRAME_DELTA_SEC: f64 = 0.1;
/// 间奏圆点行:margin 0.4em、元素 padding 0.8em
const DOT_MARGIN_EM: f64 = 0.4;
const DOT_PAD_EM: f64 = 0.8;
/// 行 opacity:高亮 0.85,其余 1.0(上游 resolveOpacity)
const LINE_OPACITY_HIGHLIGHTED: f64 = 0.85;

/// 文本 alpha 的 ease-out 过渡(上游 CSS 变量 transition)
struct AlphaTween {
    from: f64,
    to: f64,
    start: Instant,
    duration_sec: f64,
}

impl AlphaTween {
    fn new(value: f64) -> Self {
        Self {
            from: value,
            to: value,
            start: Instant::now(),
            duration_sec: 0.0,
        }
    }

    fn retarget(&mut self, to: f64, now: Instant) {
        if (self.to - to).abs() < 1e-3 {
            return;
        }
        self.from = self.sample(now);
        self.to = to;
        self.start = now;
        self.duration_sec = if to > self.from {
            ALPHA_TO_ACTIVE_SEC
        } else {
            ALPHA_TO_DIM_SEC
        };
    }

    fn sample(&self, now: Instant) -> f64 {
        if self.duration_sec <= 0.0 {
            return self.to;
        }
        let t = (now.duration_since(self.start).as_secs_f64() / self.duration_sec).clamp(0.0, 1.0);
        // CSS ease-out 的近似
        let eased = 1.0 - (1.0 - t) * (1.0 - t) * (1.0 - t);
        self.from + (self.to - self.from) * eased
    }

    fn animating(&self, now: Instant) -> bool {
        self.duration_sec > 0.0 && now.duration_since(self.start).as_secs_f64() < self.duration_sec
    }
}

pub struct LyricPage {
    handle: Option<AudioPlayerHandle>,
    timeline: Timeline,
    detector: SeekDetector,

    /// 播放位置(本地外推,毫秒)
    position_ms: f64,
    event_position_ms: f64,
    event_wall: Instant,
    playing: bool,
    was_playing: bool,

    container_size: Option<Size<Pixels>>,
    container_width: f64,
    container_height: f64,
    /// 根字号(max(5vh, 2.5vw, 14px),随窗口变化)
    font_size: f64,
    calculator: LayoutCalculator,

    pos_y: Vec<Spring>,
    scale: Vec<Spring>,
    alpha: Vec<AlphaTween>,
    /// 入场(2H 滑入)是否完成
    entered: bool,
    /// 强制重排版(首帧/尺寸变化/播放状态翻转)
    layout_dirty: bool,
    /// 当前 posY 弹簧参数(trace 记录)
    current_stiffness: f64,
    current_damping: f64,
    /// 最近一次排版的间奏圆点 y(trace 记录)
    last_interlude_y: Option<f64>,
    /// 排版事件缓冲(trace 写出用)
    pending_events: Vec<serde_json::Value>,

    trace: Option<TraceState>,
    last_frame: Option<Instant>,
    wall_origin: Instant,
}

impl LyricPage {
    pub fn new(
        handle: Option<AudioPlayerHandle>,
        lines: Vec<LyricLine>,
        trace: Option<TraceState>,
    ) -> Self {
        let count = lines.len();
        let scale_patch = scale_policy::full_patch(scale_policy::main_line());
        let mut scale = Vec::with_capacity(count);
        for _ in 0..count {
            let mut spring = Spring::new(100.0);
            spring.update_params(scale_patch, 0.0);
            scale.push(spring);
        }
        Self {
            handle,
            timeline: Timeline::new(lines),
            detector: SeekDetector::default(),
            position_ms: 0.0,
            event_position_ms: 0.0,
            event_wall: Instant::now(),
            playing: false,
            was_playing: false,
            container_size: None,
            container_width: 0.0,
            container_height: 0.0,
            font_size: 0.0,
            calculator: LayoutCalculator::new(count, 0.0),
            pos_y: (0..count).map(|_| Spring::new(0.0)).collect(),
            scale,
            alpha: (0..count).map(|_| AlphaTween::new(TEXT_ALPHA_DIM)).collect(),
            entered: false,
            layout_dirty: true,
            current_stiffness: 0.0,
            current_damping: 0.0,
            last_interlude_y: None,
            pending_events: Vec::new(),
            trace,
            last_frame: None,
            wall_origin: Instant::now(),
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
        let Some(handle) = &self.handle else {
            return;
        };
        let handle = handle.clone();
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

    /// 间奏圆点行总高(上游 totalInterludeHeight = 圆点元素高 + 2×0.4em margin)
    fn interlude_total_height(&self) -> f64 {
        let font = self.font_size;
        let vh = self.container_height / 100.0;
        let dots = (font * 0.5).max(vh).min(font * 3.0) + font * DOT_PAD_EM;
        dots + font * DOT_MARGIN_EM * 2.0
    }

    /// posY 弹簧参数(上游 getPosYSpringPolicy):
    /// seek/间奏 → slow(90/15);播完 → medium(140/22);
    /// 否则按焦点行与索引上一行的开始间隔动态刚度
    fn pos_y_params(&self, forced_slow: bool) -> SpringParams {
        if forced_slow || self.timeline.in_interlude() {
            pos_y_policy::slow()
        } else if self.timeline.end_of_song() {
            pos_y_policy::medium()
        } else {
            let scroll_to = self.timeline.scroll_to_index();
            let lines = self.timeline.lines();
            if scroll_to == 0 || scroll_to >= lines.len() {
                pos_y_policy::slow()
            } else {
                let interval =
                    lines[scroll_to].start_time_ms - lines[scroll_to - 1].start_time_ms;
                pos_y_policy::by_interval_ms(interval)
            }
        }
    }

    /// 单帧排版(上游 calcLayout + setLineTransformations 的合并)
    fn calc_layout(&mut self, is_seek: bool, media_ms: f64, reason: &str) {
        let count = self.timeline.lines().len();
        if count == 0 || self.container_height <= 0.0 {
            return;
        }
        let first_layout = !self.entered;

        // 焦点解析(上游 FocusController.resolve,无用户滚动挂起)
        let focus_on_interlude = self.timeline.focus_on_interlude();
        let can_display = self.timeline.can_display_interlude();
        let (target, interlude) = if focus_on_interlude && can_display {
            let anchor = self
                .timeline
                .active_interlude()
                .map(|inter| inter.anchor)
                .unwrap_or(-1);
            (
                LayoutTarget::Interlude(anchor),
                Some(InterludeLayout {
                    total_height: self.interlude_total_height(),
                    anchor,
                }),
            )
        } else if focus_on_interlude {
            // 圆点剩余时间不足以展示:焦点顺延到锚点后一行
            let next = (self
                .timeline
                .active_interlude()
                .map(|inter| inter.anchor)
                .unwrap_or(-1)
                + 1)
                .max(0) as usize;
            (LayoutTarget::Line(next.min(count - 1)), None)
        } else if self.timeline.end_of_song() {
            (LayoutTarget::Line(count - 1), None)
        } else {
            (
                LayoutTarget::Line(self.timeline.scroll_to_index().min(count - 1)),
                None,
            )
        };

        let ctx = FrameContext {
            container_height: self.container_height,
            align_position: ALIGN_POSITION,
            align_anchor: AlignAnchor::Top,
            overscan_px: OVERSCAN_PX,
            scroll_offset: 0.0,
            target,
            interlude,
        };
        let result = self.calculator.layout(&ctx);
        self.last_interlude_y = result.interlude_y;

        // 入场(上游:行创建时 posY = 2×H,首次排版以 Seek 语义整块滑入)
        if first_layout {
            let below = 2.0 * self.container_height;
            for spring in &mut self.pos_y {
                spring.set_position(below);
            }
            self.entered = true;
        }

        // stagger(上游:仅 PlaybackTick 开启;自视口顶部向下累积)
        let stagger = !is_seek && !first_layout;
        let scroll_to = self.timeline.scroll_to_index();
        let mut delay = 0.0;
        let mut base_delay = STAGGER_BASE_DELAY_SEC;
        let mut delays = Vec::with_capacity(count);
        for (index, instruction) in result.lines.iter().enumerate() {
            let d = if stagger { delay } else { 0.0 };
            delays.push(d);
            self.pos_y[index].set_target_position(instruction.y, d);
            if stagger && instruction.y + instruction.height >= 0.0 {
                delay += base_delay;
                if index >= scroll_to {
                    base_delay /= STAGGER_DECAY;
                }
            }
        }

        // scale 与文本 alpha 目标(上游 setLineTransformations,每次排版全量设置)
        let highlighted: &[usize] = self.timeline.highlighted();
        let latest = self.timeline.latest_highlighted().unwrap_or(scroll_to);
        let now = Instant::now();
        for index in 0..count {
            let active =
                highlighted.contains(&index) || (index >= scroll_to && index < latest);
            let scale_target = if active || !self.playing {
                100.0
            } else {
                SCALE_ASPECT
            };
            self.scale[index].set_target_position(scale_target, 0.0);
            self.alpha[index].retarget(
                if active {
                    TEXT_ALPHA_ACTIVE
                } else {
                    TEXT_ALPHA_DIM
                },
                now,
            );
        }

        self.pending_events.push(serde_json::json!({
            "ev": "retarget",
            "reason": reason,
            "media": media_ms,
            "seek": is_seek,
            "stagger": stagger,
            "first": first_layout,
            "k": self.current_stiffness,
            "d": self.current_damping,
            "target": format!("{target:?}"),
            "interludeY": result.interlude_y,
            "delays": delays,
        }));
    }

    /// 上游 resolveBlurLevel(blur 视觉属 Phase 2,先供 trace 对比)
    fn blur_level(&self, index: usize, in_viewport: bool, active: bool) -> f64 {
        if !in_viewport {
            return 5.0;
        }
        if active {
            return 0.0;
        }
        let scroll_to = self.timeline.scroll_to_index();
        let latest = self.timeline.latest_highlighted().unwrap_or(scroll_to);
        let level = 1.0
            + if index < scroll_to {
                (scroll_to - index) as f64 + 1.0
            } else {
                (index as f64 - latest as f64).abs()
            };
        // 窄视口(≤1024)×0.8;renderStyles 处再 min(5)
        let narrowed = self.container_width <= 1024.0;
        let level = if narrowed { level * 0.8 } else { level };
        level.min(5.0)
    }

    fn animating(&self, now: Instant) -> bool {
        self.playing
            || self.pos_y.iter().any(|spring| !spring.arrived())
            || self.scale.iter().any(|spring| !spring.arrived())
            || self.alpha.iter().any(|tween| tween.animating(now))
            || self.trace.is_some()
    }

    fn pixel_size(size: Size<Pixels>) -> (f64, f64) {
        ((size.width / px(1.0)) as f64, (size.height / px(1.0)) as f64)
    }
}


impl Render for LyricPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 全窗口开启稳定字形光栅(patches/gpui-pre):光栅不随亚像素相位
        // 重建,由 GPU 双线性采样负责亚像素放置——与 WebView 的位图层
        // 移动一致,消除动画尾部的 AA 抖动
        window.set_stable_glyph_raster(true);

        let now = Instant::now();
        let had_last_frame = self.last_frame.is_some();
        let delta = self
            .last_frame
            .map(|last| now.duration_since(last).as_secs_f64())
            .unwrap_or(0.0)
            .clamp(0.0, MAX_FRAME_DELTA_SEC);
        self.last_frame = Some(now);
        // trace 模式下弹簧步长与浏览器 harness 一致(确定性对比)
        let delta = if self.trace.is_some() {
            if had_last_frame {
                1.0 / 60.0
            } else {
                0.0
            }
        } else {
            delta
        };

        // 1) 时钟:trace 模式用确定性固定步进(与浏览器 harness 同构),
        //    否则按音频事件外推
        if let Some(trace) = self.trace.as_mut() {
            let mut applied = Vec::new();
            let media = trace.clock.step_ms(&mut applied);
            self.position_ms = media;
            self.playing = true;
            for seek in applied {
                tracing::info!("剧本 seek: @{:.0}ms → {:.0}ms", seek.at_ms, seek.to_ms);
            }
        } else if self.playing {
            self.position_ms =
                self.event_position_ms + now.duration_since(self.event_wall).as_millis() as f64;
        } else {
            self.position_ms = self.event_position_ms;
        }

        // 2) 时间轴同步(seek 检测 → diff)
        let wall_ms = self.wall_origin.elapsed().as_millis() as f64;
        let is_seek = self
            .detector
            .detect(self.position_ms, wall_ms, self.playing);
        let diff: TimelineDiff = self.timeline.sync(self.position_ms, is_seek);

        // 3) 容器与字号(随窗口变化,变化即重排版)
        self.container_size = Some(window.bounds().size);
        let (width, height) = Self::pixel_size(window.bounds().size);
        let size_changed = (width - self.container_width).abs() > 0.01
            || (height - self.container_height).abs() > 0.01;
        self.container_width = width;
        self.container_height = height;
        let font_size = (height * 0.05).max(width * 0.025).max(14.0);
        if (font_size - self.font_size).abs() > 0.01 {
            self.font_size = font_size;
            let line_height = font_size * LINE_TOTAL_EM;
            let count = self.timeline.lines().len();
            for index in 0..count {
                self.calculator.set_line_height(index, line_height);
            }
            self.layout_dirty = true;
        }
        let playing_flipped = self.was_playing != self.playing;
        self.was_playing = self.playing;

        // 4) 重排版(上游 syncTime:diff.hasChanged → updateSpringParams + calcLayout;
        //    播放状态翻转/首帧/尺寸变化 → ConfigChange 语义的强制重排版)
        if diff.has_changed || self.layout_dirty || playing_flipped || size_changed {
            let forced_slow = is_seek || !self.entered;
            if diff.is_interlude_changed
                || diff.is_scroll_to_changed
                || diff.is_end_of_song_changed
                || forced_slow
            {
                let params = self.pos_y_params(forced_slow);
                self.current_stiffness = params.stiffness;
                self.current_damping = params.damping;
                let patch = pos_y_policy::patch(params);
                for spring in &mut self.pos_y {
                    spring.update_params(patch, 0.0);
                }
            }
            let reason = if !self.entered {
                "rebuild-view"
            } else if is_seek {
                "seek"
            } else if playing_flipped || size_changed {
                "config-change"
            } else {
                "playback-tick"
            };
            self.calc_layout(is_seek, self.position_ms, reason);
            self.layout_dirty = false;
        }

        // 5) 弹簧与 alpha 推进
        for spring in &mut self.pos_y {
            spring.update(delta);
        }
        for spring in &mut self.scale {
            spring.update(delta);
        }

        if self.animating(now) {
            window.request_animation_frame();
        }

        // 6) trace 记录(事件 + 逐帧;先做不可变快照,再进可变借用)
        let trace_snapshot = self.trace.is_some().then(|| {
            let count = self.timeline.lines().len();
            let highlighted: &[usize] = self.timeline.highlighted();
            let latest = self
                .timeline
                .latest_highlighted()
                .unwrap_or(self.timeline.scroll_to_index());
            let ys: Vec<f64> = self
                .pos_y
                .iter()
                .map(|spring| spring.current_position())
                .collect();
            let ss: Vec<f64> = self
                .scale
                .iter()
                .map(|spring| spring.current_position())
                .collect();
            let as_: Vec<f64> = self.alpha.iter().map(|tween| tween.sample(now)).collect();
            let hl: Vec<usize> = highlighted.to_vec();
            let ops: Vec<f64> = (0..count)
                .map(|index| {
                    if highlighted.contains(&index) {
                        LINE_OPACITY_HIGHLIGHTED
                    } else {
                        1.0
                    }
                })
                .collect();
            let motion_buffer = self.container_height * MOTION_BUFFER_RATIO;
            let line_h = self.font_size * LINE_TOTAL_EM;
            let blurs: Vec<f64> = (0..count)
                .map(|index| {
                    let active = highlighted.contains(&index)
                        || (index >= self.timeline.scroll_to_index() && index < latest);
                    let y = ys[index];
                    let in_viewport = y <= self.container_height + OVERSCAN_PX + motion_buffer
                        && y + line_h >= -(OVERSCAN_PX + motion_buffer);
                    self.blur_level(index, in_viewport, active)
                })
                .collect();
            (ys, ss, as_, ops, blurs, hl)
        });
        if let (Some(trace), Some((ys, ss, as_, ops, blurs, hl))) =
            (self.trace.as_mut(), trace_snapshot)
        {
            for event in self.pending_events.drain(..) {
                trace.recorder.write(&event);
            }
            let latest = self
                .timeline
                .latest_highlighted()
                .unwrap_or(self.timeline.scroll_to_index());
            trace.recorder.write(&serde_json::json!({
                "wall": wall_ms,
                "media": self.position_ms,
                "playing": self.playing,
                "seek": is_seek,
                "scrollTo": self.timeline.scroll_to_index(),
                "latest": latest,
                "hl": hl,
                "k": self.current_stiffness,
                "d": self.current_damping,
                "interludeY": self.last_interlude_y,
                "y": ys,
                "s": ss,
                "a": as_,
                "op": ops,
                "blur": blurs,
            }));
            if !trace.done && self.position_ms >= trace.duration_ms {
                trace.done = true;
                let frames = trace.recorder.frames();
                tracing::info!(
                    "trace 完成: {} 帧,已写入 {}",
                    frames,
                    trace.path.display()
                );
                if trace.quit_on_done {
                    std::process::exit(0);
                }
            }
        }

        // 7) 渲染可见行(裁剪范围与上游 isInViewport 同式)
        let font = self.font_size;
        let line_box = font * LINE_HEIGHT_EM;
        let text_width = (self.container_width - font * TEXT_INSET_X_EM * 2.0).max(0.0);
        let pad_y = font * WRAPPER_PAD_EM;
        let inset_x = font * TEXT_INSET_X_EM;
        let line_total = font * LINE_TOTAL_EM;
        let motion_buffer = self.container_height * MOTION_BUFFER_RATIO;
        let top_bound = -(OVERSCAN_PX + motion_buffer);
        let bottom_bound = self.container_height + OVERSCAN_PX + motion_buffer;
        let highlighted: &[usize] = self.timeline.highlighted();

        let mut children = Vec::new();
        for (index, line) in self.timeline.lines().iter().enumerate() {
            let y = self.pos_y[index].current_position();
            if !(y <= bottom_bound && y + line_total >= top_bound) {
                continue;
            }
            let text: SharedString = line.full_line_text().into();
            if text.trim().is_empty() {
                continue;
            }
            let scale = self.scale[index].current_position() / 100.0;
            let line_opacity = if highlighted.contains(&index) {
                LINE_OPACITY_HIGHLIGHTED
            } else {
                1.0
            };
            let text_alpha = self.alpha[index].sample(now);

            // 上游用 CSS transform: scale() 缩放行元素(transform-origin 为
            // 行左缘垂直居中,文字排版静态);等价实现:单层静态排版文字 +
            // 绘制期变换(patches/gpui-pre 暴露的字形精灵矩阵)
            let text_element = div()
                .absolute()
                .left(px(inset_x as f32))
                .top(px(pad_y as f32))
                .w(px(text_width as f32))
                .text_size(px(font as f32))
                .line_height(px(line_box as f32))
                .font_weight(FontWeight::from(600.0))
                .text_color(Hsla {
                    h: 0.0,
                    s: 0.0,
                    l: 1.0,
                    a: text_alpha as f32,
                })
                .child(text);
            children.push(
                div()
                    .id(("lyric-line", index))
                    .absolute()
                    .left(px(0.0))
                    .top(px(y as f32))
                    .w(px(self.container_width as f32))
                    .h(px(line_total as f32))
                    .opacity(line_opacity as f32)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener({
                            let start_ms = line.start_time_ms;
                            move |this: &mut LyricPage, _event, _window, _cx| {
                                this.send_seek(start_ms / 1000.0);
                            }
                        }),
                    )
                    .child(ScaledElement::new(
                        text_element,
                        scale as f32,
                        gpui::point(px(0.0), px((line_box / 2.0) as f32)),
                    )),
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
