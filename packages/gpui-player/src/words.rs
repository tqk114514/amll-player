//! 逐词渐变揭示:上游 `chunkAndSplitLyricWords` + `WebMaskAnimator`
//! (dom/animation/mask)的移植。
//!
//! 上游机制:每行拆成词元(CJK 逐字、拉丁连续词合并、空格为零时长词元),
//! 每个词元一个 span,各挂一条 bright→dark 的线性渐变 mask;所有 span
//! 的 mask-position 由同一"共享光标"时间线驱动——词元内匀速扫过
//! (宽/时长)、词元间停留,首词元多扫 1.5×渐变宽、末词元多扫 0.5×,
//! clamp 保证词元前全暗、词元后全亮;词元自带 1em padding(布局被
//! margin 抵消),是扫入前的预滚缓冲区。激活行 bright=1.0/dark=0.4,
//! 未激活行为整行 solid 0.2(经行 opacity 0.85/1.0 调制)。
//!
//! GPUI 无 mask-image:用 with_content_mask 裁剪 + 阶梯 alpha 切片
//! (FADE_SLICES 段)等价实现渐变边缘。

use gpui::{
    App, Bounds, ContentMask, Element, ElementId, Font, GlobalElementId, Hsla,
    InspectorElementId, IntoElement, LayoutId, Pixels, SharedString, Style, TextAlign, TextRun,
    Window, point, px, size,
};

use crate::timeline::LyricWord;

/// 渐变带宽度 = wordFadeWidth(0.5)× 行盒(1.2em)= 0.6em
pub const FADE_EM: f64 = 0.6;
/// 词元 padding(上游 1em,margin −1em 抵消布局)
pub const WORD_PAD_EM: f64 = 1.0;
/// 渐变边缘的切片数(无 mask-image 的阶梯逼近)
const FADE_SLICES: usize = 8;
/// 逐词上浮量(上游 createFloatAnimation:-0.05em,composite add)
const FLOAT_EM: f64 = 0.05;
/// 上浮动画最短时长(上游 Math.max(1e3, 词时长))
const FLOAT_MIN_MS: f64 = 1000.0;

/// 词元(atom):CJK 单字 / 拉丁词组 / 空格(零时长)
#[derive(Debug, Clone, PartialEq)]
pub struct WordAtom {
    pub text: String,
    pub start_ms: f64,
    pub end_ms: f64,
}

/// span:词元的呈现单元(连续拉丁词元合并为一个 span)
#[derive(Debug, Clone)]
pub struct WordSpan {
    pub text: String,
    pub start_ms: f64,
    pub end_ms: f64,
    /// 行内内容左缘偏移(逻辑像素,布局时测得)
    pub x: f64,
    pub width: f64,
}

fn is_cjk_char(ch: char) -> bool {
    // 上游 /^[\p{Unified_Ideograph}\u0800-\u9FFC]+$/ 的实用近似
    matches!(ch as u32, 0x0800..=0x9ffc)
}

fn is_cjk_text(text: &str) -> bool {
    !text.is_empty() && text.chars().all(is_cjk_char)
}

/// 按空白切成保留空格的片段(等价 JS `split(/(\s+)/)`)
fn split_keep_whitespace(text: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut current_is_space: Option<bool> = None;
    for ch in text.chars() {
        let is_space = ch.is_whitespace();
        if let Some(prev_space) = current_is_space
            && prev_space != is_space
        {
            parts.push(current.clone());
            current.clear();
        }
        current_is_space = Some(is_space);
        current.push(ch);
    }
    if !current.is_empty() {
        parts.push(current);
    }
    parts
}

/// 拆词元(上游 chunkAndSplitLyricWords 的 CJK/空格/时间分配语义;
/// ruby/roman 不在本阶段范围)。时间按非空字符数均分,CJK 多字词逐字
/// 顺序分配,空格为零时长词元。
pub fn atomize(words: &[LyricWord]) -> Vec<WordAtom> {
    let mut atoms = Vec::new();
    for word in words {
        let non_space_len = word
            .text
            .chars()
            .filter(|ch| !ch.is_whitespace())
            .count()
            .max(1) as f64;
        let unit = (word.end_time_ms - word.start_time_ms) / non_space_len;
        let mut offset: f64 = 0.0;
        for part in split_keep_whitespace(&word.text) {
            if part.trim().is_empty() {
                let at = word.start_time_ms + offset * unit;
                atoms.push(WordAtom {
                    text: part.to_string(),
                    start_ms: at,
                    end_ms: at,
                });
                continue;
            }
            let part_len = part.chars().filter(|ch| !ch.is_whitespace()).count() as f64;
            if is_cjk_text(part.as_str()) && part.chars().count() > 1 {
                for ch in part.chars() {
                    let start = word.start_time_ms + offset * unit;
                    atoms.push(WordAtom {
                        text: ch.to_string(),
                        start_ms: start,
                        end_ms: start + unit,
                    });
                    offset += 1.0;
                }
            } else {
                let start = word.start_time_ms + offset * unit;
                atoms.push(WordAtom {
                    text: part.to_string(),
                    start_ms: start,
                    end_ms: start + part_len * unit,
                });
                offset += part_len;
            }
        }
    }
    atoms
}

