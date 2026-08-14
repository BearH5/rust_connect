//! GPST 隧道端到端验证：先登录拿 cookie，再建隧道，验证握手成功 + 拿到 client_ip。
//!
//! 用法（从环境变量读凭据）：
//! ```bash
//! GP_SERVER="114.250.31.2:4430" GP_USER="WLWH" GP_PASS="w12345678W" \
//!   cargo run --example tunnel_test
//! ```
//!
//! 成功后保持连接 30 秒观察 DPD，然后退出。

use gp_login::{login as gp_login, GpLoginConfig};
use gp_tunnel::connect as gp_connect;
use std::time::Duration;

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("debug")).init();

    let server = std::env::var("GP_SERVER").expect("请设置 GP_SERVER");
    let username = std::env::var("GP_USER").expect("请设置 GP_USER");
    let password = std::env::var("GP_PASS").expect("请设置 GP_PASS");

    println!("=== GlobalProtect 隧道端到端测试 ===");
    println!("服务器: {server}\n");

    // 1. 登录拿 cookie
    println!("[1/2] GP 登录...");
    let cfg = GpLoginConfig { server: server.clone(), username, password };
    let auth = match gp_login(&cfg) {
        Ok(a) => {
            println!("  ✅ 登录成功: gateway={}, user={}", a.gateway, a.user);
            a
        }
        Err(e) => {
            eprintln!("  ❌ 登录失败: {e}");
            std::process::exit(1);
        }
    };

    // 2. 建隧道
    println!("[2/2] 建 GPST 隧道（getconfig + START_TUNNEL 握手）...");
    let (config, tunnel) = match gp_connect(&auth.gateway, &auth.cookie) {
        Ok((config, tunnel)) => {
            println!("  ✅ 隧道建立成功！");
            println!("     client_ip: {}.{}.{}.{}",
                config.client_ip[0], config.client_ip[1], config.client_ip[2], config.client_ip[3]);
            println!("     netmask:   {}.{}.{}.{}",
                config.netmask[0], config.netmask[1], config.netmask[2], config.netmask[3]);
            println!("     mtu:       {}", config.mtu);
            println!("     tunnel_url: {}", config.tunnel_url);
            println!("     routes:    {} 条", config.routes.len());
            for r in &config.routes { println!("       - {r}"); }
            println!("     dns:       {} 个", config.dns.len());
            for d in &config.dns { println!("       - {d}"); }
            println!("     rekey:     {:?}", config.rekey_timeout);
            (config, tunnel)
        }
        Err(e) => {
            eprintln!("  ❌ 隧道建立失败: {e}");
            std::process::exit(1);
        }
    };

    // 3. 保持 30 秒观察 DPD（隧道对象持有 IO 线程，drop 即断开）
    println!("\n隧道已建立，保持 30 秒观察 DPD 保活...");
    println!("（IO 线程每 10s 发 DPD，若 20s 无收包会判定连接死）\n");
    let _ = &config; // 保留引用
    std::thread::sleep(Duration::from_secs(30));

    println!("\n测试完成，断开隧道");
    tunnel.stop();
    drop(tunnel);
    println!("✅ 隧道已断开");
}
