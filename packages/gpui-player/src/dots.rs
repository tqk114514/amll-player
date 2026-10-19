//! 间奏圆点演出(上游 `lyric-player/base/interlude-dots.ts` 的移植)。
//!
//! 生命周期:入场淡入(行 180ms)→ 三点错峰淡入(各 750ms、间隔 80ms,
//! t² 曲线)→ 逐点点亮(breathe 模式按点亮曲线依次亮,第三点目标较低;
//! fallback-hold 短间奏直接全亮)→ 呼吸缩放(容器绕中心 1.0↔1.25,
//! 呼吸进度曲线)→ 退场(两段缩放 + 250ms 淡出)→ 间奏被截断时 150ms 淡出。
//!
//! 上游用 CSS `mask` 相关演出与 DOM transform;此处输出纯数值快照,
//! 渲染由调用方(lyric_page)完成。
//!
//! 圆点的渲染(DotsElement):三颗 SDF 几何圆(quad 圆角=半径,等价
//! 上游 span + border-radius: 50%),呼吸/退场的缩放与 posY 弹簧全部
//! 经绘制期矩阵应用——本地 patch 让 quad 携带变换矩阵(顶点变换 +
//! 像素着色器逆矩阵回局部空间求 SDF),变换作用域内不做像素吸附,
//! 慢速呼吸(≈0.24px/帧)不再出现 1px 级跳变的微动。

/// 退场总时长(EXIT_PHASE1 + EXIT_PHASE2)
const EXIT_TOTAL_MS: f64 = 1000.0;
/// 淡入后、点亮前的保持(前奏间奏无此延迟)
const ENTER_HOLD_MS: f64 = 500.0;
/// 容器行入场淡入时长
const ENTER_FADE_MS: f64 = 180.0;
/// 单颗圆点入场淡入时长
const DOT_ENTER_FADE_MS: f64 = 750.0;
/// 圆点之间的错峰间隔
const DOT_ENTER_STAGGER_MS: f64 = 80.0;
/// 三颗圆点完全淡入所需总时长(80×2+750)
pub const DOT_ENTER_TOTAL_MS: f64 = 910.0;
const BREATHE_BASE_PERIOD_MS: f64 = 4000.0;
const FALLBACK_HOLD_THRESHOLD_MS: f64 = 3000.0;
const BREATHE_MAX_SCALE: f64 = 1.25;
const DOT_INACTIVE_OPACITY: f64 = 0.2;
const EXIT_PHASE1_MS: f64 = 750.0;
const EXIT_PHASE2_MS: f64 = 250.0;
/// 退场末段的整体淡出时长
const EXIT_FADE_MS: f64 = 250.0;
/// 第三颗圆点点亮后的拖尾
const DOT3_TRAILING_MS: f64 = 750.0;
/// 间奏被截断时的整体淡出
pub const DISMISS_FADE_MS: f64 = 150.0;
/// 圆点行的左右留白(上游 var(--lyric-line-padding-x) = 1em,与行文字同源)
pub const WORD_PAD_EM: f64 = 1.0;
/// 圆点直径(上游 .3em)
pub const DOT_SIZE_EM: f64 = 0.3;
/// 圆点间距(上游 gap .18em)
pub const DOT_GAP_EM: f64 = 0.18;
/// 圆点行内容的上下留白(上游 padding .4em,即 dotMargin)
pub const DOT_MARGIN_EM: f64 = 0.4;

fn lighting_easing() -> impl Fn(f64) -> f64 {
    crate::ease::css_bezier(0.56, 0.01, 0.45, 1.0)
}

fn enter_fade_easing() -> impl Fn(f64) -> f64 {
    crate::ease::css_bezier(0.59, 0.02, 0.07, 1.0)
}

fn exit_phase1_easing() -> impl Fn(f64) -> f64 {
    crate::ease::css_bezier(0.14, 0.06, 0.25, 1.0)
}

fn exit_phase2_easing() -> impl Fn(f64) -> f64 {
    crate::ease::css_bezier(0.29, 0.03, 1.0, 0.38)
}

