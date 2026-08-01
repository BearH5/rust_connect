//! 登录模块 demo binary —— 验证 EasyConnect 登录流程。
//!
//! 这部分走普通 HTTPS，不涉及 Sangfor 特殊 TLS 指纹（那只在隧道握手层）。
//! 用法：
//!   cargo run --release -- --server rvpn.zju.edu.cn:443 \
//!       --username 学号 --password 密码

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
            "-h" | "--help" => {
                print_usage();
                std::process::exit(0);
            }
            _ => return Err(format!("未知参数: {a}")),
        }
    }

    Ok(Args {
        server: server.ok_or_else(|| {
            print_usage();
            "缺少 --server".to_string()
        })?,
        username: username.ok_or("缺少 --username")?,
        password: password.ok_or("缺少 --password")?,
    })
}

fn print_usage() {
    eprintln!(
        "用法: ec-login --server <host:port> --username <用户名> --password <密码>\n\
         示例:\n\
         \x20 ec-login --server rvpn.zju.edu.cn:443 --username 319xxxx --password xxxx"
    );
}

fn main() {
    println!("=== EasyConnect 登录模块 (纯 Rust + reqwest) ===\n");

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
        Ok(step) => match step {
            LoginStep::Done(twf) => {
                if twf.is_empty() {
                    println!("\n✅ 登录成功（响应未返回新 TwfID，沿用临时 TwfID）");
                } else {
                    println!("\n✅ 登录成功！授权 TwfID: {twf}");
                }
                println!("   说明：登录流程验证通过。后续 token 构造与隧道握手需要这个 TwfID。");
            }
            LoginStep::NeedSms => {
                println!("\n⚠ 需要短信验证码（NeedSms）");
                println!("   登录主流程理解正确，服务端要求 SMS 二次验证。");
                println!("   后续实现 SMS 流程：POST /por/login_sms.csp → /por/login_sms1.csp");
            }
            LoginStep::NeedTotp => {
                println!("\n⚠ 需要 TOTP 验证码（NeedTotp）");
                println!("   登录主流程理解正确，服务端要求 TOTP 二次验证。");
                println!("   后续实现 TOTP 流程：POST /por/login_token.csp");
            }
            LoginStep::NeedCert => {
                println!("\n⚠ 需要证书认证（NeedCert）");
                println!("   登录主流程理解正确，服务端要求证书二次验证。");
            }
            LoginStep::Failed(reason) => {
                println!("\n❌ 登录失败");
                println!("   服务端响应:\n{reason}");
                std::process::exit(1);
            }
        },
        Err(e) => {
            println!("\n❌ 登录过程出错: {e}");
            std::process::exit(1);
        }
    }
}
