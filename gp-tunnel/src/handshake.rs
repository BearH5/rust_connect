//! GPST 隧道握手：TLS 连接 + GET + START_TUNNEL。
//!
//! 对照 openconnect gpst.c 第 719-836 行（gpst_connect）。
//!
//! 流程：
//!   1. TCP 连接 gateway
//!   2. TLS 握手（标准 TLS，无指纹要求）
//!   3. 发 `GET <tunnel_url>?user=<user>&authcookie=<authcookie> HTTP/1.1\r\n\r\n`
//!   4. 读 12 字节，== "START_TUNNEL" 即成功
//!   5. 该 TLS 连接后续直接当二进制流读写帧

use crate::error::GpTunnelError;
use crate::frame::START_TUNNEL;
use native_tls::TlsStream;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

/// 握手成功后的 TLS 流（后续读写 GPST 帧）。
pub type GpTlsStream = TlsStream<TcpStream>;

/// 从 cookie 字符串中提取 user 和 authcookie 字段值。
///
/// cookie 形如 `authcookie=xxx&user=yyy&computer=zzz`。
/// 对照 gpst.c 第 742-744 行 filter_opts(..., "user,authcookie", 1)。
pub fn extract_user_authcookie(cookie: &str) -> Result<(String, String), GpTunnelError> {
    let mut user = None;
    let mut authcookie = None;
    for pair in cookie.split('&') {
        let mut it = pair.splitn(2, '=');
        let key = it.next().unwrap_or("");
        let val = it.next().unwrap_or("");
        match key {
            "user" => user = Some(urlencoding::decode(val).map(|s| s.into_owned()).unwrap_or_default()),
            "authcookie" => authcookie = Some(urlencoding::decode(val).map(|s| s.into_owned()).unwrap_or_default()),
            _ => {}
        }
    }
    let user = user.ok_or_else(|| GpTunnelError::Other("cookie 缺少 user 字段".into()))?;
    let authcookie = authcookie.ok_or_else(|| GpTunnelError::Other("cookie 缺少 authcookie 字段".into()))?;
    Ok((user, authcookie))
}

/// 建立 TLS 连接并发送 GET 握手，返回成功后的 TLS 流。
///
/// 对照 gpst.c 第 742-798 行。
///
/// - `gateway_host`：如 "114.250.31.2"
/// - `gateway_port`：如 4430
/// - `tunnel_url`：如 "/ssl-tunnel-connect.sslvpn"
/// - `cookie`：gp-login 拼的完整 cookie
pub fn connect_tls_and_handshake(
    gateway_host: &str,
    gateway_port: u16,
    tunnel_url: &str,
    cookie: &str,
) -> Result<GpTlsStream, GpTunnelError> {
    // 1. TCP 连接
    let addr = format!("{gateway_host}:{gateway_port}");
    log::info!("[gp-tunnel] TCP 连接 -> {addr}");
    let tcp = TcpStream::connect(&addr)?;
    tcp.set_read_timeout(Some(Duration::from_secs(30)))?;
    tcp.set_write_timeout(Some(Duration::from_secs(30)))?;

    // 2. TLS 握手（接受任意证书，GP 自签名常见）
    log::info!("[gp-tunnel] TLS 握手 -> {gateway_host}");
    let tls_connector = native_tls::TlsConnector::builder()
        .danger_accept_invalid_certs(true)
        .danger_accept_invalid_hostnames(true)
        .build()?;
    let mut tls = tls_connector.connect(gateway_host, tcp)?;

    // 3. 从 cookie 提取 user 和 authcookie
    let (user, authcookie) = extract_user_authcookie(cookie)?;

    // 4. 发 GET 请求（无 header，认证全在 query string）
    //    对照 gpst.c 第 742-744 行
    let request = format!(
        "GET {tunnel_url}?user={user}&authcookie={authcookie} HTTP/1.1\r\n\r\n",
        user = urlencoding::encode(&user),
        authcookie = urlencoding::encode(&authcookie),
    );
    log::info!("[gp-tunnel] 发送 GET 握手 (path={tunnel_url}, user={user})");
    tls.write_all(request.as_bytes())?;
    tls.flush()?;

    // 5. 读 12 字节，判断 START_TUNNEL
    //    对照 gpst.c 第 755-785 行
    let mut buf = [0u8; 256];
    log::debug!("[gp-tunnel] 读取握手响应（期望 START_TUNNEL）...");
    let n = tls.read(&mut buf)?;
    if n == 0 {
        return Err(GpTunnelError::GatewayDisconnected);
    }
    if n >= START_TUNNEL.len() && &buf[..START_TUNNEL.len()] == START_TUNNEL {
        log::info!("[gp-tunnel] 握手成功（收到 START_TUNNEL）");
        // 移除读超时，后续 IO 线程会用自己的超时
        tls.get_ref().set_read_timeout(None).ok();
        tls.get_ref().set_write_timeout(None).ok();
        return Ok(tls);
    }

    // 不是 START_TUNNEL，尝试解析 HTTP 状态码（gpst.c 第 776-785 行）
    let resp_text = String::from_utf8_lossy(&buf[..n]);
    log::error!("[gp-tunnel] 握手失败，响应: {resp_text}");
    // HTTP 502 -> cookie 被拒（gpst.c 第 782 行）
    if resp_text.contains("502") {
        return Err(GpTunnelError::CookieRejected);
    }
    Err(GpTunnelError::HandshakeFailed(resp_text.into()))
}
