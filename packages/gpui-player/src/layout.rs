//! 上游 `lyric-player/base/layout.ts` 中 `LayoutCalculator` 的移植。
//!
//! 职责:行高前缀和、焦点度量(行/间奏圆点)、`viewportStartY` 推导、
//! 间奏圆点行占位(spacer)、视口裁剪标记与拖动滚动边界。
//! App 实际配置:alignAnchor=Top、alignPosition=0.25(react-full 预置)。

/// 焦点对齐锚点(上游 LayoutAlignAnchor)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlignAnchor {
    Top,
    /// App 未使用;语义完整性保留(core 默认值)
    #[allow(dead_code)]
    Center,
    #[allow(dead_code)] // 与上游对齐保留;App 未使用
    Bottom,
}

/// 排版焦点目标
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LayoutTarget {
    /// 对齐某行歌词(索引已钳制到合法范围)
    Line(usize),
    /// 对齐间奏圆点行,`anchor` 为锚点行索引(前奏时为 -1)
    Interlude(i64),
}

/// 单帧排版上下文(上游 FrameContext + LayoutConfig 的合并)
pub struct FrameContext {
    pub container_height: f64,
    pub align_position: f64,
    pub align_anchor: AlignAnchor,
    pub overscan_px: f64,
    /// 用户滚动偏移(原型 M3 恒 0,M4 拖动接入)
    pub scroll_offset: f64,
    pub target: LayoutTarget,
    /// 间奏圆点行占位:总高与锚点;仅当圆点可展示时为 Some
    pub interlude: Option<InterludeLayout>,
}

/// 间奏圆点行的占位几何
#[derive(Debug, Clone, Copy)]
pub struct InterludeLayout {
    /// 圆点行总高(圆点元素高 + 上下 margin)
    pub total_height: f64,
    /// 锚点行索引(前奏为 -1)
    pub anchor: i64,
}

#[derive(Debug, Clone, Copy)]
pub struct LineInstruction {
    pub y: f64,
    pub height: f64,
    /// 排版时刻的视口命中标记(trace 与测试消费;渲染按实时位置另行裁剪)
    #[allow(dead_code)]
    pub in_viewport: bool,
}

pub struct LayoutResult {
    pub lines: Vec<LineInstruction>,
    /// 间奏圆点行的 y(仅当间奏占位激活)
    pub interlude_y: Option<f64>,
    /// 底栏 y(原型无底栏内容,trace 记录)
    #[allow(dead_code)]
    pub bottom_y: f64,
    /// 拖动滚动安全边界(上游 beginFrame 的 bounds;M4 拖动接入)
    #[allow(dead_code)]
    pub bounds_min: f64,
    #[allow(dead_code)]
    pub bounds_max: f64,
}

/// 前缀和排版器(上游 LayoutCalculator)
pub struct LayoutCalculator {
    heights: Vec<f64>,
    prefix: Vec<f64>,
    total: f64,
}

impl LayoutCalculator {
    pub fn new(count: usize, default_height: f64) -> Self {
        Self {
            heights: vec![default_height; count],
            prefix: vec![0.0; count + 1],
            total: 0.0,
        }
    }

    pub fn set_line_height(&mut self, index: usize, height: f64) {
        if index < self.heights.len() {
            self.heights[index] = height;
        }
    }

    #[allow(dead_code)] // trace 与测试消费
    pub fn line_height(&mut self, index: usize) -> f64 {
        self.ensure_prefix();
        if index >= self.heights.len() {
            return 0.0;
        }
        self.prefix[index + 1] - self.prefix[index]
    }

    fn ensure_prefix(&mut self) {
        let mut sum = 0.0;
        self.prefix[0] = 0.0;
        for i in 0..self.heights.len() {
            sum += self.heights[i];
            self.prefix[i + 1] = sum;
        }
        self.total = sum;
    }

