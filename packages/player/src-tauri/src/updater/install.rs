//! 更新包验签与各平台安装逻辑。

use anyhow::Context;
use tauri::{AppHandle, Runtime};
use tracing::*;

/// 校验下载内容的 minisign 签名
pub fn verify_signature(
    data: &[u8],
    signature_base64: &str,
    pubkey_base64: &str,
) -> anyhow::Result<()> {
    use base64::Engine;

    let pubkey_bytes = base64::engine::general_purpose::STANDARD
        .decode(pubkey_base64)
        .context("解码更新公钥失败")?;
    let pubkey_text = String::from_utf8(pubkey_bytes).context("更新公钥不是有效的 UTF-8")?;
    let public_key =
        minisign_verify::PublicKey::decode(&pubkey_text).context("解析更新公钥失败")?;
    let signature_bytes = base64::engine::general_purpose::STANDARD
        .decode(signature_base64)
        .context("解码更新签名失败")?;
    let signature_text =
        String::from_utf8(signature_bytes).context("更新签名不是有效的 UTF-8")?;
    let signature =
        minisign_verify::Signature::decode(&signature_text).context("解析更新签名失败")?;
    public_key
        .verify(data, &signature, true)
        .context("更新包签名不匹配")
}

/// 探测当前安装方式,用于选择更新清单中的目标产物。
/// 返回 None 表示该平台产物不带安装方式后缀(macOS),
/// Linux 非 AppImage 安装同样返回 None,由调用方决定是否继续。
pub fn detect_installer_kind(product_name: &str) -> anyhow::Result<Option<&'static str>> {
    #[cfg(target_os = "windows")]
    {
        let kind = match detect_windows_installer(product_name) {
            WindowsInstaller::Nsis => "nsis",
            WindowsInstaller::Msi => "msi",
        };
        info!("检测到安装方式: {kind}");
        return Ok(Some(kind));
    }
    #[cfg(target_os = "linux")]
    {
        let _ = product_name;
        // AppImage 运行时会设置 APPIMAGE 环境变量;deb/rpm 安装暂不支持自动更新
        return Ok(std::env::var_os("APPIMAGE").map(|_| "appimage"));
    }
    #[cfg(target_os = "macos")]
    {
        let _ = product_name;
        Ok(None)
    }
}

/// 按平台执行安装。Windows 会启动安装器并立刻退出本进程;
/// macOS/Linux 完成文件替换后返回,由前端触发重启。
pub async fn run_install<R: Runtime>(
    app: &AppHandle<R>,
    download_path: &std::path::Path,
) -> anyhow::Result<()> {
    let product_name = app.package_info().name.clone();

    #[cfg(target_os = "windows")]
    {
        use windows::core::{HSTRING, w};
        use windows::Win32::UI::Shell::ShellExecuteW;
        use windows::Win32::UI::WindowsAndMessaging::SW_SHOW;

        let detected = detect_windows_installer(&product_name);
        // 下载内容嗅探与注册表检测结果交叉校验,防止清单指向了错误的产物
        let bytes = std::fs::read(download_path)
            .with_context(|| format!("读取更新包 {download_path:?} 失败"))?;
        let sniffed = if infer::app::is_exe(&bytes) {
            WindowsInstaller::Nsis
        } else if infer::archive::is_msi(&bytes) {
            WindowsInstaller::Msi
        } else {
            anyhow::bail!("更新包格式无法识别,已取消安装");
        };
        if sniffed != detected {
            warn!("注册表检测的安装方式与更新包类型不一致,以更新包类型为准");
        }

        // 转发当前进程参数,安装器完成后用新版本原样重启
        let current_args = std::env::args_os().skip(1).collect::<Vec<_>>();
        let (file, parameters) = match sniffed {
            WindowsInstaller::Nsis => {
                let mut args: Vec<String> = ["/P", "/R", "/UPDATE", "/ARGS"]
                    .into_iter()
                    .map(str::to_string)
                    .collect();
                args.extend(current_args.iter().map(|arg| escape_nsis_arg(arg)));
                (
                    HSTRING::from(download_path.as_os_str()),
                    args.join(" "),
                )
            }
            WindowsInstaller::Msi => {
                let system_root =
                    std::env::var("SYSTEMROOT").unwrap_or_else(|_| "C:\\Windows".to_string());
                let launch_args = current_args
                    .iter()
                    .map(|arg| escape_msi_property_arg(arg))
                    .collect::<Vec<_>>()
                    .join(" ");
                let launch_args = if launch_args.is_empty() {
                    "\"\"".to_string()
                } else {
                    launch_args
                };
                (
                    HSTRING::from(format!("{system_root}\\System32\\msiexec.exe")),
                    format!(
                        "/i \"{}\" /passive /promptrestart AUTOLAUNCHAPP=True LAUNCHAPPARGS={launch_args}",
                        download_path.display()
                    ),
                )
            }
        };

        info!("启动安装器: {file:?} {parameters}");
        let result = unsafe { ShellExecuteW(None, w!("open"), &file, &HSTRING::from(parameters), None, SW_SHOW) };
        if result.0 as isize <= 32 {
            anyhow::bail!("启动安装器失败 (ShellExecuteW 返回 {result:?})");
        }
        // 安装器需要本进程退出后才能替换文件,这里直接结束应用
        std::process::exit(0);
    }

    #[cfg(target_os = "linux")]
    {
        let _ = product_name;
        install_appimage(download_path).await
    }

    #[cfg(target_os = "macos")]
    {
        let _ = product_name;
        install_macos_bundle(download_path).await
    }

    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    {
        let _ = (app, product_name, download_path);
        anyhow::bail!("当前平台不支持自动更新")
    }
}

