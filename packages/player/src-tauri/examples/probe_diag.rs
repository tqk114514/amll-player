//! 诊断工具:用与更新模块完全相同的请求头复现资产下载请求,打印状态、
//! 响应头与响应体开头,用于定位 403 等拒绝的真实原因。
//!
//! 用法: cargo run -p amll-player --example probe_diag -- <资产URL>

use std::time::Duration;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let url = std::env::args()
        .nth(1)
        .expect("用法: probe_diag <资产URL> [不带Range]");

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;

    for label in ["带 Range", "不带 Range", "无 User-Agent"] {
        let mut request = client.get(&url).header(
            reqwest::header::ACCEPT,
            "application/octet-stream",
        );
        if label != "无 User-Agent" {
            request = request.header(
                reqwest::header::USER_AGENT,
                "amll-player/0.0.1+1151",
            );
        }
        if label == "带 Range" {
            request = request.header(reqwest::header::RANGE, "bytes=0-0");
        }
        let response = request.send().await?;
        let status = response.status();
        let headers = response.headers().clone();
        let final_url = response.url().to_string();
        let body = response.bytes().await?;
        println!("=== {label} ===");
        println!("status: {status}");
        println!("final url: {final_url}");
        println!(
            "ratelimit-remaining: {:?}",
            headers.get("x-ratelimit-remaining")
        );
        println!(
            "content-range: {:?}, content-length: {:?}",
            headers.get("content-range"),
            headers.get("content-length")
        );
        println!(
            "body head: {}",
            String::from_utf8_lossy(&body[..body.len().min(400)])
        );
        println!();
    }
    Ok(())
}
