//! TUN 模式连接流程编排：管理员权限检测 + wintun.dll 准备 + 路由计算 + TunBridge。
//!
//! 与 vpn.rs（SOCKS5/PAC 模式）平行：login → request_token → build_token →
//! request_ip → L3Conn → TunBridge（虚拟网卡全局代理，需要管理员权限）。
//!
//! 这部分代码运行在 `connect_tun` command spawn 的 tokio task 里，
//! `cancel` 用于断开时停止转发并清理路由。

use std::path::PathBuf;
use std::sync::Arc;

use ec_proxy::tun::{TunBridge, TunRoute};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::Notify;

/// 更新系统托盘 tooltip，反映连接状态。
fn update_tray_tooltip(app: &AppHandle, text: &str) {
    if let Some(tray) = app.tray_by_id("main") {
        let _ = tray.set_tooltip(Some(text));
    }
}

/// 连接流程的事件 payload（用于 `vpn:progress`）。
#[derive(Debug, Clone, serde::Serialize)]
struct ProgressPayload<'a> {
    stage: &'a str,
    message: &'a str,
}

/// 检测当前进程是否有管理员权限。
///
/// Windows：执行 `net session`——该命令只有管理员能成功（非管理员返回
/// "Access is denied"）。比起 Win32 token 检查，子进程方案零额外依赖且可靠。
/// 非 Windows：TUN 模式暂未支持，直接返回 false。
pub fn is_admin() -> bool {
    #[cfg(windows)]
    {
        std::process::Command::new("net")
            .args(["session"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// 以管理员身份重新启动当前进程（UAC 弹窗）。
///
/// 用 PowerShell `Start-Process -Verb RunAs` 触发 UAC。
/// 重启时带 `--relaunched-as-admin` 参数，新实例据此跳过再次提权。
/// 返回 Ok(true) 表示已发起提权（当前实例应退出），
/// Ok(false) 表示用户取消了 UAC（留在当前实例）。
pub fn relaunch_as_admin() -> std::io::Result<bool> {
    #[cfg(windows)]
    {
        let exe = std::env::current_exe()?;
        let exe_str = exe.to_string_lossy().to_string();
        // 用 PowerShell 提权重启；Start-Process -Verb RunAs 会弹 UAC。
        // 返回 -1 表示用户取消/失败，返回 0 表示已启动。
        let script = format!(
            "try {{ $p = Start-Process -FilePath '{}' -ArgumentList '--relaunched-as-admin' -Verb RunAs -PassThru; if ($p) {{ exit 0 }} else {{ exit -1 }} }} catch {{ exit -1 }}",
            exe_str.replace('\'', "''")
        );
        let out = std::process::Command::new("powershell")
            .args(["-NoProfile", "-Command", &script])
            .output()?;
        Ok(out.status.success())
    }
    #[cfg(not(windows))]
    {
        let _ = (); // 非 Windows 暂不支持
        Ok(false)
    }
}

/// 确保 wintun.dll 存在，不存在则从官网下载并解压。
///
/// 目标路径：`%APPDATA%/rust_connect/wintun.dll`。
/// 下载 `wintun-0.14.1.zip`（PowerShell Invoke-WebRequest），
/// Expand-Archive 解压后从 `wintun/bin/amd64/wintun.dll` 拷贝过去，
/// 并清理临时 zip 与解压目录。
pub fn ensure_wintun() -> std::io::Result<PathBuf> {
    let dir = std::env::var("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir())
        .join("rust_connect");
    std::fs::create_dir_all(&dir)?;
    let dll_path = dir.join("wintun.dll");

    if dll_path.exists() {
        return Ok(dll_path);
    }

    // 下载 zip
    let zip_path = dir.join("wintun.zip");
    let download_cmd = format!(
        "Invoke-WebRequest -Uri 'https://www.wintun.net/builds/wintun-0.14.1.zip' -OutFile '{}'",
        zip_path.display()
    );
    let status = std::process::Command::new("powershell")
        .args(["-NoProfile", "-Command", download_cmd.as_str()])
        .status()?;
    if !status.success() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            "wintun.dll 下载失败",
        ));
    }

    // 解压
    let extract_cmd = format!(
        "Expand-Archive -Path '{}' -DestinationPath '{}' -Force",
        zip_path.display(),
        dir.display()
    );
    let status = std::process::Command::new("powershell")
        .args(["-NoProfile", "-Command", extract_cmd.as_str()])
        .status()?;
    if !status.success() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            "wintun.zip 解压失败",
        ));
    }

    // 解压后结构: dir/wintun/bin/amd64/wintun.dll
    let extracted = dir
        .join("wintun")
        .join("bin")
        .join("amd64")
        .join("wintun.dll");
    if !extracted.exists() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "解压后找不到 wintun.dll",
        ));
    }
    std::fs::copy(&extracted, &dll_path)?;
    let _ = std::fs::remove_file(&zip_path);
    let _ = std::fs::remove_dir_all(dir.join("wintun"));
    Ok(dll_path)
}

