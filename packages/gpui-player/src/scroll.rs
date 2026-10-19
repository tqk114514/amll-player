//! 上游 `lyric-player/base/scroll.ts` 的 `ScrollInteractionEngine` 移植
//! (状态机部分;事件翻译由调用方接入)。
//!
//! 语义(上游原样):
//! - 触摸:突破 10px 意图阈值才算滑动意图;offset = startOffset − Δy
//!   (边界内钳制);速度按 px/ms 逐事件跟踪;松手 |speed| > 0.05 起惯性,
//!   单帧 `offset −= speed·Δt`、`speed ×= 0.95^(Δt/16.67)`,衰减到
//!   ≤ 0.05 结束;小于 0.1 的松手速度直接归零
//! - 滚轮:离散步进(像素模式直加,行模式 ×50),150ms 防抖窗口内持续
//!   交互;滚轮会打断进行中的惯性
//! - 触摸按下打断惯性/滚轮计时;若本次触摸未确认意图(轻点),
//!   恢复"被打断前有交互"的收尾语义
//! - 滚动结束(墙钟)`scroll_end_at`,500ms 后 `can_resume_auto_align`
//! - `update_boundary(min, max)`:min/max 各向 0 收拢,offset 钳制其中
//!
//! 时刻由调用方注入(墙钟毫秒),便于确定性测试;
//! 惯性与防抖由 render 循环逐帧 `advance` 驱动。

/// 滚动静止后允许恢复自动对齐的等待时间
pub const AUTO_ALIGN_RESUME_DELAY_MS: f64 = 500.0;
/// 滚轮交互结束防抖时间
const WHEEL_IDLE_TIMEOUT_MS: f64 = 150.0;
/// 惯性动画基准物理帧时间(1000/60)
const BASE_FRAME_DURATION_MS: f64 = 1000.0 / 60.0;
/// 惯性单帧最大跨度阈值,超过视为掉帧丢弃该步进
const MAX_INERTIA_FRAME_DELTA_MS: f64 = 100.0;
/// 触摸滑动意图阈值(px)
const INTENT_THRESHOLD_PX: f64 = 10.0;
/// 惯性停止速度阈值(px/ms)
const SPEED_STOP: f64 = 0.05;
/// 小于该值的松手速度直接归零
const SPEED_ZERO_ON_RELEASE: f64 = 0.1;

/// 输入类型(上游 ScrollInputType;触摸分支接线后用于 onInteractionStart)
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollInput {
    Touch,
    Wheel,
}

/// 触摸移动结果(触摸分支接线后由事件循环消费)
#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
pub struct TouchMoveResult {
    /// 意图确认且 offset 已更新
    pub moved: bool,
    /// 本次调用首次确认滑动意图(应触发挂起 + InteractionStart 排版)
    pub intent_started: bool,
}

/// `advance` 的产出(continuous/active 供触摸接线后的事件循环消费,
/// 现由测试锁定语义)
#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
pub struct ScrollAdvance {
    /// 本帧滚动内容有更新(触摸/惯性为真;滚轮步进在 `wheel` 中即时应用)
    pub updated: bool,
    /// 是否为连续输入(触摸/惯性 RAF;滚轮离散步进为假,保留弹簧平滑)
    pub continuous: bool,
    /// 惯性/防抖仍在进行,需要继续请求动画帧
    pub active: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Phase {
    /// 静止(scroll_end_at 表达 500ms 恢复窗口)
    Idle,
    /// 触摸按下未确认意图
    TouchPending,
    /// 触摸拖动中(意图已确认)
    TouchDragging,
    /// 滚轮防抖窗口内
    Wheeling,
    /// 松手惯性中
    Inertia,
}

/// 滚动交互状态机(上游 ScrollInteractionEngine)
pub struct ScrollEngine {
    offset: f64,
    min_offset: f64,
    max_offset: f64,
    phase: Phase,
    start_x: f64,
    start_y: f64,
    last_y: f64,
    start_offset: f64,
    /// 触摸速度(px/ms,正值 = 手指向下移 = 视口向上走)
    speed: f64,
    /// 最近一次触摸事件的时刻(速度跟踪用)
    last_timestamp_ms: f64,
    wheel_idle_ms: f64,
    scroll_end_at: Option<f64>,
    interacting: bool,
    /// 触摸按下时是否打断了既有交互(惯性/滚轮窗)
    interrupted: bool,
}

impl Default for ScrollEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl ScrollEngine {
    pub fn new() -> Self {
        Self {
            offset: 0.0,
            min_offset: 0.0,
            max_offset: 0.0,
            phase: Phase::Idle,
            start_x: 0.0,
            start_y: 0.0,
            last_y: 0.0,
            start_offset: 0.0,
            speed: 0.0,
            last_timestamp_ms: 0.0,
            wheel_idle_ms: 0.0,
            scroll_end_at: None,
            interacting: false,
            interrupted: false,
        }
    }

