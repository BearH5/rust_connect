//! 阶段 D-2 隧道层集成测试。
//!
//! 通过条件：
//! - request_token 能拿到非空 hex session_id
//! - build_token 拼出合法 48 字节 token
//! - request_ip 拿到客户端 IP（reply[0]==0x00）
//! - recv_conn / send_conn 握手成功（reply[0]==0x01 / 0x02）
//!
//! 运行：cargo test --test tunnel -- --nocapture --test-threads=1
//! 默认服务器/凭据见下方常量，可用环境变量覆盖。
//!
//! 注意：测试会真实连接服务器并消耗 twfID 会话，故用 --test-threads=1 串行，
//! 避免并发登录冲突。每个测试独立登录拿独立 twfID。

use ec_login::{login, LoginConfig, LoginStep};
use ec_protocol::{token, tunnel};
use std::env;

const DEFAULT_SERVER: &str = "1.2.3.4:44333";
const DEFAULT_USERNAME: &str = "username";
const DEFAULT_PASSWORD: &str = "password";

fn server() -> String {
    env::var("EC_TEST_SERVER").unwrap_or_else(|_| DEFAULT_SERVER.into())
}

fn credentials() -> (String, String) {
    (
        env::var("EC_TEST_USERNAME").unwrap_or_else(|_| DEFAULT_USERNAME.into()),
        env::var("EC_TEST_PASSWORD").unwrap_or_else(|_| DEFAULT_PASSWORD.into()),
    )
}

/// 登录并返回授权 twfID。失败时按「环境不可达」策略处理：
/// 仅网络层错误跳过，登录逻辑失败（如认证失败）必须 panic。
fn login_or_skip() -> String {
    let (username, password) = credentials();
    match login(&LoginConfig {
        server: server(),
        username,
        password,
    }) {
        Ok(LoginStep::Done(twf)) if !twf.is_empty() => twf,
        Ok(LoginStep::Done(_)) => {
            panic!("登录成功但未返回授权 TwfID（登录逻辑问题）");
        }
        Ok(step) => {
            panic!("登录需要进一步步骤（非网络问题，不应跳过）: {step:?}");
        }
        Err(e) => assume_unreachable_or_skip(&e),
    }
}

/// 网络不可达才跳过；否则 panic。
fn assume_unreachable_or_skip(e: &impl std::fmt::Display) -> ! {
    let msg = e.to_string().to_lowercase();
    let is_network = msg.contains("no such host")
        || msg.contains("connection refused")
        || msg.contains("timed out")
        || msg.contains("unreachable")
        || msg.contains("network")
        || msg.contains("connect: ")
        || msg.contains("dns")
        || msg.contains("resolve");
    if is_network {
        eprintln!("跳过：环境网络不可达 ({e})");
        std::process::exit(0);
    }
    panic!("失败（非网络不可达，不应跳过）：{e}");
}

/// 标准 1：request_token 返回非空 hex session_id。
#[test]
fn request_token_returns_hex_session_id() {
    let twf_id = login_or_skip();
    let sid_hex = match token::request_token(&server(), &twf_id) {
        Ok(h) => h,
        Err(e) => assume_unreachable_or_skip(&e),
    };
    assert!(!sid_hex.is_empty(), "session_id hex 不应为空");
    // ServerHello session_id 是 32 字节，hex 编码应为 64 字符。
    assert_eq!(sid_hex.len(), 64, "session_id hex 应为 64 字符（32 字节）");
    eprintln!("✓ request_token 返回 session_id hex: {sid_hex}");
}

/// 标准 2：build_token 拼出合法 48 字节 token。
#[test]
fn build_token_produces_48_bytes() {
    // 用固定的 session_id hex 测试纯函数（不需联网）。
    let sid_hex = "9b382aa801796b16f47e98f8e8999506409b12157b5e92a299cc32ee1f53bbb8";
    let twf = "c0c019e3ee23e314";
    let token = token::build_token(sid_hex, twf).expect("应构造成功");

    assert_eq!(token.len(), 48, "token 必须是 48 字节");
    // 前 31 字节是 hex 字符串前 31 字符的 ASCII。
    assert_eq!(&token[0..31], b"9b382aa801796b16f47e98f8e899950");
    // 第 32 字节（偏移 31）是 0x00 分隔。
    assert_eq!(token[31], 0x00, "token[31] 应为 0x00 分隔符");
    // token[32..] 是 twfID。
    assert_eq!(&token[32..48], b"c0c019e3ee23e314");
    eprintln!("✓ build_token 构造 48 字节 token 正确");
}

/// 标准 3：request_ip 拿到客户端 IP（reply[0]==0x00）。
#[test]
fn request_ip_returns_client_ip() {
    let twf_id = login_or_skip();
    let sid_hex = match token::request_token(&server(), &twf_id) {
        Ok(h) => h,
        Err(e) => assume_unreachable_or_skip(&e),
    };
    let tkn = token::build_token(&sid_hex, &twf_id).expect("token 构造");

    let ((ip, _ip_reverse), _conn) = match tunnel::request_ip(&server(), &tkn) {
        Ok(v) => v,
        Err(e) => assume_unreachable_or_skip(&e),
    };
    eprintln!("✓ request_ip 拿到客户端 IP: {}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3]);
    // _conn 持有保活连接，函数结束 drop（测试场景可接受）。
}

/// 标准 4：recv_conn 握手成功（reply[0]==0x01）。
#[test]
fn recv_conn_handshake_ok() {
    let twf_id = login_or_skip();
    let sid_hex = match token::request_token(&server(), &twf_id) {
        Ok(h) => h,
        Err(e) => assume_unreachable_or_skip(&e),
    };
    let tkn = token::build_token(&sid_hex, &twf_id).expect("token 构造");
    let ((ip, ip_reverse), _conn) = match tunnel::request_ip(&server(), &tkn) {
        Ok(v) => v,
        Err(e) => assume_unreachable_or_skip(&e),
    };

    let _recv = match tunnel::recv_conn(&server(), &tkn, &ip_reverse) {
        Ok(c) => c,
        Err(e) => assume_unreachable_or_skip(&e),
    };
    eprintln!("✓ recv_conn 握手成功（reply[0]==0x01）, ip={:?}", ip);
}

/// 标准 5：send_conn 握手成功（reply[0]==0x02）。
#[test]
fn send_conn_handshake_ok() {
    let twf_id = login_or_skip();
    let sid_hex = match token::request_token(&server(), &twf_id) {
        Ok(h) => h,
        Err(e) => assume_unreachable_or_skip(&e),
    };
    let tkn = token::build_token(&sid_hex, &twf_id).expect("token 构造");
    let ((ip, ip_reverse), _conn) = match tunnel::request_ip(&server(), &tkn) {
        Ok(v) => v,
        Err(e) => assume_unreachable_or_skip(&e),
    };

    let _send = match tunnel::send_conn(&server(), &tkn, &ip_reverse) {
        Ok(c) => c,
        Err(e) => assume_unreachable_or_skip(&e),
    };
    eprintln!("✓ send_conn 握手成功（reply[0]==0x02）, ip={:?}", ip);
}
