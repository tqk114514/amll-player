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
//!
//! emphasize 辉光(上游 dom/animation/emphasize):shouldEmphasize
//! 选词(CJK ≥1s / 拉丁 ≥1s 且 2-7 字符)→ 逐字符 span + 错峰延迟
//! (de + du/2.5/count·i)→ 涨落曲线(双贝塞尔)驱动绕字心缩放、
//! 字符互推、textShadow 白辉光(blur 作用域以字形光栅自生光晕,
//! α 经渐变承载——亮部 transX·blur、暗部 ×0.4)与附加正弦上浮
//! (du×1.4、提前 400ms);末词 amount×1.6 / blur×1.5 / du×1.2,
//! 封顶 1.2 / 0.8。

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
/// emphasize 辉光动画的参数与曲线(上游 dom/animation/emphasize/index.ts)
const EMP_EASING_MID: f64 = 0.5;
/// 悬停/末词幅度倍率与封顶(上游 calculateEmphasizeParams)
const EMP_AMOUNT_CAP: f64 = 1.2;
const EMP_BLUR_CAP: f64 = 0.8;

/// 词元(atom):CJK 单字 / 拉丁词组 / 空格(零时长)
#[derive(Debug, Clone, PartialEq)]
pub struct WordAtom {
    pub text: String,
    pub start_ms: f64,
    pub end_ms: f64,
    /// [emphasize] 所属原始词的信息(词达标时所有 atom 共享同一份)
    pub emp_word: Option<EmphasizeWord>,
}

/// [emphasize] 原始词的元信息(上游 chunk/word 级)
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EmphasizeWord {
    /// 词开始(绝对毫秒)
    pub start_ms: f64,
    /// 词结束(绝对毫秒)
    pub end_ms: f64,
    /// 词内字符数(用于逐字错峰与互推)
    pub char_count: usize,
    /// 是否为整行最后一个词(末词幅度加剧)
    pub is_last_word: bool,
}

/// [emphasize] 单个 span(= 单个字符)的动画参数(上游 EmphasizeParams
/// + wordDe 逐字延迟)
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Emphasize {
    /// de:词相对行开始的延迟(毫秒)
    pub line_delay_ms: f64,
    /// animateDu:辉光时长(末词已 ×1.2)
    pub du_ms: f64,
    /// 辉光/缩放幅度(末词已 ×1.6,封顶 1.2)
    pub amount: f64,
    /// 辉光透明度幅度(末词已 ×1.5,封顶 0.8)
    pub blur: f64,
    /// 字符在词内的序号
    pub char_index: usize,
    /// 词内字符总数
    pub char_count: usize,
    /// 是否末词(上浮曲线不需要,保留给调用方判定)
    pub is_last_word: bool,
}

