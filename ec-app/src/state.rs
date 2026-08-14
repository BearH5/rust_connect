//! 前端可见的 VPN 状态机 + Tauri 全局状态。
//!
//! - `VpnStatus` 是通过 `get_status` command 返回给前端的枚举（serde 序列化）。
//! - `VpnSession` 描述一个活跃会话：cancel 令牌 + 当前状态。
//! - `AppState` 注入到 Tauri 的 `manage()`，由 command 函数共享。

use std::sync::Arc;
use tokio::sync::Notify;

/// 前端可见的 VPN 状态（通过 get_status command 返回）。
///
/// 用 `#[serde(tag = "state")]` 内部标记，序列化后形如：
///   - `{"state":"Disconnected"}`
///   - `{"state":"Connecting"}`
///   - `{"state":"Connected","client_ip":"10.10.0.1","socks_bind":"127.0.0.1:1080"}`
///   - `{"state":"Error","message":"..."}`
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "state")]
pub enum VpnStatus {
    /// 未连接（无会话或会话已结束）。
    Disconnected,
    /// 连接中（登录或建隧道阶段）。
    Connecting,
    /// 已连接。client_ip 是服务端分配的内网 IP，socks_bind 是本地 SOCKS5 监听地址。
    Connected {
        client_ip: String,
        socks_bind: String,
    },
    /// 出错。
    Error { message: String },
}

/// 一个活跃的 VPN 会话。
pub struct VpnSession {
    /// 用于断开时取消代理 task。
    pub cancel: Arc<Notify>,
    /// 当前状态快照。
    pub status: VpnStatus,
}

/// Tauri 全局状态。通过 `tauri::Builder::manage` 注入，command 函数用
/// `tauri::State<'_, AppState>` 拿到共享引用。
pub struct AppState {
    /// 当前活跃会话；None 表示未连接。
    pub session: std::sync::Mutex<Option<VpnSession>>,
    /// JSON 配置持久化层。
    pub config: std::sync::Mutex<crate::config::ConfigStore>,
    /// 启用系统代理前的原始设置，断开时恢复。
    pub original_proxy: std::sync::Mutex<Option<crate::system_proxy::OriginalProxySettings>>,
    /// PAC HTTP server 的 task handle，断开时 abort。
    pub pac_server: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// 提权重启后待自动连接的 profile_id（仅提权实例启动时设置，前端拉取一次后清空）。
    pub pending_auto_connect: std::sync::Mutex<Option<String>>,
}

impl AppState {
    /// 便捷方法：把会话状态更新为 `Connected`。
    /// 仅在代理真正建好隧道后调用（由 vpn task 推送的事件触发）。
    pub fn set_connected(&self, client_ip: String, socks_bind: String) {
        let mut session = self.session.lock().unwrap();
        if let Some(s) = session.as_mut() {
            s.status = VpnStatus::Connected {
                client_ip,
                socks_bind,
            };
        }
    }

    /// 便捷方法：把会话状态更新为 `Error`。
    pub fn set_error(&self, message: String) {
        let mut session = self.session.lock().unwrap();
        if let Some(s) = session.as_mut() {
            s.status = VpnStatus::Error { message };
        }
    }
}
