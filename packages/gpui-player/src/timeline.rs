//! 歌词时间轴状态机:行高亮生命周期、间奏判定与 seek 检测。
//!
//! 移植自 `@applemusic-like-lyrics/core` 的 `lyric-player/base/timeline.ts`
//! 与 `seek-detector.ts`(语义见各常量注释),是滚动布局的输入。

/// 相邻行间隔达到该值视为间奏(上游 MIN_INTERLUDE_GAP)
pub const MIN_INTERLUDE_GAP_MS: f64 = 7000.0;
/// seek 判定:抖动容差(上游 JITTER_TOLERANCE)
const JITTER_TOLERANCE_MS: f64 = 150.0;
/// seek 判定:媒体时钟允许落后墙钟的比例(上游 DRIFT_SLACK)
const DRIFT_SLACK: f64 = 0.5;
/// seek 判定:媒体时钟单帧可信推进上限(上游 MAX_TRUSTED_GAP)
const MAX_TRUSTED_GAP_MS: f64 = 800.0;

/// 一行歌词(原型只关心时间与文本)
#[derive(Debug, Clone)]
pub struct LyricLine {
    pub start_time_ms: f64,
    pub end_time_ms: f64,
    pub words: Vec<LyricWord>,
}

#[derive(Debug, Clone)]
pub struct LyricWord {
    pub start_time_ms: f64,
    pub end_time_ms: f64,
    pub text: String,
}

