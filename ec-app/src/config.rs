//! JSON 配置持久化：profiles + 全局设置。
//!
//! 配置文件路径：`<config_dir>/rust_connect/config.json`，
//! 其中 `config_dir` 由 `dirs::config_dir()` 决定
//!（Windows: `%APPDATA%`，Linux: `~/.config`，macOS: `~/Library/Application Support`）。
//! 不存在时返回默认空配置，并在首次 `save()` 时创建目录。

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// 一个 VPN 连接配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub id: String,
    pub name: String,
    /// 形如 "rvpn.zju.edu.cn:443"，无协议前缀。
    pub server: String,
    pub username: String,
    pub password: String,
    pub socks_port: u16,
}

impl Profile {
    /// 新建 profile（自动生成 uuid v4 作为 id）。
    #[allow(dead_code)]
    pub fn new(
        name: String,
        server: String,
        username: String,
        password: String,
        socks_port: u16,
    ) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name,
            server,
            username,
            password,
            socks_port,
        }
    }

    /// 默认 SOCKS5 端口。
    pub const DEFAULT_SOCKS_PORT: u16 = 1080;
}

impl Default for Profile {
    /// 反序列化时缺字段的兜底，也是手动构造的合理默认。
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            server: String::new(),
            username: String::new(),
            password: String::new(),
            socks_port: Self::DEFAULT_SOCKS_PORT,
        }
    }
}

/// 全局设置。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Settings {
    pub auto_reconnect: bool,
    pub auto_start: bool,
    pub minimize_to_tray: bool,
    /// 代理模式："pac"（系统代理，默认）或 "tun"（TUN 全局代理，需管理员）。
    #[serde(default = "default_proxy_mode")]
    pub proxy_mode: String,
}

/// proxy_mode 默认 "pac"。
fn default_proxy_mode() -> String {
    "pac".to_string()
}

/// 顶层配置：profiles 列表 + 上次使用的 profile + 设置。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AppConfig {
    pub profiles: Vec<Profile>,
    pub last_profile_id: Option<String>,
    pub settings: Settings,
}

/// 配置存储：内存中的 `AppConfig` + 磁盘路径。
pub struct ConfigStore {
    pub config: AppConfig,
    pub path: PathBuf,
}

impl ConfigStore {
    /// 加载配置。路径：`<config_dir>/rust_connect/config.json`。
    /// 文件不存在或解析失败则返回默认空配置（并记录路径供后续 save）。
    pub fn load() -> Self {
        let path = Self::config_path();
        let config = if path.exists() {
            std::fs::read_to_string(&path)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default()
        } else {
            AppConfig::default()
        };
        ConfigStore { config, path }
    }

    /// 把当前配置写回磁盘（先建目录再写）。
    pub fn save(&self) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(&self.config)?;
        std::fs::write(&self.path, json)?;
        Ok(())
    }

    /// 计算配置文件路径。
    ///
    /// 通过 `dirs::config_dir()` 获取跨平台配置目录：
    /// - Windows: `%APPDATA%` (C:\Users\<user>\AppData\Roaming)
    /// - Linux: `~/.config`
    /// - macOS: `~/Library/Application Support`
    /// 兜底: 当前目录。
    fn config_path() -> PathBuf {
        let base =
            dirs::config_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
        base.join("rust_connect").join("config.json")
    }
}