    pub fn offset(&self) -> f64 {
        self.offset
    }

    /// 惯性/滚轮窗/触摸仍在进行(需要继续请求动画帧)
    pub fn is_active(&self) -> bool {
        !matches!(self.phase, Phase::Idle)
    }

    /// 触摸开始:打断惯性/滚轮计时,意图重新判定;
    /// 多指场景由调用方以最新手指位置再次调用(上游重锚定)
    #[allow(dead_code)] // 触摸分支:上游状态机完整移植,平台暴露原始触摸后接线
    pub fn touch_start(&mut self, x: f64, y: f64, now_ms: f64) {
        self.interrupted = self.phase == Phase::Inertia || self.phase == Phase::Wheeling;
        self.phase = Phase::TouchPending;
        self.start_x = x;
        self.start_y = y;
        self.last_y = y;
        self.start_offset = self.offset;
        self.speed = 0.0;
        self.last_timestamp_ms = now_ms;
        self.wheel_idle_ms = 0.0;
    }

    /// 触摸移动
    #[allow(dead_code)] // 触摸分支:上游状态机完整移植,平台暴露原始触摸后接线
    pub fn touch_move(&mut self, x: f64, y: f64, now_ms: f64) -> TouchMoveResult {
        if self.phase == Phase::TouchPending {
            let confirmed = (y - self.start_y).abs() > INTENT_THRESHOLD_PX
                || (x - self.start_x).abs() > INTENT_THRESHOLD_PX;
            if !confirmed {
                return TouchMoveResult { moved: false, intent_started: false };
            }
            self.phase = Phase::TouchDragging;
            self.scroll_end_at = None;
            self.interacting = true;
            self.apply_touch_position(y);
            self.track_touch_speed(y, now_ms);
            return TouchMoveResult { moved: true, intent_started: true };
        }
        if self.phase != Phase::TouchDragging {
            return TouchMoveResult { moved: false, intent_started: false };
        }
        self.apply_touch_position(y);
        self.track_touch_speed(y, now_ms);
        TouchMoveResult { moved: true, intent_started: false }
    }

    fn apply_touch_position(&mut self, y: f64) {
        self.offset = self.clamp(self.start_offset - (y - self.start_y));
    }

    fn track_touch_speed(&mut self, y: f64, now_ms: f64) {
        let dt = now_ms - self.last_timestamp_ms;
        if dt > 0.0 {
            self.speed = (y - self.last_y) / dt;
        }
        self.last_y = y;
        self.last_timestamp_ms = now_ms;
    }

