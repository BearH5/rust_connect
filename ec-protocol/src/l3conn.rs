//! L3Conn：收发双连接封装（对照 l3conn.go）。
//!
//! 封装 send_conn（只写）+ recv_conn（只读），实现 Read/Write。
//! 读写出错时自动重连（重新握手），最多重试 5 次（对照 l3conn.go 的 sendErrCount/recvErrCount）。
//!
//! 与 Go 版的区别：Go 版重连用 `easyConnectClient.RecvConn()`（依赖外部 Client 引用）；
//! Rust 侧重连只需 server/token/ip_reverse（都是 Copy/Clone），存在 L3Conn 内部即可。

use std::io::{self, Read, Write};

use ec_utls::UtlsConn;

use crate::token::TOKEN_LEN;
use crate::tunnel;
use crate::TunnelError;

/// 最大重连次数（对照 l3conn.go 的 sendErrCount/recvErrCount < 5）。
const MAX_RETRY: u32 = 5;

/// 收发双连接。send 只写，recv 只读，各自独立重连。
pub struct L3Conn {
    server: String,
    token: [u8; TOKEN_LEN],
    ip_reverse: [u8; 4],
    send: UtlsConn,
    recv: UtlsConn,
    send_err_count: u32,
    recv_err_count: u32,
}

impl L3Conn {
    /// 建立收发双连接。对照 l3conn.go:75 NewL3Conn。
    ///
    /// 先建 send_conn，再建 recv_conn（顺序与 Go 版一致）。
    pub fn new(
        server: &str,
        token: &[u8; TOKEN_LEN],
        ip_reverse: &[u8; 4],
    ) -> Result<Self, TunnelError> {
        let send = tunnel::send_conn(server, token, ip_reverse)?;
        let recv = tunnel::recv_conn(server, token, ip_reverse)?;
        Ok(Self {
            server: server.to_string(),
            token: *token,
            ip_reverse: *ip_reverse,
            send,
            recv,
            send_err_count: 0,
            recv_err_count: 0,
        })
    }

    /// 重连 send 连接（出错时调用）。对照 l3conn.go:48-56。
    fn reconnect_send(&mut self) -> io::Result<()> {
        // 旧连接 drop 时自动关闭（UtlsConn::Drop 调 ec_conn_close）。
        let new_send = tunnel::send_conn(&self.server, &self.token, &self.ip_reverse)
            .map_err(tunnel_err_to_io)?;
        self.send = new_send;
        Ok(())
    }

    /// 重连 recv 连接（出错时调用）。对照 l3conn.go:31-40。
    fn reconnect_recv(&mut self) -> io::Result<()> {
        let new_recv = tunnel::recv_conn(&self.server, &self.token, &self.ip_reverse)
            .map_err(tunnel_err_to_io)?;
        self.recv = new_recv;
        Ok(())
    }
}

impl Read for L3Conn {
    /// 尽力读。对照 l3conn.go:23-42。
    /// 读取出错时重连 recv_conn，最多重试 MAX_RETRY 次。
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            match self.recv.read(buf) {
                Ok(n) => return Ok(n),
                Err(e) => {
                    if self.recv_err_count >= MAX_RETRY {
                        return Err(io::Error::new(
                            e.kind(),
                            format!("recv 重连已达上限({MAX_RETRY}): {e}"),
                        ));
                    }
                    eprintln!("[l3conn] recv 读取出错，重连 ({e})");
                    self.reconnect_recv()?;
                    self.recv_err_count += 1;
                }
            }
        }
    }
}

impl Write for L3Conn {
    /// 尽力写。对照 l3conn.go:45-63。
    /// 写入出错时重连 send_conn，最多重试 MAX_RETRY 次。
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        loop {
            match self.send.write(buf) {
                Ok(n) => return Ok(n),
                Err(e) => {
                    if self.send_err_count >= MAX_RETRY {
                        return Err(io::Error::new(
                            e.kind(),
                            format!("send 重连已达上限({MAX_RETRY}): {e}"),
                        ));
                    }
                    eprintln!("[l3conn] send 写入出错，重连 ({e})");
                    self.reconnect_send()?;
                    self.send_err_count += 1;
                }
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        self.send.flush()
    }
}

/// 把 TunnelError 转成 io::Error（用于 Read/Write 的错误传播）。
fn tunnel_err_to_io(e: TunnelError) -> io::Error {
    io::Error::new(io::ErrorKind::Other, e.to_string())
}

/// 字节流读写抽象，供上层（ec-proxy）解耦 L3Conn 具体类型。
/// L3Conn 已实现 Read/Write，自动满足此 trait。
pub trait L3ConnLike: std::io::Read + std::io::Write + Send {
    // marker trait，无额外方法
}

impl L3ConnLike for L3Conn {}

/// L3Conn 的只读半（recv_conn）。可独立 send 到另一个线程。
///
/// 设计原因：device.rs 的桥接需要读、写两个线程并行（读阻塞在 recv 上时，
/// 写线程仍能把出站包发给 send_conn）。L3Conn 单对象无法 &mut 并行，
/// 故拆成两半，各自持有重连所需信息。
pub struct L3ReadHalf {
    server: String,
    token: [u8; TOKEN_LEN],
    ip_reverse: [u8; 4],
    recv: UtlsConn,
    err_count: u32,
}

impl Read for L3ReadHalf {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            match self.recv.read(buf) {
                Ok(n) => return Ok(n),
                Err(e) => {
                    if self.err_count >= MAX_RETRY {
                        return Err(io::Error::new(
                            e.kind(),
                            format!("recv 重连已达上限({MAX_RETRY}): {e}"),
                        ));
                    }
                    eprintln!("[l3conn] recv-half 读取出错，重连 ({e})");
                    self.recv = tunnel::recv_conn(&self.server, &self.token, &self.ip_reverse)
                        .map_err(tunnel_err_to_io)?;
                    self.err_count += 1;
                }
            }
        }
    }
}

/// L3Conn 的只写半（send_conn）。可独立 send 到另一个线程。
pub struct L3WriteHalf {
    server: String,
    token: [u8; TOKEN_LEN],
    ip_reverse: [u8; 4],
    send: UtlsConn,
    err_count: u32,
}

impl Write for L3WriteHalf {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        loop {
            match self.send.write(buf) {
                Ok(n) => return Ok(n),
                Err(e) => {
                    if self.err_count >= MAX_RETRY {
                        return Err(io::Error::new(
                            e.kind(),
                            format!("send 重连已达上限({MAX_RETRY}): {e}"),
                        ));
                    }
                    eprintln!("[l3conn] send-half 写入出错，重连 ({e})");
                    self.send = tunnel::send_conn(&self.server, &self.token, &self.ip_reverse)
                        .map_err(tunnel_err_to_io)?;
                    self.err_count += 1;
                }
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        self.send.flush()
    }
}

impl L3Conn {
    /// 拆成读写两半，各自可独立 send 到线程。
    ///
    /// 消费 L3Conn（拆分后原对象不再可用）。
    pub fn split(self) -> (L3ReadHalf, L3WriteHalf) {
        (
            L3ReadHalf {
                server: self.server.clone(),
                token: self.token,
                ip_reverse: self.ip_reverse,
                recv: self.recv,
                err_count: self.recv_err_count,
            },
            L3WriteHalf {
                server: self.server,
                token: self.token,
                ip_reverse: self.ip_reverse,
                send: self.send,
                err_count: self.send_err_count,
            },
        )
    }
}
