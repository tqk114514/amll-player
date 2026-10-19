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
//!   左缘垂直居中;左对齐文字下缩放前后的文字共享左缘与垂直中心),
//!   经绘制期元素变换矩阵作用于字形精灵(本地 patch)
//! - 行亮度(上游两条独立 CSS transition 叠加):
//!   1. wrapper opacity `transition: opacity .4s ease`——高亮 0.85、其余
//!      1.0(resolveOpacity),淡行 opacity 补间还原
//!   2. mask alpha `--bright/--dark-mask-alpha var(--mask-alpha-duration)
//!      ease-out`(`@property` 注册的可过渡自定义属性)——激活 .2/.2→1/.4
//!      (0.3s),退激活回落(0.45s);退激活时遮罩位置冻结(maskAnimator
//!      .pause()),已唱部分随 alpha 平滑变暗而非瞬间消失,float 同步
//!      反演回落(playbackRate=-1)
//! - 逐词揭示:上游 mask-image 线性渐变 → 精灵像素渐变(patch),带内
//!   逐像素线性插值、无阶梯近似;blur 是 Phase 2
//! - 间奏:圆点行占位 spacer 已按上游参与布局;圆点演出为容器整体
//!   `transform-origin: center` 的缩放呼吸/两段退场——三颗 SDF 几何圆
//!   (quad 圆角=半径)经绘制期矩阵缩放(本地 patch:quad 携带变换,
//!   顶点变换 + 逆矩阵回局部空间求 SDF,变换作用域内不做像素吸附)
//! - 初始入场:全部行 posY 起于 2×H(屏幕下方),以 slow 参数整块滑入(上游行为)
//! - 拖动滚动(M4b):上游 ScrollInteractionEngine 状态机(scroll.rs)——
//!   触摸 10px 意图阈值、松手惯性 0.95^(Δt/16.67) 衰减、滚轮 150ms 防抖
//!   离散步进(弹簧平滑)、边界钳制;滚动挂起期间 FocusController 冻结
//!   焦点,滚轮静止 500ms 且焦点行进入视口后恢复自动对齐;seek(非触摸
//!   滚动中)清空滚动状态。平台未暴露原始触摸,触摸平移经 gpui 手势
//!   识别器以滚轮事件投递;引擎触摸分支已完整移植待接线

use std::time::Instant;

use amll_player_core::{AudioPlayerHandle, AudioThreadEvent, AudioThreadMessage};
use gpui_kit::{
    Context, Hsla, InteractiveElement, IntoElement, MouseButton, ParentElement, Pixels,
    Render, SharedString, Size, Styled, FontWeight, Window, div, px, rgba,
};

use crate::layout::{AlignAnchor, FrameContext, InterludeLayout, LayoutCalculator};
use crate::spring::{pos_y_policy, scale_policy, Spring, SpringParams};
use crate::timeline::{LyricLine, SeekDetector, Timeline, TimelineDiff};
use crate::trace::{PixelTrace, TraceState};
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
/// 词 mask alpha(上游 --bright/--dark-mask-alpha;行 opacity 另行调制):
/// 激活行 bright=1.0 / dark=0.4;未激活行 solid 0.2
const MASK_ALPHA_BRIGHT: f64 = 1.0;
const MASK_ALPHA_DARK: f64 = 0.4;
const MASK_ALPHA_SOLID: f64 = 0.2;
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
/// wrapper opacity 的 CSS transition:`transition: opacity .4s`(默认 ease)
const LINE_OPACITY_TRANSITION_SEC: f64 = 0.4;

/// 排版原因(上游 LayoutReason)。策略映射(上游 consts.ts):
/// stagger 仅正常播放开启;直贴 Y(snapPosY)仅连续滚动(触摸拖动跟手),
/// 滚轮步进保留弹簧平滑;Seek/RebuildView 重置间奏演出。
/// M5 验收:帧率计数(滑动 2s 窗口的原始帧间隔统计)
#[derive(Debug, Default)]
struct FpsCounter {
    /// 原始帧间隔样本(秒;未被动画钳制污染)
    samples: std::collections::VecDeque<f64>,
}

impl FpsCounter {
    fn push(&mut self, raw_delta_sec: f64) {
        if raw_delta_sec <= 0.0 {
            return;
        }
        self.samples.push_back(raw_delta_sec);
        while self.samples.len() > 2 && self.samples.iter().sum::<f64>() > 2.0 {
            self.samples.pop_front();
        }
    }

    fn fps(&self) -> f64 {
        if self.samples.is_empty() {
            return 0.0;
        }
        self.samples.len() as f64 / self.samples.iter().sum::<f64>()
    }

