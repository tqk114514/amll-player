//! 歌词时间轴状态机:行高亮生命周期、间奏判定与 seek 检测。
//!
//! 移植自 `@applemusic-like-lyrics/core` 的 `lyric-player/base/timeline.ts`
//! 与 `seek-detector.ts`。高亮生命周期(上游语义):
//! 行进入 `[start, end)` 即 playing 并加入高亮;唱完后保持高亮(lingering),
//! 直到新行开始、进入间奏(焦点交圆点并清空高亮)、歌曲播完或 seek。

/// 相邻行间隔达到该值视为间奏(上游 MIN_INTERLUDE_GAP)
pub const MIN_INTERLUDE_GAP_MS: f64 = 7000.0;
/// seek 判定:抖动容差(上游 JITTER_TOLERANCE)
const JITTER_TOLERANCE_MS: f64 = 150.0;
/// seek 判定:媒体时钟允许落后墙钟的比例(上游 DRIFT_SLACK)
const DRIFT_SLACK: f64 = 0.5;
/// seek 判定:媒体时钟单帧可信推进上限(上游 MAX_TRUSTED_GAP)
const MAX_TRUSTED_GAP_MS: f64 = 800.0;
/// 间奏圆点可展示所需的最小剩余演出时间(上游 bodyMs ≥ DOT_ENTER_TOTAL_MS,
/// 即 end − 入场点 ≥ ENTER_HOLD_MS + EXIT_TOTAL_MS + DOT_ENTER_TOTAL_MS)
const INTERLUDE_DISPLAY_MIN_REMAIN_MS: f64 = 500.0 + 1000.0 + 910.0;

/// 一行歌词(原型只关心时间与文本)
#[derive(Debug, Clone)]
pub struct LyricLine {
    pub start_time_ms: f64,
    pub end_time_ms: f64,
    pub words: Vec<LyricWord>,
}

#[derive(Debug, Clone)]
pub struct LyricWord {
    #[allow(dead_code)] // M4 逐词渐变揭示将消费
    pub start_time_ms: f64,
    #[allow(dead_code)] // M4 逐词渐变揭示将消费
    pub end_time_ms: f64,
    pub text: String,
}

impl LyricLine {
    pub fn full_line_text(&self) -> String {
        self.words.iter().map(|w| w.text.as_str()).collect()
    }
}

/// 预计算的间奏区间(上游 precalculatedInterludes)
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Interlude {
    pub start_ms: f64,
    pub end_ms: f64,
    /// 锚点行索引:间奏前最后一行;前奏间奏为 -1
    pub anchor: i64,
}

/// 时间轴的单帧增量变化(上游 TimelineDiff)
#[derive(Debug, Default, Clone)]
pub struct TimelineDiff {
    pub has_changed: bool,
    #[allow(dead_code)] // 与上游对齐保留;调用方直接消费 SeekDetector 结果
    pub is_time_jumped: bool,
    pub is_interlude_changed: bool,
    pub is_scroll_to_changed: bool,
    pub is_end_of_song_changed: bool,
    #[allow(dead_code)] // 与上游对齐保留;焦点翻转已含在 interlude/highlight 变化中
    pub is_focus_changed: bool,
}

/// seek 检测器(上游 SeekDetector):
/// 倒退必判跳;前进时,媒体推进量减去可信推进量超过容差判跳。
#[derive(Debug, Default)]
pub struct SeekDetector {
    last_media_time_ms: Option<f64>,
    last_wall_time_ms: Option<f64>,
}

impl SeekDetector {
    /// 输入当前媒体时间(毫秒)与墙钟时间(毫秒),返回是否判定为 seek。
    /// 暂停期间调用应传 `is_playing = false`(容差固定 150ms)。
    pub fn detect(&mut self, media_time_ms: f64, wall_time_ms: f64, is_playing: bool) -> bool {
        let Some(last_media) = self.last_media_time_ms else {
            self.last_media_time_ms = Some(media_time_ms);
            self.last_wall_time_ms = Some(wall_time_ms);
            return false;
        };
        let last_wall = self.last_wall_time_ms.unwrap_or(wall_time_ms);
        self.last_media_time_ms = Some(media_time_ms);
        self.last_wall_time_ms = Some(wall_time_ms);

        let media_delta = media_time_ms - last_media;
        if media_delta < 0.0 {
            // 倒退:必判跳
            return true;
        }
        let wall_delta = (wall_time_ms - last_wall).max(0.0);
        let expected = if is_playing {
            wall_delta.min(MAX_TRUSTED_GAP_MS)
        } else {
            0.0
        };
        let tolerance = if is_playing {
            JITTER_TOLERANCE_MS.max(wall_delta * DRIFT_SLACK)
        } else {
            JITTER_TOLERANCE_MS
        };
        media_delta - expected > tolerance
    }
}

