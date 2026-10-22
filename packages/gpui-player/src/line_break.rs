//! 长行断行(上游 `utils/lyric-line-break.ts` 的移植)。
//!
//! 上游机制:CSS `white-space: pre-wrap` 让浏览器先自然换行,
//! LineBalancer 再用 DP 把断点重排成"各行长度方差最小"的均衡断行,
//! 断点偏好(奖惩从优到劣):标点 > 空格 > CJK 词界 > 普通词中。
//! 单个不可分词自身超宽时被迫独立成行(大惩罚)。
//!
//! 逐公式移植(代价函数与常量一一对应);已知偏差一处:上游用
//! Intl.Segmenter 找 CJK 词边界(词首低惩罚 0.15、词中 0.5),无分词
//! 库可用,这里近似为"两个 CJK 字符之间"均按词界惩罚——中文断行
//! 略宽松,方差项主导平衡,视觉差异极小(注释见 calc_balanced_breaks)。

use crate::words::is_cjk_char;

/// 断行子节点:宽度(逻辑像素)+ 文本 + 是否纯空白
#[derive(Debug, Clone)]
pub struct WrapChild {
    pub width: f64,
    pub text: String,
    pub is_space: bool,
}

/// 单个词超过容器宽度时的大惩罚倍数(上游 OVERFLOW_PENALTY_MULTIPLIER)
const OVERFLOW_PENALTY_MULTIPLIER: f64 = 1000.0;
/// 截断 CJK 词组边界的惩罚比例(相对容器宽度)
const CJK_BREAK_PENALTY_RATIO: f64 = 0.15;
/// 截断普通文本(非空格、非 CJK 词界)的惩罚比例
const NORMAL_BREAK_PENALTY_RATIO: f64 = 0.5;
/// 在空格处断开的奖励比例
const SPACE_BREAK_REWARD_RATIO: f64 = 0.4;
/// 在标点处断开的奖励比例(比空格更高)
const PUNCTUATION_BREAK_REWARD_RATIO: f64 = 0.6;

/// 上游 PUNCTUATION_REGEX 结尾标点判定
fn ends_with_punctuation(text: &str) -> bool {
    matches!(
        text.chars().last(),
        Some(',' | '.' | ';' | ':' | '!' | '?' | '，' | '。' | '；' | '：' | '！' | '？' | '、'
            | '）' | '】' | '》' | '」' | '』' | '’' | '”' | ')' | ']' | '}' | '>' | '~' | '…')
    )
}

/// 计算平均行长度的断点位置(上游 calcBalancedBreaks)。
/// 返回升序的子节点索引,在它们前面断行(即这些子节点是新行开头)。
pub fn calc_balanced_breaks(children: &[WrapChild], container_width: f64) -> Vec<usize> {
    let n = children.len();
    if n == 0 || container_width <= 0.0 {
        return Vec::new();
    }
    // 字符偏移(供 CJK 边界查询)
    let mut char_offsets = vec![0usize; n + 1];
    for i in 0..n {
        char_offsets[i + 1] = char_offsets[i] + children[i].text.chars().count();
    }
    // 宽度前缀和
    let mut prefix_width = vec![0.0_f64; n + 1];
    for i in 0..n {
        prefix_width[i + 1] = prefix_width[i] + children[i].width;
    }
    if prefix_width[n] <= container_width {
        return Vec::new();
    }
    // CJK 边界近似:偏移处的前后都是 CJK 字符(见模块注释的偏差说明)
    let full_text: String = children.iter().map(|c| c.text.as_str()).collect::<String>();
    let chars: Vec<char> = full_text.chars().collect();
    let is_cjk_boundary = |offset: usize| -> bool {
        offset > 0
            && offset < chars.len()
            && is_cjk_char(chars[offset - 1])
            && is_cjk_char(chars[offset])
    };

    // dp[i] = 从 i 到 n-1 排版的最小代价;next[i] = 最优断点
    let mut dp = vec![f64::INFINITY; n + 1];
    let mut next_break = vec![0usize; n + 1];
    dp[n] = 0.0;
    let penalty_cjk = (container_width * CJK_BREAK_PENALTY_RATIO).powi(2);
    let penalty_normal = (container_width * NORMAL_BREAK_PENALTY_RATIO).powi(2);
    for i in (0..n).rev() {
        for j in i + 1..=n {
            let w = prefix_width[j] - prefix_width[i];
            let line_cost = if w > container_width {
                if j == i + 1 {
                    // 单个无法分割的词自身比容器宽,被迫独立成行
                    (w - container_width).powi(2) * OVERFLOW_PENALTY_MULTIPLIER
                } else {
                    // 行内多个词超宽的组合跳过
                    continue;
                }
            } else {
                // 迫使所有行的长度方差最小化
                (container_width - w).powi(2)
            };
            let break_penalty = if j < n {
                let prev = &children[j - 1];
                if ends_with_punctuation(&prev.text) {
                    -(container_width * PUNCTUATION_BREAK_REWARD_RATIO).powi(2)
                } else if prev.is_space {
                    -(container_width * SPACE_BREAK_REWARD_RATIO).powi(2)
                } else if is_cjk_boundary(char_offsets[j]) {
                    penalty_cjk
                } else {
                    penalty_normal
                }
            } else {
                0.0
            };
            let total = line_cost + break_penalty + dp[j];
            if total < dp[i] {
                dp[i] = total;
                next_break[i] = j;
            }
        }
    }
    let mut breaks = Vec::new();
    let mut curr = 0usize;
    while curr < n {
        curr = next_break[curr];
        if curr == 0 {
            // next_break 未初始化(不可达;首词超宽路径保证 dp[0] 有限)
            break;
        }
        if curr < n {
            breaks.push(curr);
        }
    }
    breaks
}

