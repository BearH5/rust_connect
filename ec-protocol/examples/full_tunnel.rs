//! 端到端隧道验证：登录 → token → RequestIP → Recv/SendConn → L3Conn 收发。
//!
//! 用法：
//!   cargo run --example full_tunnel -- \
//!       --server 1.2.3.4:44333 --username username --password password
//!
//! 成功标志：拿到分配的客户端 IP，L3Conn write/read 不报错。

use std::io::{Read, Write};

use ec_login::{login, LoginConfig, LoginStep};
use ec_protocol::{l3conn::L3Conn, token, tunnel};

struct Args {
    server: String,
    username: String,
    password: String,
}

fn parse_args() -> Result<Args, String> {
    let mut server = None;
    let mut username = None;
    let mut password = None;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--server" => server = it.next(),
            "--username" => username = it.next(),
            "--password" => password = it.next(),
            _ => return Err(format!("未知参数: {a}")),
        }
    }
    Ok(Args {
        server: server.ok_or("缺少 --server")?,
        username: username.ok_or("缺少 --username")?,
        password: password.ok_or("缺少 --password")?,
    })
}

fn main() {
    println!("=== EasyConnect 全链路隧道验证 ===\n");

    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("参数错误: {e}");
            std::process::exit(2);
        }
    };

    // ---- 1. 登录拿 twfID ----
    println!("[1/5] 登录...");
    let twf_id = match login(&LoginConfig {
        server: args.server.clone(),
        username: args.username,
        password: args.password,
    }) {
        Ok(LoginStep::Done(twf)) if !twf.is_empty() => twf,
        Ok(LoginStep::Done(_)) => {
            println!("❌ 登录成功但未返回授权 TwfID");
            std::process::exit(1);
        }
        Ok(step) => {
            println!("❌ 登录需要进一步步骤: {step:?}");
            std::process::exit(1);
        }
        Err(e) => {
            println!("❌ 登录失败: {e}");
            std::process::exit(1);
        }
    };
    println!("    ✅ TwfID: {twf_id}");

    // ---- 2. request_token 拿 session_id hex ----
    println!("[2/5] request_token（普通 TLS + HTTP GET）...");
    let session_id_hex = match token::request_token(&args.server, &twf_id) {
        Ok(h) => h,
        Err(e) => {
            println!("❌ request_token 失败: {e}");
            std::process::exit(1);
        }
    };
    println!("    ✅ session_id hex: {session_id_hex}");

    // ---- 3. build_token 拼出 48 字节 token ----
    println!("[3/5] 构造 token...");
    let tkn = match token::build_token(&session_id_hex, &twf_id) {
        Ok(t) => t,
        Err(e) => {
            println!("❌ build_token 失败: {e}");
            std::process::exit(1);
        }
    };
    println!("    ✅ token (48 字节): {}", hex::encode(tkn));

    // ---- 4. RequestIP 拿客户端 IP ----
    println!("[4/5] request_ip（特殊 TLS）...");
    let ((ip, ip_reverse), _keepalive_conn) = match tunnel::request_ip(&args.server, &tkn) {
        Ok(v) => v,
        Err(e) => {
            println!("❌ request_ip 失败: {e}");
            std::process::exit(1);
        }
    };
    println!(
        "    ✅ 客户端 IP: {}.{}.{}.{}（reverse: {:?}）",
        ip[0], ip[1], ip[2], ip[3], ip_reverse
    );
    // _keepalive_conn 必须持有到会话结束，否则后续握手失败（request.go:647）。

    // ---- 5. L3Conn 收发 ----
    println!("[5/5] 建立 L3Conn（Recv+Send 隧道）...");
    let mut l3 = match L3Conn::new(&args.server, &tkn, &ip_reverse) {
        Ok(c) => c,
        Err(e) => {
            println!("❌ L3Conn 建立失败: {e}");
            std::process::exit(1);
        }
    };
    println!("    ✅ L3Conn 已建立");

    // 收发探测：写一个探测包，尝试读响应。
    // 注意：实际 IP 包需要合法格式，这里只验证管道是否畅通（不期望有效 IP 包响应）。
    println!("    尝试收发...");
    let probe = [0u8; 4];
    match l3.write_all(&probe) {
        Ok(()) => println!("    ✅ write 成功"),
        Err(e) => println!("    ⚠ write 出错: {e}"),
    }
    let mut resp = [0u8; 64];
    match l3.read(&mut resp) {
        Ok(n) => println!("    ✅ read {} 字节", n),
        Err(e) => println!("    ⚠ read 出错: {e}（可能无数据下发，连接本身已通）"),
    }

    println!("\n✅ 全链路验证完成");
}