/// span:词元的呈现单元(连续拉丁词元合并为一个 span;
/// emphasize 词逐字符拆分,每字符一个 span)
#[derive(Debug, Clone)]
pub struct WordSpan {
    pub text: String,
    pub start_ms: f64,
    pub end_ms: f64,
    /// 行内内容左缘偏移(逻辑像素,布局时测得)
    pub x: f64,
    pub width: f64,
    /// [emphasize] 该字符的辉光参数(非 emphasize 词为 None)
    pub emp: Option<Emphasize>,
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

/// 上浮的阶段状态(上游 enable/disable 生命周期):
/// - Active:激活,随媒体时间正向推进
/// - Reversing:退激活反演中,按墙钟从冻结值匀速回落
/// - Sunk:反演完成保持落回(上游动画播完 fill: both),直到重新激活
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FloatPhase {
    Active,
    Reversing(FloatReverse),
    Sunk,
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
    for (word_index, word) in words.iter().enumerate() {
        let non_space_len = word
            .text
            .chars()
            .filter(|ch| !ch.is_whitespace())
            .count()
            .max(1) as f64;
        let unit = (word.end_time_ms - word.start_time_ms) / non_space_len;
        // [emphasize] 词级判定(上游 shouldEmphasize):CJK 词 ≥1s;
        // 拉丁词 ≥1s 且去除首尾空格后 2-7 字符。达标则全部 atom
        // 共享词元信息(char_count 为词内非空字符数)
        let duration = word.end_time_ms - word.start_time_ms;
        let trimmed_len = word.text.trim().chars().count();
        let emphasized = duration >= 1000.0
            && (is_cjk_text(word.text.trim()) || (2..=7).contains(&trimmed_len));
        let emp_word = if emphasized {
            Some(EmphasizeWord {
                start_ms: word.start_time_ms,
                end_ms: word.end_time_ms,
                char_count: word.text.chars().filter(|ch| !ch.is_whitespace()).count().max(1),
                is_last_word: word_index + 1 == words.len(),
            })
        } else {
            None
        };
        let mut offset: f64 = 0.0;
        for part in split_keep_whitespace(&word.text) {
            if part.trim().is_empty() {
                let at = word.start_time_ms + offset * unit;
                atoms.push(WordAtom {
                    text: part.to_string(),
                    start_ms: at,
                    end_ms: at,
                    emp_word,
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
                        emp_word,
                    });
                    offset += 1.0;
                }
            } else {
                let start = word.start_time_ms + offset * unit;
                atoms.push(WordAtom {
                    text: part.to_string(),
                    start_ms: start,
                    end_ms: start + part_len * unit,
                    emp_word,
                });
                offset += part_len;
            }
        }
    }
    atoms
}

/// 词元 → span:连续的非 CJK、非空格词元合并(上游 currentGroup),
/// CJK 单字与空格各自独立
pub fn group_spans(atoms: &[WordAtom], line_start_ms: f64) -> Vec<WordSpan> {
    let mut spans: Vec<WordSpan> = Vec::new();
    let mut group: Vec<&WordAtom> = Vec::new();
    let flush_group = |group: &mut Vec<&WordAtom>, spans: &mut Vec<WordSpan>| {
        if group.is_empty() {
            return;
        }
        // [emphasize] 上游按 chunk(合并后的词)整体演出:任一子词达标
        // 则整组逐字符拆分(拉丁字符也是 characterElements),按组时长
        // 均分时间(揭示光标在组内仍匀速扫过)
        if let Some(emp_word) = group.iter().find_map(|atom| atom.emp_word) {
            let text = group.iter().map(|atom| atom.text.as_str()).collect::<String>();
            let trimmed: String = text.trim().chars().collect();
            let char_count = trimmed.chars().count().max(1);
            let group_start = group[0].start_ms;
            let group_end = group.last().expect("非空").end_ms;
            let group_len = (group_end - group_start).max(1e-9);
            let duration = emp_word.end_ms - emp_word.start_ms;
            let (amount, blur, du) = emphasize_params(duration, emp_word.is_last_word);
            for (char_index, ch) in trimmed.chars().enumerate() {
                let start = group_start + group_len * char_index as f64 / char_count as f64;
                let end = group_start + group_len * (char_index + 1) as f64 / char_count as f64;
                spans.push(WordSpan {
                    text: ch.to_string(),
                    start_ms: start,
                    end_ms: end,
                    x: 0.0,
                    width: 0.0,
                    emp: Some(Emphasize {
                        line_delay_ms: emp_word.start_ms - line_start_ms,
                        du_ms: du,
                        amount,
                        blur,
                        char_index,
                        char_count,
                        is_last_word: emp_word.is_last_word,
                    }),
                });
            }
            group.clear();
            return;
        }
        let text = group.iter().map(|atom| atom.text.as_str()).collect::<String>();
        spans.push(WordSpan {
            text,
            start_ms: group[0].start_ms,
            end_ms: group.last().expect("非空").end_ms,
            x: 0.0,
            width: 0.0,
            emp: None,
        });
        group.clear();
    };
    for atom in atoms {
        let is_space = atom.text.trim().is_empty();
        if is_space {
            flush_group(&mut group, &mut spans);
            spans.push(WordSpan {
                text: atom.text.clone(),
                start_ms: atom.start_ms,
                end_ms: atom.end_ms,
                x: 0.0,
                width: 0.0,
                emp: None,
            });
        } else if is_cjk_text(atom.text.as_str()) {
            flush_group(&mut group, &mut spans);
            // [emphasize] CJK 词达标:逐字符 span,共享词级参数,
            // char_index 按词内顺序(上游 wordDe = de + du/2.5/count·i)
            let span_emp = atom.emp_word.map(|emp_word| {
                let duration = emp_word.end_ms - emp_word.start_ms;
                let (amount, blur, du) = emphasize_params(duration, emp_word.is_last_word);
                // CJK 词内字符序号:同词的 atom 按 start 顺序推
                let char_index = atoms
                    .iter()
                    .filter(|other| other.emp_word == atom.emp_word)
                    .take_while(|other| other.start_ms <= atom.start_ms)
                    .count()
                    .saturating_sub(1);
                Emphasize {
                    line_delay_ms: emp_word.start_ms - line_start_ms,
                    du_ms: du,
                    amount,
                    blur,
                    char_index,
                    char_count: emp_word.char_count,
                    is_last_word: emp_word.is_last_word,
                }
            });
            spans.push(WordSpan {
                text: atom.text.clone(),
                start_ms: atom.start_ms,
                end_ms: atom.end_ms,
                x: 0.0,
                width: 0.0,
                emp: span_emp,
            });
        } else {
            group.push(atom);
        }
    }
    flush_group(&mut group, &mut spans);
    spans
}

