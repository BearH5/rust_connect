//! Tauri command 函数：前端通过 `invoke` 调用这些函数。
//!
//! 所有 command 都接收 `tauri::State<'_, AppState>` 拿到共享状态。

use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::Notify;

use crate::config::Profile;
use crate::state::{AppState, VpnSession, VpnStatus};

/// 按 `profile_id` 连接。spawn tokio task 跑 `connect_vpn`。
#[tauri::command]
pub async fn connect(
    app: AppHandle,
    state: State<'_, AppState>,
    profile_id: String,
) -> Result<(), String> {
    // 1. 从 config 找 profile
    let profile = {
        let cfg = state.config.lock().unwrap();
        cfg.config
            .profiles
            .iter()
            .find(|p| p.id == profile_id)
            .cloned()
            .ok_or_else(|| format!("profile 不存在: {profile_id}"))?
    };

    // 2. 检查是否已有活跃连接
    {
        let session = state.session.lock().unwrap();
        if session.is_some() {
            return Err("已有一个活跃连接".into());
        }
    }

    // 3. 设状态为 Connecting，spawn task
    let cancel = Arc::new(Notify::new());
    {
        let mut session = state.session.lock().unwrap();
        *session = Some(VpnSession {
            cancel: cancel.clone(),
            status: VpnStatus::Connecting,
        });
    }

    // 记下 last_profile_id
    {
        let mut store = state.config.lock().unwrap();
        store.config.last_profile_id = Some(profile.id.clone());
        let _ = store.save();
    }

    let app_clone = app.clone();
    let server = profile.server.clone();
    let username = profile.username.clone();
    let password = profile.password.clone();
    let socks_port = profile.socks_port;

    // 读取代理模式（tun 需要管理员 + wintun.dll）
    let proxy_mode = {
        let cfg = state.config.lock().unwrap();
        cfg.config.settings.proxy_mode.clone()
    };
    let is_tun_mode = proxy_mode == "tun";

    // TUN 模式预检：管理员权限 + wintun.dll
    let wintun_path = if is_tun_mode {
        if !crate::tun_mode::is_admin() {
            #[cfg(target_os = "linux")]
            {
                let exe = std::env::current_exe()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|_| "<ec-app>".into());
                return Err(format!(
                    "TUN 模式需要 CAP_NET_ADMIN 权限。请运行以下命令后重启程序：\n\
                     sudo setcap cap_net_admin+ep {}",
                    exe
                ));
            }
            #[cfg(target_os = "windows")]
            {
                return Err("TUN 模式需要管理员权限，请以管理员身份运行本程序".into());
            }
            #[cfg(not(any(target_os = "linux", target_os = "windows")))]
            {
                return Err("TUN 模式当前不支持此平台".into());
            }
        }
        Some(
            tokio::task::spawn_blocking(crate::tun_mode::ensure_wintun)
                .await
                .map_err(|e| format!("wintun.dll 准备 task panic: {e}"))?
                .map_err(|e| format!("wintun.dll 准备失败: {e}"))?,
        )
    } else {
        None
    };

    tokio::spawn(async move {
        let app_for_status = app_clone.clone();
        let result = if is_tun_mode {
            crate::tun_mode::connect_tun_vpn(
                app_clone.clone(),
                server,
                username,
                password,
                cancel.clone(),
                wintun_path.expect("tun 模式必有 wintun 路径"),
            )
            .await
        } else {
            crate::vpn::connect_vpn(
                app_clone.clone(),
                server,
                username,
                password,
                socks_port,
                cancel.clone(),
            )
            .await
        };
        match result {
            Ok(()) => {
                log::info!("VPN 连接结束（正常）");
            }
            Err(e) => {
                log::error!("VPN 连接失败: {e}");
            }
        }
        // task 结束后清理会话状态：把 session 置空，
        // 这样 get_status 会返回 Disconnected。
        let st = app_for_status.state::<AppState>();
        let mut session = st.session.lock().unwrap();
        // 只有当当前 session 的 cancel 令牌和本 task 一致时才清理，
        // 避免 disconnect 已经 take 过又覆盖新的会话。这里直接 take 即可：
        // 若 disconnect 先执行，session 已是 None；否则由这里清。
        *session = None;
        let _ = app_for_status.emit(
            "vpn:status",
            serde_json::json!({ "state": "disconnected" }),
        );
    });

    Ok(())
}

