#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod config;
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
    tauri::Builder::default()
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--autostart"]),
        ))
        .manage(AppState {
            session: std::sync::Mutex::new(None),
            config: std::sync::Mutex::new(ConfigStore::load()),
            original_proxy: std::sync::Mutex::new(None),
            pac_server: std::sync::Mutex::new(None),
        })
        .invoke_handler(tauri::generate_handler![
            commands::connect,
            commands::connect_tun,
            commands::disconnect,
            commands::get_status,
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
