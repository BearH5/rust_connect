//! TUN 全局代理模式：L3Conn ↔ tun2 虚拟网卡双向转发。
//!
//! TUN 模式下不需要 smoltcp/SOCKS5：系统 TCP/IP 栈直接处理 IP 包，
//! L3Conn 只是 IP 包管道。
//!
//! 数据流：
//!   VPN → L3Conn 读线程 → device.write() → 系统路由 → 应用
//!   应用 → 系统路由 → device.read() → L3Conn 写线程 → VPN
//!
//! 多线程方案（关键）：tun2 的 `Device` 不支持 clone，但所有平台的
//! `Device` 都提供 `split(self) -> (Reader, Writer)`——Windows 后端内部是
//! `Arc<wintun::Session>`（wintun 的 session 天然支持并发读/写），Reader/Writer
//! 可各 move 进一个 OS 线程独立阻塞，互不干扰。这正是 tun2 官方
//! `examples/split.rs` 的用法，无需 async feature，也无需锁。

use std::io::{Read, Write};
use std::net::Ipv4Addr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

use ec_protocol::l3conn::L3Conn;

/// Windows 创建进程标志：不弹控制台窗口。
/// powershell / net / route 等控制台程序默认会闪黑窗，加此标志后静默运行。
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

/// 内网路由（资源 IP 范围 → TUN 接口）。
#[derive(Clone, Debug)]
pub struct TunRoute {
    pub network: String, // 如 "192.168.1.0"
    pub mask: String,    // 如 "255.255.255.0"
}

impl TunRoute {
    pub fn new(network: impl Into<String>, mask: impl Into<String>) -> Self {
        Self {
            network: network.into(),
            mask: mask.into(),
        }
    }
}

/// TUN 模式桥接。持有停止标志和转发线程句柄。
///
/// 注意：device 被 `split()` 消费后由两个转发线程的 Reader/Writer
/// （内部 `Arc<Session>`）持有，线程退出即 drop、wintun 会话随之关闭、
/// 网卡自动清理，因此本结构不直接持有 device。
pub struct TunBridge {
    /// 停止标志：置位后转发线程在下一次 read/write 返回时退出。
    stop: Arc<AtomicBool>,
    /// 转发线程句柄。
    #[allow(dead_code)]
    threads: Vec<thread::JoinHandle<()>>,
}