impl LyricLine {
    pub fn full_line_text(&self) -> String {
        self.words.iter().map(|w| w.text.as_str()).collect()
    }
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

/// 时间轴:维护行命中与高亮集合。
///
/// 高亮生命周期(上游语义):行进入 `[start, end)` 即 playing 并加入高亮;
/// 唱完后保持高亮,直到新行开始、进入间奏、歌曲播完或 seek 才熄灭;
/// `scroll_to_index` 恒为高亮行的最小索引。
#[derive(Debug, Default)]
pub struct Timeline {
    lines: Vec<LyricLine>,
    /// 高亮行索引集合,按索引有序
    highlighted: Vec<usize>,
    /// 间奏锚点行:间奏开始前最后一行的索引
    interlude_anchor: Option<usize>,
    last_time_ms: f64,
}

impl Timeline {
    pub fn new(lines: Vec<LyricLine>) -> Self {
        let mut lines = lines;
        lines.sort_by(|a, b| {
            a.start_time_ms
                .partial_cmp(&b.start_time_ms)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        Self {
            lines,
            highlighted: Vec::new(),
            interlude_anchor: None,
            last_time_ms: 0.0,
        }
    }

    pub fn lines(&self) -> &[LyricLine] {
        &self.lines
    }

    pub fn highlighted(&self) -> &[usize] {
        &self.highlighted
    }

    pub fn scroll_to_index(&self) -> Option<usize> {
        self.highlighted.first().copied()
    }

    /// 间奏锚点行(焦点解析用);无间奏时为 None
    pub fn interlude_anchor(&self) -> Option<usize> {
        self.interlude_anchor
    }

    /// 当前时间是否处于间奏(有锚点即视为间奏中)
    pub fn in_interlude(&self) -> bool {
        self.interlude_anchor.is_some()
    }

    /// 推进时间轴。`is_seek` 由 SeekDetector 给出。
    pub fn sync(&mut self, time_ms: f64, is_seek: bool) {
        if is_seek {
            self.highlighted.clear();
            self.interlude_anchor = None;
        }
        let went_backward = time_ms < self.last_time_ms;
        self.last_time_ms = time_ms;

        // 行命中差分:新进入播放的行加入高亮
        for (index, line) in self.lines.iter().enumerate() {
            if line.start_time_ms <= time_ms && time_ms < line.end_time_ms {
                if !self.highlighted.contains(&index) {
                    self.highlighted.push(index);
                    self.highlighted.sort_unstable();
                    // 新行开始:熄灭所有早于该行的高亮(唱完的旧行退场)
                    self.highlighted.retain(|&i| i >= index);
                }
            }
        }

        // 行退场后保持高亮(生命周期见结构体注释);仅在以下情况熄灭:
        // 1) 无任何行在播且不在间奏 —— 判定歌曲结束或前奏
        let any_playing = self
            .lines
            .iter()
            .any(|line| line.start_time_ms <= time_ms && time_ms < line.end_time_ms);
        if !any_playing {
            let next_start = self
                .lines
                .iter()
                .find(|line| line.start_time_ms > time_ms)
                .map(|line| line.start_time_ms);
            match next_start {
                // 距下一行开始 >= 7000ms:进入/保持间奏
                Some(start) if start - time_ms >= MIN_INTERLUDE_GAP_MS => {
                    self.interlude_anchor = Some(self.highlighted.last().copied().unwrap_or(
                        // 高亮为空时锚定在间奏前的最后一行
                        self.lines
                            .iter()
                            .rposition(|line| line.end_time_ms <= time_ms)
                            .unwrap_or(0),
                    ));
                }
                // 歌曲播完:熄灭全部高亮(上游"播完"场景)
                None => {
                    self.highlighted.clear();
                    self.interlude_anchor = None;
                }
                // 前奏/普通间隙:保持当前高亮不动
                Some(_) => {}
            }
        } else {
            self.interlude_anchor = None;
        }

        // 倒退播放(未判 seek 的小幅回退在 SeekDetector 已拦截,
        // 此处兜底:高亮中结束时间已越过下一行开始的行应熄灭)
        if went_backward {
            if let Some(&first) = self.highlighted.first() {
                let first_start = self.lines[first].start_time_ms;
                self.highlighted.retain(|&i| {
                    let line = &self.lines[i];
                    line.end_time_ms > time_ms || line.start_time_ms >= first_start
                });
            }
        }
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
        // 唱完但未到间奏阈值(距下一行 5500ms):保持高亮
        assert_eq!(t.highlighted(), &[0]);
        assert_eq!(t.scroll_to_index(), Some(0));
        t.sync(4000.0, false);
        // 距下一行 4000ms:仍不是间奏,但也不是任何行在播
        // 8000-4000=4000 < 7000:保持高亮
        assert_eq!(t.highlighted(), &[0]);
        t.sync(5000.0, false);
        // 8000-5000=3000 < 7000:保持
        assert_eq!(t.highlighted(), &[0]);
        t.sync(8000.0, false);
        // 第二行开始:旧高亮退场
        assert_eq!(t.highlighted(), &[1]);
        assert_eq!(t.scroll_to_index(), Some(1));
    }

    #[test]
    fn interlude_anchor_set_on_gap() {
        let mut t = timeline(vec![
            line(0.0, 2000.0, "第一行"),
            line(15000.0, 17000.0, "很久之后"),
        ]);
        t.sync(1000.0, false);
        t.sync(2001.0, false);
        t.sync(5000.0, false);
        // 15000-5000=10000 >= 7000:间奏,锚点为最后唱过的行
        assert!(t.in_interlude());
        assert_eq!(t.interlude_anchor(), Some(0));
        // 间奏期间高亮保持(锚点行熄灭规则:上游 interlude 状态下高亮仍指向锚点)
        t.sync(15000.0, false);
        assert!(!t.in_interlude());
        assert_eq!(t.highlighted(), &[1]);
    }

    #[test]
    fn seek_rebuilds_state() {
        let mut detector = SeekDetector::default();
        let mut t = timeline(vec![
            line(0.0, 2000.0, "a"),
            line(2000.0, 4000.0, "b"),
            line(4000.0, 6000.0, "c"),
        ]);
        // 正常推进
        for (media, wall) in [(0.0, 0.0), (500.0, 500.0), (1000.0, 1000.0)] {
            let seek = detector.detect(media, wall, true);
            t.sync(media, seek);
        }
        assert_eq!(t.highlighted(), &[0]);
        // 前进跳到 5000ms:墙钟走了 16ms,媒体跳了 4000ms → 判 seek
        let seek = detector.detect(5000.0, 1016.0, true);
        assert!(seek, "大幅前进应判 seek");
        t.sync(5000.0, true);
        assert_eq!(t.highlighted(), &[2]);
        // 倒退
        let seek = detector.detect(1000.0, 1032.0, true);
        assert!(seek, "倒退必判 seek");
        t.sync(1000.0, true);
        assert_eq!(t.highlighted(), &[0]);
    }

    #[test]
    fn seek_detector_tolerates_jitter() {
        let mut d = SeekDetector::default();
        // 首次调用建立基线
        assert!(!d.detect(0.0, 0.0, true));
        // 100ms 内的正常推进(60fps 帧间隔)
        assert!(!d.detect(16.0, 16.0, true));
        // 媒体比墙钟慢 40%:在容差内(墙钟 500,容差 250)
        assert!(!d.detect(316.0, 500.0, true));
        // 暂停期间时间冻结:不判跳
        assert!(!d.detect(316.0, 5000.0, false));
        // 媒体倒退(哪怕暂停期间的小幅倒退):上游语义必判跳
        assert!(d.detect(200.0, 6000.0, false));
        // seek 后从新位置正常推进:不判跳
        assert!(!d.detect(216.0, 7000.0, true));
    }

    #[test]
    fn song_end_clears_highlights() {
        let mut t = timeline(vec![line(0.0, 2000.0, "唯一一行")]);
        t.sync(1000.0, false);
        assert_eq!(t.highlighted(), &[0]);
        t.sync(3000.0, false);
        // 播完且无下一行:熄灭
        assert!(t.highlighted().is_empty());
        assert!(!t.in_interlude());
    }
}