    fn avg_frame_ms(&self) -> f64 {
        if self.samples.is_empty() {
            return 0.0;
        }
        self.samples.iter().sum::<f64>() / self.samples.len() as f64 * 1000.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LayoutReason {
    PlaybackTick,
    #[allow(dead_code)]
    Resize,
    #[allow(dead_code)]
    InteractionStart,
    ContinuousScroll,
    DiscreteScroll,
    Seek,
    RebuildView,
    ConfigChange,
}

/// 歌词字体(App 实配:字体栈回退 Segoe UI,字重 600)
fn lyric_font() -> gpui::Font {
    gpui::Font {
        family: SharedString::from(".SystemUIFont"),
        weight: FontWeight::from(600.0),
        ..Default::default()
    }
}

/// CSS 缓动关键字(补间曲线,精确 cubic-bezier 定义见 ease.rs)
#[derive(Clone, Copy, PartialEq)]
enum TweenEasing {
    /// `ease-out`:cubic-bezier(0, 0, 0.58, 1)
    EaseOut,
    /// `ease`:cubic-bezier(0.25, 0.1, 0.25, 1)
    Ease,
}

impl TweenEasing {
    fn apply(self, t: f64) -> f64 {
        match self {
            Self::EaseOut => crate::ease::ease_out()(t),
            Self::Ease => crate::ease::ease()(t),
        }
    }
}

/// CSS transition 的数值等价:起点采样、方向决定时长、精确缓动曲线
struct AlphaTween {
    from: f64,
    to: f64,
    start: Instant,
    duration_sec: f64,
    easing: TweenEasing,
}

impl AlphaTween {
    fn new(value: f64) -> Self {
        Self {
            from: value,
            to: value,
            start: Instant::now(),
            duration_sec: 0.0,
            easing: TweenEasing::EaseOut,
        }
    }

    /// mask alpha 的过渡(上游 gradientMask 类切换):
    /// 升 → --mask-alpha-duration .3s,降 → .45s(基类值),均 ease-out
    fn retarget(&mut self, to: f64, now: Instant) {
        let from = self.sample(now);
        let duration_sec = if to > from {
            ALPHA_TO_ACTIVE_SEC
        } else {
            ALPHA_TO_DIM_SEC
        };
        self.retarget_eased(to, now, duration_sec, TweenEasing::EaseOut);
    }

    fn retarget_eased(
        &mut self,
        to: f64,
        now: Instant,
        duration_sec: f64,
        easing: TweenEasing,
    ) {
        if (self.to - to).abs() < 1e-3 {
            return;
        }
        self.from = self.sample(now);
        self.to = to;
        self.start = now;
        self.duration_sec = duration_sec;
        self.easing = easing;
    }

    fn sample(&self, now: Instant) -> f64 {
        if self.duration_sec <= 0.0 {
            return self.to;
        }
        let t = (now.duration_since(self.start).as_secs_f64() / self.duration_sec).clamp(0.0, 1.0);
        self.from + (self.to - self.from) * self.easing.apply(t)
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
    alpha_bright: Vec<AlphaTween>,
    alpha_dark: Vec<AlphaTween>,
    /// wrapper opacity 补间(上游 transition: opacity .4s ease)
    line_opacity: Vec<AlphaTween>,
    /// 各行的激活态(退激活沿判定的基准)
    prev_active: Vec<bool>,
    /// 各行上浮阶段(激活随媒体推进 / 退激活反演回落 / 反演完成保持落回)
    float_phase: Vec<crate::words::FloatPhase>,
    /// 退激活期间的遮罩冻结时刻(上游 maskAnimator.pause();alpha 补间
    /// 收敛后清除)
    mask_frozen: Vec<Option<f64>>,
    /// 各行的词元 span 布局(随字号重建)
    word_spans: Vec<Vec<crate::words::WordSpan>>,
    /// 入场(2H 滑入)是否完成
    entered: bool,
    /// 强制重排版(首帧/尺寸变化/播放状态翻转)
    layout_dirty: bool,
    /// 当前 posY 弹簧参数(trace 记录)
    current_stiffness: f64,
    current_damping: f64,
    /// 最近一次排版的间奏圆点 y(trace 记录)
    last_interlude_y: Option<f64>,
    /// 间奏圆点行的 posY 弹簧(上游 interludeDots.posY)
    dots_y: Spring,
    /// 圆点行是否完成首次钉位(与行入场同语义)
    dots_entered: bool,
    /// 间奏被截断时的 150ms 淡出(截断时刻, 起始透明度, 三点透明度, 缩放)
    dots_dismiss: Option<(f64, f64, [f64; 3], f64)>,
    /// 最近一帧可见的圆点状态(退场淡出的起点快照)
    last_dots_state: Option<(f64, [f64; 3], f64)>,
    /// 排版事件缓冲(trace 写出用)
    pending_events: Vec<serde_json::Value>,
    /// 滚动交互引擎(上游 ScrollInteractionEngine)
    scroll_engine: crate::scroll::ScrollEngine,
    /// 对齐焦点状态机(上游 FocusController:滚动挂起期间冻结目标)
    focus: crate::layout::FocusController,
    /// 用户滚动过,尚未回归自动对齐
    is_auto_align_suspended: bool,
    /// 最近一次交互是否触摸(上游 isTouchScrolled:触摸滚动期间 seek 不重置滚动)
    is_touch_scrolled: bool,
    /// 滚动触发的待重排版原因(策略映射的消费入口)
    scroll_dirty: Option<crate::scroll::ScrollLayoutReason>,
    /// M5 验收:帧率计数
    fps: FpsCounter,

    trace: Option<TraceState>,
    /// 像素级追踪(--trace-pixels):逐帧读回呈现输出并差分
    pixel_trace: Option<PixelTrace>,
    last_frame: Option<Instant>,
    wall_origin: Instant,
}

impl LyricPage {
    pub fn new(
        handle: Option<AudioPlayerHandle>,
        lines: Vec<LyricLine>,
        trace: Option<TraceState>,
        pixel_dir: Option<std::path::PathBuf>,
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
            dots_y: Spring::new(0.0),
            dots_entered: false,
            dots_dismiss: None,
            last_dots_state: None,
            scale,
            alpha_bright: (0..count).map(|_| AlphaTween::new(MASK_ALPHA_SOLID)).collect(),
            alpha_dark: (0..count).map(|_| AlphaTween::new(MASK_ALPHA_SOLID)).collect(),
            line_opacity: (0..count).map(|_| AlphaTween::new(1.0)).collect(),
            prev_active: vec![false; count],
            float_phase: vec![crate::words::FloatPhase::Active; count],
            mask_frozen: vec![None; count],
            word_spans: (0..count).map(|_| Vec::new()).collect(),
            entered: false,
            layout_dirty: true,
            current_stiffness: 0.0,
            current_damping: 0.0,
            last_interlude_y: None,
            pending_events: Vec::new(),
            scroll_engine: crate::scroll::ScrollEngine::new(),
            focus: crate::layout::FocusController::new(),
            is_auto_align_suspended: false,
            is_touch_scrolled: false,
            scroll_dirty: None,
            fps: FpsCounter::default(),
            trace,
            pixel_trace: pixel_dir.map(PixelTrace::new),
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

    /// 间奏圆点行(上游 InterludeDotsEl):三颗 SDF 几何圆(quad 圆角=
    /// 半径,等价上游 border-radius: 50%),呼吸/退场缩放与 posY 弹簧
    /// 全部经绘制期矩阵——本地 patch 让 quad 携带变换(顶点变换 +
    /// 逆矩阵回局部空间求 SDF),变换作用域内不做像素吸附;
    /// 缩放绕容器盒中心,圆点垂直居中即群中心高度
    fn build_dots_element(
        &self,
        opacity: f64,
        scale: f64,
        dot_opacities: [f64; 3],
    ) -> gpui_kit::AnyElement {
        let font = self.font_size;
        // 上游 .FmKaba_interludeDots:padding .4em 1em、
        // height clamp(.5em, 1vh, 3em)(content-box,加上下 padding)
        let box_h = self.dots_box_height();
        let box_w = font
            * (crate::dots::WORD_PAD_EM * 2.0
                + crate::dots::DOT_SIZE_EM * 3.0
                + crate::dots::DOT_GAP_EM * 2.0);
        let dot = font * crate::dots::DOT_SIZE_EM;
        let gap = font * crate::dots::DOT_GAP_EM;
        let inset = font * crate::dots::WORD_PAD_EM;

        // 实际群中心(左缘 0 + 盒宽/2,dots_y + 盒高/2)
        let group_center = (box_w / 2.0, self.dots_y.current_position() + box_h / 2.0);
        // 局部群中心:窗口内常量,未变换的 quad 恒在内容遮罩内
        let local_group_center = (font * 2.0, font * 4.0);
        // 未缩放圆心相对群中心:水平 = padding + 半径 + i×(直径+间距) − 盒宽/2;垂直 = 0
        let mut local_centers = [(0.0, 0.0); 3];
        for (index, center) in local_centers.iter_mut().enumerate() {
            let x = inset + dot / 2.0 + index as f64 * (dot + gap);
            *center = (x - box_w / 2.0, 0.0);
        }

        crate::dots::DotsElement::new(
            local_centers,
            dot,
            local_group_center,
            group_center,
            scale,
            opacity,
            dot_opacities,
        )
        .into_any_element()
    }

    /// 圆点元素盒高(上游 height: clamp(.5em, 1vh, 3em) + 上下 padding .8em)
    fn dots_box_height(&self) -> f64 {
        let font = self.font_size;
        let vh = self.container_height / 100.0;
        (font * 0.5).max(vh).min(font * 3.0) + font * DOT_PAD_EM
    }

    /// 间奏圆点行总高(上游 totalInterludeHeight = 圆点元素高 + 2×0.4em margin)
    fn interlude_total_height(&self) -> f64 {
        self.dots_box_height() + self.font_size * DOT_MARGIN_EM * 2.0
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

    /// 单帧排版(上游 calcLayout + setLineTransformations 的合并;
    /// reason 决定策略:stagger / 直贴 Y / 间奏重置)
    fn calc_layout(&mut self, reason: LayoutReason, media_ms: f64, trace_reason: &str) {
        let count = self.timeline.lines().len();
        if count == 0 || self.container_height <= 0.0 {
            return;
        }
        let first_layout = !self.entered;
        let snap_pos_y = matches!(reason, LayoutReason::ContinuousScroll);

        // 焦点解析(上游 FocusController.resolve):滚动挂起期间冻结
        let focus_on_interlude = self.timeline.focus_on_interlude();
        let can_display = self.timeline.can_display_interlude();
        let active_interlude = self.timeline.active_interlude();
        let target = self.focus.resolve(
            focus_on_interlude,
            active_interlude.map(|inter| inter.anchor),
            can_display,
            self.timeline.end_of_song(),
            self.timeline.scroll_to_index().min(count.saturating_sub(1)),
            count,
            self.is_auto_align_suspended,
        );
        // 间奏占位独立于焦点(上游 ctx.interlude = canDisplay ? interlude :
        // undefined,不看 isFocusOnInterlude——残留高亮期间 spacer 仍在)
        let interlude = if can_display {
            active_interlude.map(|inter| InterludeLayout {
                total_height: self.interlude_total_height(),
                anchor: inter.anchor,
            })
        } else {
            None
        };

        let mut ctx = FrameContext {
            container_height: self.container_height,
            align_position: ALIGN_POSITION,
            align_anchor: AlignAnchor::Top,
            overscan_px: OVERSCAN_PX,
            scroll_offset: self.scroll_engine.offset(),
            target,
            interlude,
        };
        let mut result = self.calculator.layout(&ctx);
        // 滚动边界与钳制(上游 scrollEngine.updateBoundary)
        let clamped_offset = self
            .scroll_engine
            .update_boundary(result.bounds_min, result.bounds_max);
        if (clamped_offset - ctx.scroll_offset).abs() > 1e-9 {
            ctx.scroll_offset = clamped_offset;
            result = self.calculator.layout(&ctx);
        }
        self.last_interlude_y = result.interlude_y;

        // 入场(上游:行创建时 posY = 2×H,首次排版以 Seek 语义整块滑入)
        if first_layout {
            let below = 2.0 * self.container_height;
            for spring in &mut self.pos_y {
                spring.set_position(below);
            }
            self.entered = true;
        }

        // stagger(上游 LayoutReasonStrategyMap:仅 PlaybackTick 开启;
        // 自视口顶部向下累积);ContinuousScroll 直贴 Y(触摸拖动跟手)
        let stagger = matches!(reason, LayoutReason::PlaybackTick) && !first_layout;
        let scroll_to = self.timeline.scroll_to_index();
        let mut delay = 0.0;
        let mut base_delay = STAGGER_BASE_DELAY_SEC;
        let mut delays = Vec::with_capacity(count);
        for (index, instruction) in result.lines.iter().enumerate() {
            let d = if stagger { delay } else { 0.0 };
            delays.push(d);
            if snap_pos_y {
                self.pos_y[index].set_position(instruction.y);
            } else {
                self.pos_y[index].set_target_position(instruction.y, d);
            }
            if stagger && instruction.y + instruction.height >= 0.0 {
                delay += base_delay;
                if index >= scroll_to {
                    base_delay /= STAGGER_DECAY;
                }
            }
        }

        // scale、文本 alpha 与行 opacity 目标(上游 setLineTransformations +
        // resolveOpacity,每次排版全量设置;过渡在补间中还原 CSS transition)
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
            // wrapper opacity:高亮 0.85、其余 1.0,0.4s ease 过渡
            self.line_opacity[index].retarget_eased(
                if highlighted.contains(&index) {
                    LINE_OPACITY_HIGHLIGHTED
                } else {
                    1.0
                },
                now,
                LINE_OPACITY_TRANSITION_SEC,
                TweenEasing::Ease,
            );
            let (bright, dark) = if active {
                (MASK_ALPHA_BRIGHT, MASK_ALPHA_DARK)
            } else {
                (MASK_ALPHA_SOLID, MASK_ALPHA_SOLID)
            };
            self.alpha_bright[index].retarget(bright, now);
            self.alpha_dark[index].retarget(dark, now);
            // 退激活:冻结遮罩推进与 float 起点,进入反演回落;
            // 重新激活则回到媒体驱动(上游 enable 重设动画时钟)
            if active {
                self.float_phase[index] = crate::words::FloatPhase::Active;
                self.mask_frozen[index] = None;
            } else if self.prev_active[index] {
                self.float_phase[index] = crate::words::FloatPhase::Reversing(
                    crate::words::FloatReverse {
                        elapsed_ms: 0.0,
                        media_at_disable_ms: media_ms,
                    },
                );
                self.mask_frozen[index] = Some(media_ms);
            }
            self.prev_active[index] = active;
        }

        // 间奏圆点行:posY 弹簧到 interludeY + dotMargin(上游 setTransform
        // 的 immediate = snapPosY:连续滚动直贴跟手)
        if let Some(interlude_y) = result.interlude_y {
            let dot_margin = self.font_size * crate::dots::DOT_MARGIN_EM;
            let target = interlude_y + dot_margin;
            if snap_pos_y || !self.dots_entered {
                self.dots_y.set_position(target);
                self.dots_entered = true;
            } else {
                self.dots_y.set_target_position(target, 0.0);
            }
        }

        self.pending_events.push(serde_json::json!({
            "ev": "retarget",
            "reason": trace_reason,
            "media": media_ms,
            "seek": matches!(reason, LayoutReason::Seek),
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

    /// 行是否在真实视口内(上游 isInRenderRange(false),不含 overscan)
    fn row_in_viewport(&self, index: usize) -> bool {
        let Some(spring) = self.pos_y.get(index) else {
            return false;
        };
        let y = spring.current_position();
        let height = self.font_size * LINE_TOTAL_EM;
        self.container_height > 0.0 && y < self.container_height && y + height > 0.0
    }

    fn animating(&self, now: Instant) -> bool {
        self.playing
            || self.pos_y.iter().any(|spring| !spring.arrived())
            || self.scale.iter().any(|spring| !spring.arrived())
            || self.alpha_bright.iter().any(|tween| tween.animating(now))
            || self.alpha_dark.iter().any(|tween| tween.animating(now))
            || self.line_opacity.iter().any(|tween| tween.animating(now))
            || self
                .float_phase
                .iter()
                .any(|phase| matches!(phase, crate::words::FloatPhase::Reversing(_)))
            || !self.dots_y.arrived()
            || self.scroll_engine.is_active()
            || self.scroll_dirty.is_some()
            || self.trace.is_some()
    }

    fn pixel_size(size: Size<Pixels>) -> (f64, f64) {
        ((size.width / px(1.0)) as f64, (size.height / px(1.0)) as f64)
    }
}


impl Render for LyricPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = Instant::now();
        // 像素级追踪:先读回上一呈现帧(此时渲染目标尚未被本帧覆盖)
        let prev_media = self.position_ms;
        let captured_pixels = if self.pixel_trace.is_some() {
            window.capture_frame_pixels()
        } else {
            None
        };
        let raw_delta = self
            .last_frame
            .map(|last| now.duration_since(last).as_secs_f64())
            .unwrap_or(0.0);
        self.fps.push(raw_delta);
        let delta = raw_delta.clamp(0.0, MAX_FRAME_DELTA_SEC);
        self.last_frame = Some(now);

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
        let was_focus_on_interlude = self.timeline.focus_on_interlude();
        let is_seek = self
            .detector
            .detect(self.position_ms, wall_ms, self.playing);
        let diff: TimelineDiff = self.timeline.sync(self.position_ms, is_seek);

        // 滚动引擎逐帧推进(惯性物理与滚轮防抖,墙钟驱动)
        let scroll_advance = self.scroll_engine.advance(delta * 1000.0, wall_ms);
        if scroll_advance.updated {
            self.scroll_dirty = Some(crate::scroll::ScrollLayoutReason::ContinuousScroll);
        }
        // 恢复自动对齐 / seek 重置(上游 syncTime 的滚动分支):
        // - seek(非触摸滚动中):清空全部滚动状态回归自动对齐
        // - 焦点行切换且新行已进入真实视口、滚动静止超 500ms:同样回归
        if diff.has_changed {
            let scroll_to = self.timeline.scroll_to_index();
            if diff.is_time_jumped {
                if !self.is_touch_scrolled {
                    self.scroll_engine.reset_scroll(0.0);
                    self.is_auto_align_suspended = false;
                    self.is_touch_scrolled = false;
                }
            } else if (diff.is_scroll_to_changed || was_focus_on_interlude)
                && !self.timeline.focus_on_interlude()
                && self.timeline.is_playing_line(scroll_to)
                && self.is_auto_align_suspended
                && self.scroll_engine.can_resume_auto_align(wall_ms)
                && self.row_in_viewport(scroll_to)
            {
                self.scroll_engine.reset_scroll(0.0);
                self.is_auto_align_suspended = false;
                self.is_touch_scrolled = false;
            }
        }

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
            // 词元 span 布局:CJK 逐字/拉丁成组/空格零时长,逐 span 测宽排位
            let font = lyric_font();
            for (index, line) in self.timeline.lines().iter().enumerate() {
                let atoms = crate::words::atomize(&line.words);
                let mut spans = crate::words::group_spans(&atoms);
                let mut x = 0.0;
                for span in &mut spans {
                    let shared: SharedString = span.text.clone().into();
                    let runs = [gpui::TextRun {
                        len: shared.len(),
                        font: font.clone(),
                        color: Hsla::default(),
                        ..Default::default()
                    }];
                    let shaped = window
                        .text_system()
                        .shape_line(shared, px(font_size as f32), &runs, None);
                    span.width = (shaped.width() / px(1.0)) as f64;
                    span.x = x;
                    x += span.width;
                }
                self.word_spans[index] = spans;
            }
            self.layout_dirty = true;
        }
        // 间奏消失(唱到下一行):非 seek 时圆点做 150ms 淡出(上游 dismiss)
        if diff.is_interlude_changed
            && let Some((opacity, dot_opacities, scale)) = self.last_dots_state.take()
            && !diff.is_time_jumped
            && opacity > 0.0
        {
            self.dots_dismiss = Some((self.position_ms, opacity, dot_opacities, scale));
        }

        let playing_flipped = self.was_playing != self.playing;
        self.was_playing = self.playing;

        // 4) 重排版(上游 syncTime:diff.hasChanged → updateSpringParams +
        //    calcLayout;播放状态翻转/首帧/尺寸变化 → ConfigChange;
        //    滚动事件 → 对应滚动原因的策略)
        let scroll_reason = self.scroll_dirty.take();
        if diff.has_changed
            || self.layout_dirty
            || playing_flipped
            || size_changed
            || scroll_reason.is_some()
        {
            let forced_slow = diff.is_time_jumped || !self.entered;
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
                self.dots_y.update_params(patch, 0.0);
            }
            let (reason, trace_reason) = if let Some(scroll_reason) = scroll_reason {
                match scroll_reason {
                    crate::scroll::ScrollLayoutReason::InteractionStart => {
                        (LayoutReason::InteractionStart, "interaction-start")
                    }
                    crate::scroll::ScrollLayoutReason::ContinuousScroll => {
                        (LayoutReason::ContinuousScroll, "continuous-scroll")
                    }
                    crate::scroll::ScrollLayoutReason::DiscreteScroll => {
                        (LayoutReason::DiscreteScroll, "discrete-scroll")
                    }
                }
            } else if !self.entered {
                (LayoutReason::RebuildView, "rebuild-view")
            } else if diff.is_time_jumped {
                (LayoutReason::Seek, "seek")
            } else if playing_flipped || size_changed {
                (LayoutReason::ConfigChange, "config-change")
            } else {
                (LayoutReason::PlaybackTick, "playback-tick")
            };
            self.calc_layout(reason, self.position_ms, trace_reason);
            self.layout_dirty = false;
        }

        // 5) 弹簧与补间推进(float 反演按帧增量累加墙钟,不渲染即暂停)
        for spring in &mut self.pos_y {
            spring.update(delta);
        }
        self.dots_y.update(delta);
        for spring in &mut self.scale {
            spring.update(delta);
        }
        for phase in &mut self.float_phase {
            if let crate::words::FloatPhase::Reversing(reverse) = phase {
                reverse.elapsed_ms += delta * 1000.0;
            }
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
            let as_: Vec<f64> = self
                .alpha_bright
                .iter()
                .map(|tween| tween.sample(now))
                .collect();
            let a_darks: Vec<f64> = self
                .alpha_dark
                .iter()
                .map(|tween| tween.sample(now))
                .collect();
            let hl: Vec<usize> = highlighted.to_vec();
            let ops: Vec<f64> = (0..count)
                .map(|index| self.line_opacity[index].sample(now))
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
            (ys, ss, as_, a_darks, ops, blurs, hl)
        });
        // 焦点行各词元的揭示边缘(与浏览器 harness 的 reveal 采样对齐比对)
        let fade = self.font_size * crate::words::FADE_EM;
        let pad = self.font_size * crate::words::WORD_PAD_EM;
        let probe_line = self.timeline.scroll_to_index();
        let reveal: Vec<serde_json::Value> = self
            .word_spans
            .get(probe_line)
            .map(|spans| {
                spans
                    .iter()
                    .enumerate()
                    .map(|(i, span)| {
                        serde_json::json!({
                            "e": crate::words::reveal_edge(spans, i, self.position_ms, fade, pad),
                            "w": span.width,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        if let (Some(trace), Some((ys, ss, as_, a_darks, ops, blurs, hl))) =
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
                "reveal": reveal,
                "k": self.current_stiffness,
                "d": self.current_damping,
                "so": self.scroll_engine.offset(),
                "interludeY": self.last_interlude_y,
                "y": ys,
                "s": ss,
                "a": as_,
                "aDark": a_darks,
                "op": ops,
                "blur": blurs,
            }));
            if let (Some(pixel_trace), Some(pixels)) = (&mut self.pixel_trace, captured_pixels) {
                pixel_trace.record(pixels, prev_media, wall_ms, &mut trace.recorder);
            }
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

        // 7) 渲染可见行:布局完全静态——所有行恒定叠放在布局原点,
        // 滚动偏移(posY)与缩放都经绘制期矩阵应用(与上游 CSS
        // transform: translateY + scale 的机制一致)。字形绘制位置
        // 逐帧不变,光栅天然稳定。
        let font = self.font_size;
        let line_box = font * LINE_HEIGHT_EM;
        let text_width = (self.container_width - font * TEXT_INSET_X_EM * 2.0).max(0.0);
        let pad_y = font * WRAPPER_PAD_EM;
        let inset_x = font * TEXT_INSET_X_EM;
        let line_total = font * LINE_TOTAL_EM;
        let motion_buffer = self.container_height * MOTION_BUFFER_RATIO;
        let top_bound = -(OVERSCAN_PX + motion_buffer);
        let bottom_bound = self.container_height + OVERSCAN_PX + motion_buffer;

        let mut children: Vec<gpui_kit::AnyElement> = Vec::new();
        // 点击映射表:各行的可视纵带与其起始时间(行本体不再承载事件)
        let mut click_bands: Vec<(f64, f64, f64)> = Vec::new();
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
            click_bands.push((y, y + line_total, line.start_time_ms));

            // 退激活收尾:alpha 补间收敛后解除遮罩冻结;float 反演完成后
            // 进入 Sunk(保持落回,上游 fill: both),直到重新激活——
            // 若此处回退到媒体驱动,已唱完的词会瞬跳回全浮起位置
            let alpha_settled = !self.alpha_bright[index].animating(now)
                && !self.alpha_dark[index].animating(now);
            if alpha_settled {
                self.mask_frozen[index] = None;
            }
            match self.float_phase[index] {
                crate::words::FloatPhase::Reversing(reverse)
                    if alpha_settled
                        && crate::words::float_reverse_finished(&self.word_spans[index], &reverse) =>
                {
                    self.float_phase[index] = crate::words::FloatPhase::Sunk;
                }
                _ => {}
            }

            // 逐词揭示元素:亮暗 alpha 与行 opacity 都经补间传入——
            // 激活/退激活的亮度过渡、退激活的遮罩冻结与 float 阶段
            // 全部在此闭合(元素按 alpha 是否相异自选渐变/单色路径)
            let word_element = crate::words::WordLineElement::new(
                self.word_spans[index].clone(),
                lyric_font(),
                font,
                line_box,
                self.alpha_bright[index].sample(now),
                self.alpha_dark[index].sample(now),
                self.position_ms,
                self.mask_frozen[index],
                self.float_phase[index],
            );
            // 静态文字层:位置恒为 (内缩 1em, 上留白 0.4em)
            let text_element = div()
                .absolute()
                .left(px(inset_x as f32))
                .top(px(pad_y as f32))
                .w(px(text_width as f32))
                .h(px(line_box as f32))
                .child(word_element);
            children.push(
                div()
                    .id(("lyric-line", index))
                    .absolute()
                    .left(px(0.0))
                    .top(px(0.0))
                    .w(px(self.container_width as f32))
                    .h(px(line_total as f32))
                    .opacity(self.line_opacity[index].sample(now) as f32)
                    .child(ScaledElement::new(
                        text_element,
                        scale as f32,
                        gpui::point(px(0.0), px(y as f32)),
                        gpui::point(px(0.0), px((line_box / 2.0) as f32)),
                    ))
                    .into_any_element(),
            );
        }

        // 间奏圆点行(上游 InterludeDotsEl):焦点在间奏且可展示时演出;
        // 间奏被正常结束截断时做 150ms 淡出,seek 截断则立即消失
        // 上游:圆点元素只在 canDisplay && interlude 存在时定位演出,
        // 与焦点无关(残留高亮/滚动挂起期间圆点仍在原位)
        let dots_visible =
            self.timeline.can_display_interlude() && self.timeline.active_interlude().is_some();
        if dots_visible {
            self.dots_dismiss = None;
            let interlude = self.timeline.active_interlude().expect("focus 校验过");
            let is_prelude = interlude.anchor == -1;
            let entry = self.timeline.interlude_entry_ms();
            let dots_timeline =
                crate::dots::DotsTimeline::new(interlude.start_ms, interlude.end_ms, entry, is_prelude);
            let snapshot = crate::dots::resolve_snapshot(&dots_timeline, self.position_ms);
            if snapshot.active && dots_timeline.displayable() {
                self.last_dots_state =
                    Some((snapshot.opacity, snapshot.dot_opacities, snapshot.scale));
                children.push(self.build_dots_element(
                    snapshot.opacity,
                    snapshot.scale,
                    snapshot.dot_opacities,
                ));
            } else {
                self.last_dots_state = None;
            }
        } else if let Some((start_ms, initial, dot_opacities, scale)) = self.dots_dismiss {
            // 退场淡出期(150ms):行透明度衰减,三点保持截断时的状态
            if self.position_ms < start_ms + crate::dots::DISMISS_FADE_MS {
                let opacity = crate::dots::dismiss_opacity(self.position_ms, start_ms, initial);
                children.push(self.build_dots_element(opacity, scale, dot_opacities));
            } else {
                self.dots_dismiss = None;
            }
        }

        // M5 验收:帧率叠层(右上角,不拦截交互——点击层在其后覆盖全窗)
        children.push(
            div()
                .id("fps-overlay")
                .absolute()
                .top(px(8.0))
                .right(px(12.0))
                .text_size(px(12.0))
                .text_color(gpui::hsla(0.0, 0.0, 1.0, 0.65))
                .child(SharedString::from(format!(
                    "{:.1} fps · {:.2} ms",
                    self.fps.fps(),
                    self.fps.avg_frame_ms(),
                )))
                .into_any_element(),
        );

        // 全窗口点击映射层:按点击纵坐标命中的可视行发起 seek
        children.push(
            div()
                .id("lyric-click-layer")
                .absolute()
                .size_full()
                .on_scroll_wheel(cx.listener(
                    move |this: &mut LyricPage, event: &gpui::ScrollWheelEvent, _window, cx| {
                        // gpui 传 Windows 原生符号(向前滚为正);浏览器语义
                        // 向下滚为正,取反。Pixels 为精确步进;Lines 按上游
                        // 非像素模式 ×50
                        let (delta, line_mode) = match event.delta {
                            gpui::ScrollDelta::Pixels(p) => {
                                (-(p.y / px(1.0)) as f64, false)
                            }
                            gpui::ScrollDelta::Lines(l) => (-(l.y) as f64, true),
                        };
                        let interaction_started = this.scroll_engine.wheel(delta, line_mode);
                        if interaction_started {
                            this.is_auto_align_suspended = true;
                            this.is_touch_scrolled = false;
                        }
                        this.scroll_dirty =
                            Some(crate::scroll::ScrollLayoutReason::DiscreteScroll);
                        cx.notify();
                    },
                ))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(
                        move |this: &mut LyricPage, event: &gpui::MouseDownEvent, _window, _cx| {
                            let click_y = (event.position.y / px(1.0)) as f64;
                            for (top, bottom, start_ms) in &click_bands {
                                if click_y >= *top && click_y < *bottom {
                                    this.send_seek(start_ms / 1000.0);
                                    break;
                                }
                            }
                        },
                    ),
                )
                .into_any_element(),
        );

        div()
            .id("lyric-page")
            .relative()
            .size_full()
            .bg(rgba(0x0a0a0cff))
            .children(children)
    }
}
