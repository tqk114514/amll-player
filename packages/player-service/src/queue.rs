//! 播放队列(上游 `play-queue-manager.ts` 的移植)。
//!
//! 状态机与上游逐式对应:originalList(顺序表)/ playList(播放表,
//! 洗牌时为重排)/ currentIndex / repeatMode(Off/One/All)/
//! shuffleActive。播放触发经回调上抛(由 Service 接 AudioPlayer),
//! 持久化由调用方决定时机(`persisted_state()` 导出,`restore_*` 导入)。

use serde::{Deserialize, Serialize};

/// 重复模式(上游 RepeatMode)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum RepeatMode {
    #[default]
    Off,
    One,
    All,
}

/// 队列持久化快照(上游 persistedQueueStateAtom 的 localStorage 内容;
/// songId→Song 的解析由调用方在恢复时查库完成)
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PersistedQueueState {
    pub song_ids: Vec<String>,
    pub original_song_ids: Vec<String>,
    pub current_index: i32,
    pub repeat_mode: RepeatMode,
    pub shuffle_active: bool,
    pub playlist_id: Option<i32>,
    /// 暂停时刻的播放位置(秒)
    pub position: f64,
}

/// Fisher-Yates(上游 shuffleArray;Rng 由调用方注入以便测试)
fn shuffle_by<T>(items: &mut [T], rand: &mut impl FnMut(usize) -> usize) {
    for i in (1..items.len()).rev() {
        let j = rand(i + 1);
        items.swap(i, j);
    }
}

/// 播放队列管理器。播放动作为 `Option<usize>` 输出(要播放的队列索引),
/// 由 Service 层消费并驱动 AudioPlayer——保持纯逻辑可测。
#[derive(Debug, Default)]
pub struct PlayQueue {
    original_list: Vec<String>,
    play_list: Vec<String>,
    current_index: i32,
    repeat_mode: RepeatMode,
    shuffle_active: bool,
    playlist_id: Option<i32>,
    /// 本次调用产生的播放指令(消费后清空)
    pending_play: Option<usize>,
}

impl PlayQueue {
    pub fn new() -> Self {
        Self::default()
    }

