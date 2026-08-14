#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod config;
mod elevation;
mod gp_mode;
mod silent_console;
mod state;
mod system_proxy;
mod tun_mode;
mod vpn;

use config::ConfigStore;
use state::AppState;
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Manager, WindowEvent,
};

fn main() {
    // 分配并隐藏控制台：让 wintun-bindings 等库派生的 netsh/net 等控制台
    // 子进程继承隐藏控制台，不弹出可见黑窗。必须在任何子进程创建前调用。
    silent_console::hide_console();

    // 提权实例由 UAC 启动，带 `--relaunched-as-admin {token}` 参数。
    // 1) signal 命名事件通知原进程退出（token 即事件名后缀）。
    // 2) 跳过 single-instance 插件，否则被判定为"第二实例"自动退出。
    let args: Vec<String> = std::env::args().collect();
    let admin_flag_idx = args.iter().position(|a| a == "--relaunched-as-admin");
    if let Some(idx) = admin_flag_idx {
        if let Some(token) = args.get(idx + 1) {
            crate::elevation::signal_handshake(token);
        }
    }
    let relaunched_as_admin = admin_flag_idx.is_some();

    let mut builder = tauri::Builder::default();
    if !relaunched_as_admin {
        // 单实例：第二个实例启动时，激活已有窗口并退出新实例。
        // VPN 软件多开会导致会话冲突、路由重复等问题，必须禁止。
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.show();
                let _ = window.set_focus();
            }
        }));
    }
    builder
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--autostart"]),
        ))
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .manage(AppState {
            session: std::sync::Mutex::new(None),
            config: std::sync::Mutex::new(ConfigStore::load()),
            original_proxy: std::sync::Mutex::new(None),
            pac_server: std::sync::Mutex::new(None),
            pending_auto_connect: std::sync::Mutex::new(None),
        })
        .invoke_handler(tauri::generate_handler![
            commands::connect,
            commands::connect_tun,
            commands::disconnect,
            commands::get_status,
            commands::get_pending_auto_connect,
            commands::list_profiles,
            commands::save_profile,
            commands::delete_profile,
            commands::get_settings,
            commands::save_settings,
            commands::enable_autostart,
            commands::disable_autostart,
            commands::is_autostart_enabled,
        ])
        .setup(|app| {
            // 提权实例：从 last_profile_id 读待连接 profile，前端拉取后自动连接。
            // 普通启动时跳过。这里重新检测参数，避免闭包捕获 main 局部变量。
            let is_relaunched = std::env::args().any(|a| a == "--relaunched-as-admin");
            if is_relaunched {
                if let Some(state) = app.try_state::<AppState>() {
                    let pid = state.config.lock().unwrap().config.last_profile_id.clone();
                    if let Some(pid) = pid {
                        *state.pending_auto_connect.lock().unwrap() = Some(pid);
                    }
                }
            }

            // 系统托盘菜单：显示窗口 / 断开 / 退出
            let show = MenuItem::with_id(app, "show", "显示窗口", true, None::<&str>)?;
            let disconnect_item =
                MenuItem::with_id(app, "disconnect", "断开", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &disconnect_item, &quit])?;

            let _tray = TrayIconBuilder::with_id("main")
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("RustConnect - 未连接")
                .menu(&menu)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => {
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                    "disconnect" => {
                        // 复用 disconnect 逻辑：通知 cancel + 恢复系统代理
                        if let Some(state) = app.try_state::<AppState>() {
                            let session = {
                                let mut s = state.session.lock().unwrap();
                                s.take()
                            };
                            if let Some(session) = session {
                                session.cancel.notify_waiters();
                            }
                            // 恢复系统代理
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
                        }
                    }
                    "quit" => {
                        app.exit(0);
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    // 双击托盘图标显示窗口
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        let app = tray.app_handle();
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                })
                .build(app)?;

            Ok(())
        })
        .on_window_event(|window, event| {
            // 关闭窗口时最小化到托盘（而非退出），让 VPN 后台运行
            if let WindowEvent::CloseRequested { api, .. } = event {
                // 只在用户主动关闭时拦截（settings 的 minimize_to_tray 控制是否启用）
                let should_minimize = {
                    if let Some(state) = window.app_handle().try_state::<AppState>() {
                        state.config.lock().unwrap().config.settings.minimize_to_tray
                    } else {
                        true // 默认最小化
                    }
                };
                if should_minimize {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
