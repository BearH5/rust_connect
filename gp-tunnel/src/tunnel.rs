//! GpTunnel：GPST 隧道主体，持有 TLS 流并运行 IO 线程收发帧。
//!
//! 架构（策略 A：GP 专用转发，不复用 TunBridge）：
//!   - 单线程 owning TLS 流，用阻塞 IO + channel 对外
//!   - 收：TLS read -> 解帧 -> tx.send（IP 包出给 TUN 写线程）
//!   - 发：rx.recv（TUN 读线程来的 IP 包）-> 加帧 -> TLS write
//!   - DPD：10 秒周期发 keepalive 帧；20 秒无收包判定连接死
//!
//! 用 std 线程而非 tokio：GpTunnel::connect 在 ec-app 的 spawn_blocking 里调用，
//! 整个隧道跑在阻塞线程池，与 EasyConnect 的 L3Conn（阻塞 utls）模型一致。

use crate::config::GpTunnelConfig;
use crate::error::GpTunnelError;
use crate::frame::{
    encode_data_frame, encode_dpd_frame, parse_frame_header, HEADER_LEN,
};
use crate::handshake::{connect_tls_and_handshake, GpTlsStream};
use std::io::{Read, Write};
use std::net::ToSocketAddrs;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};

/// DPD/keepalive 间隔（秒，gpst.c 第 589 行默认 10）。
const DPD_INTERVAL: Duration = Duration::from_secs(10);
/// 死对端判定：距上次收包超过 2*DPD（gpst.c mainloop.c 第 465 行）。
const DPD_DEAD: Duration = Duration::from_secs(20);
/// 读缓冲最大长度（一个帧头+payload）。
const READ_BUF_SIZE: usize = 65535 + HEADER_LEN;

/// 从 cookie 解析 gateway host:port（cookie 里没有，从 gateway 参数传）。
/// GpTunnel 建立 getconfig + 握手后，返回配置和隧道对象。
pub fn connect(
    gateway: &str, // 形如 "114.250.31.2:4430" 或 "https://114.250.31.2:4430"
    cookie: &str,
) -> Result<(GpTunnelConfig, GpTunnel), GpTunnelError> {
    let base = normalize_base_url(gateway);
    let (host, port) = parse_host_port(&base)?;

    // 1. getconfig.esp 拉配置
    let config = crate::config::fetch_tunnel_config(&base, cookie)?;

    // 2. TLS 连接 + GET 握手
    let tls = connect_tls_and_handshake(&host, port, &config.tunnel_url, cookie)?;

    // 3. 启 IO 线程
    let tunnel = GpTunnel::new(tls);

    Ok((config, tunnel))
}

/// GPST 隧道对象。持有 IO 线程和两个 channel 端。
///
/// - `tunnel.recv()`：从隧道读一个 IP 包（给 TUN 写线程用）
/// - `tunnel.send(ip_packet)`：向隧道写一个 IP 包（TUN 读线程调用）
pub struct GpTunnel {
    /// 发送端：TUN 读线程把 IP 包发到这里 -> IO 线程写 TLS
    tx_to_tunnel: mpsc::Sender<Vec<u8>>,
    /// 接收端：IO 线程从 TLS 读出 IP 包发到这里 -> TUN 写线程取
    rx_from_tunnel: mpsc::Receiver<Vec<u8>>,
    /// 停止标志
    stop: Arc<AtomicBool>,
    /// IO 线程句柄
    io_thread: Option<thread::JoinHandle<()>>,
}

impl GpTunnel {
    /// 包装已握手的 TLS 流，启动 IO 线程。
    fn new(mut tls: GpTlsStream) -> Self {
        let (tx_to_tunnel, rx_to_tunnel) = mpsc::channel::<Vec<u8>>();
        let (tx_from_tunnel, rx_from_tunnel) = mpsc::channel::<Vec<u8>>();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_clone = Arc::clone(&stop);

        let io_thread = thread::Builder::new()
            .name("gp-tunnel-io".into())
            .spawn(move || {
                run_io_loop(&mut tls, rx_to_tunnel, tx_from_tunnel, stop_clone);
            })
            .expect("spawn gp-tunnel-io");

        Self {
            tx_to_tunnel,
            rx_from_tunnel,
            stop,
            io_thread: Some(io_thread),
        }
    }