fn exit_fade_easing() -> impl Fn(f64) -> f64 {
    crate::ease::css_bezier(0.43, 0.08, 0.83, 0.31)
}

fn clamp01(v: f64) -> f64 {
    v.clamp(0.0, 1.0)
}

/// 容器呼吸式缩放的进度曲线(上游 breathingProgress)
fn breathing_progress(t: f64) -> f64 {
    if t <= 0.0 {
        return 0.0;
    }
    if t >= 1.0 {
        return 1.0;
    }
    let angle = 4.0 * std::f64::consts::PI * t;
    let s = angle.sin();
    let c = angle.cos();
    t - 0.084 * s + 0.008 * (1.0 - c) + 0.0046 * s * (c - s)
}

/// 点亮分数 → 圆点透明度:未点亮 0.2 与全亮 0.9 之间线性插值
fn dot_opacity(fraction: f64) -> f64 {
    DOT_INACTIVE_OPACITY + 0.7 * clamp01(fraction)
}

/// 容器行入场淡入系数
fn enter_opacity(internal_ms: f64) -> f64 {
    enter_fade_easing()(clamp01(internal_ms / ENTER_FADE_MS))
}

/// 单颗圆点的入场淡入系数(错峰 + t²)
fn dot_enter_alpha(index: usize, internal_ms: f64) -> f64 {
    let t = clamp01((internal_ms - index as f64 * DOT_ENTER_STAGGER_MS) / DOT_ENTER_FADE_MS);
    t * t
}

/// 点亮分数:开始延迟后按点亮曲线推进至目标
fn compute_dot_fraction(start_delay: f64, duration: f64, internal_ms: f64, target: f64) -> f64 {
    if internal_ms <= start_delay {
        return 0.0;
    }
    let local_t = (internal_ms - start_delay) / duration;
    lighting_easing()(clamp01(local_t)) * target
}

/// 一场圆点演出的时间轴参数(上游 setInterlude 时计算)
#[derive(Debug, Clone)]
pub struct DotsTimeline {
    /// 演出锚点(自然进入 = 间奏开始;seek 进入 = 进入时刻)
    pub anchor_ms: f64,
    delay_end_ms: f64,
    body_end_ms: f64,
    total_end_ms: f64,
    breathe_period_ms: f64,
    segment_ms: f64,
    dot3_duration_ms: f64,
    dot3_target: f64,
    fallback_hold: bool,
}

impl DotsTimeline {
    /// 按间奏区间与进入方式推导时间轴;body 不足 DOT_ENTER_TOTAL 时
    /// 不可展示(上游返回 false,由调用方判定 can_display)
    pub fn new(_interlude_start_ms: f64, interlude_end_ms: f64, entry_ms: f64, is_prelude: bool) -> Self {
        let delay_end = if is_prelude { 0.0 } else { ENTER_HOLD_MS };
        let body = (interlude_end_ms - entry_ms) - delay_end - EXIT_TOTAL_MS;
        let fallback_hold = body < FALLBACK_HOLD_THRESHOLD_MS;
        let breathe_cycles = (body / BREATHE_BASE_PERIOD_MS).floor().max(1.0);
        let breathe_period = if body > 0.0 { body / breathe_cycles } else { 1.0 };
        let segment = ((body + DOT3_TRAILING_MS) / 3.0).round();
        let dot3_duration = (body - segment * 2.0).max(0.0);
        let dot3_target = if segment > 0.0 { dot3_duration / segment } else { 0.0 };
        Self {
            anchor_ms: entry_ms,
            delay_end_ms: delay_end,
            body_end_ms: delay_end + body,
            total_end_ms: delay_end + body + EXIT_TOTAL_MS,
            breathe_period_ms: breathe_period,
            segment_ms: segment,
            dot3_duration_ms: dot3_duration,
            dot3_target: dot3_target.clamp(0.0, 1.0),
            fallback_hold,
        }
    }

