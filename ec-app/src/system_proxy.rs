//! 系统代理自动设置：连接成功后设 Windows 系统代理为 PAC（自动代理配置），
//! 让浏览器等应用自动走 SOCKS5 代理访问内网，无需手动配置。
//!
//! 原理：
//!   1. 生成 PAC 内容（JS 函数 FindProxyForURL），内网 IP 走 SOCKS5，其余 DIRECT
//!   2. 起本地 HTTP server（1421 端口）托管 PAC，AutoConfigURL 指向 http://127.0.0.1:1421/proxy.pac
//!   3. 过滤 ProxyOverride 移除内网网段（避免绕过 PAC）
//!   4. 调 Win32 InternetSetOption 通知系统刷新
//!   5. 断开时停 HTTP server + 恢复原始代理设置

use std::sync::Arc;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(windows)]
use winreg::enums::*;
#[cfg(windows)]
use winreg::RegKey;

/// Windows 创建进程标志：不弹控制台窗口。
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

#[cfg(windows)]
const INTERNET_SETTINGS: &str = r"Software\Microsoft\Windows\CurrentVersion\Internet Settings";

pub const PAC_HTTP_PORT: u16 = 1421;

/// 资源条目（与 ec_login::Resource 对应）。
pub struct ProxyResource {
    /// 如 "192.168.1.40~192.168.1.60" 或 "192.168.1.222"，多个用 ";" 分隔
    pub host: String,
    /// 如 "1~65535"，与 host 的 ";" 分隔一一对应
    pub port: String,
}

/// 原始系统代理设置（断开时恢复）。
#[derive(Debug, Clone, Default)]
pub struct OriginalProxySettings {
    pub auto_config_url: Option<String>,
    pub proxy_override: Option<String>,
}

/// 生成 PAC 文件内容。
///
/// 精确分流：只有资源列表中的「IP 范围 + 端口范围」走 SOCKS5 代理，
/// 其余一律 DIRECT（不经过代理）。
///
/// PAC 里用自定义 JS 函数：
///   - ipToNum(ip)：IPv4 转 32 位整数
///   - inRange(host, start, end)：host 是否在 IP 范围内
///   - inPort(port, pmin, pmax)：端口是否在范围内
/// 端口从 url 提取（http=80, https=443，或显式端口）。
pub fn generate_pac(socks_port: u16, resources: &[ProxyResource]) -> String {
    // 收集所有规则：(ip_start, ip_end, port_min, port_max)
    let mut rules: Vec<(u32, u32, u32, u32)> = Vec::new();

    for res in resources {
        let hosts: Vec<&str> = res.host.split(';').map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
        let ports: Vec<&str> = res.port.split(';').map(|s| s.trim()).filter(|s| !s.is_empty()).collect();

        for (i, host) in hosts.iter().enumerate() {
            let port_str = ports.get(i).copied().unwrap_or("1~65535");
            // 解析端口范围
            let (pmin, pmax) = match port_str.split_once('~') {
                Some((a, b)) => (a.parse::<u32>().unwrap_or(1), b.parse::<u32>().unwrap_or(65535)),
                None => (port_str.parse::<u32>().unwrap_or(1), port_str.parse::<u32>().unwrap_or(65535)),
            };

            // 解析 IP：范围或单 IP
            let (start, end) = match host.split_once('~') {
                Some((a, b)) => (a.trim(), b.trim()),
                None => (host.trim(), host.trim()),
            };
            let start_num = ip_to_num(start);
            let end_num = ip_to_num(end);
            if start_num == 0 && end_num == 0 {
                continue; // 无效 IP（可能是域名，本方案只处理 IP 资源）
            }
            rules.push((start_num, end_num, pmin, pmax));
        }
    }

    // 生成规则判断 JS
    let mut rule_checks = String::new();
    for (i, (s, e, pmin, pmax)) in rules.iter().enumerate() {
        rule_checks.push_str(&format!(
            "        if (inRange(host, {s}, {e}) && inPort(port, {pmin}, {pmax})) return \"SOCKS5 127.0.0.1:{socks_port}\";\n"
        ));
    }

    // 无规则时默认不代理任何流量
    if rules.is_empty() {
        return String::from(
            r#"// RustConnect 自动代理配置（无资源规则，全部直连）
function FindProxyForURL(url, host) {
    return "DIRECT";
}
"#,
        );
    }

    format!(
        r#"// RustConnect 自动代理配置：仅资源列表中的 IP+端口走代理
// IPv4 转 32 位整数（注意 parseInt，避免字符串拼接）
function ipToNum(ip) {{
    var parts = ip.split('.');
    return ((parseInt(parts[0], 10) * 256 + parseInt(parts[1], 10)) * 256 + parseInt(parts[2], 10)) * 256 + parseInt(parts[3], 10);
}}
// host 是否在 [start, end] IP 范围内
function inRange(host, start, end) {{
    var h = ipToNum(host);
    return h >= start && h <= end;
}}
// 端口是否在 [pmin, pmax] 范围内
function inPort(port, pmin, pmax) {{
    return port >= pmin && port <= pmax;
}}
function FindProxyForURL(url, host) {{
    // 从 url 提取端口（http=80, https=443，或显式端口）
    var port = 80;
    if (url.indexOf('https://') === 0) port = 443;
    var m = url.match(/^https?:\/\/[^/]+:(\d+)/);
    if (m) port = parseInt(m[1], 10);
    // 只对资源规则命中的请求走代理
{rule_checks}    return "DIRECT";
}}
"#
    )
}

