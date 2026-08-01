//! SOCKS5 server（用 fast-socks5 库处理协议，自定义拨号走 NetStack）。
//!
//! fast-socks5 负责：握手、方法协商、命令解析、错误回复。
//! 我们负责：CONNECT 时用 NetStack::dial_tcp 出站拨号（走 VPN 隧道）。
//!
//! 关键 API 适配（fast-socks5 0.10，已核对源码 server.rs）：
//! - 没有 `Socks5Server::listener_from_tokio()`，也没有 `Socks5Socket::get_mut()`。
//!   改用低层模式（对照 fast-socks5 的 examples/simple_tcp_server.rs）：
//!     自己 accept TcpStream → `Socks5Socket::new(stream, config.clone())`
//!     → `upgrade_to_socks5()`（消费 self，返回握手后的 socket）
//!     → `into_inner()` 取出内部 TcpStream 做双向转发。
//! - 默认 `Config::<DenyAuthentication>::default()` 在 `auth: None` 时会走 no-auth
//!   分支（can_accept_method 选 SOCKS5_AUTH_METHOD_NONE），所以无需额外认证配置
//!   即可接受无认证客户端。
//! - `set_execute_command(false)` 关掉内置系统 TCP 拨号；
//!   `set_dns_resolve(false)` 关掉内置 DNS（我们只处理 IP 目标）。
//! - 升级握手成功后必须手动回复成功响应 `[05 00 00 01 00 00 00 00 00 00]`
//!   （因为 execute_command 关了，fast-socks5 不会替我们写）。

use std::net::SocketAddr;
use std::sync::Arc;

use fast_socks5::server::{Config, Socks5Socket};
use fast_socks5::util::target_addr::TargetAddr;
use fast_socks5::Socks5Command;
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};

use crate::netstack::NetStackHandle;

/// 启动 SOCKS5 server。
///
/// `listener` 由上层（proxy.rs）绑定后传入。每个客户端连接 spawn 一个 task 处理。
pub async fn serve_socks5(
    listener: TcpListener,
    handle: NetStackHandle,
) -> std::io::Result<()> {
    // 配置：关掉内置拨号和 DNS（我们接管出站走 NetStack）。
    let mut config = Config::default();
    config.set_execute_command(false);
    config.set_dns_resolve(false);
    let config = Arc::new(config);

    log::info!("[socks5] 监听 {}", listener.local_addr()?);

    loop {
        match listener.accept().await {
            Ok((stream, peer)) => {
                log::debug!("[socks5] 客户端连入: {peer}");
                let handle = handle.clone();
                let config = Arc::clone(&config);
                tokio::spawn(async move {
                    if let Err(e) = handle_connection(stream, config, &handle).await {
                        log::debug!("[socks5] 连接处理失败: {e}");
                    }
                });
            }
            Err(e) => log::warn!("[socks5] accept 失败: {e}"),
        }
    }
}

/// 处理单个客户端连接：握手 → 解析命令 → 经 NetStack 出站 → 双向转发。
async fn handle_connection(
    stream: TcpStream,
    config: Arc<Config>,
    handle: &NetStackHandle,
) -> std::io::Result<()> {
    // 把裸 TcpStream 包成 Socks5Socket，完成 SOCKS5 握手与命令解析。
    // upgrade_to_socks5 消费 self 并返回握手后的 socket。
    let sock = Socks5Socket::new(stream, config);
    let sock = sock
        .upgrade_to_socks5()
        .await
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;

    // 取命令与目标地址。注意：Socks5Command 未实现 Clone，所以直接用引用比对。
    if sock.cmd().as_ref() != Some(&Socks5Command::TCPConnect) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "仅支持 CONNECT",
        ));
    }

    let target_addr = sock
        .target_addr()
        .cloned()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "无目标地址"))?;

    let target: SocketAddr = match target_addr {
        TargetAddr::Ip(addr) => addr,
        TargetAddr::Domain(domain, port) => {
            // DNS 中转：用系统本地 DNS 解析域名，再通过 VPN 隧道连接解析到的 IP。
            // 注：这台 VPN 服务器未提供远程 DNS（rclist.csp 的 dnsserver=0.0.0.0），
            // 所以用本地解析。若本地 DNS 解析不了内网域名，会报错。
            log::info!("[socks5] DNS 解析 {domain}:{port}");
            let host_port = format!("{domain}:{port}");
            let mut addrs = tokio::net::lookup_host(&host_port)
                .await
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, format!("DNS 解析失败: {e}")))?;
            addrs
                .next()
                .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, format!("DNS 无结果: {domain}")))?
        }
    };

    log::info!("[socks5] CONNECT {target}");

    // 经 NetStack（VPN 隧道）出站拨号。
    let mut remote = handle.dial_tcp(target).await?;

    // 升级后 execute_command=false，fast-socks5 不会写成功响应，这里手动写：
    //   VER=05 REP=00 RSV=00 ATYP=01(IPv4) BND.ADDR=0.0.0.0 BND.PORT=0
    let mut client = sock.into_inner();
    client
        .write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
        .await?;

    // 双向转发。
    tokio::io::copy_bidirectional(&mut client, &mut remote).await?;
    Ok(())
}
