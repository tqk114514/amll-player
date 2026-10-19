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

/// 虚拟时钟:墙钟 1× 速度推进(media = 流逝墙钟 + seek 偏移),
/// 与浏览器 harness 同构;画面冻结不产生数据空洞(墙钟连续,恢复后
/// 采样点继续),帧节奏差异由对比脚本的重采样处理。
/// 剧本 seek 以墙钟为轴:at_ms 为墙钟毫秒,to_ms 为跳转后的媒体时间。
pub struct TraceClock {
    start: std::time::Instant,
    offset_ms: f64,
    pending: std::collections::VecDeque<TraceSeek>,
}

impl TraceClock {
    pub fn new(score: &TraceScore) -> Self {
        Self {
            start: std::time::Instant::now(),
            offset_ms: 0.0,
            pending: score.seeks.clone().into(),
        }
    }

    /// 当前虚拟媒体时间;墙钟越过剧本点时应用 seek 并返回事件
    pub fn step_ms(&mut self, applied: &mut Vec<AppliedSeek>) -> f64 {
        let wall = self.start.elapsed().as_millis() as f64;
        while self.pending.front().is_some_and(|seek| wall >= seek.at_ms) {
            let seek = self.pending.pop_front().expect("front 已校验");
            self.offset_ms += seek.to_ms - (wall + self.offset_ms);
            applied.push(AppliedSeek {
                at_ms: seek.at_ms,
                to_ms: seek.to_ms,
            });
        }
        wall + self.offset_ms
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

/// 像素级追踪:逐帧读回呈现输出,记录帧间逐像素差分指标(微动的
/// 客观度量)与采样 PNG,写入同一份 trace JSONL(`ev: "pixel"`)。
pub struct PixelTrace {
    dir: PathBuf,
    prev: Option<(u32, u32, Vec<u8>)>,
    frame: usize,
    saved: usize,
}

impl PixelTrace {
    pub fn new(dir: PathBuf) -> Self {
        if let Some(parent) = dir.parent() && !parent.as_os_str().is_empty() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::create_dir_all(&dir);
        Self {
            dir,
            prev: None,
            frame: 0,
            saved: 0,
        }
    }

    /// `captured` 是最近呈现的一帧(在下一帧绘制前读回,逐帧调用
    /// 即得呈现输出的连续记录);与上一帧差分后写指标,
    /// 有变化的帧按间隔采样存 PNG 供肉眼复核。
    pub fn record(
        &mut self,
        captured: image::RgbaImage,
        media: f64,
        wall: f64,
        recorder: &mut TraceRecorder,
    ) {
        let (width, height) = captured.dimensions();
        let raw = captured.into_raw();
        if let Some((prev_w, prev_h, prev)) = &self.prev
            && *prev_w == width
            && *prev_h == height
        {
            let mut diff_sum: u64 = 0;
            let mut changed: u64 = 0;
            let mut max_diff: u8 = 0;
            for (a, b) in prev.as_chunks::<4>().0.iter().zip(raw.as_chunks::<4>().0) {
                let mut px_diff: u8 = 0;
                for channel in 0..4 {
                    px_diff = px_diff.max((a[channel] as i16 - b[channel] as i16).unsigned_abs() as u8);
                }
                if px_diff > 4 {
                    changed += 1;
                }
                diff_sum += px_diff as u64;
                max_diff = max_diff.max(px_diff);
            }
            let pixels = width as u64 * height as u64;
            recorder.write(&serde_json::json!({
                "ev": "pixel",
                "media": media,
                "wall": wall,
                "frame": self.frame,
                "meanDiff": diff_sum as f64 / pixels as f64,
                "changedPx": changed,
                "maxDiff": max_diff,
            }));
            // 采样:有像素变化时每 8 帧存一张(上限 800 张)
            if changed > 0
                && self.frame.is_multiple_of(8)
                && self.saved < 800
                && let Some(img) = image::RgbaImage::from_raw(width, height, raw.clone())
            {
                let path = self.dir.join(format!("f{:05}_m{:.0}.png", self.frame, media));
                if img.save(&path).is_ok() {
                    self.saved += 1;
                }
            }
        }
        self.prev = Some((width, height, raw));
        self.frame += 1;
    }
}
