//! 仅登录验证：调用 ec-login 拿到 twfID，确认登录链路通。
//! 不涉及隧道层。

use ec_login::{login, LoginConfig, LoginStep};

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
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("参数错误: {e}");
            std::process::exit(2);
        }
    };
    let cfg = LoginConfig {
        server: args.server,
        username: args.username,
        password: args.password,
    };
    match login(&cfg) {
        Ok(LoginStep::Done(twf)) => {
            if twf.is_empty() {
                println!("✅ 登录成功（沿用临时 TwfID）");
            } else {
                println!("✅ 登录成功，TwfID: {twf}");
            }
        }
        Ok(step) => {
            println!("⚠ 登录需要进一步步骤: {step:?}");
            std::process::exit(1);
        }
        Err(e) => {
            println!("❌ 登录失败: {e}");
            std::process::exit(1);
        }
    }
}