/// 词元 → span:连续的非 CJK、非空格词元合并(上游 currentGroup),
/// CJK 单字与空格各自独立
pub fn group_spans(atoms: &[WordAtom]) -> Vec<WordSpan> {
    let mut spans: Vec<WordSpan> = Vec::new();
    let mut group: Vec<&WordAtom> = Vec::new();
    let flush_group = |group: &mut Vec<&WordAtom>, spans: &mut Vec<WordSpan>| {
        if group.is_empty() {
            return;
        }
        let text = group.iter().map(|atom| atom.text.as_str()).collect::<String>();
        spans.push(WordSpan {
            text,
            start_ms: group[0].start_ms,
            end_ms: group.last().expect("非空").end_ms,
            x: 0.0,
            width: 0.0,
        });
        group.clear();
    };
    for atom in atoms {
        let is_space = atom.text.trim().is_empty();
        if is_space || is_cjk_text(atom.text.as_str()) {
            flush_group(&mut group, &mut spans);
            spans.push(WordSpan {
                text: atom.text.clone(),
                start_ms: atom.start_ms,
                end_ms: atom.end_ms,
                x: 0.0,
                width: 0.0,
            });
        } else {
            group.push(atom);
        }
    }
    flush_group(&mut group, &mut spans);
    spans
}

/// 共享光标:media 时刻第 index 个 span 的遮罩边缘位置
/// (内容坐标,相对该 span 内容左缘;≤0 全暗,≥width 全亮)。
///
/// 上游把整行所有 span 的 mask-position 写在同一条关键帧时间线上:
/// pos_i = initialPos_i + 光标位移,clamp 到 [minOffset, 0];
/// 光标在每个 span 的时长内匀速前进 span.width(+首 1.5×fade、
/// 末 0.5×fade),间隙停留。
pub fn reveal_edge(spans: &[WordSpan], index: usize, media_ms: f64, fade: f64, pad: f64) -> f64 {
    let width_before: f64 = spans[..index].iter().map(|span| span.width).sum();
    let initial = -width_before - fade - spans[index].width - pad - fade;
    let mut moved = 0.0;
    for (i, span) in spans.iter().enumerate() {
        let extras = if i == 0 { 1.5 } else { 0.0 } * fade
            + if i + 1 == spans.len() { 0.5 } else { 0.0 } * fade;
        let move_px = span.width + extras;
        if media_ms >= span.end_ms {
            moved += move_px;
        } else if media_ms > span.start_ms {
            let duration = (span.end_ms - span.start_ms).max(1e-9);
            moved += move_px * ((media_ms - span.start_ms) / duration).min(1.0);
            break;
        } else {
            break;
        }
    }
    let min_offset = -(spans[index].width + 2.0 * pad + fade);
    let pos = (initial + moved).clamp(min_offset, 0.0);
    pos + spans[index].width + pad + fade / 2.0
}

/// 渐变边缘处的内容 alpha 插值(bright→dark 的线性渐变)
pub fn mask_alpha(edge: f64, fade: f64, x: f64, bright: f64, dark: f64) -> f64 {
    let t = ((x - (edge - fade / 2.0)) / fade).clamp(0.0, 1.0);
    dark + (bright - dark) * (1.0 - t)
}

/// 逐词揭示行元素:静态词布局 + 每帧按媒体时间计算各 span 的揭示边缘,
/// 以内容裁剪 + 阶梯 alpha 切片绘制。
pub struct WordLineElement {
    spans: Vec<WordSpan>,
    font: Font,
    font_size: f64,
    line_height: f64,
    /// gradient 模式(bright/dark 双色揭示);false = solid(整行单色)
    active: bool,
    bright: f64,
    dark: f64,
    media_ms: f64,
    fade: f64,
    pad: f64,
}