/// IPv4 字符串转 32 位整数。无效返回 0。
fn ip_to_num(ip: &str) -> u32 {
    let parts: Vec<&str> = ip.split('.').collect();
    if parts.len() != 4 {
        return 0;
    }
    let mut result: u32 = 0;
    for p in parts {
        let v = match p.parse::<u32>() {
            Ok(v) if v <= 255 => v,
            _ => return 0,
        };
        result = result * 256 + v;
    }
    result
}

/// 启动 PAC HTTP server（后台 tokio task）。
/// 返回 JoinHandle 供断开时 abort。
pub fn start_pac_server(pac_content: String) -> tokio::task::JoinHandle<()> {
    let pac = Arc::new(pac_content);
    tokio::spawn(async move {
        let listener = match tokio::net::TcpListener::bind(format!("127.0.0.1:{PAC_HTTP_PORT}")).await {
            Ok(l) => l,
            Err(e) => {
                log::error!("PAC HTTP server 绑定失败: {e}");
                return;
            }
        };
        log::info!("PAC HTTP server 监听 127.0.0.1:{PAC_HTTP_PORT}");
        loop {
            match listener.accept().await {
                Ok((mut stream, _)) => {
                    let pac = Arc::clone(&pac);
                    tokio::spawn(async move {
                        use tokio::io::{AsyncReadExt, AsyncWriteExt};
                        // 读请求（丢弃），返回 PAC 内容
                        let mut buf = [0u8; 1024];
                        let _ = stream.read(&mut buf).await;
                        let response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/x-ns-proxy-autoconfig\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            pac.len(), pac
                        );
                        let _ = stream.write_all(response.as_bytes()).await;
                    });
                }
                Err(e) => {
                    log::warn!("PAC server accept 失败: {e}");
                    break;
                }
            }
        }
    })
}