#[cfg(target_os = "windows")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WindowsInstaller {
    Nsis,
    Msi,
}

/// 检测当前应用的安装方式:优先查注册表的卸载表项,
/// 找不到时退化为检查安装目录内的 NSIS 卸载器,默认按 NSIS 处理。
#[cfg(target_os = "windows")]
fn detect_windows_installer(product_name: &str) -> WindowsInstaller {
    use windows::core::{HSTRING, PWSTR};
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, RegCloseKey, RegEnumKeyExW,
        RegGetValueW, RegOpenKeyExW, RRF_RT_REG_SZ,
    };

    const UNINSTALL_PATH: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall";

    unsafe fn read_string(hive: HKEY, subkey: &str, value: &str) -> Option<String> {
        let mut buffer = [0u16; 1024];
        let mut size = buffer.len() as u32 * 2;
        let status = unsafe {
            RegGetValueW(
                hive,
                &HSTRING::from(subkey),
                &HSTRING::from(value),
                RRF_RT_REG_SZ,
                None,
                Some(buffer.as_mut_ptr().cast()),
                Some(std::ptr::from_mut(&mut size)),
            )
        };
        if status != ERROR_SUCCESS {
            return None;
        }
        Some(
            String::from_utf16_lossy(&buffer[..size as usize / 2])
                .trim_end_matches('\0')
                .to_string(),
        )
    }

    for hive in [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE] {
        let mut uninstall_key = HKEY::default();
        unsafe {
            if RegOpenKeyExW(
                hive,
                &HSTRING::from(UNINSTALL_PATH),
                None,
                KEY_READ,
                &mut uninstall_key,
            ) != ERROR_SUCCESS
            {
                continue;
            }

            let mut detected = None;
            let mut index = 0u32;
            while detected.is_none() {
                let mut name_buffer = [0u16; 256];
                let mut name_len = name_buffer.len() as u32;
                let status = RegEnumKeyExW(
                    uninstall_key,
                    index,
                    Some(PWSTR(name_buffer.as_mut_ptr())),
                    &mut name_len,
                    None,
                    None,
                    None,
                    None,
                );
                if status != ERROR_SUCCESS {
                    break;
                }
                index += 1;

                let subkey = String::from_utf16_lossy(&name_buffer[..name_len as usize]);
                let display_name =
                    read_string(uninstall_key, &subkey, "DisplayName").unwrap_or_default();
                if !display_name.eq_ignore_ascii_case(product_name) {
                    continue;
                }
                let uninstall_string =
                    read_string(uninstall_key, &subkey, "UninstallString").unwrap_or_default();
                let lowered = uninstall_string.to_ascii_lowercase();
                detected = Some(if lowered.contains("msiexec") {
                    WindowsInstaller::Msi
                } else {
                    WindowsInstaller::Nsis
                });
            }

            let _ = RegCloseKey(uninstall_key).ok();
            if let Some(detected) = detected {
                info!("通过注册表检测到安装方式: {detected:?}");
                return detected;
            }
        }
    }

    // 未在注册表中找到(如开发构建):安装目录内有 NSIS 卸载器即视为 NSIS
    if std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("uninstall.exe").exists()))
        .unwrap_or(false)
    {
        info!("检测到安装目录内的 NSIS 卸载器");
        return WindowsInstaller::Nsis;
    }
    warn!("未检测到安装方式,默认按 NSIS 处理");
    WindowsInstaller::Nsis
}

/// NSIS 参数转义:在 std 规则之上额外转义 '/',避免被 NSIS 当作参数开头
#[cfg(target_os = "windows")]
fn escape_nsis_arg(arg: &std::ffi::OsStr) -> String {
    let arg = arg.to_string_lossy();
    let mut cmd: Vec<char> = Vec::new();

    let quote = arg.chars().any(|c| c == ' ' || c == '\t' || c == '/') || arg.is_empty();
    if quote {
        cmd.push('"');
    }
    let mut backslashes: usize = 0;
    for x in arg.chars() {
        if x == '\\' {
            backslashes += 1;
        } else {
            if x == '"' {
                // 内部 '"' 前补 n+1 个反斜杠,总计 2n+1
                cmd.extend((0..=backslashes).map(|_| '\\'));
            }
            backslashes = 0;
        }
        cmd.push(x);
    }
    if quote {
        // 结尾 '"' 前补 n 个反斜杠,总计 2n
        cmd.extend((0..backslashes).map(|_| '\\'));
        cmd.push('"');
    }
    cmd.into_iter().collect()
}