    /// 触摸结束。`remaining_fingers > 0` 时重锚到剩余手指不触发惯性。
    /// 返回是否开启了惯性。
    #[allow(dead_code)] // 触摸分支:上游状态机完整移植,平台暴露原始触摸后接线
    pub fn touch_end(&mut self, x: f64, y: f64, now_ms: f64, remaining_fingers: usize) -> bool {
        if remaining_fingers > 0 {
            self.start_x = x;
            self.start_y = y;
            self.last_y = y;
            self.start_offset = self.offset;
            self.speed = 0.0;
            self.last_timestamp_ms = now_ms;
            return false;
        }
        match self.phase {
            Phase::TouchPending => {
                // 未确认意图(轻点):若按下时打断了交互,则按上游恢复收尾
                self.phase = Phase::Idle;
                if self.interrupted {
                    self.end_interaction(now_ms);
                }
                self.interrupted = false;
                false
            }
            Phase::TouchDragging => {
                self.phase = Phase::Idle;
                if self.speed.abs() < SPEED_ZERO_ON_RELEASE {
                    self.speed = 0.0;
                }
                if self.speed.abs() > SPEED_STOP {
                    self.phase = Phase::Inertia;
                    true
                } else {
                    self.end_interaction(now_ms);
                    false
                }
            }
            _ => false,
        }
    }

    /// 触摸取消(未确认意图的收尾,等价 touch_end 的轻点分支)
    #[allow(dead_code)] // 触摸分支:上游状态机完整移植,平台暴露原始触摸后接线
    pub fn touch_cancel(&mut self, now_ms: f64) {
        self.phase = Phase::Idle;
        self.speed = 0.0;
        self.start_y = 0.0;
        if self.interrupted {
            self.end_interaction(now_ms);
        }
        self.interrupted = false;
    }

    /// 滚轮步进(打断惯性)。`delta_y` 为浏览器语义:向下滚为正。
    /// `line_mode` 对应非 DOM_DELTA_PIXEL(上游 ×50)。
    /// 返回是否首次进入交互(应触发挂起 + InteractionStart)。
    #[allow(clippy::too_many_arguments)]
    pub fn wheel(&mut self, delta_y: f64, line_mode: bool) -> bool {
        self.phase = Phase::Wheeling;
        self.wheel_idle_ms = WHEEL_IDLE_TIMEOUT_MS;
        self.scroll_end_at = None;
        let was_interacting = self.interacting;
        self.interacting = true;
        self.offset = self.clamp(
            self.offset
                + if line_mode {
                    delta_y * 50.0
                } else {
                    delta_y
                },
        );
        !was_interacting
    }

    /// 每帧推进:惯性物理与滚轮防抖倒计时
    pub fn advance(&mut self, delta_ms: f64, now_ms: f64) -> ScrollAdvance {
        match self.phase {
            Phase::Inertia => {
                if delta_ms <= 0.0 || delta_ms > MAX_INERTIA_FRAME_DELTA_MS {
                    // 掉帧/挂起恢复:丢弃该步进,保持状态
                    return ScrollAdvance { updated: false, continuous: true, active: true };
                }
                if self.speed.abs() > SPEED_STOP {
                    self.offset = self.clamp(self.offset - self.speed * delta_ms);
                    self.speed *= 0.95_f64.powf(delta_ms / BASE_FRAME_DURATION_MS);
                    ScrollAdvance { updated: true, continuous: true, active: true }
                } else {
                    self.phase = Phase::Idle;
                    self.end_interaction(now_ms);
                    ScrollAdvance { updated: false, continuous: true, active: false }
                }
            }
            Phase::Wheeling => {
                self.wheel_idle_ms -= delta_ms;
                if self.wheel_idle_ms <= 0.0 {
                    self.phase = Phase::Idle;
                    self.end_interaction(now_ms);
                }
                ScrollAdvance { updated: false, continuous: false, active: self.phase == Phase::Wheeling }
            }
            _ => ScrollAdvance { updated: false, continuous: false, active: false },
        }
    }

    /// 滚动结束是否已超过恢复等待时间
    pub fn can_resume_auto_align(&self, now_ms: f64) -> bool {
        self.scroll_end_at
            .is_some_and(|end| now_ms - end >= AUTO_ALIGN_RESUME_DELAY_MS)
    }

    /// 更新滚动边界并返回钳制后的实际 offset(上游 updateBoundary)
    pub fn update_boundary(&mut self, min: f64, max: f64) -> f64 {
        self.min_offset = min.min(0.0);
        self.max_offset = max.max(0.0);
        self.offset = self.clamp(self.offset);
        self.offset
    }

