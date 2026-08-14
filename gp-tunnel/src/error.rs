//! GPST 隧道协议错误类型。

use thiserror::Error;

#[derive(Debug, Error)]
pub enum GpTunnelError {
    #[error("HTTP 请求失败: {0}")]
    Http(#[from] reqwest::Error),

    #[error("TLS 错误: {0}")]
    Tls(#[from] native_tls::Error),

    #[error("TLS 握手失败: {0}")]
    TlsHandshake(#[from] native_tls::HandshakeError<std::net::TcpStream>),

    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),

    #[error("XML 解析失败: {0}")]
    Parse(String),

    #[error("getconfig 失败: {0}")]
    GetConfig(String),

    #[error("隧道握手失败: 期望 START_TUNNEL，实际收到: {0}")]
    HandshakeFailed(String),

    #[error("网关立即断开连接")]
    GatewayDisconnected,

    #[error("cookie 不被网关接受（HTTP 502）")]
    CookieRejected,

    #[error("帧格式错误: {0}")]
    Frame(String),

    #[error("隧道已关闭")]
    Closed,

    #[error("DPD 超时：服务端无响应")]
    DpdTimeout,

    #[error("其他错误: {0}")]
    Other(String),
}
