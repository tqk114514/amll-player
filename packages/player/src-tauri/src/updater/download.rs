//! 支持多连接分块与断点续传的文件下载器。

use std::io::SeekFrom;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, bail};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use tokio::sync::Mutex;
use tracing::*;

/// 单个分块的最大重试次数
const CHUNK_RETRY_COUNT: usize = 3;
/// 每累计这么多字节把断点状态落盘一次
const STATE_FLUSH_INTERVAL: u64 = 1024 * 1024;
/// 进度汇报间隔,避免事件洪水
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

/// 下载断点状态,持久化为 `<dest>.state`
#[derive(Debug, Serialize, Deserialize)]
struct DownloadState {
    url: String,
    etag: Option<String>,
    total: u64,
    chunks: Vec<ChunkState>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ChunkState {
    start: u64,
    /// 闭区间末尾
    end: u64,
    received: u64,
}

impl ChunkState {
    fn len(&self) -> u64 {
        self.end - self.start + 1
    }
}

/// 把 [0, total) 切成 connections 个尽量均匀的闭区间
fn split_chunks(total: u64, connections: usize) -> Vec<ChunkState> {
    let connections = connections.max(1).min(total.max(1) as usize);
    let base = total / connections as u64;
    let remainder = total % connections as u64;
    let mut chunks = Vec::with_capacity(connections);
    let mut start = 0;
    for i in 0..connections {
        let len = base + u64::from((i as u64) < remainder);
        if len == 0 {
            break;
        }
        chunks.push(ChunkState {
            start,
            end: start + len - 1,
            received: 0,
        });
        start += len;
    }
    chunks
}

fn state_path(dest: &Path) -> PathBuf {
    let mut name = dest
        .file_name()
        .expect("下载目标必须带文件名")
        .to_os_string();
    name.push(".state");
    dest.with_file_name(name)
}

/// 读取断点状态;URL/ETag/总长任一不匹配都视为失效,重新开始
async fn load_or_create_state(
    state_path: &Path,
    url: &str,
    etag: Option<&String>,
    total: u64,
    connections: usize,
) -> anyhow::Result<DownloadState> {
    if state_path.exists() {
        match tokio::fs::read(state_path).await {
            Ok(bytes) => match serde_json::from_slice::<DownloadState>(&bytes) {
                Ok(state) => {
                    if state.url == url && state.total == total && state.etag.as_ref() == etag {
                        return Ok(state);
                    }
                    info!("下载断点状态与当前任务不匹配,重新开始下载");
                }
                Err(e) => warn!("下载断点状态损坏,重新开始下载: {e}"),
            },
            Err(e) => warn!("读取下载断点状态失败,重新开始下载: {e}"),
        }
    }
    Ok(DownloadState {
        url: url.to_string(),
        etag: etag.cloned(),
        total,
        chunks: split_chunks(total, connections),
    })
}

fn persist_state(state: &DownloadState, state_path: &Path) {
    match serde_json::to_string(state) {
        Ok(json) => {
            if let Err(e) = std::fs::write(state_path, json) {
                warn!("写入下载断点状态失败: {e}");
            }
        }
        Err(e) => warn!("序列化下载断点状态失败: {e}"),
    }
}

/// 按文件大小分配并发连接数:小文件开多连接纯属握手开销浪费,
/// 大文件才逐步提高,且不超过调用方给定的上限
fn connections_for(total: u64, max_connections: usize) -> usize {
    const MIB: u64 = 1024 * 1024;
    const SMALL_MAX: u64 = MIB;
    const MEDIUM_MAX: u64 = 8 * MIB;
    const LARGE_MAX: u64 = 32 * MIB;
    let wanted = match total {
        ..=SMALL_MAX => 1,
        ..=MEDIUM_MAX => 2,
        ..=LARGE_MAX => 4,
        _ => 8,
    };
    wanted.min(max_connections.max(1))
}

/// 多连接分块下载 url 到 dest。
///
/// on_progress 会在探测完成后与下载过程中被节流调用,返回前保证汇报一次终态;
/// 下载被中断时断点状态保留在 `<dest>.state`,下次调用自动续传。
pub async fn download_file(
    url: &str,
    dest: &Path,
    max_connections: usize,
    mut on_progress: impl FnMut(u64, u64) + Send + 'static,
) -> anyhow::Result<()> {
    let client = Arc::new(reqwest::Client::new());
    let probe = probe_url(&client, url).await?;
    let total = probe.total;

    if !probe.range_supported {
        warn!("服务器不支持 Range 请求,退化为单流下载: {url}");
        let response = probe.response.expect("探测响应已确认存在");
        return download_single_stream(response, dest, total, &mut on_progress).await;
    }

    let connections = connections_for(total, max_connections);
    let state_path = state_path(dest);
    let state = load_or_create_state(&state_path, url, probe.etag.as_ref(), total, connections).await?;
    let initial_received = state.chunks.iter().map(|chunk| chunk.received).sum::<u64>();
    if initial_received > 0 {
        info!("发现下载断点,从 {initial_received}/{total} 字节处继续");
    }

    // 预分配文件空间,分块写入时按偏移落盘
    let file = tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .open(dest)
        .await
        .with_context(|| format!("打开下载目标 {dest:?} 失败"))?;
    file.set_len(total).await?;
    drop(file);

    let state = Arc::new(Mutex::new(state));
    let downloaded = Arc::new(AtomicU64::new(initial_received));
    on_progress(initial_received, total);

    let chunk_count = state.lock().await.chunks.len();
    let mut tasks = tokio::task::JoinSet::new();
    for index in 0..chunk_count {
        let completed = {
            let state = state.lock().await;
            state.chunks[index].received >= state.chunks[index].len()
        };
        if completed {
            continue;
        }
        tasks.spawn(run_chunk(
            client.clone(),
            url.to_string(),
            dest.to_path_buf(),
            state_path.clone(),
            state.clone(),
            downloaded.clone(),
            index,
        ));
    }

    // 终止信号到达时补发一次终态汇报,保证调用方总能看到最后的进度
    let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel::<()>();
    let reporter_downloaded = downloaded.clone();
    let reporter = tokio::spawn(async move {
        let mut last_reported = initial_received;
        let mut interval = tokio::time::interval(PROGRESS_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    let received = reporter_downloaded.load(Ordering::Relaxed);
                    if received != last_reported {
                        last_reported = received;
                        on_progress(received, total);
                    }
                }
                _ = &mut stop_rx => {
                    on_progress(reporter_downloaded.load(Ordering::Relaxed), total);
                    return;
                }
            }
        }
    });

    let mut failure = None;
    while let Some(joined) = tasks.join_next().await {
        match joined.context("下载任务异常退出")? {
            Ok(()) => {}
            Err(e) => {
                error!("下载分块失败,取消其余分块: {e:#}");
                failure = Some(e);
                tasks.abort_all();
            }
        }
    }

    let _ = stop_tx.send(());
    let _ = reporter.await;

    if let Some(e) = failure {
        // 保留断点状态,下次调用自动续传
        bail!("下载中断,已保留断点供下次续传: {e:#}");
    }

    debug_assert_eq!(downloaded.load(Ordering::Relaxed), total);
    std::fs::remove_file(&state_path).ok();
    Ok(())
}