    /// 发送 IP 包到隧道（TUN 读线程调用）。
    /// 非阻塞：放入 channel，IO 线程异步写 TLS。
    pub fn send(&self, ip_packet: Vec<u8>) -> Result<(), GpTunnelError> {
        self.tx_to_tunnel
            .send(ip_packet)
            .map_err(|_| GpTunnelError::Closed)
    }

    /// 从隧道接收 IP 包（TUN 写线程调用）。阻塞直到有包或隧道关闭。
    pub fn recv(&self) -> Result<Vec<u8>, GpTunnelError> {
        self.rx_from_tunnel
            .recv()
            .map_err(|_| GpTunnelError::Closed)
    }

    /// 拆成读写两半 + 守卫，各自可独立 move 进线程。
    ///
    /// - `GpTunnelReader`：持有 recv channel 端，TUN 写线程用它收 IP 包写网卡
    /// - `GpTunnelWriter`：持有 send channel 端，TUN 读线程用它把网卡读到的 IP 包发隧道
    /// - `GpTunnelGuard`：持有 stop 标志和 IO 线程句柄，drop 时停隧道。由主 task 持有。
    ///
    /// 消费 self（拆分后原对象不可用）。
    pub fn split(self) -> (GpTunnelReader, GpTunnelWriter, GpTunnelGuard) {
        (
            GpTunnelReader {
                rx: self.rx_from_tunnel,
            },
            GpTunnelWriter {
                tx: self.tx_to_tunnel,
            },
            GpTunnelGuard {
                stop: self.stop,
                io_thread: self.io_thread,
            },
        )
    }

    /// 请求停止 IO 线程。
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
        drop(self.tx_to_tunnel.send(Vec::new()));
    }
}

/// 隧道读半（收 IP 包）。可独立 move 进 TUN 写线程。
pub struct GpTunnelReader {
    rx: mpsc::Receiver<Vec<u8>>,
}

impl GpTunnelReader {
    /// 阻塞接收一个 IP 包。隧道关闭时返回 Err。
    pub fn recv(&self) -> Result<Vec<u8>, GpTunnelError> {
        self.rx.recv().map_err(|_| GpTunnelError::Closed)
    }
}

/// 隧道写半（发 IP 包）。可独立 move 进 TUN 读线程。
pub struct GpTunnelWriter {
    tx: mpsc::Sender<Vec<u8>>,
}

impl GpTunnelWriter {
    /// 发送 IP 包到隧道。隧道关闭时返回 Err。
    pub fn send(&self, ip_packet: Vec<u8>) -> Result<(), GpTunnelError> {
        self.tx
            .send(ip_packet)
            .map_err(|_| GpTunnelError::Closed)
    }
}

/// 隧道守卫：持有停止标志和 IO 线程句柄。drop 时停止 IO 线程。
pub struct GpTunnelGuard {
    stop: Arc<AtomicBool>,
    io_thread: Option<thread::JoinHandle<()>>,
}

impl GpTunnelGuard {
    /// 请求停止 IO 线程并等待退出。
    pub fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // io_thread 的 rx_to_tunnel 在所有 Sender drop 后会返回 Err 让其退出。
        // GpTunnelWriter drop 时关闭 Sender，但这里无法确保 Writer 已 drop，
        // 所以 stop 只置标志，join 在 Drop 里做。
        if let Some(handle) = self.io_thread.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for GpTunnelGuard {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.io_thread.take() {
            let _ = handle.join();
        }
    }
}