/// TUN 模式连接：需要管理员权限 + wintun.dll。spawn tokio task 跑 connect_tun_vpn。
#[tauri::command]
pub async fn connect_tun(
    app: AppHandle,
    state: State<'_, AppState>,
    profile_id: String,
) -> Result<(), String> {
    // 1. 检查管理员权限
    if !crate::tun_mode::is_admin() {
        #[cfg(target_os = "linux")]
        {
            // Linux：用 setcap 授予 CAP_NET_ADMIN（不重启）。提示用户手动执行。
            let exe = std::env::current_exe()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| "<ec-app>".into());
            return Err(format!(
                "TUN 模式需要 CAP_NET_ADMIN 权限。请运行以下命令后重启程序：\n\
                 sudo setcap cap_net_admin+ep {}",
                exe
            ));
        }
        #[cfg(target_os = "windows")]
        {
            // 方案 B：UAC 提权重启当前程序。新实例以管理员运行后，
            // 用户再点连接即可。返回提示让前端显示。
            match crate::tun_mode::relaunch_as_admin() {
                Ok(true) => {
                    return Err("已请求管理员权限，请在弹出的确认窗口点击「是」。新窗口启动后再次点击连接即可。".into());
                }
                Ok(false) => {
                    return Err("需要管理员权限才能使用 TUN 模式。已尝试提权但被取消，请手动以管理员身份运行本程序，或改用系统代理模式。".into());
                }
                Err(e) => {
                    return Err(format!("提权重启失败: {e}，请手动以管理员身份运行本程序，或改用系统代理模式。"));
                }
            }
        }
        #[cfg(not(any(target_os = "linux", target_os = "windows")))]
        {
            return Err("TUN 模式当前不支持此平台".into());
        }
    }

    // 2. 确保 wintun.dll（下载/解压可能耗时，放 spawn_blocking）
    let wintun_path = tokio::task::spawn_blocking(crate::tun_mode::ensure_wintun)
        .await
        .map_err(|e| format!("wintun.dll 准备 task panic: {e}"))?
        .map_err(|e| format!("wintun.dll 准备失败: {e}"))?;

    // 3. 从 config 找 profile
    let profile = {
        let cfg = state.config.lock().unwrap();
        cfg.config
            .profiles
            .iter()
            .find(|p| p.id == profile_id)
            .cloned()
            .ok_or_else(|| format!("profile 不存在: {profile_id}"))?
    };

    // 4. 检查是否已有活跃连接
    {
        let session = state.session.lock().unwrap();
        if session.is_some() {
            return Err("已有一个活跃连接".into());
        }
    }

    // 5. 设状态为 Connecting，spawn task
    let cancel = Arc::new(Notify::new());
    {
        let mut session = state.session.lock().unwrap();
        *session = Some(VpnSession {
            cancel: cancel.clone(),
            status: VpnStatus::Connecting,
        });
    }

    // 记下 last_profile_id
    {
        let mut store = state.config.lock().unwrap();
        store.config.last_profile_id = Some(profile.id.clone());
        let _ = store.save();
    }

    let app_clone = app.clone();
    let server = profile.server.clone();
    let username = profile.username.clone();
    let password = profile.password.clone();

    tokio::spawn(async move {
        let app_for_status = app_clone.clone();
        match crate::tun_mode::connect_tun_vpn(
            app_clone.clone(),
            server,
            username,
            password,
            cancel.clone(),
            wintun_path,
        )
        .await
        {
            Ok(()) => {
                log::info!("TUN 连接结束（正常）");
            }
            Err(e) => {
                log::error!("TUN 连接失败: {e}");
            }
        }
        // task 结束后清理会话状态：把 session 置空，
        // 这样 get_status 会返回 Disconnected。
        let st = app_for_status.state::<AppState>();
        let mut session = st.session.lock().unwrap();
        *session = None;
        let _ = app_for_status.emit(
            "vpn:status",
            serde_json::json!({ "state": "disconnected" }),
        );
    });

    Ok(())
}