    /// 覆盖偏移并终止惯性/防抖(Seek、恢复自动对齐、重建视图时)
    pub fn reset_scroll(&mut self, target_offset: f64) {
        self.phase = Phase::Idle;
        self.wheel_idle_ms = 0.0;
        self.scroll_end_at = None;
        self.interacting = false;
        self.interrupted = false;
        self.speed = 0.0;
        self.start_y = 0.0;
        self.last_y = 0.0;
        self.start_x = 0.0;
        self.offset = self.clamp(target_offset);
    }

    fn end_interaction(&mut self, now_ms: f64) {
        self.interacting = false;
        self.scroll_end_at = Some(now_ms);
    }

    fn clamp(&self, value: f64) -> f64 {
        value.clamp(
            self.min_offset.min(self.max_offset),
            self.min_offset.max(self.max_offset),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 逐事件喂触摸:按时间戳推进速度跟踪
    fn drag(engine: &mut ScrollEngine, from_y: f64, to_y: f64, steps: usize, now_ms: f64) {
        let step_ms = 16.0;
        engine.touch_start(0.0, from_y, now_ms);
        for i in 1..=steps {
            let y = from_y + (to_y - from_y) * i as f64 / steps as f64;
            engine.touch_move(0.0, y, now_ms + i as f64 * step_ms);
        }
    }

    #[test]
    fn touch_requires_intent_threshold() {
        let mut e = ScrollEngine::new();
        e.update_boundary(-1000.0, 1000.0);
        e.touch_start(0.0, 100.0, 0.0);
        // 9px:未确认意图
        let r = e.touch_move(0.0, 91.0, 16.0);
        assert!(!r.moved && !r.intent_started);
        // 突破 10px:确认 + 应用位移(offset = start − Δy = 100 − 89 = 11)
        let r = e.touch_move(0.0, 89.0, 32.0);
        assert!(r.moved && r.intent_started);
        assert!((e.offset() - 11.0).abs() < 1e-9);
    }

    #[test]
    fn touch_drag_clamps_to_boundary() {
        let mut e = ScrollEngine::new();
        e.update_boundary(-100.0, 200.0);
        drag(&mut e, 0.0, -500.0, 10, 0.0);
        assert!((e.offset() - 200.0).abs() < 1e-9, "上界钳制: {}", e.offset());
        drag(&mut e, 0.0, 500.0, 10, 200.0);
        assert!((e.offset() + 100.0).abs() < 1e-9, "下界钳制: {}", e.offset());
    }

    #[test]
    fn inertia_decays_and_stops() {
        let mut e = ScrollEngine::new();
        e.update_boundary(-10000.0, 10000.0);
        // 快速上滑(手指向上 = 负 y 位移 = 内容向下滚 = offset 增大)
        drag(&mut e, 500.0, 100.0, 10, 0.0);
        let started = e.touch_end(0.0, 100.0, 160.0, 0);
        assert!(started, "速度足够应起惯性");
        assert!(e.is_active());
        // 逐帧推进:offset 单调增长、速度按 0.95^(Δt/16.67) 衰减
        let mut now = 160.0;
        let mut prev_offset = e.offset();
        let mut steps = 0;
        while e.is_active() && steps < 2000 {
            now += 16.67;
            let adv = e.advance(16.67, now);
            assert!(adv.continuous);
            if adv.updated {
                assert!(e.offset() >= prev_offset, "惯性期间 offset 单调");
                prev_offset = e.offset();
            }
            steps += 1;
        }
        assert!(!e.is_active(), "惯性应终止");
        assert!(!e.can_resume_auto_align(now), "刚结束不在恢复窗口内");
        assert!(e.can_resume_auto_align(now + AUTO_ALIGN_RESUME_DELAY_MS));
    }

    #[test]
    fn slow_release_has_no_inertia() {
        let mut e = ScrollEngine::new();
        e.update_boundary(-1000.0, 1000.0);
        // 15px 位移过意图阈值,但拖了 300ms(速度 0.05~0.1 之间偏小段)
        drag(&mut e, 100.0, 85.0, 15, 0.0);
        let started = e.touch_end(0.0, 85.0, 240.0, 0);
        assert!(!started, "慢速松手不起惯性");
        assert!(!e.is_active());
    }

    #[test]
    fn wheel_steps_discretely_and_debounces() {
        let mut e = ScrollEngine::new();
        e.update_boundary(-1000.0, 1000.0);
        assert!(e.wheel(120.0, false), "首次滚轮触发交互开始");
        assert!(!e.wheel(120.0, false), "窗口内不重复触发");
        assert!((e.offset() - 240.0).abs() < 1e-9);
        // 行模式 ×50
        e.wheel(-2.0, true);
        assert!((e.offset() - 140.0).abs() < 1e-9);
        // 150ms 防抖内保持交互,之后结束并计时恢复窗口
        assert!(e.is_active());
        let _ = e.advance(149.0, 249.0);
        assert!(e.is_active(), "防抖窗口未到");
        let adv = e.advance(2.0, 251.0);
        assert!(!adv.active && !adv.updated);
        assert!(!e.is_active());
        assert!(e.can_resume_auto_align(251.0 + AUTO_ALIGN_RESUME_DELAY_MS));
    }

    #[test]
    fn update_boundary_clamps_offset_into_new_range() {
        let mut e = ScrollEngine::new();
        e.update_boundary(-1000.0, 1000.0);
        e.offset = 800.0;
        // 边界收紧:offset 被钳回,返回钳制值
        let clamped = e.update_boundary(-50.0, 100.0);
        assert!((clamped - 100.0).abs() < 1e-9);
        assert!((e.offset() - 100.0).abs() < 1e-9);
        // min/max 向 0 收拢
        e.offset = -500.0;
        assert!((e.update_boundary(-10.0, 10.0) + 10.0).abs() < 1e-9);
    }

    #[test]
    fn reset_clears_everything() {
        let mut e = ScrollEngine::new();
        e.update_boundary(-1000.0, 1000.0);
        e.wheel(500.0, false);
        e.reset_scroll(0.0);
        assert!(!e.is_active());
        assert!(!e.can_resume_auto_align(10000.0), "scroll_end 被清除");
        assert!(e.offset().abs() < 1e-9);
    }

    #[test]
    fn touching_interrupts_inertia_and_tap_restores_end() {
        let mut e = ScrollEngine::new();
        e.update_boundary(-10000.0, 10000.0);
        drag(&mut e, 500.0, 0.0, 10, 0.0);
        assert!(e.touch_end(0.0, 0.0, 160.0, 0), "起惯性");
        // 按下打断惯性
        e.touch_start(0.0, 300.0, 200.0);
        assert!(!e.is_active() || matches!(e.phase, Phase::TouchPending));
        // 轻点(无意图)结束:因打断过交互 → 收尾计时恢复窗口
        e.touch_end(0.0, 300.0, 220.0, 0);
        assert!(!e.is_active());
        assert!(!e.can_resume_auto_align(220.0));
        assert!(e.can_resume_auto_align(220.0 + AUTO_ALIGN_RESUME_DELAY_MS));
    }
}

/// 滚动触发的排版原因(上游 LayoutReason 的滚动子集)。
/// 策略映射(上游 consts.ts):三者皆禁用 stagger;
/// 仅 ContinuousScroll 直贴 Y(触摸拖动跟手),滚轮步进保留弹簧平滑。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollLayoutReason {
    /// 用户交互挂起开始(触摸/滚轮触发;滚轮与步进合并为 DiscreteScroll,
    /// 触摸接线后单独触发)
    #[allow(dead_code)]
    InteractionStart,
    /// 连续高频滚动(触摸拖动或松手后的惯性)
    ContinuousScroll,
    /// 离散单步滚动(滚轮单次滚动)
    DiscreteScroll,
}