    /// 洗牌随机源(生产用真随机)
    fn rand(&self) -> impl FnMut(usize) -> usize {
        move |n: usize| {
            use std::time::{SystemTime, UNIX_EPOCH};
            // xorshift 混合纳秒时钟,对 i∈[1,len] 的折半区间取模
            let t = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.subsec_nanos() as u64 ^ d.as_secs())
                .unwrap_or(0x9E3779B9);
            let mut x = t | 1;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x % n as u64) as usize
        }
    }

    fn play_at(&mut self, index: i32) {
        if index < 0 || index as usize >= self.play_list.len() {
            return;
        }
        self.current_index = index;
        self.pending_play = Some(index as usize);
    }

    fn find_in_play_list(&self, song_id: &str) -> Option<usize> {
        self.play_list.iter().position(|s| s == song_id)
    }

    // ==== 队列设置 ====

    /// 设置完整队列并从头播放(shuffle 开启则先洗牌)
    pub fn set_queue(&mut self, song_ids: Vec<String>, playlist_id: Option<i32>) {
        if song_ids.is_empty() {
            return;
        }
        self.original_list = song_ids.clone();
        self.playlist_id = playlist_id;
        if self.shuffle_active {
            let mut list = song_ids;
            shuffle_by(&mut list, &mut self.rand());
            self.play_list = list;
        } else {
            self.play_list = song_ids;
        }
        self.play_at(0);
    }

    pub fn replace_queue_and_play(&mut self, song_id: String) {
        self.original_list = vec![song_id.clone()];
        self.play_list = vec![song_id];
        self.playlist_id = None;
        self.play_at(0);
    }

    pub fn add_to_queue(&mut self, song_id: String) {
        if self.original_list.contains(&song_id) {
            return;
        }
        self.original_list.push(song_id.clone());
        if self.shuffle_active {
            // 随机模式:插到当前播放位置下一位
            let insert_at = (self.current_index + 1).max(0) as usize;
            let at = insert_at.min(self.play_list.len());
            self.play_list.insert(at, song_id);
        } else {
            self.play_list.push(song_id);
        }
    }

    // ==== 播放控制 ====

    pub fn play_at_index(&mut self, index: usize) {
        self.play_at(index as i32);
    }

    /// 用户点下一首(无视单曲循环)
    pub fn advance_for_user(&mut self) {
        if self.play_list.is_empty() {
            return;
        }
        let next = ((self.current_index + 1) as usize) % self.play_list.len();
        self.play_at(next as i32);
    }

    /// 用户点上一首(环绕)
    pub fn retreat_for_user(&mut self) {
        if self.play_list.is_empty() {
            return;
        }
        let prev = if self.current_index <= 0 {
            self.play_list.len() as i32 - 1
        } else {
            self.current_index - 1
        };
        self.play_at(prev);
    }

    /// 曲目自然播完:单曲循环重播;列表循环回卷;Off 播完即停
    /// (停止 = 无播放指令 + `should_stop` 为真)
    pub fn advance_for_auto_end(&mut self) -> bool {
        if self.play_list.is_empty() {
            return false;
        }
        if self.repeat_mode == RepeatMode::One {
            self.play_at(self.current_index);
            return false;
        }
        let next = self.current_index + 1;
        if next as usize >= self.play_list.len() {
            if self.repeat_mode == RepeatMode::All {
                self.play_at(0);
                return false;
            }
            // RepeatMode::Off:停止
            return true;
        }
        self.play_at(next);
        false
    }

    // ==== 模式 ====

    pub fn set_repeat_mode(&mut self, mode: RepeatMode) {
        self.repeat_mode = mode;
    }

    /// 循环切换 Off → All → One → Off(上游 cycleRepeatMode)
    pub fn cycle_repeat_mode(&mut self) {
        self.repeat_mode = match self.repeat_mode {
            RepeatMode::Off => RepeatMode::All,
            RepeatMode::All => RepeatMode::One,
            RepeatMode::One => RepeatMode::Off,
        };
    }

    pub fn toggle_shuffle(&mut self) {
        let current_song = (self.current_index >= 0)
            .then(|| self.play_list.get(self.current_index as usize))
            .flatten()
            .cloned();
        self.shuffle_active = !self.shuffle_active;
        if self.shuffle_active {
            let mut list = self.original_list.clone();
            shuffle_by(&mut list, &mut self.rand());
            self.play_list = list;
        } else {
            self.play_list = self.original_list.clone();
        }
        if let Some(song_id) = current_song
            && let Some(new_index) = self.find_in_play_list(&song_id)
        {
            self.current_index = new_index as i32;
        }
    }

    // ==== 队列修改 ====

    pub fn remove_song(&mut self, song_id: &str) {
        let Some(remove_index) = self.find_in_play_list(song_id) else {
            return;
        };
        self.original_list.retain(|s| s != song_id);
        self.play_list.remove(remove_index);
        let was_current = remove_index as i32 == self.current_index;
        if (remove_index as i32) < self.current_index {
            self.current_index -= 1;
        } else if was_current {
            if self.play_list.is_empty() {
                self.current_index = -1;
            } else if self.current_index as usize >= self.play_list.len() {
                self.current_index = 0;
            }
            if self.current_index >= 0 {
                // 移除的是当前播放曲:重播新位置的曲目
                self.play_at(self.current_index);
            }
        }
    }

    // ==== 持久化/恢复 ====

    pub fn persisted_state(&self, position_sec: f64) -> PersistedQueueState {
        PersistedQueueState {
            song_ids: self.play_list.clone(),
            original_song_ids: self.original_list.clone(),
            current_index: self.current_index,
            repeat_mode: self.repeat_mode,
            shuffle_active: self.shuffle_active,
            playlist_id: self.playlist_id,
            position: position_sec,
        }
    }

    /// 从快照恢复(song→song_id 映射已由调用方查库解析;
    /// original 为空时用 play_list 兜底,currentIndex 钳制边界)
    pub fn restore(&mut self, state: PersistedQueueState) -> bool {
        if state.song_ids.is_empty() {
            return false;
        }
        self.play_list = state.song_ids;
        self.original_list = {
            let mut o = state.original_song_ids;
            if o.is_empty() {
                o = self.play_list.clone();
            }
            o
        };
        if self.play_list.is_empty() {
            return false;
        }
        self.repeat_mode = state.repeat_mode;
        self.shuffle_active = state.shuffle_active;
        self.playlist_id = state.playlist_id;
        self.current_index = state.current_index.min(self.play_list.len() as i32 - 1);
        if self.current_index < 0 {
            self.current_index = 0;
        }
        true
    }

    // ==== 查询 ====

    pub fn current_song_id(&self) -> Option<&str> {
        (self.current_index >= 0)
            .then(|| self.play_list.get(self.current_index as usize))
            .flatten()
            .map(|s| s.as_str())
    }

    pub fn play_list(&self) -> &[String] {
        &self.play_list
    }

    pub fn current_index(&self) -> i32 {
        self.current_index
    }

    pub fn repeat_mode(&self) -> RepeatMode {
        self.repeat_mode
    }

    pub fn is_shuffle_active(&self) -> bool {
        self.shuffle_active
    }

    pub fn playlist_id(&self) -> Option<i32> {
        self.playlist_id
    }

    /// 取走播放指令(要播放的队列索引);None = 无动作
    pub fn take_pending_play(&mut self) -> Option<usize> {
        self.pending_play.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advance_respects_repeat_modes() {
        let mut q = PlayQueue::new();
        q.set_queue(vec!["a".into(), "b".into(), "c".into()], None);
        assert_eq!(q.take_pending_play(), Some(0));

        // Off:顺序到底,越界停止
        assert!(!q.advance_for_auto_end());
        assert_eq!(q.current_song_id(), Some("b"));
        assert!(!q.advance_for_auto_end());
        assert_eq!(q.current_song_id(), Some("c"));
        assert!(q.advance_for_auto_end(), "Off 播完应停止");
        assert_eq!(q.current_song_id(), Some("c"), "停止时保持末曲");

        // All:回卷
        q.set_repeat_mode(RepeatMode::All);
        assert!(!q.advance_for_auto_end());
        assert_eq!(q.current_song_id(), Some("a"), "列表循环回卷");

        // One:单曲重播
        q.set_repeat_mode(RepeatMode::One);
        assert!(!q.advance_for_auto_end());
        assert_eq!(q.current_song_id(), Some("a"), "单曲循环原地");
    }

    #[test]
    fn user_advance_wraps_and_ignores_one_repeat() {
        let mut q = PlayQueue::new();
        q.set_queue(vec!["a".into(), "b".into()], None);
        q.set_repeat_mode(RepeatMode::One);
        q.advance_for_user();
        assert_eq!(q.current_song_id(), Some("b"));
        q.advance_for_user();
        assert_eq!(q.current_song_id(), Some("a"), "用户切歌环绕且无视单曲循环");
        q.retreat_for_user();
        assert_eq!(q.current_song_id(), Some("b"));
    }

    #[test]
    fn remove_current_song_replays_next() {
        let mut q = PlayQueue::new();
        q.set_queue(vec!["a".into(), "b".into(), "c".into()], None);
        q.play_at_index(1);
        q.take_pending_play();
        q.remove_song("b");
        assert_eq!(q.play_list(), &["a".to_string(), "c".to_string()]);
        assert_eq!(q.current_song_id(), Some("c"), "移除当前曲应接播后曲");
        // 移除前面的曲:索引前移,不触发播放
        q.remove_song("a");
        assert_eq!(q.current_index(), 0);
    }

    #[test]
    fn restore_clamps_index_and_backfills_original() {
        let mut q = PlayQueue::new();
        let state = PersistedQueueState {
            song_ids: vec!["x".into(), "y".into()],
            original_song_ids: vec![],
            current_index: 7,
            repeat_mode: RepeatMode::All,
            shuffle_active: false,
            playlist_id: Some(3),
            position: 12.5,
        };
        assert!(q.restore(state));
        assert_eq!(q.current_index(), 1, "越界索引钳到末尾");
        assert_eq!(q.original_list, vec!["x".to_string(), "y".to_string()]);
        assert_eq!(q.repeat_mode(), RepeatMode::All);
        let p = q.persisted_state(0.0);
        assert_eq!(p.song_ids, vec!["x".to_string(), "y".to_string()]);
        assert_eq!(p.position, 0.0);
    }
}