impl TunBridge {
    /// 建立 TUN 网卡 + 双向转发 + 加路由。
    ///
    /// `l3conn` 是已建立隧道的 L3Conn（被拆成读写两半）。
    /// `client_ip` 是 RequestIP 拿到的 IP（作 TUN 接口地址）。
    /// `routes` 是内网网段（资源 IP 范围转的）。
    /// `wintun_dll_path` 是 wintun.dll 的路径（None 则用默认查找，即工作目录）。
    pub fn start(
        l3conn: L3Conn,
        client_ip: [u8; 4],
        routes: &[TunRoute],
        wintun_dll_path: Option<&str>,
    ) -> std::io::Result<Self> {
        // 1. 建 TUN 网卡
        let mut config = tun2::configure();
        config
            .tun_name("RustConnect")
            .address(Ipv4Addr::from(client_ip))
            .netmask(Ipv4Addr::new(255, 255, 255, 0))
            .mtu(1400)
            .up();

        // Windows 指定 wintun.dll 路径
        #[cfg(windows)]
        if let Some(path) = wintun_dll_path {
            config.platform_config(|pc| {
                pc.wintun_file(path);
            });
        }
        #[cfg(not(windows))]
        let _ = wintun_dll_path;

        let device = tun2::create(&config)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, format!("创建 TUN 失败: {e}")))?;
        log::info!("TUN 网卡已创建（接口地址 {}）", Ipv4Addr::from(client_ip));

        // 2. 加路由（内网网段 → TUN 接口，网关即本机 TUN 地址）。
        let client_ip_str = format!("{}.{}.{}.{}", client_ip[0], client_ip[1], client_ip[2], client_ip[3]);

        // Windows: 用 PowerShell New-NetRoute 显式指定 -InterfaceIndex，避免路由落到物理网卡。
        // 先查 RustConnect 网卡的 ifIndex。
        #[cfg(target_os = "windows")]
        {
            let if_index = {
                let out = std::process::Command::new("powershell")
                    .args([
                        "-NoProfile",
                        "-Command",
                        "(Get-NetAdapter -Name 'RustConnect').ifIndex",
                    ])
                    .creation_flags(CREATE_NO_WINDOW)
                    .output();
                match out {
                    Ok(o) if o.status.success() => {
                        String::from_utf8_lossy(&o.stdout).trim().to_string()
                    }
                    _ => String::new(),
                }
            };
            for route in routes {
                // New-NetRoute -DestinationPrefix <network>/24 -InterfaceIndex <idx> -NextHop <client_ip>
                let cmd = if !if_index.is_empty() {
                    format!(
                        "New-NetRoute -DestinationPrefix '{}/24' -InterfaceIndex {} -NextHop '{}' -ErrorAction SilentlyContinue; exit 0",
                        route.network, if_index, client_ip_str
                    )
                } else {
                    format!(
                        "route add {} mask {} {} metric 1",
                        route.network, route.mask, client_ip_str
                    )
                };
                let result = std::process::Command::new("powershell")
                    .args(["-NoProfile", "-Command", &cmd])
                    .creation_flags(CREATE_NO_WINDOW)
                    .output();
                match result {
                    Ok(out) if out.status.success() => {
                        log::info!("路由添加成功: {} mask {} (ifIndex {})", route.network, route.mask, if_index);
                    }
                    Ok(out) => {
                        let err = String::from_utf8_lossy(&out.stderr);
                        // 路由已存在不算失败
                        if !err.contains("already exists") && !err.contains("The object already exists") {
                            log::warn!("路由添加失败 {}: {}", route.network, err);
                        }
                    }
                    Err(e) => log::warn!("route 命令失败: {e}"),
                }
            }
        }

        // Linux: 先 up 接口，再设地址，再加路由
        #[cfg(target_os = "linux")]
        {
            let _ = std::process::Command::new("ip")
                .args(["link", "set", "RustConnect", "up"])
                .status();
            let addr = format!("{}/24", client_ip_str);
            let _ = std::process::Command::new("ip")
                .args(["addr", "add", &addr, "dev", "RustConnect"])
                .status();
            for route in routes {
                let net = format!("{}/24", route.network);
                let _ = std::process::Command::new("ip")
                    .args(["route", "add", &net, "dev", "RustConnect"])
                    .status();
                log::info!("路由添加: {} dev RustConnect", net);
            }
        }

        #[cfg(not(any(target_os = "linux", target_os = "windows")))]
        {
            let _ = &client_ip_str;
        }

        // 3. 双向转发：L3Conn 和 TUN 设备都拆成读写两半，各起一个 OS 线程
        let (mut tun_reader, mut tun_writer) = device.split();
        let (mut read_half, mut write_half) = l3conn.split();

        let stop = Arc::new(AtomicBool::new(false));
        let mut threads = Vec::new();

        // 读线程：VPN → TUN（服务端下发的 IP 包写进虚拟网卡）
        let stop_read = Arc::clone(&stop);
        threads.push(
            thread::Builder::new()
                .name("tun-vpn-to-device".into())
                .spawn(move || {
                    // wintun 单包最大 65535（Windows 上 set_mtu 不生效），
                    // 用最大包长避免截包；栈上 64KB 在默认线程栈内安全。
                    let mut buf = [0u8; 65535];
                    loop {
                        if stop_read.load(Ordering::Relaxed) {
                            log::info!("[tun] 停止信号，vpn-to-device 线程退出");
                            return;
                        }
                        match read_half.read(&mut buf) {
                            Ok(0) => {
                                log::info!("[tun] VPN 读 EOF，转发线程退出");
                                return;
                            }
                            Ok(n) => {
                                if let Err(e) = tun_writer.write_all(&buf[..n]) {
                                    log::error!("[tun] 写 TUN 失败: {e}");
                                    return;
                                }
                            }
                            Err(e) => {
                                log::error!("[tun] VPN 读出错: {e}");
                                return;
                            }
                        }
                    }
                })
                .expect("spawn tun vpn-to-device"),
        );

        // 写线程：TUN → VPN（应用发出的 IP 包发给服务端）
        let stop_write = Arc::clone(&stop);
        threads.push(
            thread::Builder::new()
                .name("tun-device-to-vpn".into())
                .spawn(move || {
                    let mut buf = [0u8; 65535];
                    loop {
                        if stop_write.load(Ordering::Relaxed) {
                            log::info!("[tun] 停止信号，device-to-vpn 线程退出");
                            return;
                        }
                        match tun_reader.read(&mut buf) {
                            Ok(0) => {
                                log::info!("[tun] TUN 读 EOF，转发线程退出");
                                return;
                            }
                            Ok(n) => {
                                let b = &buf[..n];
                                // 只转发合法的 IPv4 单播包（源=客户端 IP、目标非组播/广播）。
                                // Windows 会在网卡 up 后发 IPv6 RS/NS、mDNS/LLMNR/SSDP 等
                                // 链路本地流量，服务端收到 IPv6/组播/广播包会直接关闭
                                // send_conn（SHUTDOWN），必须在这里丢弃。
                                let ipv4_unicast = n >= 20
                                    && b[0] >> 4 == 4
                                    && b[12] == client_ip[0]
                                    && b[13] == client_ip[1]
                                    && b[14] == client_ip[2]
                                    && b[15] == client_ip[3]
                                    && b[16] < 224
                                    && !(b[16] == client_ip[0]
                                        && b[17] == client_ip[1]
                                        && b[18] == client_ip[2]
                                        && b[19] == 255);
                                if ipv4_unicast {
                                    if let Err(e) = write_half.write_all(&buf[..n]) {
                                        log::error!("[tun] 写 VPN 失败: {e}");
                                        return;
                                    }
                                }
                            }
                            Err(e) => {
                                log::error!("[tun] TUN 读出错: {e}");
                                return;
                            }
                        }
                    }
                })
                .expect("spawn tun device-to-vpn"),
        );

        Ok(TunBridge { stop, threads })
    }

    /// 请求停止转发（置停止标志，detach 线程句柄）。
    ///
    /// 转发线程阻塞在 read 上时无法立即中断（wintun receive 为阻塞 API），
    /// 因此不 join；线程会在下一次 read/write 返回（L3Conn 断开或设备关闭）
    /// 后检查标志自行退出，随后 Reader/Writer drop、会话关闭、网卡清理。
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.threads.clear();
        log::info!("[tun] 已请求停止转发");
    }

    /// 清理路由（断开时调用）。
    pub fn cleanup_routes(routes: &[TunRoute], client_ip: [u8; 4]) {
        #[cfg(target_os = "linux")]
        {
            let _ = client_ip;
            for route in routes {
                let net = format!("{}/24", route.network);
                let _ = std::process::Command::new("ip")
                    .args(["route", "del", &net, "dev", "RustConnect"])
                    .status();
                log::info!("路由已删除: {}", net);
            }
        }
        #[cfg(target_os = "windows")]
        {
            let client_ip_str = format!("{}.{}.{}.{}", client_ip[0], client_ip[1], client_ip[2], client_ip[3]);
            for route in routes {
                let _ = std::process::Command::new("route")
                    .args(["delete", &route.network, "mask", &route.mask, &client_ip_str])
                    .creation_flags(CREATE_NO_WINDOW)
                    .output();
                log::info!("路由已删除: {}", route.network);
            }
        }
        #[cfg(not(any(target_os = "linux", target_os = "windows")))]
        {
            let _ = (routes, client_ip);
        }
    }
}

impl Drop for TunBridge {
    fn drop(&mut self) {
        self.stop();
    }
}