    /// 该时间轴是否具备展示条件(上游 bodyMs ≥ DOT_ENTER_TOTAL_MS)
    pub fn displayable(&self) -> bool {
        self.body_end_ms - self.delay_end_ms >= DOT_ENTER_TOTAL_MS
    }
}

/// 一帧的圆点视觉状态
#[derive(Debug, Clone, Copy)]
pub struct DotsSnapshot {
    pub active: bool,
    /// 容器行透明度
    pub opacity: f64,
    /// 容器呼吸/退场缩放(绕中心)
    pub scale: f64,
    /// 三颗圆点各自的透明度
    pub dot_opacities: [f64; 3],
}

impl Default for DotsSnapshot {
    fn default() -> Self {
        Self {
            active: false,
            opacity: 0.0,
            scale: 1.0,
            dot_opacities: [0.0; 3],
        }
    }
}

/// 按当前媒体时间推导圆点快照(上游 resolveSnapshot)
pub fn resolve_snapshot(timeline: &DotsTimeline, media_ms: f64) -> DotsSnapshot {
    let elapsed = media_ms - timeline.anchor_ms;
    let mut snapshot = DotsSnapshot::default();
    if elapsed >= timeline.total_end_ms || elapsed < 0.0 {
        return snapshot;
    }
    snapshot.active = true;
    if elapsed < timeline.delay_end_ms {
        return snapshot;
    }
    let internal = elapsed - timeline.delay_end_ms;
    let write_dots = |snapshot: &mut DotsSnapshot, fractions: [f64; 3]| {
        for (index, fraction) in fractions.into_iter().enumerate() {
            snapshot.dot_opacities[index] =
                dot_opacity(fraction) * dot_enter_alpha(index, internal);
        }
    };
    if elapsed >= timeline.body_end_ms {
        // 退场:第三点拖尾补亮 + 两段缩放 + 末段淡出
        let exit_elapsed = elapsed - timeline.body_end_ms;
        let fade_t = clamp01((exit_elapsed - EXIT_PHASE1_MS) / EXIT_FADE_MS);
        snapshot.opacity = enter_opacity(internal) * (1.0 - exit_fade_easing()(fade_t));
        if exit_elapsed < EXIT_PHASE1_MS {
            snapshot.scale = 1.0 + exit_phase1_easing()(exit_elapsed / EXIT_PHASE1_MS) * 0.25;
        } else {
            let phase2 = clamp01((exit_elapsed - EXIT_PHASE1_MS) / EXIT_PHASE2_MS);
            snapshot.scale = BREATHE_MAX_SCALE - exit_phase2_easing()(phase2) * 0.85;
        }
        let trailing = clamp01(exit_elapsed / DOT3_TRAILING_MS);
        let target = timeline.dot3_target;
        write_dots(
            &mut snapshot,
            [1.0, 1.0, target + (1.0 - target) * trailing],
        );
        return snapshot;
    }
    snapshot.opacity = enter_opacity(internal);
    if timeline.fallback_hold {
        snapshot.scale = 1.0;
        write_dots(&mut snapshot, [1.0, 1.0, 1.0]);
        return snapshot;
    }
    // breathe:容器呼吸缩放 + 逐点点亮
    let progress = breathing_progress(
        (internal % timeline.breathe_period_ms) / timeline.breathe_period_ms,
    );
    snapshot.scale = if progress <= 0.5 {
        1.0 + progress / 0.5 * 0.25
    } else {
        BREATHE_MAX_SCALE - (progress - 0.5) / 0.5 * 0.25
    };
    let segment = timeline.segment_ms;
    write_dots(
        &mut snapshot,
        [
            compute_dot_fraction(0.0, segment, internal, 1.0),
            compute_dot_fraction(segment, segment, internal, 1.0),
            compute_dot_fraction(
                segment * 2.0,
                timeline.dot3_duration_ms,
                internal,
                timeline.dot3_target,
            ),
        ],
    );
    snapshot
}