/// MSI 属性值转义:参数会经过 ShellExecute 与 msiexec 两层解析
#[cfg(target_os = "windows")]
fn escape_msi_property_arg(arg: &std::ffi::OsStr) -> String {
    let mut arg = arg.to_string_lossy().to_string();
    if arg.is_empty() {
        // 空参数会被 ShellExecute 丢弃,这里保留为显式空串
        return "\"\"\"\"".to_string();
    }
    if !arg.contains(' ') && !arg.contains('"') {
        return arg;
    }
    if arg.contains('"') {
        arg = arg.replace('"', r#""""""#);
    }
    if arg.starts_with('-') {
        if let Some((name, value)) = arg.split_once('=') {
            format!("{name}=\"\"{value}\"\"")
        } else {
            format!("\"\"{arg}\"\"")
        }
    } else {
        format!("\"\"{arg}\"\"")
    }
}

/// 用下载好的 AppImage 原位替换当前文件
#[cfg(target_os = "linux")]
async fn install_appimage(download_path: &std::path::Path) -> anyhow::Result<()> {
    let target = std::env::var_os("APPIMAGE")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::current_exe().ok())
        .context("未找到当前 AppImage 路径")?;
    let parent = target.parent().context("无法定位 AppImage 所在目录")?;

    // 与目标同目录暂存,保证 rename 在同一文件系统内原子完成
    let staged = parent.join(format!(
        ".{}.update",
        target
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default()
    ));
    std::fs::copy(download_path, &staged)
        .with_context(|| format!("写入暂存文件 {staged:?} 失败"))?;
    std::fs::rename(&staged, &target).with_context(|| format!("替换 {target:?} 失败"))?;
    info!("AppImage 已更新到 {target:?},等待前端重启");
    Ok(())
}

/// 解包 .app.tar.gz 并原位替换当前应用包
#[cfg(target_os = "macos")]
async fn install_macos_bundle(download_path: &std::path::Path) -> anyhow::Result<()> {
    use std::path::PathBuf;

    let current_exe = std::env::current_exe().context("获取当前程序路径失败")?;
    // current_exe 形如 /Applications/AMLL Player.app/Contents/MacOS/amll-player
    let bundle_path = current_exe
        .ancestors()
        .find(|path| path.extension().is_some_and(|ext| ext == "app"))
        .context("未找到当前应用的 .app 包目录")?
        .to_path_buf();

    // 暂存区与应用同目录,保证 rename 在同一文件系统内完成
    let parent = bundle_path.parent().context("无法定位应用所在目录")?;
    let staged_dir = parent.join(".amll-update-staging");
    let _ = std::fs::remove_dir_all(&staged_dir);
    std::fs::create_dir_all(&staged_dir).context("无法在应用目录创建暂存区(权限不足?)")?;

    let unpack_result = (|| -> anyhow::Result<PathBuf> {
        let archive_file = std::fs::File::open(download_path)?;
        let decoder = flate2::read::GzDecoder::new(archive_file);
        let mut archive = tar::Archive::new(decoder);
        archive.set_preserve_permissions(true);
        for entry in archive.entries().context("读取更新包失败")? {
            let mut entry = entry.context("遍历更新包失败")?;
            entry.unpack(&staged_dir).context("解包更新内容失败")?;
        }
        std::fs::read_dir(&staged_dir)?
            .find_map(|entry| Some(entry.ok()?.path()))
            .find(|path| path.is_dir() && path.extension().is_some_and(|ext| ext == "app"))
            .context("更新包中未找到 .app 应用目录")
    })();
    let staged_app = match unpack_result {
        Ok(staged_app) => staged_app,
        Err(e) => {
            std::fs::remove_dir_all(&staged_dir).ok();
            return Err(e);
        }
    };

    // 备份当前应用包,新包就位失败时回滚
    let backup_path = parent.join(format!(
        "{}.old",
        bundle_path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default()
    ));
    let _ = std::fs::remove_dir_all(&backup_path);
    std::fs::rename(&bundle_path, &backup_path)
        .context("移动当前应用失败(权限不足或被占用)")?;
    if let Err(e) = std::fs::rename(&staged_app, &bundle_path) {
        std::fs::rename(&backup_path, &bundle_path).ok();
        std::fs::remove_dir_all(&staged_dir).ok();
        return Err(e).context("替换应用失败");
    }

    std::fs::remove_dir_all(&backup_path).ok();
    std::fs::remove_dir_all(&staged_dir).ok();
    info!("应用已更新到 {bundle_path:?},等待前端重启");
    Ok(())
}