/// IO 线程主循环。
///
/// 用一个读线程单独处理 TLS read（阻塞），主循环用 select 模型处理
/// TLS write + DPD 定时器。由于 std 没有真正的 select，用如下策略：
///   - 单独起一个子线程读 TLS -> tx_from_tunnel
///   - 主循环：channel recv（待发包）超时唤醒 -> write TLS + DPD 判定
fn run_io_loop(
    tls: &mut GpTlsStream,
    rx_to_tunnel: mpsc::Receiver<Vec<u8>>,
    tx_from_tunnel: mpsc::Sender<Vec<u8>>,
    stop: Arc<AtomicBool>,
) {
    // 因为 TLS 流不能同时被两个线程读写（&mut self），用 Arc<Mutex> 包一层。
    // 但更简单：读和写都在本线程，用 set_nonblocking + 轮询。
    // 实际方案：用一个单独读线程 + Arc<Mutex<TlsStream>>。
    // 但 native-tls 的 TlsStream 不是 Send + Sync 友好...
    //
    // 最简方案：把 TLS 流的读和写都在单线程内交替进行。
    // 用 TcpStream 的非阻塞模式 + 定时轮询 channel。
    let stream = tls.get_ref();
    let _ = stream.set_nonblocking(true);

    let mut last_rx = Instant::now();
    let mut last_tx = Instant::now();
    let mut read_buf = [0u8; READ_BUF_SIZE];

    loop {
        if stop.load(Ordering::Relaxed) {
            log::info!("[gp-tunnel] IO 线程收到停止信号，退出");
            return;
        }

        // 1. 尝试从 TLS 读一帧（非阻塞）
        match tls.read(&mut read_buf) {
            Ok(0) => {
                log::info!("[gp-tunnel] TLS 读 EOF，IO 线程退出");
                return;
            }
            Ok(n) => {
                last_rx = Instant::now();
                if n < HEADER_LEN {
                    log::warn!("[gp-tunnel] 短包（{} 字节 < 16），丢弃", n);
                } else {
                    match parse_frame_header(&read_buf[..n]) {
                        Ok(hdr) => {
                            if hdr.is_dpd {
                                log::debug!("[gp-tunnel] 收到 DPD 响应");
                            } else {
                                // 数据帧：payload 从 offset 16 开始
                                let payload = &read_buf[HEADER_LEN..HEADER_LEN + hdr.payload_len];
                                if payload.len() == hdr.payload_len {
                                    if tx_from_tunnel.send(payload.to_vec()).is_err() {
                                        log::info!("[gp-tunnel] TUN 端已关闭，IO 线程退出");
                                        return;
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            log::warn!("[gp-tunnel] 帧解析失败: {e}（丢弃 {} 字节）", n);
                        }
                    }
                }
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                // 无数据可读，继续
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => {
                // 被中断，重试
            }
            Err(e) => {
                log::error!("[gp-tunnel] TLS 读错误: {e}，IO 线程退出");
                return;
            }
        }

        // 2. 尝试从 channel 取待发包（非阻塞）
        match rx_to_tunnel.try_recv() {
            Ok(ip_packet) => {
                if !ip_packet.is_empty() {
                    let frame = encode_data_frame(&ip_packet);
                    log::debug!("[gp-tunnel] 发送 IP 包（{} 字节）", ip_packet.len());
                    if let Err(e) = tls.write_all(&frame) {
                        log::error!("[gp-tunnel] TLS 写失败: {e}，IO 线程退出");
                        return;
                    }
                    let _ = tls.flush();
                    last_tx = Instant::now();
                }
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {
                log::info!("[gp-tunnel] 发送端已关闭，IO 线程退出");
                return;
            }
        }

        // 3. DPD/keepalive 判定
        let now = Instant::now();
        // keepalive：距上次发包超 10s -> 发 DPD 帧
        if now.duration_since(last_tx) >= DPD_INTERVAL {
            let dpd = encode_dpd_frame();
            if let Err(e) = tls.write_all(&dpd) {
                log::error!("[gp-tunnel] DPD 发送失败: {e}，IO 线程退出");
                return;
            }
            let _ = tls.flush();
            last_tx = now;
            log::debug!("[gp-tunnel] 发送 DPD/keepalive");
        }
        // 死对端：距上次收包超 20s -> 判定连接死
        if now.duration_since(last_rx) >= DPD_DEAD {
            log::error!("[gp-tunnel] DPD 超时（20s 无收包），判定连接死");
            return;
        }

        // 4. 短暂休眠避免空转（非阻塞模式下）
        thread::sleep(Duration::from_millis(10));
    }
}

// ===================== URL 辅助 =====================

/// 标准化为 "https://host:port"（无尾斜杠）。
fn normalize_base_url(server: &str) -> String {
    let s = server.trim().trim_end_matches('/');
    if s.starts_with("http://") || s.starts_with("https://") {
        s.to_string()
    } else {
        format!("https://{s}")
    }
}

/// 从 "https://host:port" 解析出 (host, port)。
fn parse_host_port(base: &str) -> Result<(String, u16), GpTunnelError> {
    let no_scheme = base
        .strip_prefix("https://")
        .or_else(|| base.strip_prefix("http://"))
        .unwrap_or(base);
    // 用 ToSocketAddrs 解析 host:port
    let addr = no_scheme
        .to_socket_addrs()
        .map_err(|e| GpTunnelError::Other(format!("解析 {no_scheme} 失败: {e}")))?
        .next()
        .ok_or_else(|| GpTunnelError::Other(format!("无法解析地址: {no_scheme}")))?;
    Ok((addr.ip().to_string(), addr.port()))
}