async fn probe_url(client: &reqwest::Client, url: &str) -> anyhow::Result<ProbeResult> {
    let response = client
        .get(url)
        .header(reqwest::header::RANGE, "bytes=0-0")
        .send()
        .await
        .context("请求下载目标失败")?
        .error_for_status()
        .context("下载目标请求被拒绝")?;
    let etag = response
        .headers()
        .get(reqwest::header::ETAG)
        .and_then(|value| value.to_str().ok().map(str::to_string));

    if response.status() == reqwest::StatusCode::PARTIAL_CONTENT {
        let total = response
            .headers()
            .get(reqwest::header::CONTENT_RANGE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.rsplit('/').next())
            .and_then(|value| value.parse().ok())
            .context("Content-Range 缺少总长度信息")?;
        anyhow::ensure!(total > 0, "服务器报告的文件大小为 0");
        Ok(ProbeResult {
            total,
            etag,
            range_supported: true,
            response: None,
        })
    } else {
        let total = response
            .content_length()
            .context("服务器未返回文件大小且不支持 Range 请求")?;
        anyhow::ensure!(total > 0, "服务器报告的文件大小为 0");
        Ok(ProbeResult {
            total,
            etag,
            range_supported: false,
            response: Some(response),
        })
    }
}

struct ProbeResult {
    total: u64,
    etag: Option<String>,
    range_supported: bool,
    /// Range 不受支持时,探测响应本身就是完整的响应体,直接复用
    response: Option<reqwest::Response>,
}

async fn download_single_stream(
    response: reqwest::Response,
    dest: &Path,
    total: u64,
    on_progress: &mut impl FnMut(u64, u64),
) -> anyhow::Result<()> {
    let mut file = tokio::fs::File::create(dest).await?;
    let mut stream = response.bytes_stream();
    let mut received = 0u64;
    // 汇报粒度:至少 1%,同时不超过 128 次汇报
    let step = (total / 100).max(64 * 1024);
    let mut next_report = 0u64;
    while let Some(bytes) = stream.next().await {
        let bytes = bytes.context("下载数据流中断")?;
        file.write_all(&bytes).await?;
        received += bytes.len() as u64;
        if received >= next_report {
            next_report = received + step;
            on_progress(received, total);
        }
    }
    file.flush().await?;
    on_progress(total, total);
    Ok(())
}

