//! 协议层错误类型。

use std::io;

/// 协议层统一错误。
#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    /// TLS 连接/握手失败（来自 ec-utls）。
    #[error("TLS 错误: {0}")]
    Tls(#[from] ec_utls::UtlsError),

    /// I/O 错误。
    #[error("I/O 错误: {0}")]
    Io(#[from] io::Error),

    /// token 构造或 request_token 失败。
    #[error("token 错误: {0}")]
    Token(String),

    /// RequestIP 失败（拿到 IP 这一步）。
    #[error("RequestIP 错误: {0}")]
    RequestIp(String),
}

/// 隧道握手（RecvConn/SendConn）错误。
///
/// 对照 protocol.go 的 Sangfor 命令码分派。
#[derive(Debug, thiserror::Error)]
pub enum TunnelError {
    /// TLS 连接/握手失败（来自 ec-utls）。
    #[error("TLS 错误: {0}")]
    Tls(#[from] ec_utls::UtlsError),

    /// I/O 错误。
    #[error("I/O 错误: {0}")]
    Io(#[from] io::Error),

    /// 响应首字节不符合预期。
    #[error("意外的握手响应: 0x{0:02x}")]
    UnexpectedReply(u8),

    /// 服务端 SHUTDOWN（cmd 0x08）—— 会话被永久终止，需全新重登录。
    /// 对照 protocol.go:23 ErrSangforShutdown。
    #[error("服务端 SHUTDOWN (cmd 0x08)，需全新重登录")]
    Shutdown,

    /// 服务端 RECONNECTLATER（cmd 0x05/06/07/09）—— 忙碌/冲突，应 sleep 后重试。
    /// 对照 protocol.go:30 ErrSangforReconnectLater。
    #[error("服务端 RECONNECT_LATER (cmd 0x{0:02x})，应重试")]
    ReconnectLater(u8),
}