/// 间奏被截断时的整体淡出(上游 dismiss / updateFadeOut):
/// `dismiss_start_ms` 为截断时刻,`initial` 为截断时的透明度
pub fn dismiss_opacity(media_ms: f64, dismiss_start_ms: f64, initial: f64) -> f64 {
    let progress = clamp01((media_ms - dismiss_start_ms) / DISMISS_FADE_MS);
    initial * (1.0 - exit_fade_easing()(progress))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timeline(start: f64, end: f64, entry: f64, prelude: bool) -> DotsTimeline {
        DotsTimeline::new(start, end, entry, prelude)
    }

    #[test]
    fn bezier_endpoints_and_monotonicity() {
        let ease = enter_fade_easing();
        assert!((ease(0.0)).abs() < 1e-9);
        assert!((ease(1.0) - 1.0).abs() < 1e-9);
        // 单调递增
        let mut prev = 0.0;
        for k in 1..=20 {
            let v = ease(k as f64 / 20.0);
            assert!(v >= prev, "缓动应单调递增");
            prev = v;
        }
    }

    #[test]
    fn dots_hold_then_hidden_after_total() {
        let t = timeline(10000.0, 30000.0, 10000.0, false);
        assert!(t.displayable());
        // 入场保持期(500ms):isActive 但完全透明(上游语义)
        let before = resolve_snapshot(&t, 10400.0);
        assert!(before.active, "保持期已激活");
        assert!(before.opacity.abs() < 1e-9, "保持期不显示");
        let after = resolve_snapshot(&t, 32000.0);
        assert!(!after.active, "演出结束后不显示");
    }

    #[test]
    fn dots_enter_staggered_and_light_sequentially() {
        let t = timeline(10000.0, 30000.0, 10000.0, false);
        // 内部 500ms:三点的错峰淡入清晰可见
        let s = resolve_snapshot(&t, 11000.0);
        assert!(s.active);
        assert!(s.dot_opacities[0] > s.dot_opacities[1], "点1 先于点2 淡入");
        assert!(s.dot_opacities[1] > s.dot_opacities[2], "点2 先于点3 淡入");
        // 点3 尚在入场淡入,远未点亮(点亮分数为 0 → 底透明度×入场系数)
        assert!(s.dot_opacities[2] < DOT_INACTIVE_OPACITY, "点3 未点亮");
    }

    #[test]
    fn prelude_has_no_enter_hold() {
        let t = timeline(0.0, 10000.0, 0.0, true);
        assert!((t.delay_end_ms).abs() < 1e-9, "前奏无入场保持");
        let s = resolve_snapshot(&t, 100.0);
        assert!(s.active, "前奏立即开始演出");
    }

    #[test]
    fn short_interlude_falls_back_to_hold() {
        // body < 3000ms:三点直接全亮,不呼吸
        let t = timeline(0.0, 3900.0, 0.0, true);
        assert!(t.displayable(), "body=2900 ≥ 910 可展示");
        let s = resolve_snapshot(&t, 2500.0);
        assert!((s.scale - 1.0).abs() < 1e-9, "fallback-hold 不呼吸");
        let lit: Vec<f64> = s.dot_opacities.to_vec();
        assert!(lit.iter().all(|&a| a > 0.8), "短间奏三点保持全亮: {lit:?}");
    }

    #[test]
    fn exit_fades_row_and_shrinks() {
        let t = timeline(10000.0, 30000.0, 10000.0, false);
        // body = 20000-500-1000 = 18500;body_end = 19000;total_end = 20000
        let exiting = resolve_snapshot(&t, 29100.0);
        assert!(exiting.active, "退场期仍在演出");
        assert!(exiting.scale > 1.0, "退场第一阶段放大: {}", exiting.scale);
        let near_end = resolve_snapshot(&t, 29995.0);
        assert!(near_end.opacity < 1.0, "退场末段淡出: {}", near_end.opacity);
        assert!(near_end.scale < 1.0, "退场第二阶段回落: {}", near_end.scale);
    }

    #[test]
    fn dismiss_fades_from_initial_opacity() {
        let a = dismiss_opacity(1000.0, 900.0, 0.9);
        assert!(a < 0.9 && a > 0.0);
        let done = dismiss_opacity(2000.0, 900.0, 0.9);
        assert!(done.abs() < 1e-6, "150ms 后完全消失");
    }
}