/// [emphasize] 上游 calculateEmphasizeParams:amount/blur 随 du 的
/// 塑形(>1 开方,<1 立方)、基准倍率、末词增幅与封顶。
/// 返回 (amount, blur, du);du 已含 Math.max(1000, duration)。
pub fn emphasize_params(duration_ms: f64, is_last_word: bool) -> (f64, f64, f64) {
    let du = duration_ms.max(1000.0);
    let mut amount = du / 2000.0;
    amount = if amount > 1.0 { amount.sqrt() } else { amount * amount * amount };
    let mut blur = du / 3000.0;
    blur = if blur > 1.0 { blur.sqrt() } else { blur * blur * blur };
    amount *= 0.6;
    blur *= 0.5;
    let mut du = du;
    if is_last_word {
        amount *= 1.6;
        blur *= 1.5;
        du *= 1.2;
    }
    (amount.min(EMP_AMOUNT_CAP), blur.min(EMP_BLUR_CAP), du)
}

/// [emphasize] 上游 empEasing:前半 bezIn(0.2,0.4,0.58,1) 升,
/// 后半 1−bezOut(0.3,0,0.58,1) 回落——涨落曲线
pub fn emphasize_easing(x: f64) -> f64 {
    let x = x.clamp(0.0, 1.0);
    if x < EMP_EASING_MID {
        crate::ease::css_bezier(0.2, 0.4, 0.58, 1.0)((x / EMP_EASING_MID).clamp(0.0, 1.0))
    } else {
        1.0 - crate::ease::css_bezier(0.3, 0.0, 0.58, 1.0)(
            ((x - EMP_EASING_MID) / (1.0 - EMP_EASING_MID)).clamp(0.0, 1.0),
        )
    }
}

/// [emphasize] 字符 i 的错峰延迟(上游 wordDe = de + du/2.5/count·i)
pub fn emphasize_word_delay(emp: &Emphasize) -> f64 {
    emp.line_delay_ms + emp.du_ms / 2.5 / emp.char_count as f64 * emp.char_index as f64
}

