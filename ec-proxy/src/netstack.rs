//! NetStack：smoltcp Interface + SocketSet，提供出站 TCP。
//!
//! 对照 zju-connect 的 gVisor Stack：驱动 TCP/IP 栈，把出站 TCP 流转成 IP 包。
//! poll 循环在 tokio task 里跑，周期性调 iface.poll。

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex as StdMutex};
use std::task::Waker;

use smoltcp::iface::{Config, Interface, SocketHandle, SocketSet};
use smoltcp::socket::tcp::{Socket as TcpSocket, SocketBuffer, State as TcpState};
use smoltcp::storage::RingBuffer;
use smoltcp::time::{Duration, Instant};
use smoltcp::wire::{HardwareAddress, IpAddress, IpCidr, IpEndpoint, Ipv4Address};
use tokio::sync::{mpsc, Notify};

use crate::device::L3ConnDevice;
use crate::stream::{NetStream, SharedControl, TcpSocketControl, TcpSocketState};

/// smoltcp 栈。持有 poll 循环的停止信号。
pub struct NetStack {
    stop: Arc<Notify>,
}

/// poll 循环与 NetStream 共享的 per-socket 控制块映射。
pub(crate) type SocketMap = Arc<StdMutex<HashMap<SocketHandle, SharedControl>>>;

impl NetStack {
    /// 创建并启动 NetStack。
    ///
    /// `device` 是已 spawn 桥接线程的 L3ConnDevice。
    /// `client_ip` 是 RequestIP 拿到的 4 字节 IP（作 smoltcp 源地址）。
    pub fn new(mut device: L3ConnDevice, client_ip: [u8; 4]) -> std::io::Result<(Self, NetStackHandle)> {
        let mut iface_config = Config::new(HardwareAddress::Ip);
        iface_config.random_seed = rand::random();
        let mut iface = Interface::new(iface_config, &mut device, Instant::now());
        iface.update_ip_addrs(|addrs| {
            addrs
                .push(IpCidr::new(
                    IpAddress::v4(client_ip[0], client_ip[1], client_ip[2], client_ip[3]),
                    32,
                ))
                .expect("iface client ip");
        });
        iface
            .routes_mut()
            .add_default_ipv4_route(Ipv4Address::new(0, 0, 0, 1))
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::AddrNotAvailable, e))?;
        iface.set_any_ip(true);

        let socket_set = SocketSet::new(vec![]);
        let sockets: SocketMap = Arc::new(StdMutex::new(HashMap::new()));
        let stop = Arc::new(Notify::new());

        let (dial_tx, dial_rx) = mpsc::unbounded_channel::<DialRequest>();
        let stop_clone = Arc::clone(&stop);
        let wake = device.wake_notify();

        tokio::spawn(poll_loop(
            iface,
            device,
            socket_set,
            sockets.clone(),
            dial_rx,
            stop_clone,
            wake.clone(),
        ));

        let handle = NetStackHandle {
            dial_tx,
            sockets,
            wake,
        };
        Ok((Self { stop }, handle))
    }

    pub async fn stop(&self) {
        self.stop.notify_waiters();
    }
}

/// 出站拨号请求（SOCKS5 → poll 循环）。
struct DialRequest {
    remote: SocketAddr,
    done: tokio::sync::oneshot::Sender<std::io::Result<(SocketHandle, SharedControl)>>,
}

/// SOCKS5 侧持有的句柄，用于发起 dial。
#[derive(Clone)]
pub struct NetStackHandle {
    dial_tx: mpsc::UnboundedSender<DialRequest>,
    #[allow(dead_code)]
    sockets: SocketMap,
    /// 出站数据写入后唤醒 poll 循环用。
    wake: Arc<tokio::sync::Notify>,
}

impl NetStackHandle {
    /// 发起出站 TCP 连接，返回 NetStream。
    pub async fn dial_tcp(&self, remote: SocketAddr) -> std::io::Result<NetStream> {
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        self.dial_tx
            .send(DialRequest {
                remote,
                done: done_tx,
            })
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "netstack 已停止"))?;
        let (handle, control) = done_rx
            .await
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "dial 无响应"))??;
        let mut stream = NetStream::new(handle, control);
        stream.set_wake(self.wake.clone());
        Ok(stream)
    }
}