/// 间奏圆点元素:三颗 SDF 几何圆 + 绘制期矩阵。
///
/// 矩阵语义 p' = C + s·(p − C_local):C 为实际群中心(含 posY 弹簧、
/// 呼吸缩放的中心),C_local 为窗口内固定的局部群中心——未变换的
/// quad 恒落在内容遮罩内(局部裁剪可过),每帧只有矩阵变化,
/// 几何恒定、亚像素运动经顶点矩阵平滑。
pub struct DotsElement {
    /// 未缩放圆心,相对局部群中心(常量;y 即群中心高度)
    local_centers: [(f64, f64); 3],
    /// 圆点直径(逻辑像素,未缩放;上游 .3em)
    dot_diameter: f64,
    /// 局部群中心(窗口坐标,常量)
    local_group_center: (f64, f64),
    /// 实际群中心(逻辑像素,窗口坐标;含 posY 弹簧)
    group_center: (f64, f64),
    /// 容器缩放(呼吸/退场)
    scale: f64,
    /// 容器透明度(含 dismiss 衰减)
    opacity: f64,
    /// 三颗圆点各自透明度
    dot_opacities: [f64; 3],
}

impl DotsElement {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        local_centers: [(f64, f64); 3],
        dot_diameter: f64,
        local_group_center: (f64, f64),
        group_center: (f64, f64),
        scale: f64,
        opacity: f64,
        dot_opacities: [f64; 3],
    ) -> Self {
        Self {
            local_centers,
            dot_diameter,
            local_group_center,
            group_center,
            scale,
            opacity,
            dot_opacities,
        }
    }
}

impl gpui::Element for DotsElement {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<gpui::ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&gpui::GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut gpui::Window,
        cx: &mut gpui::App,
    ) -> (gpui::LayoutId, Self::RequestLayoutState) {
        // 仅作绘制载体:几何全部经矩阵应用,元素自身占位 0×0
        (window.request_layout(gpui::Style::default(), [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&gpui::GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        _bounds: gpui::Bounds<gpui::Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _window: &mut gpui::Window,
        _cx: &mut gpui::App,
    ) -> Self::PrepaintState {
    }

    fn paint(
        &mut self,
        _id: Option<&gpui::GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        _bounds: gpui::Bounds<gpui::Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut gpui::Window,
        _cx: &mut gpui::App,
    ) {
        let scale_factor = window.scale_factor() as f64;
        let s = self.scale as f32;
        let local_x = (self.local_group_center.0 * scale_factor) as f32;
        let local_y = (self.local_group_center.1 * scale_factor) as f32;
        let center_x = (self.group_center.0 * scale_factor) as f32;
        let center_y = (self.group_center.1 * scale_factor) as f32;
        let matrix = gpui::TransformationMatrix {
            rotation_scale: [[s, 0.0], [0.0, s]],
            translation: [center_x - s * local_x, center_y - s * local_y],
        };
        let radius = gpui::px((self.dot_diameter / 2.0) as f32);
        window.with_element_transformation(matrix, |window| {
            for (index, alpha) in self.dot_opacities.iter().enumerate() {
                let (dx, dy) = self.local_centers[index];
                let bounds = gpui::Bounds {
                    origin: gpui::point(
                        gpui::px((self.local_group_center.0 + dx) as f32) - radius,
                        gpui::px((self.local_group_center.1 + dy) as f32) - radius,
                    ),
                    size: gpui::size(radius * 2.0, radius * 2.0),
                };
                let alpha = (self.opacity * alpha) as f32;
                window.paint_quad(gpui::quad(
                    bounds,
                    gpui::Corners::all(radius),
                    gpui::hsla(0.0, 0.0, 1.0, alpha),
                    gpui::Edges::default(),
                    gpui::transparent_black(),
                    gpui::BorderStyle::default(),
                ));
            }
        });
    }
}

impl gpui::IntoElement for DotsElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}
