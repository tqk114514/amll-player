//! 逐词渐变揭示:上游 `chunkAndSplitLyricWords` + `WebMaskAnimator`
//! (dom/animation/mask)的移植。
//!
//! 上游机制:每行拆成词元(CJK 逐字、拉丁连续词合并、空格为零时长词元),
//! 每个词元一个 span,各挂一条 bright→dark 的线性渐变 mask;所有 span
//! 的 mask-position 由同一"共享光标"时间线驱动——词元内匀速扫过
//! (宽/时长)、词元间停留,首词元多扫 1.5×渐变宽、末词元多扫 0.5×,
//! clamp 保证词元前全暗、词元后全亮;词元自带 1em padding(布局被
//! margin 抵消),是扫入前的预滚缓冲区。mask 常驻所有行:未激活行
//! `--bright/--dark-mask-alpha` 同为 .2(均匀暗),激活行加
//! `.gradientMask` 类变 1/.4,两个 alpha 都是 `@property` 注册的可过渡
//! 自定义属性——激活 0.3s ease-out、退激活 0.45s ease-out(退激活时
//! maskAnimator.pause() 冻结遮罩位置,已唱部分平滑回落而非瞬间消失)。
//!
//! GPUI 渲染:CSS mask 无原生等价,经本地 patch(gpui-pre
//! `with_element_mask_gradient` + 平台着色器)以**像素级线性 alpha
//! 渐变**实现——与 Web 合成器逐像素乘渐变遮罩同构,无阶梯近似。

use gpui::{
    App, Element, ElementId, Font, GlobalElementId, Hsla, InspectorElementId, IntoElement,
    LayoutId, Pixels, SharedString, Style, TextAlign, TextRun, Window, point, px, size,
};

use crate::timeline::LyricWord;

/// 渐变带宽度 = wordFadeWidth(0.5)× 行盒(1.2em)= 0.6em
pub const FADE_EM: f64 = 0.6;
/// 词元 padding(上游 1em,margin −1em 抵消布局)
pub const WORD_PAD_EM: f64 = 1.0;
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

