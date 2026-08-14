//! GlobalProtect GPST 隧道协议实现（纯 Rust，不依赖 openconnect）。
//!
//! 参考 openconnect gpst.c 的字节级实现：
//!   - 标准 TLS（无指纹要求）
//!   - 16 字节帧头（magic/ethertype/len/one/zero，字节序混合）
//!   - GET 握手读 "START_TUNNEL"
//!   - getconfig.esp 拉配置
//!   - DPD/keepalive 10 秒周期
//!
//! 用法：
//! ```ignore
//! let (config, tunnel) = gp_tunnel::connect(gateway, cookie)?;
//! // tunnel.send(ip_packet) 发包，tunnel.recv() 收包
//! ```

pub mod config;
pub mod error;
pub mod frame;
pub mod handshake;
pub mod tunnel;

pub use config::GpTunnelConfig;
pub use error::GpTunnelError;
pub use tunnel::{connect, GpTunnel, GpTunnelGuard, GpTunnelReader, GpTunnelWriter};

/// 日志截断（避免超长 XML 刷屏）。
pub(crate) fn truncate_for_log(s: &str) -> String {
    const MAX: usize = 500;
    if s.len() <= MAX {
        s.to_string()
    } else {
        format!("{}...(共{}字节)", &s[..MAX], s.len())
    }
}