impl WordLineElement {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        spans: Vec<WordSpan>,
        font: Font,
        font_size: f64,
        line_height: f64,
        active: bool,
        bright: f64,
        dark: f64,
        media_ms: f64,
    ) -> Self {
        Self {
            spans,
            font,
            font_size,
            line_height,
            active,
            bright,
            dark,
            media_ms,
            fade: font_size * FADE_EM,
            pad: font_size * WORD_PAD_EM,
        }
    }

    fn paint_span(
        &self,
        window: &mut Window,
        cx: &mut App,
        origin_x: f64,
        origin_y: f64,
        span: &WordSpan,
        alpha: f64,
    ) {
        let shared: SharedString = span.text.clone().into();
        let runs = [TextRun {
            len: shared.len(),
            font: self.font.clone(),
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
            .shape_line(shared, px(self.font_size as f32), &runs, None);
        shaped
            .paint(
                point(px(origin_x as f32), px(origin_y as f32)),
                px(self.line_height as f32),
                TextAlign::Left,
                None,
                window,
                cx,
            )
            .ok();
    }

    fn paint_slice(
        &self,
        window: &mut Window,
        cx: &mut App,
        bounds_origin: (f64, f64),
        span: &WordSpan,
        range: (f64, f64),
        alpha: f64,
    ) {
        let (from, to) = range;
        let left = (bounds_origin.0 + span.x + from).max(bounds_origin.0);
        let width = (to - from).max(0.0);
        if width <= 0.0 {
            return;
        }

        let mask = ContentMask {
            bounds: Bounds {
                origin: point(px(left as f32), px(bounds_origin.1 as f32)),
                size: size(px(width as f32), px(self.line_height as f32)),
            },
        };
        window.with_content_mask(Some(mask), |window| {
            self.paint_span(window, cx, bounds_origin.0 + span.x, bounds_origin.1, span, alpha);
        });
    }
}

impl Element for WordLineElement {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let total_width: f64 = self.spans.iter().map(|span| span.width).sum();
        let style = Style {
            size: size(px(total_width as f32).into(), px(self.line_height as f32).into()),
            ..Style::default()
        };
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Self::PrepaintState {
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let origin_x = (bounds.origin.x / px(1.0)) as f64;
        let origin_y = (bounds.origin.y / px(1.0)) as f64;
        for (index, span) in self.spans.iter().enumerate() {
            // 逐词上浮(上游 createFloatAnimation):词开始后 ease-out 上浮
            // 0.05em,最短 1s,唱完保持(fill both),行失活时随重建复位
            let float_offset = if self.active {
                let duration = (span.end_ms - span.start_ms).max(FLOAT_MIN_MS);
                let t = ((self.media_ms - span.start_ms) / duration).clamp(0.0, 1.0);
                -self.font_size * FLOAT_EM * (1.0 - (1.0 - t) * (1.0 - t) * (1.0 - t))
            } else {
                0.0
            };
            let span_y = origin_y + float_offset;
            if !self.active {
                // solid 模式:整行单色
                self.paint_span(window, cx, origin_x + span.x, span_y, span, self.dark);
                continue;
            }
            let edge = reveal_edge(&self.spans, index, self.media_ms, self.fade, self.pad);
            // 分区绘制(与上游 mask 替换语义一致,不做暗底叠加):
            // 亮头 [0, edge−F/2) / 渐变带 [edge−F/2, edge+F/2) / 暗尾 [edge+F/2, W]
            let bright_end = (edge - self.fade / 2.0).min(span.width).max(0.0);
            let band_from = (edge - self.fade / 2.0).max(0.0);
            let band_to = (edge + self.fade / 2.0).min(span.width);
            if bright_end > 0.0 {
                self.paint_slice(
                    window,
                    cx,
                    (origin_x, span_y),
                    span,
                    (0.0, bright_end),
                    self.bright,
                );
            }
            if band_to > band_from {
                let slice = (band_to - band_from) / FADE_SLICES as f64;
                for k in 0..FADE_SLICES {
                    let from = band_from + k as f64 * slice;
                    let to = (band_from + (k + 1) as f64 * slice).min(band_to);
                    let mid = (from + to) / 2.0;
                    let alpha = mask_alpha(edge, self.fade, mid, self.bright, self.dark);
                    self.paint_slice(window, cx, (origin_x, span_y), span, (from, to), alpha);
                }
            }
            if band_to < span.width {
                self.paint_slice(
                    window,
                    cx,
                    (origin_x, span_y),
                    span,
                    (band_to, span.width),
                    self.dark,
                );
            }
        }
    }
}

impl IntoElement for WordLineElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(text: &str, start: f64, end: f64, x: f64, width: f64) -> WordSpan {
        WordSpan {
            text: text.into(),
            start_ms: start,
            end_ms: end,
            x,
            width,
        }
    }

    fn demo_spans() -> Vec<WordSpan> {
        vec![
            span("前", 9000.0, 9500.0, 0.0, 38.0),
            span("奏", 9500.0, 10000.0, 38.0, 38.0),
            span(" ", 10000.0, 10000.0, 76.0, 19.0),
            span("圆", 10000.0, 10600.0, 95.0, 38.0),
        ]
    }

    #[test]
    fn atomize_splits_cjk_per_char_and_space_zero_duration() {
        let words = vec![LyricWord {
            start_time_ms: 0.0,
            end_time_ms: 3000.0,
            text: "间奏 ".into(),
        }];
        let atoms = atomize(&words);
        assert_eq!(atoms.len(), 3);
        assert_eq!(atoms[0].text, "间");
        assert_eq!(atoms[0].start_ms, 0.0);
        assert_eq!(atoms[0].end_ms, 1500.0);
        assert_eq!(atoms[1].text, "奏");
        assert_eq!(atoms[2].text, " ");
        assert_eq!(atoms[2].start_ms, atoms[2].end_ms, "空格为零时长词元");
    }

    #[test]
    fn group_spans_splits_cjk_and_merges_latin() {
        let atoms = vec![
            WordAtom { text: "Hello".into(), start_ms: 0.0, end_ms: 100.0 },
            WordAtom { text: "AMLL".into(), start_ms: 100.0, end_ms: 200.0 },
            WordAtom { text: " ".into(), start_ms: 200.0, end_ms: 200.0 },
            WordAtom { text: "词".into(), start_ms: 200.0, end_ms: 300.0 },
        ];
        let spans = group_spans(&atoms);
        assert_eq!(spans.len(), 3);
        assert_eq!(spans[0].text, "HelloAMLL", "连续拉丁词元合并");
        assert_eq!(spans[1].text, " ");
        assert_eq!(spans[2].text, "词");
    }

    #[test]
    fn reveal_edge_sweeps_from_dark_to_bright() {
        let spans = demo_spans();
        let fade = 22.0;
        let pad = 38.0;
        // 词元开始前:全暗(edge ≤ 0)
        let before = reveal_edge(&spans, 1, 9000.0, fade, pad);
        assert!(before <= 0.0, "首词开始前应全暗,got {before}");
        // 首词开始时:edge = −1.5×fade(几何推导)
        let at_start = reveal_edge(&spans, 0, 9000.0, fade, pad);
        assert!((at_start - (-1.5 * fade)).abs() < 1e-6, "got {at_start}");
        // 中间词开始时:edge = 0(内容左缘)
        let mid = reveal_edge(&spans, 3, 10000.0, fade, pad);
        assert!(mid.abs() < 1e-6, "中间词开始应从内容左缘,got {mid}");
        // 末词结束时:edge = 宽 + 0.5×fade(全亮超出右缘)
        let last = reveal_edge(&spans, 3, 10600.0, fade, pad);
        assert!((last - (38.0 + 0.5 * fade)).abs() < 1e-6, "got {last}");
        // 全部结束后:clamp 全亮
        let after = reveal_edge(&spans, 0, 11000.0, fade, pad);
        assert!(after - 0.5 * fade > 38.0 + 1.5 * fade - 1e-6, "got {after}");
    }

    #[test]
    fn reveal_edge_holds_position_in_gaps() {
        // 含真实间隙的两词:前 [9000,9400],奏 [9600,10000]
        let spans = vec![
            span("前", 9000.0, 9400.0, 0.0, 38.0),
            span("奏", 9600.0, 10000.0, 38.0, 38.0),
        ];
        let fade = 22.0;
        let pad = 38.0;
        // 间隙 [9400, 9600) 内光标不推进
        let a = reveal_edge(&spans, 1, 9450.0, fade, pad);
        let b = reveal_edge(&spans, 1, 9550.0, fade, pad);
        assert!((a - b).abs() < 1e-9, "间隙应保持位置: {a} vs {b}");
        // 且词元 1 的边缘在其开始时刻从内容左缘起扫
        let at_start = reveal_edge(&spans, 1, 9600.0, fade, pad);
        assert!(at_start.abs() < 1e-6, "got {at_start}");
    }

    #[test]
    fn mask_alpha_interpolates_across_band() {
        let edge = 50.0;
        let fade = 20.0;
        assert!((mask_alpha(edge, fade, 30.0, 1.0, 0.4) - 1.0).abs() < 1e-9, "带左全亮");
        assert!((mask_alpha(edge, fade, 70.0, 1.0, 0.4) - 0.4).abs() < 1e-9, "带右全暗");
        let mid = mask_alpha(edge, fade, 50.0, 1.0, 0.4);
        assert!((mid - 0.7).abs() < 1e-9, "带中为亮暗中点,got {mid}");
    }
}
