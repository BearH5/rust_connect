//! token 构造与 requestToken。
//!
//! 对照 zju-connect/client/easyconnect/request.go:564-605（requestToken）。
//!
//! 关键修正（比 tunnel-port-reference.md 更准确）：
//! request.go:591 的 sessionID 是 `hex.EncodeToString(ServerHello.SessionId)`，
//! 即 **hex 字符串**（32 字节 → 64 字符）。request.go:600 的 `sessionID[:31]`
//! 取的是 hex 字符串的**前 31 个 ASCII 字符**，不是字节。

use std::io::{Read, Write};

use ec_utls::{TlsMode, UtlsConn};

use crate::error::ProtocolError;

/// token 固定长度：31（session_id hex 前缀）+ 1（0x00 分隔）+ 16（twfID）= 48 字节。
pub const TOKEN_LEN: usize = 48;

/// 构造 48 字节 token。
///
/// 对照 request.go:600：
///   `c.token = (*[48]byte)([]byte(sessionID[:31] + "\x00" + c.twfID))`
///
/// - `session_id_hex`：ServerHello.SessionId 经 hex 编码后的字符串（64 字符）。
///   取其前 31 个 ASCII 字符填入 token[0..31]。
/// - `twf_id`：登录拿到的 ASCII 字符串。填入 token[32..]，不足 16 补 0，超过 16 截断。
/// - token[31] 固定为 0x00。
pub fn build_token(session_id_hex: &str, twf_id: &str) -> Result<[u8; TOKEN_LEN], ProtocolError> {
    // request.go:600 sessionID[:31] —— hex 字符串的前 31 个字符。
    // 若 session_id_hex 不足 31 字符，用 '0' 补足（保证 token 长度固定）。
    let prefix: Vec<u8> = session_id_hex
        .as_bytes()
        .iter()
        .take(31)
        .copied()
        .chain(std::iter::repeat(b'0'))
        .take(31)
        .collect();
    if prefix.len() != 31 {
        return Err(ProtocolError::Token(format!(
            "session_id_hex 前缀构造异常: 长度 {}",
            prefix.len()
        )));
    }

    let mut token = [0u8; TOKEN_LEN];
    token[0..31].copy_from_slice(&prefix);
    // token[31] = 0x00 已是默认值

    // twfID：填入 token[32..]，不足补 0，超过截断到 16 字节。
    let twf_bytes = twf_id.as_bytes();
    let twf_len = twf_bytes.len().min(16);
    token[32..32 + twf_len].copy_from_slice(&twf_bytes[..twf_len]);
    // 余下 [32+twf_len..48] 保持 0。

    Ok(token)
}

/// requestToken：用普通 TLS（HelloGolang）连接，发两个带 TWFID 的 HTTP GET，
/// 读取 ServerHello 的 session_id（token 前半段来源）。
///
/// 对照 request.go:564-605。
///
/// 返回 session_id 的 **hex 字符串**（可直接传给 `build_token`）。
///
/// 注意：必须用 ec-utls 的普通模式，不能用 reqwest/rustls——
/// 因为需要读 ServerHello.SessionId，rustls 拿不到这个字段。
pub fn request_token(server: &str, twf_id: &str) -> Result<String, ProtocolError> {
    // request.go:565 建立 TLS 连接（普通 HelloGolang）
    let mut conn = UtlsConn::connect(server, TlsMode::Normal)?;

    // request.go:591 session_id 在握手完成时就已确定，先取出来（不依赖 HTTP 响应）。
    let sid = conn.session_id()?;
    let session_id_hex = hex::encode(&sid);

    // request.go:580-586 发送两个带 TWFID 的 HTTP GET。
    // 目的不是拿响应内容，而是让服务端在这条连接上识别身份。
    let request = format!(
        "GET /por/conf.csp HTTP/1.1\r\nHost: {server}\r\nCookie: TWFID={twf_id}\r\n\
         \r\n\
         GET /por/rclist.csp HTTP/1.1\r\nHost: {server}\r\nCookie: TWFID={twf_id}\r\n\
         \r\n"
    );
    conn.write_all(request.as_bytes())
        .map_err(ProtocolError::Io)?;

    // request.go:594-598 读 8 字节，仅用于检测连接是否有效。
    let mut buf = [0u8; 8];
    let n = conn.read(&mut buf).map_err(ProtocolError::Io)?;
    if n == 0 {
        return Err(ProtocolError::Token(
            "request_token: 读取响应 0 字节，连接可能被服务端拒绝".into(),
        ));
    }

    Ok(session_id_hex)
}