/// 上游 optimize-lyric.ts 的移植(App 未关闭任何优化项,默认全开)。
/// 原型无 BG 行,相关分支省略;直接原地修改入参(上游同语义)。
///
/// 管线顺序(上游 optimizeLyricLines):
/// normalizeSpaces → resetLineTimestamps → 排序 → cleanUnintentionalOverlaps
/// → tryAdvanceStartTime(提前量:首行/间隔充足 600ms,重叠 400ms 或 70%)
pub fn optimize_lines(lines: &mut [LyricLine]) {
    // normalizeSpaces:词内连续空白折叠为单空格
    for line in lines.iter_mut() {
        for word in &mut line.words {
            collapse_spaces(&mut word.text);
        }
    }
    // resetLineTimestamps:行级时间戳以词级为准
    for line in lines.iter_mut() {
        if line.words.len() == 1
            && line.words[0].start_time_ms == 0.0
            && line.words[0].end_time_ms == 0.0
            && (line.start_time_ms != 0.0 || line.end_time_ms != 0.0)
        {
            line.words[0].start_time_ms = line.start_time_ms;
            line.words[0].end_time_ms = line.end_time_ms;
        } else if let Some(first) = line.words.first()
            && let Some(last) = line.words.last()
        {
            line.start_time_ms = first.start_time_ms;
            line.end_time_ms = last.end_time_ms;
        }
    }
    lines.sort_by(|a, b| {
        a.start_time_ms
            .partial_cmp(&b.start_time_ms)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    clean_unintentional_overlaps(lines);
    try_advance_start_time(lines);
}

fn collapse_spaces(text: &mut String) {
    // 上游 normalizeSpaces:连续空白折叠为一个空格(保留首尾单空格)
    let needs = text.chars().any(|c| c.is_whitespace() && c != ' ') || text.contains("  ");
    if !needs {
        return;
    }
    let mut out = String::with_capacity(text.len());
    let mut in_space = false;
    for ch in text.chars() {
        if ch.is_whitespace() {
            if !in_space {
                out.push(' ');
                in_space = true;
            }
        } else {
            out.push(ch);
            in_space = false;
        }
    }
    *text = out;
}

/// 清洗非刻意的重叠:重叠 ≥500ms 或 (>100ms 且 > 下一行时长 10%) 视为有意,
/// 否则把当前行结束时间截到下一行开始(上游同规则)。
fn clean_unintentional_overlaps(lines: &mut [LyricLine]) {
    for i in 0..lines.len().saturating_sub(1) {
        for j in (i + 1)..lines.len() {
            let overlap = lines[i].end_time_ms - lines[j].start_time_ms;
            if overlap <= 0.0 {
                break;
            }
            let next_duration = lines[j].end_time_ms - lines[j].start_time_ms;
            let percentage_threshold = next_duration * 0.1;
            let intentional =
                overlap >= 500.0 || (overlap > 100.0 && overlap > percentage_threshold);
            if !intentional {
                lines[i].end_time_ms = lines[j].start_time_ms;
                break;
            }
        }
    }
}

/// 提前量(上游 tryAdvanceStartTime):
/// - 首行或与上一行不重叠:提前 600ms,边界为上一行(原始)结束时间
/// - 与上一行重叠:提前 min(400ms, 重叠时长×70%),边界为上一行(已提前)开始时间
fn try_advance_start_time(lines: &mut [LyricLine]) {
    const DEFAULT_ADVANCE: f64 = 600.0;
    const FALLBACK_ADVANCE: f64 = 400.0;
    const FALLBACK_RATIO: f64 = 0.7;

    let mut prev_line_start = 0.0;
    let mut prev_line_end = 0.0;
    let mut prev_group_start = 0.0;
    let mut prev_group_end = 0.0;
    let mut has_prev = false;
    for line in lines.iter_mut() {
        let original_start = line.start_time_ms;
        let original_end = line.end_time_ms;
        let (advance, safe_boundary) = if has_prev {
            if original_start >= prev_line_end {
                (DEFAULT_ADVANCE, prev_group_end)
            } else {
                let overlap = prev_line_end - original_start;
                let amount = if overlap < FALLBACK_ADVANCE {
                    overlap * FALLBACK_RATIO
                } else {
                    FALLBACK_ADVANCE
                };
                (amount, prev_line_start)
            }
        } else {
            (DEFAULT_ADVANCE, 0.0)
        };
        let new_start = (line.start_time_ms - advance).max(safe_boundary);
        if new_start < line.start_time_ms {
            line.start_time_ms = new_start;
        }
        if has_prev {
            if original_start < prev_group_end && original_end > prev_group_start {
                prev_group_start = prev_group_start.min(original_start);
                prev_group_end = prev_group_end.max(original_end);
            } else {
                prev_group_start = original_start;
                prev_group_end = original_end;
            }
        } else {
            prev_group_start = original_start;
            prev_group_end = original_end;
        }
        prev_line_start = line.start_time_ms;
        prev_line_end = original_end;
        has_prev = true;
    }
}

/// 时间轴(上游 TimelineController)
#[derive(Debug, Default)]
pub struct Timeline {
    lines: Vec<LyricLine>,
    interludes: Vec<Interlude>,
    interlude_cursor: usize,
    /// 正在播放的行(有序)
    playing: Vec<usize>,
    /// 高亮行(playing + 唱完保持的 lingering,有序)
    highlighted: Vec<usize>,
    /// scrollToIndex(上游 snapshot 持久字段:高亮清空后仍保持,
    /// 仅在新行加入高亮或 seek 重建时更新)
    scroll_to: usize,
    active_interlude: Option<Interlude>,
    /// 本次间奏的入场时间(seek 进入时圆点按剩余时间决定是否展示)
    interlude_entry_ms: f64,
    interlude_entered_by_seek: bool,
    playback_cursor: usize,
    last_time_ms: f64,
    max_end_ms: f64,
    end_of_song: bool,
}

impl Timeline {
    pub fn new(mut lines: Vec<LyricLine>) -> Self {
        // 上游 setLyricLines 会先过优化管线(App 默认全开),时间轴消费优化后的行
        optimize_lines(&mut lines);
        let max_end_ms = lines
            .iter()
            .map(|line| line.end_time_ms)
            .fold(0.0, f64::max);
        let interludes = Self::calculate_interludes(&lines);
        Self {
            lines,
            interludes,
            interlude_cursor: 0,
            playing: Vec::new(),
            highlighted: Vec::new(),
            scroll_to: 0,
            active_interlude: None,
            interlude_entry_ms: 0.0,
            interlude_entered_by_seek: false,
            playback_cursor: 0,
            last_time_ms: 0.0,
            max_end_ms,
            end_of_song: false,
        }
    }

    /// 上游 calculateInterludes:以"已见过的最大结束时间"与下一行开始时间的
    /// 空隙判定间奏,锚点为空隙前的最后一行(前奏为 -1)。
    /// 单行歌词同样有前奏间奏(i = -1 的一次迭代)。
    fn calculate_interludes(lines: &[LyricLine]) -> Vec<Interlude> {
        let mut interludes = Vec::new();
        let mut max_end: f64 = 0.0;
        for i in -1..(lines.len() as i64 - 1) {
            if i >= 0 {
                max_end = max_end.max(lines[i as usize].end_time_ms);
            }
            let gap_end = max_end.max(lines[(i + 1) as usize].start_time_ms);
            if gap_end - max_end >= MIN_INTERLUDE_GAP_MS {
                interludes.push(Interlude {
                    start_ms: max_end,
                    end_ms: gap_end,
                    anchor: i,
                });
            }
        }
        interludes
    }

    pub fn lines(&self) -> &[LyricLine] {
        &self.lines
    }

    pub fn highlighted(&self) -> &[usize] {
        &self.highlighted
    }

    /// scrollToIndex:上游持久字段(高亮清空后保持旧值,由 seek/新行更新)
    pub fn scroll_to_index(&self) -> usize {
        self.scroll_to
    }

    /// latestHighlightedIndex:高亮行的最大索引
    pub fn latest_highlighted(&self) -> Option<usize> {
        self.highlighted.last().copied()
    }

    pub fn active_interlude(&self) -> Option<Interlude> {
        self.active_interlude
    }

    pub fn in_interlude(&self) -> bool {
        self.active_interlude.is_some()
    }

    /// 焦点是否在间奏圆点上(上游 isFocusOnInterlude:命中间奏且无任何高亮)
    pub fn focus_on_interlude(&self) -> bool {
        self.active_interlude.is_some() && self.highlighted.is_empty()
    }

    /// 间奏圆点是否可展示(上游 setInterlude 的 bodyMs ≥ DOT_ENTER_TOTAL_MS):
    /// 自然进入按间奏全长、seek 进入按剩余时间判定
    pub fn can_display_interlude(&self) -> bool {
        match self.active_interlude {
            Some(interlude) => {
                let from = if self.interlude_entered_by_seek {
                    self.interlude_entry_ms.max(interlude.start_ms)
                } else {
                    interlude.start_ms
                };
                interlude.end_ms - from >= INTERLUDE_DISPLAY_MIN_REMAIN_MS
            }
            None => false,
        }
    }

    pub fn end_of_song(&self) -> bool {
        self.end_of_song
    }

    /// 上游 resolveActiveInterlude:二分(seek)或游标推进(正常播放)
    fn resolve_active_interlude(&mut self, time_ms: f64, is_seek: bool) -> Option<Interlude> {
        if self.interludes.is_empty() {
            return None;
        }
        if is_seek {
            let mut cursor = self.interludes.len();
            let (mut left, mut right) = (0, self.interludes.len() - 1);
            while left <= right {
                let mid = left + (right - left) / 2;
                if self.interludes[mid].end_ms > time_ms {
                    cursor = mid;
                    if mid == 0 {
                        break;
                    }
                    right = mid - 1;
                } else {
                    left = mid + 1;
                }
            }
            self.interlude_cursor = cursor;
            return self
                .interludes
                .get(cursor)
                .filter(|inter| time_ms >= inter.start_ms && time_ms < inter.end_ms)
                .copied();
        }
        while self.interlude_cursor < self.interludes.len() {
            let inter = self.interludes[self.interlude_cursor];
            if time_ms >= inter.start_ms && time_ms < inter.end_ms {
                return Some(inter);
            }
            if time_ms >= inter.end_ms {
                self.interlude_cursor += 1;
            } else {
                break;
            }
        }
        None
    }

    /// 推进时间轴。`is_seek` 由 SeekDetector 给出。返回单帧增量变化。
    pub fn sync(&mut self, time_ms: f64, is_seek: bool) -> TimelineDiff {
        let prev_interlude = self.active_interlude;
        let prev_highlighted = self.highlighted.clone();
        let prev_playing = self.playing.clone();
        let prev_scroll_to = self.scroll_to;
        let prev_focus_on_interlude = self.focus_on_interlude();
        let prev_end_of_song = self.end_of_song;
        self.last_time_ms = time_ms;

        let active_interlude = self.resolve_active_interlude(time_ms, is_seek);
        if active_interlude != prev_interlude {
            self.interlude_entry_ms = time_ms;
            self.interlude_entered_by_seek = is_seek;
        }
        self.active_interlude = active_interlude;

        let is_past_last = !self.lines.is_empty() && time_ms >= self.max_end_ms;
        if is_seek {
            self.perform_seek(time_ms, active_interlude.is_some() || is_past_last);
        } else {
            self.perform_playback(time_ms);
        }

        // 间奏中无行在播 → 清空高亮(焦点交圆点);播完 → 清空高亮
        // (scrollTo 保持上游的持久语义,不随高亮清空而回退)
        if self.active_interlude.is_some() && self.playing.is_empty() {
            self.highlighted.clear();
        }
        self.end_of_song = is_past_last;
        if is_past_last {
            self.highlighted.clear();
        }

        TimelineDiff {
            // 上游 hasChanged 含 addedPlaying/removedPlaying(行退出播放也触发重排)
            has_changed: is_seek
                || self.highlighted != prev_highlighted
                || self.playing != prev_playing
                || self.active_interlude != prev_interlude
                || self.scroll_to != prev_scroll_to
                || self.end_of_song != prev_end_of_song,
            is_time_jumped: is_seek,
            is_interlude_changed: self.active_interlude != prev_interlude,
            is_scroll_to_changed: self.scroll_to != prev_scroll_to,
            is_end_of_song_changed: self.end_of_song != prev_end_of_song,
            is_focus_changed: self.focus_on_interlude() != prev_focus_on_interlude,
        }
    }

    /// 正常播放:游标扫描加入新播放行,新行开始时熄灭唱完的旧行
    fn perform_playback(&mut self, time_ms: f64) {
        // 移除不再播放的行(lingering 的高亮仍保留到新行开始)
        self.playing
            .retain(|&index| self.lines[index].start_time_ms <= time_ms && time_ms < self.lines[index].end_time_ms);
        // 游标推进:加入新开始的行
        let mut cursor = self.playback_cursor;
        let mut added = Vec::new();
        while cursor < self.lines.len() {
            let line = &self.lines[cursor];
            if line.start_time_ms > time_ms {
                break;
            }
            if line.start_time_ms <= time_ms && time_ms < line.end_time_ms && !self.playing.contains(&cursor) {
                self.playing.push(cursor);
                self.playing.sort_unstable();
                added.push(cursor);
            }
            cursor += 1;
        }
        self.playback_cursor = cursor;

        // 新行开始:熄灭所有已不在播放的高亮(上游 expiredHighlighted 语义),
        // 并把 scrollTo 更新为新高亮的最小索引(上游 performPlayback)
        if !added.is_empty() {
            for &index in &added {
                if !self.highlighted.contains(&index) {
                    self.highlighted.push(index);
                }
            }
            self.highlighted.retain(|&i| self.playing.contains(&i));
            self.highlighted.sort_unstable();
            self.scroll_to = self.highlighted.first().copied().unwrap_or(self.scroll_to);
        }
    }

    /// Seek:按目标时间重建,结果与正常播放到该时刻一致(上游 performSeek);
    /// 高亮簇 = 与锚点行开始时间重叠的所有行;间奏/播完时丢弃 lingering
    /// (dropLingeringWhenIdle),但 scrollTo 仍取簇内最小索引(上游在清空前记录)。
    fn perform_seek(&mut self, time_ms: f64, drop_lingering: bool) {
        // 二分找第一个 start > time 的行
        let first_greater = self
            .lines
            .partition_point(|line| line.start_time_ms <= time_ms);
        // 锚点行:first_greater 前最后一个有实际时长的行
        let anchor_index = (0..first_greater)
            .rev()
            .find(|&index| self.lines[index].end_time_ms > self.lines[index].start_time_ms);
        let Some(anchor_index) = anchor_index else {
            // 无锚点:清空播放与高亮,scrollTo 归零(上游 early return)
            self.playing.clear();
            self.highlighted.clear();
            self.scroll_to = 0;
            self.playback_cursor = first_greater;
            return;
        };
        let anchor_start = self.lines[anchor_index].start_time_ms;
        // 簇:0..=anchor 中与锚点开始时间重叠(end > anchorStart)的行
        let mut playing = Vec::new();
        let mut highlighted = Vec::new();
        let mut min_highlighted = anchor_index;
        for index in (0..=anchor_index).rev() {
            if self.lines[index].end_time_ms <= anchor_start {
                continue;
            }
            let line = &self.lines[index];
            if line.start_time_ms <= time_ms && time_ms < line.end_time_ms {
                playing.push(index);
            }
            highlighted.push(index);
            min_highlighted = index;
        }
        let min_playing = playing.first().copied();
        if drop_lingering && playing.is_empty() {
            highlighted.clear();
        }
        playing.sort_unstable();
        highlighted.sort_unstable();
        self.playing = playing;
        self.highlighted = highlighted;
        self.scroll_to = min_highlighted;
        self.playback_cursor = min_playing.unwrap_or(first_greater);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(start: f64, end: f64, text: &str) -> LyricLine {
        LyricLine {
            start_time_ms: start,
            end_time_ms: end,
            words: vec![LyricWord {
                start_time_ms: start,
                end_time_ms: end,
                text: text.into(),
            }],
        }
    }

    fn timeline(lines: Vec<LyricLine>) -> Timeline {
        Timeline::new(lines)
    }

    #[test]
    fn highlight_lifecycle_persists_after_line_ends() {
        let mut t = timeline(vec![
            line(0.0, 2000.0, "第一行"),
            line(8000.0, 10000.0, "第二行"),
        ]);
        t.sync(1000.0, false);
        assert_eq!(t.highlighted(), &[0]);
        t.sync(2500.0, false);
        // 唱完但距下一行 5500ms(< 7000):保持高亮
        assert_eq!(t.highlighted(), &[0]);
        assert_eq!(t.scroll_to_index(), 0);
        t.sync(5000.0, false);
        assert_eq!(t.highlighted(), &[0]);
        t.sync(8000.0, false);
        // 第二行开始:旧高亮退场
        assert_eq!(t.highlighted(), &[1]);
        assert_eq!(t.scroll_to_index(), 1);
    }

    #[test]
    fn interlude_prelude_anchors_minus_one() {
        // 首行 10s 开始:前奏 [0, 10000) 锚点 -1
        let mut t = timeline(vec![line(10000.0, 12000.0, "很久才开始")]);
        assert_eq!(t.interludes[0].anchor, -1);
        t.sync(5000.0, false);
        assert!(t.in_interlude());
        assert!(t.focus_on_interlude());
        assert_eq!(t.scroll_to_index(), 0);
    }

    #[test]
    fn interlude_flushes_highlights_and_hands_focus_to_dots() {
        let mut t = timeline(vec![
            line(0.0, 2000.0, "第一行"),
            line(15000.0, 17000.0, "很久之后"),
        ]);
        t.sync(1000.0, false);
        assert_eq!(t.highlighted(), &[0]);
        // 间奏区间是预计算的 [2000, 15000):第一行唱完后一进入即清空高亮
        t.sync(2500.0, false);
        assert!(t.in_interlude());
        assert!(t.highlighted().is_empty(), "间奏内清空高亮,焦点交圆点");
        assert!(t.focus_on_interlude());
        assert!(t.can_display_interlude(), "自然进入的 13s 间奏可展示圆点");
        assert_eq!(t.active_interlude().unwrap().anchor, 0);
        t.sync(15000.0, false);
        assert!(!t.in_interlude());
        assert_eq!(t.highlighted(), &[1]);
    }

    #[test]
    fn seek_rebuilds_lingering_highlight() {
        let mut d = SeekDetector::default();
        let mut t = timeline(vec![
            line(0.0, 2000.0, "a"),
            line(2000.0, 4000.0, "b"),
            line(4000.0, 6000.0, "c"),
            line(20000.0, 22000.0, "d"),
        ]);
        for (media, wall) in [(0.0, 0.0), (500.0, 500.0), (1000.0, 1000.0)] {
            let seek = d.detect(media, wall, true);
            t.sync(media, seek);
        }
        assert_eq!(t.highlighted(), &[0]);
        // 前进 seek 到 2500ms(b 在播):高亮 = {1},lingering 语义一致
        let seek = d.detect(2500.0, 1016.0, true);
        assert!(seek);
        t.sync(2500.0, true);
        assert_eq!(t.highlighted(), &[1]);
        // 前进 seek 到 7000ms(间隙,c 已唱完,距 d 还有 13s):间奏,清空
        let seek = d.detect(7000.0, 1032.0, true);
        assert!(seek);
        t.sync(7000.0, true);
        assert!(t.in_interlude());
        assert!(t.highlighted().is_empty());
        // 倒退 seek 到 1000ms
        let seek = d.detect(1000.0, 1048.0, true);
        assert!(seek);
        t.sync(1000.0, true);
        assert_eq!(t.highlighted(), &[0]);
    }

    #[test]
    fn seek_into_short_interlude_remain_skips_dots() {
        let mut t = timeline(vec![
            line(0.0, 2000.0, "a"),
            line(30000.0, 32000.0, "b"),
        ]);
        // 间奏 [2000, 30000);seek 到 28500ms:剩余 1500ms < 2410ms,圆点不可展示
        t.sync(28500.0, true);
        assert!(t.in_interlude());
        assert!(!t.can_display_interlude());
        // 自然进入(从头播到 2500ms):按全长判定,可展示
        let mut t2 = timeline(vec![
            line(0.0, 2000.0, "a"),
            line(30000.0, 32000.0, "b"),
        ]);
        t2.sync(2500.0, false);
        assert!(t2.can_display_interlude());
    }

    #[test]
    fn song_end_clears_highlights() {
        let mut t = timeline(vec![line(0.0, 2000.0, "唯一一行")]);
        t.sync(1000.0, false);
        assert_eq!(t.highlighted(), &[0]);
        t.sync(3000.0, false);
        assert!(t.highlighted().is_empty());
        assert!(t.end_of_song());
        assert!(!t.in_interlude());
    }

    #[test]
    fn seek_detector_tolerates_jitter() {
        let mut d = SeekDetector::default();
        assert!(!d.detect(0.0, 0.0, true));
        assert!(!d.detect(16.0, 16.0, true));
        assert!(!d.detect(316.0, 500.0, true));
        assert!(!d.detect(316.0, 5000.0, false));
        assert!(d.detect(200.0, 6000.0, false));
        assert!(!d.detect(216.0, 7000.0, true));
    }

    #[test]
    fn optimize_advances_line_starts_by_600ms() {
        // 首行 9000 → 8400;次行 14200(词尾 13680)→ 13600
        let mut lines = vec![
            LyricLine {
                start_time_ms: 9000.0,
                end_time_ms: 9000.0 + 4200.0,
                words: vec![LyricWord {
                    start_time_ms: 9000.0,
                    end_time_ms: 9000.0 + 4200.0 - 520.0,
                    text: "首行".into(),
                }],
            },
            LyricLine {
                start_time_ms: 14200.0,
                end_time_ms: 14200.0 + 3400.0,
                words: vec![LyricWord {
                    start_time_ms: 14200.0,
                    end_time_ms: 14200.0 + 3400.0 - 520.0,
                    text: "次行".into(),
                }],
            },
        ];
        optimize_lines(&mut lines);
        // resetLineTimestamps:行时间 = 词时间
        assert_eq!(lines[0].end_time_ms, 9000.0 + 4200.0 - 520.0);
        // 提前量:首行 9000−600=8400;次行 14200−600=13600 ≥ 上一行词尾
        assert_eq!(lines[0].start_time_ms, 8400.0);
        assert_eq!(lines[1].start_time_ms, 13600.0);
    }

    #[test]
    fn optimize_overlap_uses_fallback_advance() {
        // 行 A [10000, 14000),行 B [13600, 16000):重叠 400ms → 提前 400×???
        // 重叠 = prevEnd − start = 400 ≥ FALLBACK → 提前 400ms,边界 = prevLineStart(已提前)
        let mut lines = vec![
            LyricLine {
                start_time_ms: 0.0,
                end_time_ms: 14000.0,
                words: vec![LyricWord {
                    start_time_ms: 0.0,
                    end_time_ms: 14000.0,
                    text: "A".into(),
                }],
            },
            LyricLine {
                start_time_ms: 13600.0,
                end_time_ms: 16000.0,
                words: vec![LyricWord {
                    start_time_ms: 13600.0,
                    end_time_ms: 16000.0,
                    text: "B".into(),
                }],
            },
        ];
        optimize_lines(&mut lines);
        // 首行 0−600 → max(0, −600) = 0;行 B:重叠 400ms → 提前 400,边界 prevStart=0
        assert_eq!(lines[0].start_time_ms, 0.0);
        assert_eq!(lines[1].start_time_ms, 13200.0);
    }

    #[test]
    fn optimize_cleans_unintentional_overlap() {
        // 行 A end 15100,行 B start 15000:重叠 100ms ≤ 100 → 无意,A.end 截到 15000
        let mut lines = vec![
            LyricLine {
                start_time_ms: 0.0,
                end_time_ms: 15100.0,
                words: vec![LyricWord {
                    start_time_ms: 0.0,
                    end_time_ms: 15100.0,
                    text: "A".into(),
                }],
            },
            LyricLine {
                start_time_ms: 15000.0,
                end_time_ms: 16000.0,
                words: vec![LyricWord {
                    start_time_ms: 15000.0,
                    end_time_ms: 16000.0,
                    text: "B".into(),
                }],
            },
        ];
        optimize_lines(&mut lines);
        assert_eq!(lines[0].end_time_ms, 15000.0);
    }
}
