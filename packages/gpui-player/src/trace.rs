//! 采集:确定性虚拟时钟 + 逐帧 JSONL 记录。
//!
//! `--trace <file> --score <score.json>` 启动后,播放位置改由虚拟时钟驱动
//! (剧本可在指定时刻注入 seek),每帧把全部行的 y/缩放/透明度等
//! 逐项写入 JSONL,供 `scripts/compare-traces.mjs` 与上游 DOM 版
//! 的同曲采集逐帧对比。

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

/// 剧本:总时长与按时间排序的 seek 事件
#[derive(Debug, Clone)]
pub struct TraceScore {
    pub duration_ms: f64,
    pub seeks: Vec<TraceSeek>,
}

#[derive(Debug, Clone, Copy)]
pub struct TraceSeek {
    pub at_ms: f64,
    pub to_ms: f64,
}

impl TraceScore {
    /// JSON 格式:`{ "duration_ms": 60000, "seeks": [{"at_ms":30000,"to_ms":120000}] }`
    pub fn parse(text: &str) -> anyhow::Result<Self> {
        let value: serde_json::Value = serde_json::from_str(text)?;
        let duration_ms = value
            .get("duration_ms")
            .and_then(|v| v.as_f64())
            .ok_or_else(|| anyhow::anyhow!("score 缺少 duration_ms"))?;
        let mut seeks = Vec::new();
        if let Some(list) = value.get("seeks").and_then(|v| v.as_array()) {
            for item in list {
                let at_ms = item
                    .get("at_ms")
                    .and_then(|v| v.as_f64())
                    .ok_or_else(|| anyhow::anyhow!("seek 缺少 at_ms"))?;
                let to_ms = item
                    .get("to_ms")
                    .and_then(|v| v.as_f64())
                    .ok_or_else(|| anyhow::anyhow!("seek 缺少 to_ms"))?;
                seeks.push(TraceSeek { at_ms, to_ms });
            }
        }
        seeks.sort_by(|a, b| a.at_ms.total_cmp(&b.at_ms));
        Ok(Self { duration_ms, seeks })
    }
}

/// 逐帧 JSONL 写出器(每帧落盘,保证中途结束也有完整数据)
pub struct TraceRecorder {
    writer: BufWriter<File>,
    frames: usize,
}

impl TraceRecorder {
    pub fn create(path: &Path) -> std::io::Result<Self> {
        if let Some(parent) = path.parent() && !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
        Ok(Self {
            writer: BufWriter::new(File::create(path)?),
            frames: 0,
        })
    }

    pub fn write(&mut self, value: &serde_json::Value) {
        let _ = serde_json::to_writer(&mut self.writer, value);
        let _ = self.writer.write_all(b"\n");
        let _ = self.writer.flush();
        self.frames += 1;
    }

    pub fn frames(&self) -> usize {
        self.frames
    }
}

/// 本帧触发的剧本 seek
#[derive(Debug, Clone, Copy)]
pub struct AppliedSeek {
    pub at_ms: f64,
    pub to_ms: f64,
}

/// 虚拟时钟:确定性固定步进(每帧 +1000/60ms,delta 恒 1/60),
/// 与浏览器 harness 完全同构——两侧行为逐帧可比,且不受各自真实帧率影响;
/// 画面冻结只暂停推进,不产生数据空洞。剧本 seek 在 media 越过剧本点时跳变。
pub struct TraceClock {
    media_ms: f64,
    stepped: bool,
    pending: std::collections::VecDeque<TraceSeek>,
}

/// 采集帧的固定时长(毫秒)
pub const TRACE_FRAME_MS: f64 = 1000.0 / 60.0;

impl TraceClock {
    pub fn new(score: &TraceScore) -> Self {
        Self {
            media_ms: 0.0,
            stepped: false,
            pending: score.seeks.clone().into(),
        }
    }

    /// 当前虚拟媒体时间;首帧返回 0(与上游 setLyricLines 初始重建对齐),
    /// 之后每帧推进固定步长;越过剧本点时应用 seek 并返回事件
    pub fn step_ms(&mut self, applied: &mut Vec<AppliedSeek>) -> f64 {
        if self.stepped {
            self.media_ms += TRACE_FRAME_MS;
        }
        self.stepped = true;
        while self.pending.front().is_some_and(|seek| self.media_ms >= seek.at_ms) {
            let seek = self.pending.pop_front().expect("front 已校验");
            self.media_ms = seek.to_ms;
            applied.push(AppliedSeek {
                at_ms: seek.at_ms,
                to_ms: seek.to_ms,
            });
        }
        self.media_ms
    }
}

/// 页面持有的完整采集状态
pub struct TraceState {
    pub clock: TraceClock,
    pub recorder: TraceRecorder,
    pub duration_ms: f64,
    pub quit_on_done: bool,
    pub path: PathBuf,
    pub done: bool,
}

impl TraceState {
    pub fn new(score: TraceScore, path: PathBuf, quit_on_done: bool) -> anyhow::Result<Self> {
        let recorder =
            TraceRecorder::create(&path).map_err(|e| anyhow::anyhow!("创建 trace 文件失败: {e}"))?;
        Ok(Self {
            clock: TraceClock::new(&score),
            recorder,
            duration_ms: score.duration_ms,
            quit_on_done,
            path,
            done: false,
        })
    }
}