/// poll 循环：驱动 smoltcp Interface，处理 dial 请求和 socket 数据。
async fn poll_loop(
    mut iface: Interface,
    mut device: L3ConnDevice,
    mut socket_set: SocketSet<'static>,
    sockets: SocketMap,
    mut dial_rx: mpsc::UnboundedReceiver<DialRequest>,
    stop: Arc<Notify>,
    wake_notify: Arc<tokio::sync::Notify>,
) {
    loop {
        // 1. 处理新的 dial 请求（非阻塞）—— connect 必须在这里用 iface.context()
        while let Ok(req) = dial_rx.try_recv() {
            let recv_buf = 0x3FFF * 20usize;
            let mut socket = TcpSocket::new(
                SocketBuffer::new(vec![0u8; recv_buf]),
                SocketBuffer::new(vec![0u8; recv_buf]),
            );
            socket.set_keep_alive(Some(Duration::from_secs(28)));
            socket.set_timeout(Some(Duration::from_secs(7200)));

            let remote_ep = match req.remote {
                SocketAddr::V4(v4) => {
                    let [a, b, c, d] = v4.ip().octets();
                    IpEndpoint::new(
                        IpAddress::Ipv4(Ipv4Address::new(a, b, c, d)),
                        v4.port(),
                    )
                }
                SocketAddr::V6(_) => {
                    let _ = req.done.send(Err(std::io::Error::new(
                        std::io::ErrorKind::Unsupported,
                        "IPv6 暂不支持",
                    )));
                    continue;
                }
            };
            let local_port: u16 = (rand::random::<u16>() | 0x8000) + 1;

            match socket.connect(iface.context(), remote_ep, local_port) {
                Ok(()) => {
                    let handle = socket_set.add(socket);
                    let control = Arc::new(spin::Mutex::new(TcpSocketControl {
                        send_buffer: RingBuffer::new(vec![0u8; recv_buf]),
                        send_waker: None,
                        recv_buffer: RingBuffer::new(vec![0u8; recv_buf]),
                        recv_waker: None,
                        recv_state: TcpSocketState::Normal,
                        send_state: TcpSocketState::Normal,
                    }));
                    sockets.lock().unwrap().insert(handle, Arc::clone(&control));
                    let _ = req.done.send(Ok((handle, control)));
                }
                Err(e) => {
                    let _ = req.done.send(Err(std::io::Error::new(
                        std::io::ErrorKind::ConnectionRefused,
                        e,
                    )));
                }
            }
        }

        // 2. 驱动 smoltcp
        let now = Instant::now();
        let _ = iface.poll(now, &mut device, &mut socket_set);

        // 3. 搬运 socket 数据
        let mut to_remove = Vec::new();
        {
            let sock_map = sockets.lock().unwrap();
            for (&handle, control_arc) in sock_map.iter() {
                let socket = socket_set.get_mut::<TcpSocket>(handle);
                let mut control = control_arc.lock();

                if socket.state() == TcpState::Closed {
                    control.recv_state = TcpSocketState::Closed;
                    control.send_state = TcpSocketState::Closed;
                    wake(&control.recv_waker);
                    wake(&control.send_waker);
                    to_remove.push(handle);
                    continue;
                }

                // recv: socket → recv_buffer
                while socket.can_recv() && !control.recv_buffer.is_full() {
                    let r = socket.recv(|buf| (control.recv_buffer.enqueue_slice(buf), ()));
                    if r.is_err() {
                        socket.abort();
                        control.recv_state = TcpSocketState::Closed;
                        break;
                    }
                }
                wake(&control.recv_waker);

                // send: send_buffer → socket
                while socket.can_send() && !control.send_buffer.is_empty() {
                    let r = socket.send(|buf| (control.send_buffer.dequeue_slice(buf), ()));
                    if r.is_err() {
                        socket.abort();
                        control.send_state = TcpSocketState::Closed;
                        break;
                    }
                }
                if matches!(control.send_state, TcpSocketState::Close)
                    && control.send_buffer.is_empty()
                {
                    socket.close();
                    control.send_state = TcpSocketState::Closing;
                }
                wake(&control.send_waker);
            }
        }

        for h in to_remove {
            sockets.lock().unwrap().remove(&h);
            socket_set.remove(h);
        }

        // 4. 等待下一轮：有入站包立即 poll，否则按 poll_delay 睡（可被 wake 打断）
        let has_ingress = device.has_ingress();
        // cap 到 20ms：smoltcp 的 poll_delay 可能返回 28s（keep-alive 定时器）等长值，
        // 若直接 sleep 会卡住 ACK/重传等短周期事件。20ms 兜底保证及时处理，
        // 同时避免忙转（有数据时 wake_notify 会提前唤醒）。
        let poll_delay = iface
            .poll_delay(Instant::now(), &socket_set)
            .unwrap_or(Duration::from_millis(5))
            .min(Duration::from_millis(20));
        if has_ingress || poll_delay == Duration::ZERO {
            tokio::task::yield_now().await;
        } else {
            tokio::select! {
                // 入站包到达（bridge 读线程 notify）→ 立即醒来 poll（性能关键）
                _ = wake_notify.notified() => {}
                // TCP 定时器到期（ACK/重传等）
                _ = tokio::time::sleep(tokio::time::Duration::from(poll_delay)) => {}
                _ = stop.notified() => break,
            }
        }
    }
}

fn wake(waker_slot: &Option<Waker>) {
    if let Some(w) = waker_slot {
        w.wake_by_ref();
    }
}
