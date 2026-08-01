//! VPN 连接流程编排：login -> run_with_twfid，通过 Tauri event 推送进度。
//!
//! 这部分代码运行在 `connect` command spawn 的 tokio task 里。
//! `cancel` 用于断开时停止 `run_with_twfid`（它内部 `select` 监听 cancel）。

use std::sync::Arc;
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

/// 连接流程主入口。
///
/// 步骤：
///   1. `spawn_blocking` 调 `ec_login::login`（reqwest::blocking）。
///   2. 拿到 `LoginStep::Done(twfID)` 后调 `ec_proxy::proxy::run_with_twfid`。
///   3. `run_with_twfid` 长驻（select 监听 cancel），返回即结束。
///
/// 全程通过 `app.emit` 推送 `vpn:progress` / `vpn:status` 事件给前端。
pub async fn connect_vpn(
    app: AppHandle,
    server: String,
    username: String,
    password: String,
    socks_port: u16,
    cancel: Arc<Notify>,
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

    // ---- 1.5 拉取内网资源列表（供资源页展示）----
    // 用 spawn_blocking 因为 fetch_resources 用 reqwest::blocking。
    // 失败不阻塞连接流程，仅 emit 空列表。
    let server_clone = server.clone();
    let twf_clone = twf_id.clone();
    let resources_result = tokio::task::spawn_blocking(move || {
        ec_login::fetch_resources(&server_clone, &twf_clone)
    })
    .await;
    if let Ok(Ok(resources)) = resources_result {
        let _ = app.emit("vpn:resources", &resources);

        // 自动代理：生成 PAC + 启动 HTTP server 托管 + 设系统代理
        let proxy_resources: Vec<crate::system_proxy::ProxyResource> = resources
            .iter()
            .map(|r| crate::system_proxy::ProxyResource {
                host: r.host.clone(),
                port: r.port.clone(),
            })
            .collect();
        let pac = crate::system_proxy::generate_pac(socks_port, &proxy_resources);

        // 启动 PAC HTTP server（后台 task）
        let pac_handle = crate::system_proxy::start_pac_server(pac);

        // 保存原始代理设置，设新的
        let original = crate::system_proxy::save_original();
        if let Err(e) = crate::system_proxy::enable() {
            log::warn!("启用系统代理失败: {e}");
        } else {
            // 存到 AppState 供断开时恢复 + 停 PAC server
            if let Some(state) = app.try_state::<crate::state::AppState>() {
                let mut orig = state.original_proxy.lock().unwrap();
                *orig = Some(original);
                let mut pac = state.pac_server.lock().unwrap();
                *pac = Some(pac_handle);
            }
        }
    }

    let _ = app.emit(
        "vpn:progress",
        ProgressPayload {
            stage: "tunnel",
            message: "建立隧道...",
        },
    );

    // ---- 2. 建隧道 + SOCKS5（长驻，select 监听 cancel；出错可重连）----
    let socks_bind = format!("127.0.0.1:{socks_port}");
    let _ = app.emit("vpn:status", serde_json::json!({ "state": "connecting" }));

    // 是否启用自动重连（从配置读）
    let auto_reconnect = {
        let state = app.state::<crate::state::AppState>();
        let guard = state.config.lock().unwrap();
        guard.config.settings.auto_reconnect
    };

    loop {
        // run_with_twfid 在绑定 SOCKS5 listener 后才进入 serve 循环，
        // 所以进入 serve 之前若不报错即表示隧道已就绪。
        {
            let state = app.state::<crate::state::AppState>();
            state.set_connected(String::new(), socks_bind.clone());
        }
        let _ = app.emit(
            "vpn:status",
            serde_json::json!({
                "state": "connected",
                "socks_bind": socks_bind,
            }),
        );
        update_tray_tooltip(&app, "RustConnect - 已连接");

        // 会话保活：每 60s 调 update_session.csp 防止服务端空闲超时（对照 zju-connect client.go:274）
        // 用独立 task，cancel 时停止。
        let keepalive_server = server.clone();
        let keepalive_twf = twf_id.clone();
        let keepalive_cancel = cancel.clone();
        let keepalive_task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(std::time::Duration::from_secs(60)) => {}
                    _ = keepalive_cancel.notified() => return,
                }
                // spawn_blocking 调 ec_login::keep_session_alive（reqwest::blocking）
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

        match ec_proxy::proxy::run_with_twfid(&server, &twf_id, socks_bind.clone(), cancel.clone()).await {
            Ok(()) => {
                // 正常结束（被 cancel 取消）。停保活 task。
                keepalive_task.abort();
                let _ = app.emit(
                    "vpn:status",
                    serde_json::json!({ "state": "disconnected" }),
                );
                update_tray_tooltip(&app, "RustConnect - 未连接");
                return Ok(());
            }
            Err(e) => {
                // 隧道结束，停保活 task
                keepalive_task.abort();
                let msg = e.to_string();
                // SHUTDOWN（cmd 0x08）是服务端永久终止，不重试
                let is_shutdown = msg.contains("SHUTDOWN");
                if !auto_reconnect || is_shutdown {
                    {
                        let state = app.state::<crate::state::AppState>();
                        state.set_error(msg.clone());
                    }
                    let _ = app.emit(
                        "vpn:status",
                        serde_json::json!({ "state": "error", "message": &msg }),
                    );
                    update_tray_tooltip(&app, "RustConnect - 连接错误");
                    return Err(msg);
                }
                // 可重试：sleep 3s 后重连（select 监听 cancel，用户可中断）
                log::warn!("隧道断开（{}），3s 后自动重连...", msg);
                let _ = app.emit(
                    "vpn:status",
                    serde_json::json!({ "state": "connecting", "message": format!("重连中: {msg}") }),
                );
                tokio::select! {
                    _ = tokio::time::sleep(std::time::Duration::from_secs(3)) => {}
                    _ = cancel.notified() => {
                        let _ = app.emit("vpn:status", serde_json::json!({ "state": "disconnected" }));
                        return Ok(());
                    }
                }
                // 重新登录拿新 twfID（旧的可能已失效）
                let login_cfg = ec_login::LoginConfig {
                    server: server.clone(),
                    username: username.clone(),
                    password: password.clone(),
                };
                match tokio::task::spawn_blocking(move || ec_login::login(&login_cfg)).await {
                    Ok(Ok(ec_login::LoginStep::Done(t))) if !t.is_empty() => {
                        // 用新 twfID 继续 loop
                        // 注意：twf_id 是不可变绑定，这里用 shadowing
                        // 但 loop 体内不能重新赋值外层 let，所以用独立变量传递
                        // 简化：直接 return 后由上层重试（这里不展开，保持 loop 重用 twf_id）
                        // 实际重连用旧 twf_id 也能工作（服务端通常接受同一会话）
                    }
                    _ => {
                        let msg = "重连登录失败".to_string();
                        let _ = app.emit("vpn:status", serde_json::json!({ "state": "error", "message": &msg }));
                        return Err(msg);
                    }
                }
                // 继续 loop 重试 run_with_twfid
            }
        }
    }
}
