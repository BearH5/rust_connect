//! Proxy 顶层：组装 L3Conn + NetStack + SOCKS5。
//!
//! 完整流程：token → RequestIP → L3Conn → NetStack(device) → SOCKS5。
//!
//! 对照 zju-connect 的 main：把隧道层（ec-protocol）+ 用户态 TCP/IP 栈
//! （device/netstack/stream）+ SOCKS5 server（socks5）串成一条端到端链路。

use std::sync::Arc;
use tokio::sync::Notify;

use ec_protocol::{l3conn::L3Conn, token, tunnel};

use crate::device::L3ConnDevice;
use crate::netstack::NetStack;
use crate::socks5;

/// 代理运行配置。
pub struct ProxyConfig {
    /// 形如 "host:port" 的 EasyConnect 服务器地址。
    pub server: String,
    /// 登录拿到的 TwfID。
    pub twf_id: String,
    /// 本地 SOCKS5 监听地址，如 "127.0.0.1:1080"。
    pub socks_bind: String,
}

/// 启动代理：建立隧道 → 起用户态 TCP/IP 栈 → 起 SOCKS5 server。
pub async fn run(cfg: ProxyConfig) -> std::io::Result<()> {
    // 1. request_token：拿 session_id（hex），用于构造 48 字节 token。
    let sid_hex = token::request_token(&cfg.server, &cfg.twf_id)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;

    // 2. build_token：拼出 48 字节 token（session_id 前缀 + twfID）。
    let tkn = token::build_token(&sid_hex, &cfg.twf_id)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;

    // 3. request_ip：拿分配的客户端 IP。
    //    重要：这条连接**不能关闭**（request.go:647 "Request IP conn CAN NOT be closed,
    //    otherwise tx/rx handshake will fail"）。它维持服务端对这个会话的注册，
    //    关掉后服务端不再处理本会话的 IP 包（表现为 SYN 发出但永远收不到 SYN-ACK）。
    //    所以必须保活到代理结束。
    let ((ip, ip_reverse), keepalive_conn) = tunnel::request_ip(&cfg.server, &tkn)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;
    log::info!(
        "client IP: {}.{}.{}.{}",
        ip[0],
        ip[1],
        ip[2],
        ip[3]
    );

    // 4. L3Conn：建立收发双连接隧道（send 只写 / recv 只读，出错自动重连）。
    let l3 = L3Conn::new(&cfg.server, &tkn, &ip_reverse)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;

    // 5. NetStack：spawn 桥接线程把 L3Conn 包成 smoltcp Device，再起 poll 循环。
    let (device, _bridge) = L3ConnDevice::spawn(l3, 1400);
    let (_stack, handle) = NetStack::new(device, ip)?;

    // 6. SOCKS5：监听并转发。
    let listener = tokio::net::TcpListener::bind(&cfg.socks_bind).await?;
    log::info!("SOCKS5 监听 {}", cfg.socks_bind);
    let result = socks5::serve_socks5(listener, handle).await;

    // 代理结束时才释放保活连接（实际进程通常直接退出，这里 forget 更省事）。
    // 用 forget 防止它提前 drop 关闭连接。
    std::mem::forget(keepalive_conn);
    result
}

/// 用已有的 twfID 建隧道（跳过登录）。供 ec-app 调用。
///
/// 流程：request_token -> build_token -> request_ip -> L3Conn -> NetStack -> SOCKS5。
/// 通过 cancel Notify 实现可取消（ec-app 断开时 notify）。
pub async fn run_with_twfid(
    server: &str,
    twf_id: &str,
    socks_bind: String,
    cancel: Arc<Notify>,
) -> std::io::Result<()> {
    let sid_hex = token::request_token(server, twf_id)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;
    let tkn = token::build_token(&sid_hex, twf_id)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;

    let ((ip, ip_reverse), keepalive_conn) = tunnel::request_ip(server, &tkn)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;
    log::info!("client IP: {}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3]);

    let l3 = L3Conn::new(server, &tkn, &ip_reverse)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;

    let (device, _bridge) = L3ConnDevice::spawn(l3, 1400);
    let (_stack, handle) = NetStack::new(device, ip)?;

    let listener = tokio::net::TcpListener::bind(&socks_bind).await?;
    log::info!("SOCKS5 监听 {}", socks_bind);

    // 可取消的 serve：select 监听 cancel
    let serve = socks5::serve_socks5(listener, handle);
    tokio::select! {
        res = serve => {
            res?;
        }
        _ = cancel.notified() => {
            log::info!("收到 cancel，停止 SOCKS5");
        }
    }
    std::mem::forget(keepalive_conn);
    Ok(())
}
