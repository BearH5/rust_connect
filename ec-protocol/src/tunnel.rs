//! 隧道握手：RequestIP / RecvConn / SendConn。
//!
//! 对照 zju-connect/client/easyconnect：
//! - request.go:607-651（requestIP）
//! - protocol.go:81-117（RecvConn）
//! - protocol.go:135-183（SendConn）
//!
//! 三条连接都用**特殊 TLS**（L3IP/RC4/伪扩展），握手包格式统一：
//!   [cmd:4][token:48][0x00*8][ip_reverse:4] = 64 字节
//! 仅 cmd 首字节不同，期望的响应首字节也不同。

use std::io::{Read, Write};

use ec_utls::{TlsMode, UtlsConn};

use crate::token::TOKEN_LEN;
use crate::TunnelError;

/// 握手包固定长度。
pub const HANDSHAKE_LEN: usize = 4 + TOKEN_LEN + 8 + 4; // = 64

/// 构造统一的 64 字节握手包。
///
/// 对照 tunnel-port-reference.md §3.1：
///   [0..4]   cmd（4 字节命令码）
///   [4..52]  token（48 字节）
///   [52..60] 0x00 * 8（保留）
///   [60..64] ip_reverse（反序的客户端 IP）
fn build_handshake_packet(
    cmd: [u8; 4],
    token: &[u8; TOKEN_LEN],
    ip_reverse: &[u8; 4],
) -> [u8; HANDSHAKE_LEN] {
    let mut msg = [0u8; HANDSHAKE_LEN];
    msg[0..4].copy_from_slice(&cmd);
    msg[4..52].copy_from_slice(token);
    // [52..60] 已是 0
    msg[60..64].copy_from_slice(ip_reverse);
    msg
}

/// 读响应并检查首字节是否符合预期。
///
/// 返回读取到的字节数和完整响应缓冲（截断到实际读取长度）。
fn read_reply(conn: &mut UtlsConn) -> Result<(usize, Vec<u8>), TunnelError> {
    let mut reply = vec![0u8; 0x80]; // request.go:626 make([]byte, 0x80)
    let n = conn.read(&mut reply)?;
    if n == 0 {
        return Err(TunnelError::Io(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "握手响应 0 字节",
        )));
    }
    reply.truncate(n);
    Ok((n, reply))
}

/// RequestIP：用特殊 TLS 连接请求客户端 IP。
///
/// 对照 request.go:607-651。
///
/// 发送 cmd=0x00000000 的握手包，读响应：
/// - reply[0] == 0x00 → 成功
/// - reply[4..8] → 分配的客户端 IP（4 字节，大端）
/// - ip_reverse = [ip[3], ip[2], ip[1], ip[0]]
///
/// **返回的连接不能关闭**（request.go:647），否则后续 tx/rx 握手失败。
/// 调用方必须持有它直到会话结束。
pub fn request_ip(
    server: &str,
    token: &[u8; TOKEN_LEN],
) -> Result<(([u8; 4], [u8; 4]), UtlsConn), TunnelError> {
    let mut conn = UtlsConn::connect(server, TlsMode::Special)?;

    // request.go:614 RequestIP 包：cmd=0，ip_reverse 位先填 0xff*4（请求任意 IP）。
    let msg = build_handshake_packet([0x00, 0x00, 0x00, 0x00], token, &[0xff, 0xff, 0xff, 0xff]);
    conn.write_all(&msg)?;

    let (n, reply) = read_reply(&mut conn)?;

    // request.go:635 reply[0] != 0x00 → 失败
    if reply[0] != 0x00 {
        return Err(TunnelError::UnexpectedReply(reply[0]));
    }

    // request.go:639-640 提取 IP 与反序 IP
    let ip = [reply[4], reply[5], reply[6], reply[7]];
    let ip_reverse = [ip[3], ip[2], ip[1], ip[0]];

    eprintln!(
        "[request_ip] 读 {} 字节，reply[0]=0x{:02x}，分配 IP={}.{}.{}.{}",
        n,
        reply[0],
        ip[0],
        ip[1],
        ip[2],
        ip[3]
    );

    // 连接不关闭，返回给调用方保活。
    Ok(((ip, ip_reverse), conn))
}

/// RecvConn：建立接收流隧道连接。
///
/// 对照 protocol.go:81-117。
///
/// 发送 cmd=0x06000000 的握手包，期望响应首字节 0x01。
/// 成功后这条连接用于「持续读取」服务端下发的数据。
pub fn recv_conn(
    server: &str,
    token: &[u8; TOKEN_LEN],
    ip_reverse: &[u8; 4],
) -> Result<UtlsConn, TunnelError> {
    let mut conn = UtlsConn::connect(server, TlsMode::Special)?;

    // protocol.go:92-95 RECV STREAM START
    let msg = build_handshake_packet([0x06, 0x00, 0x00, 0x00], token, ip_reverse);
    conn.write_all(&msg)?;

    let (n, reply) = read_reply(&mut conn)?;

    // protocol.go:112 reply[0] != 0x01 → 失败
    if reply[0] != 0x01 {
        return Err(TunnelError::UnexpectedReply(reply[0]));
    }

    eprintln!("[recv_conn] 读 {} 字节，reply[0]=0x{:02x}", n, reply[0]);
    Ok(conn)
}

/// SendConn：建立发送流隧道连接。
///
/// 对照 protocol.go:135-183。
///
/// 发送 cmd=0x05000000 的握手包，按响应首字节（Sangfor 命令码）分派：
/// - 0x02 → 成功，这条连接用于「写入」数据
/// - 0x08 → SHUTDOWN，会话被永久终止（需全新重登录）
/// - 0x05/06/07/09 → RECONNECTLATER，忙碌/冲突（应 sleep 后重试）
/// - 其他 → 错误
pub fn send_conn(
    server: &str,
    token: &[u8; TOKEN_LEN],
    ip_reverse: &[u8; 4],
) -> Result<UtlsConn, TunnelError> {
    let mut conn = UtlsConn::connect(server, TlsMode::Special)?;

    // protocol.go:146-149 SEND STREAM START
    let msg = build_handshake_packet([0x05, 0x00, 0x00, 0x00], token, ip_reverse);
    conn.write_all(&msg)?;

    let (n, reply) = read_reply(&mut conn)?;

    // protocol.go:168-182 按响应首字节分派
    let code = reply[0];
    match code {
        0x02 => {
            eprintln!("[send_conn] 读 {} 字节，reply[0]=0x02（成功）", n);
            Ok(conn)
        }
        0x08 => {
            // protocol.go:171-174 SHUTDOWN
            eprintln!("[send_conn] 服务端 SHUTDOWN (cmd 0x08)，会话被终止");
            Err(TunnelError::Shutdown)
        }
        0x05 | 0x06 | 0x07 | 0x09 => {
            // protocol.go:175-178 RECONNECTLATER
            eprintln!("[send_conn] 服务端 RECONNECTLATER (cmd 0x{:02x})", code);
            Err(TunnelError::ReconnectLater(code))
        }
        _ => {
            eprintln!("[send_conn] 读 {} 字节，意外响应 reply[0]=0x{:02x}", n, code);
            Err(TunnelError::UnexpectedReply(code))
        }
    }
}
