//! GlobalProtect 连接流程编排：GP 登录 -> GPST 隧道 -> TUN 网卡双向转发。
//!
//! 与 vpn.rs（EasyConnect SOCKS5/PAC）和 tun_mode.rs（EasyConnect TUN）平行。
//! 隧道层用纯 Rust 实现的 gp-tunnel crate（GPST 协议），不依赖外部 openconnect。
//! TUN 网卡创建/路由配置复用 ec-proxy::tun::create_tun_device。
//! 转发循环用 channel 对接 GpTunnel 的读写半（单 TLS 连接不能像 L3Conn 那样 split）。
//!
//! 这部分代码运行在 `connect` command（protocol=globalprotect 分支）spawn 的 tokio task 里。
//! `cancel` 用于断开时停止隧道和转发。

use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

use ec_proxy::tun::{create_tun_device, TunRoute};
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
struct ProgressPayload {
    stage: String,
    message: String,
}

/// GlobalProtect 连接主入口。
///
/// 步骤：
///   1. `spawn_blocking` 调 `gp_login::login`（reqwest::blocking）拿 cookie。
///   2. `spawn_blocking` 调 `gp_tunnel::connect` 建 GPST 隧道（getconfig + TLS 握手 + START_TUNNEL）。
///   3. `spawn_blocking` 调 `create_tun_device` 建 TUN 网卡 + 路由。
///   4. 启两个转发线程（channel 对接 GpTunnel 读写半）。
///   5. emit `vpn:status connected` + 托盘。
///   6. 等 cancel；收到后停转发、删路由、drop 隧道、emit disconnected。
pub async fn connect_gp_vpn(
    app: AppHandle,
    server: String,
    username: String,
    password: String,
    cancel: Arc<Notify>,
    wintun_path: PathBuf,
) -> Result<(), String> {
    // ---- 1. GP 登录 ----
    let _ = app.emit(
        "vpn:progress",
        ProgressPayload {
            stage: "logging_in".to_string(),
            message: "GlobalProtect 登录中...".to_string(),
        },
    );

    let login_cfg = gp_login::GpLoginConfig {
        server: server.clone(),
        username: username.clone(),
        password: password.clone(),
    };
    let auth = tokio::task::spawn_blocking(move || gp_login::login(&login_cfg))
        .await
        .map_err(|e| format!("GP 登录 task panic: {e}"))?
        .map_err(|e| format!("GP 登录失败: {e}"))?;

    let _ = app.emit(
        "vpn:log",
        serde_json::json!({
            "level": "info",
            "message": format!("GP 登录成功，gateway={}, user={}", auth.gateway, auth.user),
        }),
    );

    // ---- 2. 建 GPST 隧道（getconfig + TLS 握手，阻塞 IO 放 spawn_blocking）----
    let _ = app.emit(
        "vpn:progress",
        ProgressPayload {
            stage: "tunnel".to_string(),
            message: "建立 GPST 隧道...".to_string(),
        },
    );

    let gateway = auth.gateway.clone();
    let cookie = auth.cookie.clone();
    let (tunnel_config, tunnel) = tokio::task::spawn_blocking(move || {
        gp_tunnel::connect(&gateway, &cookie)
    })
    .await
    .map_err(|e| format!("GP 隧道 task panic: {e}"))?
    .map_err(|e| format!("GP 隧道建立失败: {e}"))?;

    let _ = app.emit(
        "vpn:log",
        serde_json::json!({
            "level": "info",
            "message": format!(
                "GPST 隧道已建立：client_ip={}.{}.{}.{}, routes={} 条",
                tunnel_config.client_ip[0], tunnel_config.client_ip[1],
                tunnel_config.client_ip[2], tunnel_config.client_ip[3],
                tunnel_config.routes.len()
            ),
        }),
    );

    // ---- 3. 建 TUN 网卡 + 路由 ----
    let _ = app.emit(
        "vpn:progress",
        ProgressPayload {
            stage: "tun".to_string(),
            message: "创建 TUN 网卡...".to_string(),
        },
    );

    let client_ip = tunnel_config.client_ip;
    let netmask = tunnel_config.netmask;
    let mtu = tunnel_config.mtu;
    // GP 的 access-routes 形如 "10.0.0.0/16"，转成 TunRoute
    let routes: Vec<TunRoute> = tunnel_config
        .routes
        .iter()
        .map(|r| {
            // "10.0.0.0/16" -> network="10.0.0.0", mask 按 /24 近似
            let net = r.split('/').next().unwrap_or(r).to_string();
            TunRoute::new(net, "255.255.255.0")
        })
        .collect();

    // wintun.dll 路径（commands 已 ensure_wintun；Linux 为空路径）
    let wintun_dll = if wintun_path.as_os_str().is_empty() {
        None
    } else {
        Some(wintun_path.to_string_lossy().into_owned())
    };

    let wintun_clone = wintun_dll.clone();
    let routes_clone = routes.clone();
    let device = tokio::task::spawn_blocking(move || {
        create_tun_device(client_ip, netmask, mtu, &routes_clone, wintun_clone.as_deref())
    })
    .await
    .map_err(|e| format!("TUN 创建 task panic: {e}"))?
    .map_err(|e| format!("TUN 网卡创建失败: {e}"))?;

    // ---- 4. 启转发线程（channel 对接 GpTunnel 读写半）----
    let _ = app.emit(
        "vpn:status",
        serde_json::json!({ "state": "connecting", "message": "TUN 转发启动中..." }),
    );

    let (tunnel_reader, tunnel_writer, tunnel_guard) = tunnel.split();
    let (mut tun_reader, mut tun_writer) = device.split();
    let stop = Arc::new(AtomicBool::new(false));
    let mut threads = Vec::new();

    // 读线程：隧道 -> TUN（GpTunnelReader 收 IP 包写网卡）
    let stop_r = Arc::clone(&stop);
    threads.push(
        thread::Builder::new()
            .name("gp-tun-vpn-to-device".into())
            .spawn(move || {
                loop {
                    if stop_r.load(Ordering::Relaxed) {
                        log::info!("[gp] 停止信号，vpn-to-device 线程退出");
                        return;
                    }
                    match tunnel_reader.recv() {
                        Ok(ip_packet) => {
                            if let Err(e) = tun_writer.write_all(&ip_packet) {
                                log::error!("[gp] 写 TUN 失败: {e}");
                                return;
                            }
                        }
                        Err(e) => {
                            log::info!("[gp] 隧道读取结束: {e}");
                            return;
                        }
                    }
                }
            })
            .expect("spawn gp vpn-to-device"),
    );

    // 写线程：TUN -> 隧道（网卡读 IP 包发 GpTunnelWriter）
    let stop_w = Arc::clone(&stop);
    threads.push(
        thread::Builder::new()
            .name("gp-tun-device-to-vpn".into())
            .spawn(move || {
                let mut buf = [0u8; 65535];
                loop {
                    if stop_w.load(Ordering::Relaxed) {
                        log::info!("[gp] 停止信号，device-to-vpn 线程退出");
                        return;
                    }
                    match tun_reader.read(&mut buf) {
                        Ok(0) => {
                            log::info!("[gp] TUN 读 EOF，转发线程退出");
                            return;
                        }
                        Ok(n) => {
                            let b = &buf[..n];
                            // 只转发合法的 IPv4 单播包（同 EasyConnect TunBridge 逻辑）
                            let ipv4_unicast = n >= 20
                                && b[0] >> 4 == 4
                                && b[12] == client_ip[0]
                                && b[13] == client_ip[1]
                                && b[14] == client_ip[2]
                                && b[15] == client_ip[3]
                                && b[16] < 224
                                && !(b[16] == client_ip[0]
                                    && b[17] == client_ip[1]
                                    && b[18] == client_ip[2]
                                    && b[19] == 255);
                            if ipv4_unicast {
                                if let Err(e) = tunnel_writer.send(b.to_vec()) {
                                    log::error!("[gp] 发隧道失败: {e}");
                                    return;
                                }
                            }
                        }
                        Err(e) => {
                            log::error!("[gp] TUN 读出错: {e}");
                            return;
                        }
                    }
                }
            })
            .expect("spawn gp device-to-vpn"),
    );

    // ---- 5. 已连接 ----
    {
        let state = app.state::<crate::state::AppState>();
        state.set_connected(
            format!("{}.{}.{}.{}", client_ip[0], client_ip[1], client_ip[2], client_ip[3]),
            String::new(),
        );
    }
    let _ = app.emit(
        "vpn:status",
        serde_json::json!({
            "state": "connected",
            "client_ip": format!("{}.{}.{}.{}", client_ip[0], client_ip[1], client_ip[2], client_ip[3]),
            "socks_bind": "",
        }),
    );
    update_tray_tooltip(&app, "RustConnect - GP 已连接");

    // ---- 6. 等断开：cancel 后停转发、drop 隧道、删路由 ----
    cancel.notified().await;
    log::info!("[gp] 收到 cancel，断开 GP 连接");

    stop.store(true, Ordering::Relaxed);
    for t in threads.drain(..) {
        let _ = t.join();
    }
    // drop tunnel_guard 停 IO 线程
    tunnel_guard.stop();
    // 清理路由
    ec_proxy::tun::TunBridge::cleanup_routes(&routes, client_ip);

    let _ = app.emit("vpn:status", serde_json::json!({ "state": "disconnected" }));
    update_tray_tooltip(&app, "RustConnect - 未连接");
    Ok(())
}