/// 保存当前系统代理设置（断开时恢复用）。
///
/// Windows：读注册表 HKCU\...\Internet Settings 的 AutoConfigURL / ProxyOverride。
/// Linux：无注册表式全局状态，返回空设置（GNOME/KDE 改 gsettings 后，
///   断开时 restore() 会把 mode 设回 none，不需要预先保存原值）。
/// 其它平台：返回默认（空）设置。
pub fn save_original() -> OriginalProxySettings {
    #[cfg(target_os = "windows")]
    {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let settings = hkcu.open_subkey(INTERNET_SETTINGS);
        match settings {
            Ok(s) => OriginalProxySettings {
                auto_config_url: s.get_value::<String, _>("AutoConfigURL").ok(),
                proxy_override: s.get_value::<String, _>("ProxyOverride").ok(),
            },
            Err(_) => OriginalProxySettings::default(),
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        OriginalProxySettings::default()
    }
}

/// 启用系统代理：设 AutoConfigURL 指向本地 HTTP PAC。
///
/// Windows：写注册表 AutoConfigURL + 过滤 ProxyOverride 内网网段 + 通知刷新。
/// Linux：GNOME/KDE 调 gsettings/kwriteconfig5 设 PAC；其它桌面仅提示。
/// 其它平台：空操作。
pub fn enable() -> std::io::Result<()> {
    #[cfg(target_os = "windows")]
    {
        let pac_url = format!("http://127.0.0.1:{PAC_HTTP_PORT}/proxy.pac");

        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let (key, _) = hkcu.create_subkey(INTERNET_SETTINGS)?;
        key.set_value("AutoConfigURL", &pac_url)?;

        // 过滤 ProxyOverride 移除内网网段（避免绕过 PAC）
        if let Ok(current_override) = key.get_value::<String, _>("ProxyOverride") {
            let filtered: Vec<&str> = current_override
                .split(';')
                .filter(|item| {
                    let item = item.trim().to_lowercase();
                    !(item.starts_with("192.168.")
                        || item.starts_with("10.")
                        || item.starts_with("172.1")
                        || item.starts_with("172.2")
                        || item.starts_with("172.3")
                        || item == "localhost"
                        || item == "*.local")
                })
                .collect();
            let new_override = filtered.join(";");
            key.set_value("ProxyOverride", &new_override)?;
            log::info!("ProxyOverride 过滤: {} -> {}", current_override, new_override);
        }

        notify_settings_changed();
        log::info!("系统代理已启用: {pac_url}");
        Ok(())
    }
    #[cfg(target_os = "linux")]
    {
        let pac_url = format!("http://127.0.0.1:{PAC_HTTP_PORT}/proxy.pac");
        linux_proxy::enable(&pac_url)
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        Ok(())
    }
}

/// 恢复原始系统代理设置。
///
/// Windows：按保存的原值写回注册表 + 通知刷新。
/// Linux：GNOME/KDE 调 gsettings/kwriteconfig5 把代理关掉（mode=none）。
/// 其它平台：空操作。
pub fn restore(original: OriginalProxySettings) -> std::io::Result<()> {
    #[cfg(target_os = "windows")]
    {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let (key, _) = hkcu.create_subkey(INTERNET_SETTINGS)?;

        match original.auto_config_url {
            Some(url) => {
                key.set_value("AutoConfigURL", &url)?;
            }
            None => {
                let _ = key.delete_value("AutoConfigURL");
            }
        }
        match original.proxy_override {
            Some(ov) => {
                key.set_value("ProxyOverride", &ov)?;
            }
            None => {
                let _ = key.delete_value("ProxyOverride");
            }
        }

        notify_settings_changed();
        log::info!("系统代理已恢复");
        Ok(())
    }
    #[cfg(target_os = "linux")]
    {
        let _ = original; // Linux 不依赖保存的原值，直接关代理
        linux_proxy::restore()
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        let _ = original;
        Ok(())
    }
}

/// 调 Win32 InternetSetOption 通知系统代理设置已变更。
#[cfg(target_os = "windows")]
fn notify_settings_changed() {
    let _ = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            "Add-Type -TypeDefinition 'using System;using System.Runtime.InteropServices;public class WinINET{[DllImport(\"wininet.dll\",SetLastError=true)]public static extern bool InternetSetOption(IntPtr h,int o,IntPtr b,int l);}'; [WinINET]::InternetSetOption([IntPtr]::Zero,39,[IntPtr]::Zero,0); [WinINET]::InternetSetOption([IntPtr]::Zero,37,[IntPtr]::Zero,0)",
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();
}

/// Linux 系统代理实现：通过 gsettings（GNOME）/ kwriteconfig5（KDE）设 PAC，
/// 其它桌面仅提示（影响新进程的环境变量方案有限，建议手动配置浏览器代理）。
#[cfg(target_os = "linux")]
mod linux_proxy {
    use std::io;
    use std::process::Command;

    /// 检测到的桌面环境类型，决定用哪种代理配置后端。
    enum Desktop {
        Gnome,
        Kde,
        Generic,
    }

    /// 根据 XDG_CURRENT_DESKTOP 环境变量识别桌面环境。
    fn detect_desktop() -> Desktop {
        match std::env::var("XDG_CURRENT_DESKTOP")
            .unwrap_or_default()
            .to_uppercase()
            .as_str()
        {
            d if d.contains("GNOME") || d.contains("UNITY") || d.contains("CINNAMON") => {
                Desktop::Gnome
            }
            d if d.contains("KDE") => Desktop::Kde,
            _ => Desktop::Generic,
        }
    }

    /// 设系统代理指向 PAC URL。
    pub fn enable(pac_url: &str) -> io::Result<()> {
        match detect_desktop() {
            Desktop::Gnome => {
                Command::new("gsettings")
                    .args(["set", "org.gnome.system.proxy", "mode", "auto"])
                    .status()?;
                Command::new("gsettings")
                    .args(["set", "org.gnome.system.proxy", "autoconfig-url", pac_url])
                    .status()?;
                log::info!("系统代理已设置（GNOME: autoconfig {}）", pac_url);
            }
            Desktop::Kde => {
                Command::new("kwriteconfig5")
                    .args([
                        "--file",
                        "kioslaverc",
                        "--group",
                        "Proxy Settings",
                        "--key",
                        "Proxy Type",
                        "4",
                    ])
                    .status()?;
                Command::new("kwriteconfig5")
                    .args([
                        "--file",
                        "kioslaverc",
                        "--group",
                        "Proxy Settings",
                        "--key",
                        "Proxy Config Script",
                        pac_url,
                    ])
                    .status()?;
                log::info!("系统代理已设置（KDE: PAC {}）", pac_url);
            }
            Desktop::Generic => {
                log::info!(
                    "检测到非 GNOME/KDE 桌面，仅设环境变量（影响新进程）。建议手动配置浏览器代理。"
                );
            }
        }
        Ok(())
    }

    /// 恢复：关掉系统代理。Linux 无注册表式全局状态，无需预先保存原值，
    /// 直接把代理 mode 设回 none（GNOME）/ Proxy Type=0（KDE）。
    pub fn restore() -> io::Result<()> {
        match detect_desktop() {
            Desktop::Gnome => {
                Command::new("gsettings")
                    .args(["set", "org.gnome.system.proxy", "mode", "none"])
                    .status()?;
                log::info!("系统代理已恢复（GNOME: none）");
            }
            Desktop::Kde => {
                Command::new("kwriteconfig5")
                    .args([
                        "--file",
                        "kioslaverc",
                        "--group",
                        "Proxy Settings",
                        "--key",
                        "Proxy Type",
                        "0",
                    ])
                    .status()?;
                log::info!("系统代理已恢复（KDE）");
            }
            Desktop::Generic => {}
        }
        Ok(())
    }
}