async fn run_chunk(
    client: Arc<reqwest::Client>,
    url: String,
    dest: PathBuf,
    state_path: PathBuf,
    state: Arc<Mutex<DownloadState>>,
    downloaded: Arc<AtomicU64>,
    index: usize,
) -> anyhow::Result<()> {
    for attempt in 1..=CHUNK_RETRY_COUNT {
        if attempt > 1 {
            let backoff = Duration::from_millis(500 << (attempt - 1));
            warn!("分块 {index} 失败,{backoff:?} 后进行第 {attempt} 次尝试");
            tokio::time::sleep(backoff).await;
        }
        match stream_chunk(&client, &url, &dest, &state_path, &state, &downloaded, index).await {
            Ok(()) => return Ok(()),
            Err(e) => error!("分块 {index} 第 {attempt} 次尝试失败: {e:#}"),
        }
    }
    bail!("分块 {index} 在 {CHUNK_RETRY_COUNT} 次尝试后仍然失败")
}

async fn stream_chunk(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
    state_path: &Path,
    state: &Mutex<DownloadState>,
    downloaded: &AtomicU64,
    index: usize,
) -> anyhow::Result<()> {
    let (range_start, offset, range_end) = {
        let state = state.lock().await;
        let chunk = &state.chunks[index];
        (
            chunk.start + chunk.received,
            chunk.received,
            chunk.end,
        )
    };
    if offset > 0 {
        debug!("分块 {index} 从偏移 {offset} 处续传");
    }

    let response = client
        .get(url)
        .header(reqwest::header::RANGE, format!("bytes={range_start}-{range_end}"))
        .send()
        .await?
        .error_for_status()?;
    if response.status() != reqwest::StatusCode::PARTIAL_CONTENT {
        bail!("服务器未按 Range 请求返回 206 (状态码 {})", response.status());
    }
    // 校验响应区间起点与请求一致,防止错位数据写坏文件
    if let Some(content_range) = response
        .headers()
        .get(reqwest::header::CONTENT_RANGE)
        .and_then(|value| value.to_str().ok())
    {
        let response_start: u64 = content_range
            .split(&[' ', '-'][..])
            .find_map(|part| part.parse().ok())
            .unwrap_or(range_start);
        if response_start != range_start {
            bail!("服务器返回的数据区间 ({response_start}) 与请求 ({range_start}) 不一致");
        }
    }

    let mut file = tokio::fs::OpenOptions::new().write(true).open(dest).await?;
    file.seek(SeekFrom::Start(range_start)).await?;

    let mut stream = response.bytes_stream();
    let mut last_flush = offset;
    while let Some(bytes) = stream.next().await {
        let bytes = bytes.context("下载数据流中断")?;
        file.write_all(&bytes).await?;
        let len = bytes.len() as u64;
        let mut state = state.lock().await;
        let chunk = &mut state.chunks[index];
        chunk.received += len;
        if chunk.received > chunk.len() {
            bail!("分块 {index} 收到的数据超出请求范围");
        }
        downloaded.fetch_add(len, Ordering::Relaxed);
        if chunk.received - last_flush >= STATE_FLUSH_INTERVAL {
            last_flush = chunk.received;
            persist_state(&state, state_path);
        }
    }

    // 数据流结束,确认分块收满并落盘断点
    {
        let state = state.lock().await;
        let chunk = &state.chunks[index];
        anyhow::ensure!(
            chunk.received >= chunk.len(),
            "分块 {index} 数据不完整 ({}/{})",
            chunk.received,
            chunk.len()
        );
        persist_state(&state, state_path);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_chunks_covers_exactly() {
        let chunks = split_chunks(1000, 8);
        assert_eq!(chunks.len(), 8);
        assert_eq!(chunks.first().unwrap().start, 0);
        assert_eq!(chunks.last().unwrap().end, 999);
        // 相邻分块无缝衔接
        for pair in chunks.windows(2) {
            assert_eq!(pair[0].end + 1, pair[1].start);
        }
        assert_eq!(chunks.iter().map(|chunk| chunk.len()).sum::<u64>(), 1000);
    }

    #[test]
    fn split_chunks_handles_tiny_total() {
        let chunks = split_chunks(3, 8);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks.iter().map(|chunk| chunk.len()).sum::<u64>(), 3);
        for chunk in &chunks {
            assert_eq!(chunk.len(), 1);
        }
    }

    #[test]
    fn split_chunks_single_connection() {
        let chunks = split_chunks(1024, 1);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].len(), 1024);
    }

    #[test]
    fn connections_scale_with_size() {
        const MIB: u64 = 1024 * 1024;
        assert_eq!(connections_for(512 * 1024, 8), 1);
        assert_eq!(connections_for(4 * MIB, 8), 2);
        assert_eq!(connections_for(16 * MIB, 8), 4);
        assert_eq!(connections_for(64 * MIB, 8), 8);
        // 不超过调用方给定的上限
        assert_eq!(connections_for(64 * MIB, 2), 2);
        assert_eq!(connections_for(512 * 1024, 0), 1);
    }
}