/// 把资源列表的 host 字段转成路由（/24 网段近似）。
///
/// host 形如 "192.168.1.40~192.168.1.60;192.168.1.222"：
/// 取每段起始 IP 的前三字节作网段，去重。
pub fn resources_to_routes(resources: &[ec_login::Resource]) -> Vec<TunRoute> {
    let mut routes = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for res in resources {
        for part in res.host.split(';') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            // 取起始 IP 的前三字节作为网段（如 "192.168.1.40" → "192.168.1.0"）
            let start = part.split('~').next().unwrap_or(part).trim();
            if let Some(prefix) = start.rsplit_once('.') {
                let network = format!("{}.0", prefix.0);
                if seen.insert(network.clone()) {
                    routes.push(TunRoute {
                        network,
                        mask: "255.255.255.0".to_string(),
                    });
                }
            }
        }
    }
    routes
}

/// TUN 模式连接流程（类似 vpn.rs 的 connect_vpn，但走虚拟网卡全局代理）。
///
/// 步骤：
///   1. login（spawn_blocking）拿 twfID
///   2. request_token + build_token + request_ip（spawn_blocking）拿客户端 IP
///   3. fetch_resources 算内网路由（失败不阻塞连接，仅警告）
///   4. L3Conn::new + TunBridge::start（spawn_blocking）
///   5. emit vpn:status connected + 托盘 tooltip
///   6. 长驻等 cancel；收到后 stop 转发、删路由、emit disconnected
///
/// 注意：request_ip 返回的连接必须保活到会话结束（request.go:647），
/// 这里把它绑在 `_keepalive_conn` 里随函数退出自然 drop 关闭。
pub async fn connect_tun_vpn(
    app: AppHandle,
    server: String,
    username: String,
    password: String,
    cancel: Arc<Notify>,
    wintun_path: PathBuf,
) -> Result<(), String> {
    // ---- 1. 登录 ----
    let _ = app.emit(
        "vpn:progress",
        ProgressPayload {
            stage: "logging_in",
            message: "登录中...",
        },
    );

    let login_cfg = ec_login::LoginConfig {
        server: server.clone(),
        username: username.clone(),
        password: password.clone(),
    };
    let login_result = tokio::task::spawn_blocking(move || ec_login::login(&login_cfg))
        .await
        .map_err(|e| format!("login task panic: {e}"))?
        .map_err(|e| format!("登录失败: {e}"))?;

    let twf_id = match login_result {
        ec_login::LoginStep::Done(t) if !t.is_empty() => t,
        other => return Err(format!("登录未完成: {:?}", other)),
    };

    // ---- 2. 隧道握手（ec-utls 是 blocking 的，放 spawn_blocking）----
    let _ = app.emit(
        "vpn:progress",
        ProgressPayload {
            stage: "tunnel",
            message: "建立隧道...",
        },
    );
    let server2 = server.clone();
    let twf2 = twf_id.clone();
    let (ip, ip_reverse, tkn, _keepalive_conn) = tokio::task::spawn_blocking(
        move || -> Result<_, String> {
            let sid_hex = ec_protocol::token::request_token(&server2, &twf2)
                .map_err(|e| format!("request_token 失败: {e}"))?;
            let tkn = ec_protocol::token::build_token(&sid_hex, &twf2)
                .map_err(|e| format!("build_token 失败: {e}"))?;
            let ((ip, ip_reverse), conn) = ec_protocol::tunnel::request_ip(&server2, &tkn)
                .map_err(|e| format!("request_ip 失败: {e}"))?;
            Ok((ip, ip_reverse, tkn, conn))
        },
    )
    .await
    .map_err(|e| format!("隧道握手 task panic: {e}"))??;

    // ---- 3. 拉资源列表，转 /24 内网路由（失败不阻塞连接，仅警告）----
    let server3 = server.clone();
    let twf3 = twf_id.clone();
    let resources_result = tokio::task::spawn_blocking(move || {
        ec_login::fetch_resources(&server3, &twf3)
    })
    .await;
    let routes = match resources_result {
        Ok(Ok(resources)) => {
            let _ = app.emit("vpn:resources", &resources);
            resources_to_routes(&resources)
        }
        Ok(Err(e)) => {
            log::warn!("拉取资源列表失败（TUN 无内网路由）: {e}");
            Vec::new()
        }
        Err(e) => {
            log::warn!("拉取资源列表 task panic: {e}");
            Vec::new()
        }
    };

    // ---- 4. L3Conn + TunBridge（blocking，放 spawn_blocking）----
    let _ = app.emit("vpn:status", serde_json::json!({ "state": "connecting" }));
    let server4 = server.clone();
    let wintun = wintun_path.to_string_lossy().into_owned();
    let routes_for_start = routes.clone();
    let mut bridge = tokio::task::spawn_blocking(move || -> Result<TunBridge, String> {
        let l3 = ec_protocol::l3conn::L3Conn::new(&server4, &tkn, &ip_reverse)
            .map_err(|e| format!("L3Conn 建立失败: {e}"))?;
        TunBridge::start(l3, ip, &routes_for_start, Some(&wintun))
            .map_err(|e| format!("TUN 网卡启动失败: {e}"))
    })
    .await
    .map_err(|e| format!("TUN 启动 task panic: {e}"))??;

    // ---- 5. 已连接 ----
    {
        let state = app.state::<crate::state::AppState>();
        state.set_connected(
            format!("{}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3]),
            String::new(),
        );
    }
    let _ = app.emit("vpn:status", serde_json::json!({ "state": "connected" }));
    update_tray_tooltip(&app, "RustConnect - 已连接（TUN）");

    // ---- 6. 会话保活：每 60s 调 update_session.csp 防服务端空闲超时 ----
    let keepalive_server = server.clone();
    let keepalive_twf = twf_id.clone();
    let keepalive_cancel = cancel.clone();
    let keepalive_task = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_secs(60)) => {}
                _ = keepalive_cancel.notified() => return,
            }
            let server = keepalive_server.clone();
            let twf = keepalive_twf.clone();
            let result = tokio::task::spawn_blocking(move || {
                ec_login::keep_session_alive(&server, &twf)
            })
            .await;
            match result {
                Ok(Ok(())) => log::debug!("会话保活成功"),
                Ok(Err(e)) => log::warn!("会话保活失败: {e}"),
                Err(e) => log::warn!("会话保活 task panic: {e}"),
            }
        }
    });

    // ---- 7. 等断开：cancel 后停转发、删路由 ----
    cancel.notified().await;
    keepalive_task.abort();
    bridge.stop();
    TunBridge::cleanup_routes(&routes, ip);
    let _ = app.emit("vpn:status", serde_json::json!({ "state": "disconnected" }));
    update_tray_tooltip(&app, "RustConnect - 未连接");
    Ok(())
}
