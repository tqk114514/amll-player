//! 自研应用更新模块:检查更新、多连接断点续传下载、minisign 验签与各平台安装。
//!
//! 更新清单与签名产物沿用 CI 的 tauri-action 管线生成的 latest.json / .sig,
//! 本模块只替换掉 tauri-plugin-updater 的客户端实现。

use std::collections::HashMap;

use anyhow::{Context, bail};
use anyhow_tauri::IntoTAResult;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Runtime, ipc::Channel};
use tracing::*;

pub mod download;
pub mod install;

const USER_AGENT: &str = concat!("amll-player/", env!("CARGO_PKG_VERSION"));

/// 分块下载允许的最大并发连接数,实际按文件大小自动分配
const DOWNLOAD_CONNECTIONS: usize = 8;

#[derive(Debug, Deserialize)]
struct LatestRelease {
    version: String,
    #[serde(default)]
    notes: Option<String>,
    #[serde(default)]
    platforms: HashMap<String, PlatformEntry>,
}

#[derive(Debug, Deserialize)]
struct PlatformEntry {
    url: String,
    signature: String,
}

/// 发送给前端的检查结果
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    pub current_version: String,
    pub version: String,
    pub body: String,
}

/// 下载过程事件,通过 Channel 流式推送给前端
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event", rename_all = "camelCase")]
pub enum DownloadEvent {
    Started { total: u64 },
    Progress { received: u64, total: u64 },
    Finished,
}

#[tauri::command]
pub async fn check_update<R: Runtime>(
    app: AppHandle<R>,
) -> anyhow_tauri::TAResult<Option<UpdateInfo>> {
    let release = fetch_latest().await?;
    let current_version = app.package_info().version.clone().to_string();
    if !is_newer_version(&current_version, &release.version) {
        info!("已是最新版本: {current_version}");
        return Ok(None);
    }
    info!("发现新版本: {current_version} -> {}", release.version);
    Ok(Some(UpdateInfo {
        current_version,
        version: release.version.clone(),
        body: release.notes.unwrap_or_default(),
    }))
}

#[tauri::command]
pub async fn update_download_install<R: Runtime>(
    app: AppHandle<R>,
    channel: Channel<DownloadEvent>,
) -> anyhow_tauri::TAResult<()> {
    // 重新拉取清单而不是复用检查阶段的结果,保证下载的产物与签名一一对应
    let release = fetch_latest().await?;
    let current_version = app.package_info().version.clone().to_string();
    if !is_newer_version(&current_version, &release.version) {
        anyhow_tauri::bail!("当前已经是最新版本 ({current_version})");
    }

    let installer = install::detect_installer_kind(&app.package_info().name)?;
    if installer.is_none() && cfg!(target_os = "linux") {
        anyhow_tauri::bail!("deb/rpm 方式安装暂不支持自动更新,请通过包管理器升级");
    }
    let (os, arch) = current_platform()?;
    let entry = select_platform(&release, os, arch, installer)?;
    let url = entry.url.clone();
    let signature = entry.signature.clone();
    let version = release.version.clone();
    let pubkey = read_pubkey(&app)?;

    let temp_dir = std::env::temp_dir().join(format!("amll-updater-{version}"));
    std::fs::create_dir_all(&temp_dir)
        .with_context(|| format!("创建临时目录 {temp_dir:?} 失败"))?;
    let dest = temp_dir.join(match installer {
        Some("msi") => "installer.msi",
        Some("nsis") => "installer.exe",
        Some("appimage") => "update.AppImage",
        _ => "update.app.tar.gz",
    });

    info!("开始下载更新 {version}: {url}");
    let mut started = false;
    let progress_channel = channel.clone();
    download::download_file(&url, &dest, DOWNLOAD_CONNECTIONS, move |received, total| {
        let event = if started {
            DownloadEvent::Progress { received, total }
        } else {
            started = true;
            DownloadEvent::Started { total }
        };
        if let Err(e) = progress_channel.send(event) {
            warn!("发送下载进度事件失败: {e}");
        }
    })
    .await
    .context("下载更新包失败")?;
    let _ = channel.send(DownloadEvent::Finished);

    info!("更新包下载完成,校验签名…");
    let data = std::fs::read(&dest).with_context(|| format!("读取更新包 {dest:?} 失败"))?;
    install::verify_signature(&data, &signature, &pubkey).context("更新包签名校验失败")?;
    info!("签名校验通过,开始安装 {version}");

    install::run_install(&app, &dest).await.into_ta_result()
}

/// 从 tauri.conf.json 的 plugins.updater 段读取 minisign 公钥,
/// 与 CI 打包时使用的签名密钥对配套
fn read_pubkey<R: Runtime>(app: &AppHandle<R>) -> anyhow::Result<String> {
    let Some(serde_json::Value::Object(updater)) = app.config().plugins.0.get("updater") else {
        bail!("tauri.conf.json 缺少 plugins.updater 配置段");
    };
    let Some(serde_json::Value::String(pubkey)) = updater.get("pubkey") else {
        bail!("plugins.updater 配置缺少 pubkey");
    };
    Ok(pubkey.clone())
}

/// 更新清单与安装包同在 Release 资产中,统一走 api.github.com 分发:
/// 该域名的连通性通常好于 github.com,且资产地址本身就在此域名下
const RELEASE_API_ENDPOINT: &str =
    "https://api.github.com/repos/tqk114514/amll-player/releases/tags/main-player-dev";