/// 退激活后的上浮反演状态(上游 `disable()` 把 float-word 动画
/// `playbackRate = -1` 续播到 0,词元按各自已推进的时长匀速落回)
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FloatReverse {
    /// 反演已推进的墙钟(毫秒;由调用方按帧增量累加,不渲染即暂停)
    pub elapsed_ms: f64,
    /// 退激活时刻的媒体时间(float 起点与遮罩位置一并冻结于此)
    pub media_at_disable_ms: f64,
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

/// 单个词元的上浮进度(0..1,未经缓动):激活期随媒体时间推进;
/// 退激活后从冻结值按墙钟匀速反演回 0(反向播放时长 = 已推进时长)
pub fn float_progress(
    span: &WordSpan,
    media_ms: f64,
    reverse: Option<&FloatReverse>,
) -> f64 {
    let duration = (span.end_ms - span.start_ms).max(FLOAT_MIN_MS);
    match reverse {
        None => ((media_ms - span.start_ms) / duration).clamp(0.0, 1.0),
        Some(reverse) => {
            let t0 = ((reverse.media_at_disable_ms - span.start_ms) / duration).clamp(0.0, 1.0);
            (t0 - reverse.elapsed_ms / duration).clamp(0.0, t0)
        }
    }
}

/// 退激活反演是否全部结束(所有词元的上浮都回到 0)
pub fn float_reverse_finished(spans: &[WordSpan], reverse: &FloatReverse) -> bool {
    spans.iter().all(|span| {
        float_progress(span, f64::INFINITY, Some(reverse)) <= 0.0
    })
}

/// 逐词揭示行元素:静态词布局 + 每帧按(冻结或当前的)媒体时间计算各
/// span 的揭示边缘,以精灵像素渐变单次绘制(亮暗 alpha 经补间驱动,
/// 激活/退激活的亮度过渡由调用方传入的补间值完成)。
pub struct WordLineElement {
    spans: Vec<WordSpan>,
    font: Font,
    font_size: f64,
    line_height: f64,
    /// 遮罩亮端 alpha(上游 --bright-mask-alpha,随激活补间变化)
    bright: f64,
    /// 遮罩暗端 alpha(上游 --dark-mask-alpha)
    dark: f64,
    media_ms: f64,
    /// 退激活期间冻结的媒体时间(上游 maskAnimator.pause():遮罩位置
    /// 停在退激活瞬间,已唱部分随 alpha 回落平滑变暗)
    frozen_media_ms: Option<f64>,
    /// 退激活后的上浮反演(为 None 时随媒体时间正向推进)
    float_reverse: Option<FloatReverse>,
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
        bright: f64,
        dark: f64,
        media_ms: f64,
        frozen_media_ms: Option<f64>,
        float_reverse: Option<FloatReverse>,
    ) -> Self {
        Self {
            spans,
            font,
            font_size,
            line_height,
            bright,
            dark,
            media_ms,
            frozen_media_ms,
            float_reverse,
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
        _bounds: gpui::Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Self::PrepaintState {
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: gpui::Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let origin_x = (bounds.origin.x / px(1.0)) as f64;
        let origin_y = (bounds.origin.y / px(1.0)) as f64;
        // 两个 alpha 相等 = 均匀半透明(未激活稳态/补间收敛),单色直绘;
        // 相异 = 渐变揭示(激活中或退激活回落中)
        let solid = (self.bright - self.dark).abs() < 1e-4;
        let mask_media = self.frozen_media_ms.unwrap_or(self.media_ms);
        let scale_factor = window.scale_factor();

        for (index, span) in self.spans.iter().enumerate() {
            let span_x = origin_x + span.x;
            // 逐词上浮(上游 createFloatAnimation,ease-out 关键帧;
            // 退激活按墙钟反演回落)。经绘制期矩阵平移(顶点着色器
            // 精度)而非平移原点——字形原点按整像素吸附,直接平移会让
            // 1.75px 的上浮变成一两次 1px 瞬移
            let float_t = float_progress(span, self.media_ms, self.float_reverse.as_ref());
            let float_offset = -self.font_size * FLOAT_EM * crate::ease::ease_out()(float_t);
            let float_matrix = gpui::TransformationMatrix {
                rotation_scale: [[1.0, 0.0], [0.0, 1.0]],
                translation: [0.0, (float_offset * scale_factor as f64) as f32],
            };
            let edge = if solid {
                None
            } else {
                Some(reveal_edge(
                    &self.spans,
                    index,
                    mask_media,
                    self.fade,
                    self.pad,
                ))
            };

            if let Some(edge) = edge {
                // 渐变带 [edge−F/2, edge+F/2]:带左亮、带右暗,带内
                // 逐像素线性插值(着色器),与上游 mask 渐变语义一致
                let band_left = span_x + edge - self.fade / 2.0;
                let band_right = span_x + edge + self.fade / 2.0;
                window.with_element_mask_gradient(
                    px(band_left as f32),
                    px(band_right as f32),
                    self.bright as f32,
                    self.dark as f32,
                    |window| {
                        window.with_element_transformation(float_matrix, |window| {
                            self.paint_span(window, cx, span_x, origin_y, span, 1.0);
                        });
                    },
                );
            } else {
                window.with_element_transformation(float_matrix, |window| {
                    self.paint_span(window, cx, span_x, origin_y, span, self.dark);
                });
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
    fn float_progress_advances_with_media_and_reverses_on_disable() {
        let s = span("词", 1000.0, 2000.0, 0.0, 40.0);
        // 未开始:0;结束后:1
        assert!(float_progress(&s, 500.0, None) <= 0.0);
        assert!((float_progress(&s, 3000.0, None) - 1.0).abs() < 1e-9);
        // 中点:0.5(时长 1000 ≥ 最短 1000)
        assert!((float_progress(&s, 1500.0, None) - 0.5).abs() < 1e-9);
        // 退激活:冻结在半程,反演以 1× 回退(currentTime 递减)——
        // t0=0.5×1000ms 后归零,走 250ms 时剩 0.25
        let reverse = FloatReverse { elapsed_ms: 0.0, media_at_disable_ms: 1500.0 };
        assert!((float_progress(&s, f64::INFINITY, Some(&reverse)) - 0.5).abs() < 1e-9);
        let quarter = FloatReverse { elapsed_ms: 250.0, media_at_disable_ms: 1500.0 };
        assert!((float_progress(&s, f64::INFINITY, Some(&quarter)) - 0.25).abs() < 1e-9);
        let done = FloatReverse { elapsed_ms: 500.0 + 1e-9, media_at_disable_ms: 1500.0 };
        assert!(float_reverse_finished(&[s.clone()], &done), "反演结束");
        let undone = FloatReverse { elapsed_ms: 499.0, media_at_disable_ms: 1500.0 };
        assert!(!float_reverse_finished(&[s], &undone));
    }
}