/// 贪婪断行(CSS 自然换行的行为):累积到放不下时在下一个子节点前断。
/// 用于翻译/音译子行(上游子行是普通块,CSS 换行即贪婪,无均衡)。
/// 单个子节点自身超宽时独立成行。
pub fn greedy_breaks(children: &[WrapChild], container_width: f64) -> Vec<usize> {
    let mut breaks = Vec::new();
    if children.is_empty() || container_width <= 0.0 {
        return breaks;
    }
    let mut line_width = 0.0;
    for (index, child) in children.iter().enumerate() {
        if index > 0 && line_width + child.width > container_width && line_width > 0.0 {
            breaks.push(index);
            line_width = 0.0;
        }
        line_width += child.width;
    }
    breaks
}

/// 把纯文本切成断行子节点:CJK 逐字、拉丁连续词元成组、空白保留
/// (与 build_spans 的分块规则一致,供翻译/音译子行复用断行器)。
pub fn chunk_text(text: &str) -> Vec<WrapChild> {
    let mut children = Vec::new();
    let mut current = String::new();
    let mut current_is_cjk = false;
    let flush = |current: &mut String, children: &mut Vec<WrapChild>| {
        if !current.is_empty() {
            children.push(WrapChild {
                width: 0.0,
                text: std::mem::take(current),
                is_space: false,
            });
        }
    };
    for ch in text.chars() {
        if ch.is_whitespace() {
            flush(&mut current, &mut children);
            children.push(WrapChild {
                width: 0.0,
                text: ch.to_string(),
                is_space: true,
            });
        } else if is_cjk_char(ch) {
            if !current_is_cjk {
                flush(&mut current, &mut children);
                current_is_cjk = true;
            }
            current.push(ch);
            // CJK 逐字:每个字符独立成节点
            flush(&mut current, &mut children);
        } else {
            if current_is_cjk {
                flush(&mut current, &mut children);
                current_is_cjk = false;
            }
            current.push(ch);
        }
    }
    flush(&mut current, &mut children);
    children
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(text: &str, width: f64) -> WrapChild {
        WrapChild {
            width,
            text: text.into(),
            is_space: text.trim().is_empty(),
        }
    }

    #[test]
    fn balanced_two_cjk_words_split_evenly() {
        // 12 个汉字(各宽 10),容器 70:期望断在中间附近(方差最小)
        let children: Vec<WrapChild> = (0..12).map(|i| c(&format!("字{i}"), 10.0)).collect();
        let breaks = calc_balanced_breaks(&children, 70.0);
        assert_eq!(breaks.len(), 1, "12×10=120 宽应断成两行: {breaks:?}");
        // 均衡解:6+6(60/60)优于 7+5(70/40)与 5+7
        assert_eq!(breaks[0], 6, "方差最小应在第 6 个前断: {breaks:?}");
    }

    #[test]
    fn balanced_prefers_space_break() {
        // "AAAA BB CC"(空格宽 6):容器 34 → 空格断(奖励)优于词中截断
        let children = vec![
            c("AAAA", 24.0),
            c(" ", 6.0),
            c("BB", 12.0),
            c(" ", 6.0),
            c("CC", 12.0),
        ];
        let breaks = calc_balanced_breaks(&children, 34.0);
        // 总宽 60 > 34 必断;最优应在某个空格后(奖励)——断点为空格后的节点
        assert!(!breaks.is_empty());
        for b in &breaks {
            assert!(children[*b - 1].is_space, "应在空格处断: {breaks:?}");
        }
    }

    #[test]
    fn balanced_single_overwide_word_gets_own_line() {
        // 单词 120 宽 > 容器 70:被迫独立成行,其余正常
        let children = vec![c("超宽词", 120.0), c(" ", 6.0), c("后", 10.0), c("词", 10.0)];
        let breaks = calc_balanced_breaks(&children, 70.0);
        assert_eq!(breaks[0], 1, "超宽词后必断");
        // 后词(20 宽)可与超宽词同放?不可——超宽词 120 > 70 已独占
    }

    #[test]
    fn no_break_when_fits() {
        let children = vec![c("短", 10.0), c("行", 10.0)];
        assert!(calc_balanced_breaks(&children, 50.0).is_empty());
    }

    #[test]
    fn greedy_matches_css_behavior() {
        let children = vec![
            c("AAAA", 30.0),
            c(" ", 5.0),
            c("BB", 20.0),
            c(" ", 5.0),
            c("CCC", 25.0),
        ];
        // 容器 60:AAAA+空格+BB = 55 ≤ 60;+空格 = 60;+CCC 溢出 → 断在 CCC 前
        let breaks = greedy_breaks(&children, 60.0);
        assert_eq!(breaks, vec![4]);
    }

    #[test]
    fn chunk_cjk_per_char_latin_by_word() {
        let children = chunk_text("你好 world");
        let texts: Vec<&str> = children.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, vec!["你", "好", " ", "world"]);
        assert!(children[2].is_space);
        assert!(!children[3].is_space);
    }
}