/// [emphasize] 辉光进度(0..1;fill both——延迟前为 0,结束后保持末帧再回落)
pub fn emphasize_glow_progress(emp: &Emphasize, relative_ms: f64) -> f64 {
    ((relative_ms - emphasize_word_delay(emp)) / emp.du_ms).clamp(0.0, 1.0)
}

/// [emphasize] 附加正弦上浮进度(时长 du×1.4,提前 400ms;
/// y = −sin(xπ)·0.05em,上游 composite: add)
pub fn emphasize_float_progress(emp: &Emphasize, relative_ms: f64) -> f64 {
    let delay = emphasize_word_delay(emp) - 400.0;
    let duration = emp.du_ms * 1.4;
    ((relative_ms - delay) / duration).clamp(0.0, 1.0)
}

/// [emphasize] 整行 span 构建(上游 buildWord + createEmphasizeAnimation 的
/// 数据准备):atomize → group_spans(含 emphasize 逐字符拆分)
pub fn build_spans(words: &[LyricWord]) -> Vec<WordSpan> {
    let atoms = atomize(words);
    let line_start = words.first().map(|w| w.start_time_ms).unwrap_or(0.0);
    group_spans(&atoms, line_start)
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
/// 退激活后从冻结值按墙钟匀速反演回 0(反向播放时长 = 已推进时长);
/// 反演完成后保持 0(上游 fill: both),直到重新激活
pub fn float_progress(span: &WordSpan, media_ms: f64, phase: FloatPhase) -> f64 {
    let duration = (span.end_ms - span.start_ms).max(FLOAT_MIN_MS);
    match phase {
        FloatPhase::Active => ((media_ms - span.start_ms) / duration).clamp(0.0, 1.0),
        FloatPhase::Reversing(reverse) => {
            let t0 = ((reverse.media_at_disable_ms - span.start_ms) / duration).clamp(0.0, 1.0);
            (t0 - reverse.elapsed_ms / duration).clamp(0.0, t0)
        }
        FloatPhase::Sunk => 0.0,
    }
}

/// 退激活反演是否全部结束(所有词元的上浮都回到 0)
pub fn float_reverse_finished(spans: &[WordSpan], reverse: &FloatReverse) -> bool {
    spans.iter().all(|span| {
        float_progress(span, f64::INFINITY, FloatPhase::Reversing(*reverse)) <= 0.0
    })
}

/// [emphasize] 辉光可见:进度已开始且涨落值非零
fn gx_emphasize_visible(
    emphasize: Option<(Emphasize, f64, f64, f64)>,
) -> bool {
    matches!(emphasize, Some((_, gx, trans_x, _)) if gx > 0.0 && trans_x > 0.001)
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
    /// 上浮阶段(激活随媒体推进 / 退激活反演 / 反演完成保持落回)
    float_phase: FloatPhase,
    fade: f64,
    pad: f64,
    /// 行开始时刻(上游 line.startTime = 首词开始;emphasize 相对时间轴)
    line_start: f64,
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
        float_phase: FloatPhase,
    ) -> Self {
        let line_start = spans.first().map(|span| span.start_ms).unwrap_or(0.0);
        Self {
            spans,
            font,
            font_size,
            line_height,
            bright,
            dark,
            media_ms,
            frozen_media_ms,
            float_phase,
            fade: font_size * FADE_EM,
            pad: font_size * WORD_PAD_EM,
            line_start,
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
            let float_t = float_progress(span, self.media_ms, self.float_phase);
            let float_offset = -self.font_size * FLOAT_EM * crate::ease::ease_out()(float_t);

            // [emphasize] 辉光状态(上游 createEmphasizeAnimation):
            // 涨落曲线 transX 驱动缩放/互推/偏移/辉光,附加正弦上浮
            // 在基础 float 之上(composite: add);遮罩时间冻结时辉光
            // 同步冻结(上游 disable 不反演辉光动画)
            let emphasize = span.emp.filter(|_| !solid).map(|emp| {
                let relative = mask_media - self.line_start;
                let gx = emphasize_glow_progress(&emp, relative);
                let trans_x = emphasize_easing(gx);
                let fx = emphasize_float_progress(&emp, relative);
                (emp, gx, trans_x, fx)
            });
            let (mut offset_x, mut offset_y, mut glyph_scale) = (0.0, float_offset, 1.0f64);
            if let Some((emp, gx, trans_x, fx)) = emphasize
                && gx > 0.0
                && trans_x > 0.001
            {
                glyph_scale = 1.0 + trans_x * 0.1 * emp.amount;
                // 字符互推:越靠词尾推得越向右(上游 offsetX 方程)
                offset_x -= self.font_size
                    * trans_x
                    * 0.03
                    * emp.amount
                    * (emp.char_count as f64 / 2.0 - emp.char_index as f64);
                offset_y -= self.font_size * trans_x * 0.025 * emp.amount;
                // 附加正弦上浮:y = −sin(x·π)·0.05em(时长 du×1.4)
                offset_y -= self.font_size * 0.05 * (fx * std::f64::consts::PI).sin();
            }

            // 矩阵:绕字符中心缩放 + 平移(设备像素空间,与 ScaledElement
            // 同一约定:RS·p + [C(1−s) + T])
            let center_x = (span_x + span.width / 2.0) * scale_factor as f64;
            let center_y = (origin_y + self.line_height / 2.0) * scale_factor as f64;
            let s = glyph_scale as f32;
            let matrix = gpui::TransformationMatrix {
                rotation_scale: [[s, 0.0], [0.0, s]],
                translation: [
                    (center_x * (1.0 - glyph_scale) + offset_x * scale_factor as f64) as f32,
                    (center_y * (1.0 - glyph_scale) + offset_y * scale_factor as f64) as f32,
                ],
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
                // [emphasize] 辉光底:上游 textShadow 0 0 min(.3, blur·.3)em
                // rgba(255,255,255, transX·blur)——被遮罩文字的模糊白色
                // 拷贝。经 blur 作用域(patch)以字形光栅自身生成光晕;
                // text-shadow 的 σ ≈ 半径/2(与 box-shadow 同约定);
                // alpha 经渐变承载(亮部 transX·blur、暗部 ×0.4)
                if let Some((emp, _, trans_x, _)) = emphasize
                    && gx_emphasize_visible(emphasize)
                {
                    let glow_alpha = (trans_x * emp.blur) as f32;
                    // textShadow 半径 = min(0.3, blur·0.3)em,σ ≈ 半径/2
                    let shadow_sigma =
                        self.font_size * (emp.blur * 0.3).min(0.3) / 2.0;
                    window.with_element_transformation(matrix, |window| {
                        window.with_element_blur(px(shadow_sigma as f32), |window| {
                            window.with_element_mask_gradient(
                                px(band_left as f32),
                                px(band_right as f32),
                                glow_alpha,
                                glow_alpha * 0.4,
                                |window| {
                                    self.paint_span(window, cx, span_x, origin_y, span, 1.0);
                                },
                            );
                        });
                    });
                }
                window.with_element_mask_gradient(
                    px(band_left as f32),
                    px(band_right as f32),
                    self.bright as f32,
                    self.dark as f32,
                    |window| {
                        window.with_element_transformation(matrix, |window| {
                            self.paint_span(window, cx, span_x, origin_y, span, 1.0);
                        });
                    },
                );
            } else {
                window.with_element_transformation(matrix, |window| {
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
            emp: None,
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
            WordAtom { text: "Hello".into(), start_ms: 0.0, end_ms: 100.0, emp_word: None },
            WordAtom { text: "AMLL".into(), start_ms: 100.0, end_ms: 200.0, emp_word: None },
            WordAtom { text: " ".into(), start_ms: 200.0, end_ms: 200.0, emp_word: None },
            WordAtom { text: "词".into(), start_ms: 200.0, end_ms: 300.0, emp_word: None },
        ];
        let spans = group_spans(&atoms, 0.0);
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
    fn float_progress_advances_reverses_and_holds_sunk() {
        let s = span("词", 1000.0, 2000.0, 0.0, 40.0);
        // 激活:未开始 0;中点 0.5;结束后 1
        assert!(float_progress(&s, 500.0, FloatPhase::Active) <= 0.0);
        assert!((float_progress(&s, 1500.0, FloatPhase::Active) - 0.5).abs() < 1e-9);
        assert!((float_progress(&s, 3000.0, FloatPhase::Active) - 1.0).abs() < 1e-9);
        // 退激活:冻结在半程,墙钟以 1× 回退(currentTime 递减)——
        // t0=0.5×1000ms 后归零,走 250ms 时剩 0.25
        let reverse = FloatReverse { elapsed_ms: 0.0, media_at_disable_ms: 1500.0 };
        assert!((float_progress(&s, f64::INFINITY, FloatPhase::Reversing(reverse)) - 0.5).abs() < 1e-9);
        let quarter = FloatReverse { elapsed_ms: 250.0, media_at_disable_ms: 1500.0 };
        assert!((float_progress(&s, f64::INFINITY, FloatPhase::Reversing(quarter)) - 0.25).abs() < 1e-9);
        // 反演回落后保持 0(Sunk),不回跳到媒体时间推导的全浮起
        assert!(float_progress(&s, 99999.0, FloatPhase::Sunk) <= 0.0);
        let done = FloatReverse { elapsed_ms: 500.0 + 1e-9, media_at_disable_ms: 1500.0 };
        assert!(float_reverse_finished(&[s.clone()], &done), "反演结束");
        let undone = FloatReverse { elapsed_ms: 499.0, media_at_disable_ms: 1500.0 };
        assert!(!float_reverse_finished(&[s], &undone));
    }
}

#[cfg(test)]
mod emphasize_tests {
    use super::*;

    fn word(text: &str, start: f64, end: f64) -> LyricWord {
        LyricWord {
            start_time_ms: start,
            end_time_ms: end,
            text: text.into(),
        }
    }

    #[test]
    fn emphasize_params_shape_and_last_word() {
        // du=1000:amount=(0.5)³·0.6,blur=(1/3)³·0.5
        let (amount, blur, du) = emphasize_params(1000.0, false);
        assert!((amount - 0.075).abs() < 1e-9, "got {amount}");
        assert!((blur - 0.5 / 27.0).abs() < 1e-9, "got {blur}");
        assert!((du - 1000.0).abs() < 1e-9);
        // 末词:amount×1.6、blur×1.5、du×1.2
        let (amount, blur, du) = emphasize_params(1000.0, true);
        assert!((amount - 0.075 * 1.6).abs() < 1e-9);
        assert!((blur - 0.5 / 27.0 * 1.5).abs() < 1e-9);
        assert!((du - 1200.0).abs() < 1e-9);
        // du=6000:>1 开方;amount √3·0.6<1.2,blur √2·0.5<0.8
        let (amount, blur, du) = emphasize_params(6000.0, false);
        assert!((amount - 3.0_f64.sqrt() * 0.6).abs() < 1e-9, "got {amount}");
        assert!((blur - 2.0_f64.sqrt() * 0.5).abs() < 1e-9, "got {blur}");
        assert!((du - 6000.0).abs() < 1e-9);
        // 末词增幅触发封顶:du=8000 末词 → amount √4·0.6·1.6=1.92 → 1.2
        let (amount, _, _) = emphasize_params(8000.0, true);
        assert!((amount - EMP_AMOUNT_CAP).abs() < 1e-9, "got {amount}");
    }

    #[test]
    fn emphasize_easing_rises_then_falls() {
        assert!(emphasize_easing(0.0).abs() < 1e-9);
        assert!((emphasize_easing(0.5) - 1.0).abs() < 1e-6, "峰值应达 1");
        assert!(emphasize_easing(1.0).abs() < 1e-6, "末点回落到 0");
        // 前半单调升、后半单调降
        let mut prev = 0.0;
        for k in 1..=10 {
            let v = emphasize_easing(k as f64 / 20.0);
            assert!(v >= prev, "前半应单调升");
            prev = v;
        }
        let mut prev = 1.0;
        for k in 11..=20 {
            let v = emphasize_easing(k as f64 / 20.0);
            assert!(v <= prev, "后半应单调降");
            prev = v;
        }
    }

    #[test]
    fn emphasize_word_delay_staggers_chars() {
        let emp = Emphasize {
            line_delay_ms: 500.0,
            du_ms: 1000.0,
            amount: 0.5,
            blur: 0.2,
            char_index: 1,
            char_count: 2,
            is_last_word: false,
        };
        // wordDe = 500 + 1000/2.5/2·1 = 700
        assert!((emphasize_word_delay(&emp) - 700.0).abs() < 1e-9);
        // 辉光进度:延迟前 0,延迟+du 处 1
        assert!(emphasize_glow_progress(&emp, 0.0) <= 0.0);
        assert!((emphasize_glow_progress(&emp, 1700.0) - 1.0).abs() < 1e-9);
        // 附加上浮提前 400ms:700−400=300 开始,时长 1400
        assert!(emphasize_float_progress(&emp, 299.0) <= 0.0);
        assert!((emphasize_float_progress(&emp, 1700.0) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn build_spans_splits_emphasized_cjk_word_per_char() {
        // "流浪" 2s ≥1s CJK → 两字符 span,共享词级参数
        let words = vec![word("流浪", 1000.0, 3000.0)];
        let spans = build_spans(&words);
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].text, "流");
        assert_eq!(spans[1].text, "浪");
        for (index, span) in spans.iter().enumerate() {
            let emp = span.emp.expect("emphasize 词的 span 必带参数");
            assert_eq!(emp.char_index, index);
            assert_eq!(emp.char_count, 2);
            assert_eq!(emp.is_last_word, true, "唯一词即末词");
            // 末词 du ×1.2:2000 → 2400(上游 calculateEmphasizeParams)
            assert!((emp.du_ms - 2400.0).abs() < 1e-9, "got {}", emp.du_ms);
            // du=2000 末词:amount = (2000/2000=1 → 1³·0.6)·1.6
            assert!((emp.amount - 0.6 * 1.6).abs() < 1e-9, "got {}", emp.amount);
        }
    }

    #[test]
    fn build_spans_keeps_short_cjk_unemphasized() {
        // CJK 词 <1s 不触发
        let words = vec![word("流浪", 0.0, 800.0)];
        let spans = build_spans(&words);
        assert_eq!(spans.len(), 2);
        assert!(spans.iter().all(|span| span.emp.is_none()));
    }

    #[test]
    fn build_spans_emphasized_latin_splits_per_char() {
        // 拉丁词 ≥1s 且 2-7 字符 → 逐字符 span
        let words = vec![word("hello", 0.0, 1500.0)];
        let spans = build_spans(&words);
        assert_eq!(spans.len(), 5, "hello 逐字符拆分");
        let emp = spans[0].emp.expect("拉丁 emphasize 词必带参数");
        assert_eq!(emp.char_count, 5);
        assert_eq!(emp.char_index, 0);
        // 拉丁词 ≥8 字符不触发
        let words = vec![word("abcdefgh", 0.0, 1500.0)];
        let spans = build_spans(&words);
        assert_eq!(spans.len(), 1, "超长拉丁词不拆分");
        assert!(spans[0].emp.is_none());
    }
}
