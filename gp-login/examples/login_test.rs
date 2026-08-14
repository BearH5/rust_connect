//! GP 登录验证示例：用真实账号测试能否拿到 openconnect cookie。
//!
//! 用法（从环境变量读凭据，避免硬编码）：
//! ```bash
//! GP_SERVER="114.250.31.2:4430" GP_USER="WLWH" GP_PASS="w12345678W" \
//!   cargo run --example login_test
//! ```
//!
//! 成功会打印出 gateway、portal、user 和 cookie（已脱敏）。

use gp_login::{login, GpLoginConfig};

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let server = std::env::var("GP_SERVER").expect("请设置 GP_SERVER 环境变量，如 114.250.31.2:4430");
    let username = std::env::var("GP_USER").expect("请设置 GP_USER 环境变量");
    let password = std::env::var("GP_PASS").expect("请设置 GP_PASS 环境变量");

    println!("=== GlobalProtect 登录测试 ===");
    println!("服务器: {}", server);
    println!("账号:   {}", username);
    println!();

    let cfg = GpLoginConfig {
        server,
        username,
        password,
    };

    match login(&cfg) {
        Ok(result) => {
            println!("✅ 登录成功！");
            println!("  gateway: {}", result.gateway);
            println!("  portal:  {}", result.portal);
            println!("  user:    {}", result.user);
            // cookie 包含 authcookie，只打印长度和前缀脱敏
            println!(
                "  cookie:  (长度 {}，前 40 字符: {}...)",
                result.cookie.len(),
                &result.cookie[..result.cookie.len().min(40)]
            );
            println!();
            println!("交给 openconnect 的完整命令（cookie 已脱敏）:");
            println!(
                "  openconnect --protocol=gp --cookie='<已脱敏>' --os=linux --useragent='PAN GlobalProtect' https://{}",
                result.gateway
            );
        }
        Err(e) => {
            eprintln!("❌ 登录失败: {e}");
            std::process::exit(1);
        }
    }
}