    /// 单帧排版(上游 beginFrame + commit 的合并;scrollOffset 已钳制)
    pub fn layout(&mut self, ctx: &FrameContext) -> LayoutResult {
        self.ensure_prefix();
        let count = self.heights.len();
        let interlude_total = ctx.interlude.map(|i| i.total_height).unwrap_or(0.0);
        let active_anchor = ctx.interlude.map(|i| i.anchor);

        // 焦点度量(updateFocalMetrics)
        let (focal_top, target_height) = match ctx.target {
            LayoutTarget::Line(index) => {
                let index = index.min(count.saturating_sub(1));
                let mut top = self.prefix[index];
                if active_anchor.is_some_and(|a| (index as i64) > a) {
                    top += interlude_total;
                }
                (top, self.prefix[index + 1] - self.prefix[index])
            }
            LayoutTarget::Interlude(anchor) => {
                // anchor 可为 -1(前奏):圆点行在 prefix[anchor+1] 处
                let after = (anchor + 1).clamp(0, count as i64) as usize;
                (self.prefix[after], interlude_total)
            }
        };
        let anchor_offset = if target_height <= 0.0 {
            0.0
        } else {
            match ctx.align_anchor {
                AlignAnchor::Top => 0.0,
                AlignAnchor::Center => target_height / 2.0,
                AlignAnchor::Bottom => target_height,
            }
        };

        // commit():viewportStartY = H·align − anchorOffset − scroll − focalTop
        let viewport_start = ctx.container_height * ctx.align_position
            - anchor_offset
            - ctx.scroll_offset
            - focal_top;

        let motion_buffer = ctx.container_height * 0.4;
        let top_bound = -(ctx.overscan_px + motion_buffer);
        let bottom_bound = ctx.container_height + ctx.overscan_px + motion_buffer;

        let mut lines = Vec::with_capacity(count);
        for i in 0..count {
            let mut y = viewport_start + self.prefix[i];
            if active_anchor.is_some_and(|a| (i as i64) > a) {
                y += interlude_total;
            }
            let height = self.prefix[i + 1] - self.prefix[i];
            let in_viewport = y <= bottom_bound && y + height >= top_bound;
            lines.push(LineInstruction { y, height, in_viewport });
        }

        let interlude_y = active_anchor.map(|a| {
            let after = (a + 1).clamp(0, count as i64) as usize;
            viewport_start + self.prefix[after]
        });
        let mut bottom_y = viewport_start + self.total;
        if active_anchor.is_some() {
            bottom_y += interlude_total;
        }

        // beginFrame() 的滚动边界
        let min_offset = 0.0_f64.min(-focal_top);
        let base_pos = -focal_top + ctx.container_height * ctx.align_position - anchor_offset;
        let raw_max = base_pos + self.total + interlude_total - ctx.container_height / 2.0;

        LayoutResult {
            lines,
            interlude_y,
            bottom_y,
            bounds_min: min_offset,
            bounds_max: raw_max.max(0.0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(target: LayoutTarget, container_height: f64) -> FrameContext {
        FrameContext {
            container_height,
            align_position: 0.25,
            align_anchor: AlignAnchor::Top,
            overscan_px: 300.0,
            scroll_offset: 0.0,
            target,
            interlude: None,
        }
    }

    #[test]
    fn top_anchor_places_focus_line_top_at_align_position() {
        let mut calc = LayoutCalculator::new(4, 100.0);
        let result = calc.layout(&ctx(LayoutTarget::Line(2), 700.0));
        assert!((result.lines[2].y - 700.0 * 0.25).abs() < 1e-9);
        // 其余行按前缀和排开
        assert!((result.lines[1].y - (result.lines[2].y - 100.0)).abs() < 1e-9);
        assert!((result.lines[3].y - (result.lines[2].y + 100.0)).abs() < 1e-9);
    }

    #[test]
    fn center_anchor_places_focus_center_at_align_position() {
        let mut calc = LayoutCalculator::new(2, 100.0);
        // App 未用 center,但语义应正确:center = align·H
        let mut c = ctx(LayoutTarget::Line(0), 700.0);
        c.align_anchor = AlignAnchor::Center;
        let result = calc.layout(&c);
        assert!((result.lines[0].y + 50.0 - 700.0 * 0.25).abs() < 1e-9);
    }

    #[test]
    fn interlude_spacer_pushes_following_lines_down() {
        let mut calc = LayoutCalculator::new(4, 100.0);
        let mut c = ctx(LayoutTarget::Interlude(1), 700.0);
        c.interlude = Some(InterludeLayout { total_height: 80.0, anchor: 1 });
        let result = calc.layout(&c);
        // 圆点行自身作为焦点:顶部对齐 0.25H = 175
        assert!((result.interlude_y.unwrap() - 700.0 * 0.25).abs() < 1e-9);
        // 行 2、3 被推下 80px;行 0、1 不受影响
        assert!((result.lines[2].y - (result.lines[1].y + 100.0 + 80.0)).abs() < 1e-9);
        assert!((result.lines[3].y - (result.lines[2].y + 100.0)).abs() < 1e-9);
        assert!((result.lines[1].y - (result.lines[0].y + 100.0)).abs() < 1e-9);
    }

    #[test]
    fn prelude_interlude_anchors_dots_at_content_top() {
        let mut calc = LayoutCalculator::new(2, 100.0);
        let mut c = ctx(LayoutTarget::Interlude(-1), 700.0);
        c.interlude = Some(InterludeLayout { total_height: 60.0, anchor: -1 });
        let result = calc.layout(&c);
        // anchor=-1:圆点行在 prefix[0]=0 → viewportStart 本身
        assert!((result.interlude_y.unwrap() - 700.0 * 0.25).abs() < 1e-9);
        // 所有行都在圆点行之后,被推下 60px
        assert!((result.lines[0].y - (700.0 * 0.25 + 60.0)).abs() < 1e-9);
    }

    #[test]
    fn viewport_flag_matches_motion_buffer_bounds() {
        let mut calc = LayoutCalculator::new(10, 100.0);
        let result = calc.layout(&ctx(LayoutTarget::Line(4), 700.0));
        // 行 4 顶部在 0.25·700=175;缓冲 = 0.4H+300 = 580
        // 行 0 在 175-400=-225,底部 -125 >= -580 → 仍在缓冲内
        assert!(result.lines[0].in_viewport);
        let result = calc.layout(&ctx(LayoutTarget::Line(9), 700.0));
        // 焦点在末行时,首行在上方缓冲之外(y = 175-900 = -725,底 -625 < -580)
        assert!(!result.lines[0].in_viewport);
        assert!(result.lines[9].in_viewport);
    }
}

/// 对齐焦点状态机(上游 FocusController)。
///
/// 规则(上游原样):用户滚动挂起期间冻结上一帧的焦点目标,不再跟随
/// 播放进度;冻结在间奏圆点行而间奏已结束/不可展示时,顺延至锚点后
/// 一行;未挂起时跟随播放状态——间奏中对齐圆点行,曲末对齐末行,
/// 其余对齐 `scrollToIndex`(原型无底栏,曲末带底栏分支省略)。
pub struct FocusController {
    target: LayoutTarget,
}

impl Default for FocusController {
    fn default() -> Self {
        Self::new()
    }
}

impl FocusController {
    pub fn new() -> Self {
        Self {
            target: LayoutTarget::Line(0),
        }
    }

    /// 重置焦点至首行(上游 rebuildLyricView 时调用;原型重建即构造,
    /// 语义保留待切歌场景)
    #[allow(dead_code)]
    pub fn reset(&mut self) {
        self.target = LayoutTarget::Line(0);
    }

    /// 推导本帧对齐焦点,并记录为新的冻结目标
    #[allow(clippy::too_many_arguments)]
    pub fn resolve(
        &mut self,
        focus_on_interlude: bool,
        interlude_anchor: Option<i64>,
        can_display_interlude: bool,
        end_of_song: bool,
        scroll_to: usize,
        line_count: usize,
        suspended: bool,
    ) -> LayoutTarget {
        let clamp = |index: usize| index.min(line_count.saturating_sub(1));
        let next = if suspended {
            match self.target {
                LayoutTarget::Line(index) => LayoutTarget::Line(clamp(index)),
                LayoutTarget::Interlude(anchor) => {
                    let interlude_active = focus_on_interlude
                        && interlude_anchor.is_some()
                        && can_display_interlude;
                    if interlude_active {
                        LayoutTarget::Interlude(anchor)
                    } else {
                        LayoutTarget::Line(clamp((anchor + 1).max(0) as usize))
                    }
                }
            }
        } else if focus_on_interlude
            && let Some(anchor) = interlude_anchor
        {
            if can_display_interlude {
                LayoutTarget::Interlude(anchor)
            } else {
                LayoutTarget::Line(clamp((anchor + 1).max(0) as usize))
            }
        } else if end_of_song {
            LayoutTarget::Line(clamp(line_count.saturating_sub(1)))
        } else {
            LayoutTarget::Line(clamp(scroll_to))
        };
        self.target = next;
        next
    }
}

#[cfg(test)]
mod focus_tests {
    use super::*;

    #[test]
    fn suspended_freezes_line_target_while_playback_advances() {
        let mut f = FocusController::new();
        // 正常跟随:scrollTo 从 0 → 2
        assert_eq!(f.resolve(false, None, false, false, 0, 5, false), LayoutTarget::Line(0));
        assert_eq!(f.resolve(false, None, false, false, 2, 5, false), LayoutTarget::Line(2));
        // 挂起:冻结在 2,scrollTo 推进到 3 也不跟
        assert_eq!(f.resolve(false, None, false, false, 3, 5, true), LayoutTarget::Line(2));
        assert_eq!(f.resolve(false, None, false, false, 4, 5, true), LayoutTarget::Line(2));
        // 解除后立即跟上
        assert_eq!(f.resolve(false, None, false, false, 4, 5, false), LayoutTarget::Line(4));
    }

    #[test]
    fn suspended_interlude_falls_through_when_ended() {
        let mut f = FocusController::new();
        // 未挂起时对齐间奏(锚点 1)
        assert_eq!(f.resolve(true, Some(1), true, false, 2, 5, false), LayoutTarget::Interlude(1));
        // 挂起期间间奏仍活跃:保持
        assert_eq!(f.resolve(true, Some(1), true, false, 2, 5, true), LayoutTarget::Interlude(1));
        // 间奏结束(不再聚焦间奏):顺延到锚点后一行
        assert_eq!(f.resolve(false, None, false, false, 2, 5, true), LayoutTarget::Line(2));
        // 行数收缩时钳制
        let mut g = FocusController::new();
        assert_eq!(g.resolve(true, Some(4), true, false, 2, 3, false), LayoutTarget::Interlude(4));
        assert_eq!(g.resolve(false, None, false, false, 2, 3, true), LayoutTarget::Line(2), "anchor+1=5 钳到 2");
    }

    #[test]
    fn active_follows_end_of_song_and_short_interlude_fallthrough() {
        let mut f = FocusController::new();
        assert_eq!(f.resolve(false, None, false, true, 1, 5, false), LayoutTarget::Line(4), "曲末对齐末行");
        // 间奏不可展示:顺延锚点后一行
        assert_eq!(f.resolve(true, Some(1), false, false, 0, 5, false), LayoutTarget::Line(2));
    }
}