/// 断开连接：take 出会话并 notify cancel，恢复系统代理。
#[tauri::command]
pub async fn disconnect(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let session = {
        let mut s = state.session.lock().unwrap();
        s.take()
    };
    if let Some(session) = session {
        session.cancel.notify_waiters();
    }

    // 恢复系统代理设置
    let original = {
        let mut orig = state.original_proxy.lock().unwrap();
        orig.take()
    };
    if let Some(orig) = original {
        if let Err(e) = crate::system_proxy::restore(orig) {
            log::warn!("恢复系统代理失败: {e}");
        }
    }

    // 停止 PAC HTTP server
    let pac_handle = {
        let mut pac = state.pac_server.lock().unwrap();
        pac.take()
    };
    if let Some(handle) = pac_handle {
        handle.abort();
    }

    let _ = app.emit(
        "vpn:status",
        serde_json::json!({ "state": "disconnected" }),
    );
    Ok(())
}

/// 获取当前状态。
#[tauri::command]
pub fn get_status(state: State<'_, AppState>) -> VpnStatus {
    let session = state.session.lock().unwrap();
    match &*session {
        Some(s) => s.status.clone(),
        None => VpnStatus::Disconnected,
    }
}

/// 列出所有 profile。
#[tauri::command]
pub fn list_profiles(state: State<'_, AppState>) -> Vec<Profile> {
    state.config.lock().unwrap().config.profiles.clone()
}

/// 新增或更新 profile（空 id 视为新增，否则按 id 更新或追加）。
#[tauri::command]
pub fn save_profile(state: State<'_, AppState>, profile: Profile) -> Result<String, String> {
    let mut store = state.config.lock().unwrap();
    if profile.id.is_empty() {
        // 新增
        let mut new_profile = profile;
        new_profile.id = uuid::Uuid::new_v4().to_string();
        let id = new_profile.id.clone();
        store.config.profiles.push(new_profile);
        store.save().map_err(|e| e.to_string())?;
        Ok(id)
    } else {
        // 更新或追加
        let id = profile.id.clone();
        if let Some(existing) = store.config.profiles.iter_mut().find(|p| p.id == id) {
            *existing = profile;
        } else {
            store.config.profiles.push(profile);
        }
        store.save().map_err(|e| e.to_string())?;
        Ok(id)
    }
}

/// 删除 profile。
#[tauri::command]
pub fn delete_profile(state: State<'_, AppState>, id: String) -> Result<(), String> {
    let mut store = state.config.lock().unwrap();
    store.config.profiles.retain(|p| p.id != id);
    if store.config.last_profile_id.as_deref() == Some(id.as_str()) {
        store.config.last_profile_id = None;
    }
    store.save().map_err(|e| e.to_string())?;
    Ok(())
}

/// 获取设置。
#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> crate::config::Settings {
    state.config.lock().unwrap().config.settings.clone()
}

/// 保存设置。
#[tauri::command]
pub fn save_settings(
    state: State<'_, AppState>,
    settings: crate::config::Settings,
) -> Result<(), String> {
    let mut store = state.config.lock().unwrap();
    store.config.settings = settings;
    store.save().map_err(|e| e.to_string())?;
    Ok(())
}

/// 启用开机自启。
#[tauri::command]
pub fn enable_autostart(app: tauri::AppHandle) -> Result<(), String> {
    use tauri_plugin_autostart::ManagerExt;
    app.autolaunch()
        .enable()
        .map_err(|e| e.to_string())
}

/// 禁用开机自启。
#[tauri::command]
pub fn disable_autostart(app: tauri::AppHandle) -> Result<(), String> {
    use tauri_plugin_autostart::ManagerExt;
    app.autolaunch()
        .disable()
        .map_err(|e| e.to_string())
}

/// 查询开机自启是否启用。
#[tauri::command]
pub fn is_autostart_enabled(app: tauri::AppHandle) -> Result<bool, String> {
    use tauri_plugin_autostart::ManagerExt;
    app.autolaunch()
        .is_enabled()
        .map_err(|e| e.to_string())
}