async fn fetch_latest() -> anyhow::Result<LatestRelease> {
    let release: serde_json::Value = reqwest::Client::new()
        .get(RELEASE_API_ENDPOINT)
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .send()
        .await
        .context("请求 Release 信息失败")?
        .error_for_status()
        .context("Release 信息请求被拒绝")?
        .json()
        .await
        .context("解析 Release 信息失败")?;
    let assets = release
        .get("assets")
        .and_then(serde_json::Value::as_array)
        .context("Release 信息缺少 assets 字段")?;
    let latest_json_asset = assets
        .iter()
        .find(|asset| {
            asset.get("name").and_then(serde_json::Value::as_str) == Some("latest.json")
        })
        .context("Release 中未找到 latest.json 资产")?;
    let url = latest_json_asset
        .get("url")
        .and_then(serde_json::Value::as_str)
        .context("latest.json 资产缺少 url 字段")?;
    let response = reqwest::Client::new()
        .get(url)
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .header(reqwest::header::ACCEPT, download::DOWNLOAD_ACCEPT)
        .send()
        .await
        .context("请求更新清单资产失败")?
        .error_for_status()
        .context("更新清单资产请求被拒绝")?;
    Ok(response.json().await.context("解析更新清单失败")?)
}

/// 返回当前平台的更新清单键名前缀,如 ("windows", "x86_64")
fn current_platform() -> anyhow::Result<(&'static str, &'static str)> {
    let os = if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        bail!("当前平台不支持自动更新")
    };
    let arch = if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        bail!("当前架构不支持自动更新")
    };
    Ok((os, arch))
}

/// 按清单键名规则选择产物:优先带安装方式后缀的键,回退到无后缀的默认键
fn select_platform<'a>(
    release: &'a LatestRelease,
    os: &'static str,
    arch: &'static str,
    installer: Option<&str>,
) -> anyhow::Result<&'a PlatformEntry> {
    let mut keys = Vec::new();
    if let Some(installer) = installer {
        keys.push(format!("{os}-{arch}-{installer}"));
    }
    keys.push(format!("{os}-{arch}"));
    keys.iter()
        .find_map(|key| release.platforms.get(key))
        .with_context(|| format!("更新清单中没有当前平台 ({os}-{arch}) 的产物"))
}

/// 解析 "1.2.3+45" 形式的版本号;semver 的 build metadata 不参与优先级,
/// 所以这里拆开,三元组相同时按构建号数值比较
fn parse_version(version: &str) -> Option<(u64, u64, u64, u64)> {
    let version = version.trim().trim_start_matches('v');
    let (core, build) = match version.split_once('+') {
        Some((core, build)) => (core, build.parse().unwrap_or(0)),
        None => (version, 0),
    };
    let mut numbers = core.split('.');
    let major = numbers.next()?.parse().ok()?;
    let minor = numbers.next()?.parse().ok()?;
    let patch = numbers.next()?.parse().ok()?;
    Some((major, minor, patch, build))
}

fn is_newer_version(current: &str, remote: &str) -> bool {
    match (parse_version(current), parse_version(remote)) {
        (Some(current), Some(remote)) => remote > current,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_comparison_includes_build_number() {
        assert!(is_newer_version("0.0.1+1138", "0.0.1+1139"));
        assert!(!is_newer_version("0.0.1+1139", "0.0.1+1139"));
        assert!(!is_newer_version("0.0.1+1139", "0.0.1"));
        assert!(is_newer_version("0.0.1", "0.0.1+1"));
        assert!(is_newer_version("0.0.1+999", "0.0.2+1"));
        assert!(is_newer_version("0.0.1", "v0.0.2"));
        assert!(!is_newer_version("0.0.2+1", "0.0.1+999"));
    }

    #[test]
    fn parses_real_latest_json() {
        const SAMPLE: &str = r#"{
            "version": "0.0.1+1138",
            "notes": "测试更新",
            "pub_date": "2026-10-16T08:37:44Z",
            "platforms": {
                "windows-x86_64": { "signature": "c2ln", "url": "https://example.com/installer.msi" },
                "windows-x86_64-nsis": { "signature": "c2ln", "url": "https://example.com/installer.exe" },
                "linux-x86_64": { "signature": "c2ln", "url": "https://example.com/app.AppImage" }
            }
        }"#;
        let release: LatestRelease = serde_json::from_str(SAMPLE).unwrap();
        assert_eq!(release.version, "0.0.1+1138");
        assert_eq!(release.platforms.len(), 2 + 1);

        let nsis = select_platform(&release, "windows", "x86_64", Some("nsis")).unwrap();
        assert_eq!(nsis.url, "https://example.com/installer.exe");
        let msi = select_platform(&release, "windows", "x86_64", Some("msi")).unwrap();
        assert_eq!(msi.url, "https://example.com/installer.msi");
        let appimage = select_platform(&release, "linux", "x86_64", Some("appimage")).unwrap();
        assert_eq!(appimage.url, "https://example.com/app.AppImage");
        // 带后缀的键缺失时回退到默认键
        let default = select_platform(&release, "linux", "x86_64", Some("deb")).unwrap();
        assert_eq!(default.url, "https://example.com/app.AppImage");
        // 完全未知的平台
        assert!(select_platform(&release, "darwin", "aarch64", None).is_err());
    }
}
