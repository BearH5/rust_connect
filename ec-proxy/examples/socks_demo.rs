//! 端到端示例：登录 → 隧道 → SOCKS5。
//!
//! 把 ec-login（拿 TwfID）+ ec-proxy（token/隧道/NetStack/SOCKS5）串成一条
//! 完整链路，本地起一个 SOCKS5 server，应用经它即可访问内网。
//!
//! 用法：
//!   cargo run --example socks_demo -- \
//!       --server 1.2.3.4:44333 --username username --password password \
//!       --bind 127.0.0.1:1080

use ec_login::{login, LoginConfig, LoginStep};
use ec_proxy::proxy::{run, ProxyConfig};

struct Args {
    server: String,
    username: String,
    password: String,
    bind: String,
}

fn parse_args() -> Result<Args, String> {
    let mut server = None;
    let mut username = None;
    let mut password = None;
    let mut bind = None;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--server" => server = it.next(),
            "--username" => username = it.next(),
            "--password" => password = it.next(),
            "--bind" => bind = it.next(),
            _ => return Err(format!("未知参数: {a}")),
        }
    }
    Ok(Args {
        server: server.ok_or("缺少 --server")?,
        username: username.ok_or("缺少 --username")?,
        password: password.ok_or("缺少 --password")?,
        bind: bind.unwrap_or_else(|| "127.0.0.1:1080".into()),
    })
}

#[tokio::main]
async fn main() {
    env_logger::init();
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("参数错误: {e}");
            std::process::exit(2);
        }
    };

    // 登录（reqwest::blocking 不能在 async runtime 内直接调，用 spawn_blocking）。
    let cfg = LoginConfig {
        server: args.server.clone(),
        username: args.username,
        password: args.password,
    };
    let twf_id = match tokio::task::spawn_blocking(move || login(&cfg)).await {
        Ok(Ok(LoginStep::Done(twf))) if !twf.is_empty() => twf,
        Ok(Ok(other)) => {
            eprintln!("登录未完成: {other:?}");
            std::process::exit(1);
        }
        Ok(Err(e)) => {
            eprintln!("登录失败: {e}");
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("登录任务异常: {e}");
            std::process::exit(1);
        }
    };
    println!("TwfID: {twf_id}");

    println!("启动 SOCKS5 @ {} ...", args.bind);
    if let Err(e) = run(ProxyConfig {
        server: args.server,
        twf_id,
        socks_bind: args.bind,
    })
    .await
    {
        eprintln!("代理失败: {e}");
        std::process::exit(1);
    }
}
