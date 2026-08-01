# 阶段 3：SOCKS5 代理出口（smoltcp 出站层）实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 用 smoltcp 用户态 TCP/IP 栈把 L3Conn（IP 包流）转成 TCP 会话，外挂一个本地 SOCKS5 server，让 `curl --socks5 127.0.0.1:1080 <内网IP>` 能访问内网。

**Architecture:** 三层分离：SOCKS5 server（tokio async，用 fast-socks5 库处理协议）→ smoltcp 出站层（tokio task 跑 poll 循环 + `dial_tcp`）→ L3Conn 桥接线程（std::thread 同步阻塞 I/O，用 mpsc channel 与 async 侧通信）。smoltcp 的 `phy::Device` 把 L3Conn 包成虚拟网卡，出站 TCP 用 `TcpSocket::connect`（对照 zju-connect gVisor 的 `gonet.DialTCP`）。

**Tech Stack:** Rust + smoltcp 0.12（features: `proto-ipv4`, `socket-tcp`, `medium-ip`）+ tokio（async runtime）。依赖已有 `ec-protocol`（L3Conn）。

**对应设计文档:** `docs/specs/2026-07-31-stage3-socks5-smoltcp-design.md`

**首轮范围:** TCP only（SOCKS5 CONNECT 命令）。UDP/DNS/ICMP 留到验证架构成立后。

---

## 前置确认（已就绪）

- ec-protocol 的 `L3Conn` 已实现（`src/l3conn.rs`），实现 `Read`/`Write`（同步阻塞）
- smoltcp v0.12 Device trait 用 GAT（`type RxToken<'a>`），poll 需 `Instant` 时间驱动
- `request_ip` 返回 client_ip（用作 smoltcp Interface 的源地址）
- MTU：zju-connect gvisor 用 1400，本计划沿用

---

## 文件结构

```
rust_connect/
└── ec-proxy/                 # 新建 crate
    ├── Cargo.toml            # 依赖 ec-protocol, smoltcp, tokio
    ├── build.rs              # DLL 部署（复用 ec-protocol 模式，间接依赖 ec-utls 的 DLL）
    └── src/
        ├── lib.rs            # 模块导出
        ├── device.rs         # L3ConnDevice（smoltcp phy::Device）+ 桥接线程
        ├── netstack.rs       # NetStack（Interface + SocketSet + poll 循环 + dial_tcp）
        ├── stream.rs         # NetStream（AsyncRead/AsyncWrite，封装 TcpSocket handle）
        ├── socks5.rs         # SOCKS5 server（协议解析 + CONNECT + 双向转发）
        └── proxy.rs          # Proxy 顶层：组装 L3Conn + NetStack + SOCKS5
    └── examples/
        └── socks_demo.rs     # 端到端：登录→隧道→SOCKS5
    └── tests/
        └── socks5.rs         # 集成测试
```

各文件职责：
- `device.rs`：把同步阻塞的 L3Conn 包成 smoltcp Device。独立 OS 线程跑读写循环，Device 内部用 `Mutex<VecDeque>` 缓冲入站包、`Sender` 发出站包。
- `netstack.rs`：驱动 smoltcp `Interface` + `SocketSet`，提供 `dial_tcp()` 出站。poll 循环周期性调 `iface.poll`。
- `stream.rs`：单个出站连接的 async 读写封装，经 RingBuffer 与 poll 循环交互。
- `socks5.rs`：SOCKS5 协议（RFC 1928）的最小实现，仅 CONNECT。
- `proxy.rs`：顶层编排，把各组件串起来。
- `socks_demo.rs` / `tests/socks5.rs`：端到端验证。

---

## Task 1: ec-proxy crate 骨架

**Files:**
- Create: `ec-proxy/Cargo.toml`
- Create: `ec-proxy/build.rs`
- Create: `ec-proxy/src/lib.rs`

- [ ] **Step 1: 写 Cargo.toml**

```toml
[package]
name = "ec-proxy"
version = "0.1.0"
edition = "2021"
publish = false

[dependencies]
ec-protocol = { path = "../ec-protocol" }
# smoltcp：只启用 IPv4 + TCP + IP medium（L3 模式，对照 TUN）
smoltcp = { version = "0.12", default-features = false, features = ["std", "proto-ipv4", "socket-tcp", "medium-ip"] }
# async runtime
tokio = { version = "1", features = ["rt-multi-thread", "net", "io-util", "sync", "macros", "time"] }
# 日志
log = "0.4"
env_logger = "0.11"
# SOCKS5 协议（握手/命令解析/错误回复），我们只接管拨号
fast-socks5 = "0.10"

[[example]]
name = "socks_demo"
path = "examples/socks_demo.rs"
```

- [ ] **Step 2: 写 build.rs（DLL 部署，间接依赖 ec-utls 的 DLL）**

```rust
fn main() {
    // ec-proxy 间接依赖 ec-utls（经 ec-protocol），运行时需要 utls-bridge.dll。
    // 复用 ec-protocol/build.rs 的拷贝逻辑：从 utls-bridge 拷到 deps/。
    let src = std::path::PathBuf::from("../utls-bridge/ec_utls_bridge.dll");
    if let Ok(out_dir) = std::env::var("OUT_DIR") {
        let out_path = std::path::PathBuf::from(&out_dir);
        if let Some(debug_dir) = out_path.ancestors().nth(3) {
            let deps_dir = debug_dir.join("deps");
            let _ = std::fs::create_dir_all(&deps_dir);
            if src.exists() {
                let _ = std::fs::copy(&src, deps_dir.join("ec_utls_bridge.dll"));
                let _ = std::fs::copy(&src, debug_dir.join("ec_utls_bridge.dll"));
            }
        }
    }
    println!("cargo:rerun-if-changed=../utls-bridge/ec_utls_bridge.dll");
}
```

- [ ] **Step 3: 写 lib.rs 占位**

```rust
//! EasyConnect SOCKS5 代理出口（阶段 3）。
//!
//! 用 smoltcp 用户态 TCP/IP 栈把 L3Conn（IP 包流）转成 TCP 会话，
//! 外挂本地 SOCKS5 server，让应用通过 socks5 访问内网。

pub mod device;
pub mod netstack;
pub mod proxy;
pub mod socks5;
pub mod stream;
```

- [ ] **Step 4: 创建各模块空文件占位（让 crate 能编译）**

每个文件先放一行占位，后续 Task 填充：
- `src/device.rs`：`// Task 2 填充`
- `src/netstack.rs`：`// Task 3 填充`
- `src/stream.rs`：`// Task 4 填充`
- `src/socks5.rs`：`// Task 5 填充`
- `src/proxy.rs`：`// Task 6 填充`

- [ ] **Step 5: 验证编译**

Run:
```bash
cd D:/dev_code/study/rust_connect/ec-proxy
export PATH="/d/dev_evn/mingw64/bin:/d/dev_evn/Go/bin:$PATH"
export LIBCLANG_PATH="D:/dev_evn/cangjie/third_party/llvm/bin"
export BINDGEN_EXTRA_CLANG_ARGS="-I/d/dev_evn/mingw64/lib/gcc/x86_64-w64-mingw32/14.2.0/include -I/d/dev_evn/mingw64/x86_64-w64-mingw32/include"
cargo build
```
Expected: 编译通过（smoltcp 0.12 依赖拉取成功）。

---

## Task 2: L3ConnDevice（smoltcp Device + 桥接线程）

**Files:**
- Modify: `ec-proxy/src/device.rs`

**背景**：smoltcp 的 `phy::Device` trait（mod.rs:340）是同步的，被 `iface.poll()` 调用。L3Conn 也是同步阻塞的（Go DLL）。把 L3Conn 放独立 OS 线程，通过 channel 与 Device 通信。参考 netstack-smoltcp/src/device.rs 的 channel 模型，但我们的 Device 既有入站（L3Conn→栈）又有出站（栈→L3Conn）。

- [ ] **Step 1: 写 L3ConnDevice 完整实现**

替换 `src/device.rs` 全部内容：

```rust
//! L3ConnDevice：把同步阻塞的 L3Conn 包成 smoltcp 的 phy::Device。
//!
//! 独立 OS 线程跑 L3Conn 读写循环；Device 内部用互斥队列缓冲入站包、
//! 用 std::sync::mpsc 发出站包给桥接线程。

use std::collections::VecDeque;
use std::sync::{mpsc, Arc, Mutex};
use std::thread;

use smoltcp::phy::{self, Device, DeviceCapabilities, Medium, RxToken, TxToken};
use smoltcp::time::Instant;

/// 入站队列：桥接线程推入，Device::receive 取出（喂给 smoltcp）。
type IngressQueue = Arc<Mutex<VecDeque<Vec<u8>>>>;

/// L3Conn 桥接线程的句柄。
pub struct BridgeHandle {
    /// 发出站包给桥接线程（栈→L3Conn）。
    egress_tx: mpsc::Sender<Vec<u8>>,
    /// 桥接线程 join 句柄（用于关闭时等待）。
    join: Option<thread::JoinHandle<()>>,
}

impl Drop for BridgeHandle {
    fn drop(&mut self) {
        // 丢弃 egress_tx 会关闭 channel，桥接线程的 recv 会返回 Err 并退出。
        // （此处不显式 drop，靠 Drop 自然释放）
    }
}

/// smoltcp Device 实现：以 L3Conn 为 IP 包源/汇。
pub struct L3ConnDevice {
    ingress: IngressQueue,
    egress_tx: mpsc::Sender<Vec<u8>>,
    mtu: usize,
}

impl L3ConnDevice {
    /// 启动桥接：spawn OS 线程跑 L3Conn 读写循环，返回 Device 和句柄。
    ///
    /// `l3conn` 会被移动到桥接线程。`mtu` 应与服务器一致（zju-connect 用 1400）。
    pub fn spawn<L: ec_protocol::l3conn::L3ConnLike + Send + 'static>(
        mut l3conn: L,
        mtu: usize,
    ) -> (Self, BridgeHandle) {
        let ingress: IngressQueue = Arc::new(Mutex::new(VecDeque::new()));
        let (egress_tx, egress_rx) = mpsc::channel::<Vec<u8>>();

        let ingress_clone = Arc::clone(&ingress);
        let join = thread::Builder::new()
            .name("l3conn-bridge".into())
            .spawn(move || {
                bridge_loop(&mut l3conn, egress_rx, ingress_clone);
            })
            .expect("spawn l3conn-bridge");

        let device = L3ConnDevice {
            ingress,
            egress_tx,
            mtu,
        };
        let handle = BridgeHandle {
            egress_tx: device.egress_tx.clone(),
            join: Some(join),
        };
        (device, handle)
    }

    /// 给 poll 循环用的「是否有入站包」查询（避免空转 poll）。
    pub fn has_ingress(&self) -> bool {
        !self.ingress.lock().unwrap().is_empty()
    }
}

impl Device for L3ConnDevice {
    type RxToken<'a> = L3RxToken;
    type TxToken<'a> = L3TxToken<'a>;

    fn receive(&mut self, _timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        let pkt = self.ingress.lock().unwrap().pop_front();
        pkt.map(|p| {
            (
                L3RxToken { packet: Some(p) },
                L3TxToken {
                    tx: self.egress_tx.clone(),
                },
            )
        })
    }

    fn transmit(&mut self, _timestamp: Instant) -> Option<Self::TxToken<'_>> {
        Some(L3TxToken {
            tx: self.egress_tx.clone(),
        })
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.medium = Medium::Ip; // L3 模式，无 Ethernet 头
        caps.max_transmission_unit = self.mtu;
        caps
    }
}

/// 入站 token：持有一个 IP 包，consume 时交给 smoltcp 处理。
pub struct L3RxToken {
    packet: Option<Vec<u8>>,
}

impl RxToken for L3RxToken {
    fn consume<R, F>(mut self, f: F) -> R
    where
        F: FnOnce(&[u8]) -> R,
    {
        let pkt = self.packet.take().expect("rx token consumed twice");
        f(&pkt)
    }
}

/// 出站 token：持有 egress 发送端，consume 时把包发给桥接线程。
pub struct L3TxToken<'a> {
    tx: &'a mpsc::Sender<Vec<u8>>,
}

impl<'a> TxToken for L3TxToken<'a> {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let mut buf = vec![0u8; len];
        let result = f(&mut buf);
        // 忽略发送错误（桥接线程退出时 channel 关闭）；poll 循环会重试。
        let _ = self.tx.send(buf);
        result
    }
}

/// 桥接线程主循环：L3Conn 读写 ↔ channel。
///
/// 注意：L3Conn::read 是阻塞的。为避免写出站包时阻塞读循环，
/// 用 try_recv 非阻塞取写出站包，read 阻塞等待入站包。
/// 这意味着写出站包有延迟（要等下一个入站包唤醒）。
/// 解决：单独的写线程 + 读线程分离（见下方双线程版本）。
fn bridge_loop<L: ec_protocol::l3conn::L3ConnLike>(
    l3conn: &mut L,
    egress_rx: mpsc::Receiver<Vec<u8>>,
    ingress: IngressQueue,
) {
    // 先处理待发出的包（非阻塞），再阻塞读入站包。
    // 局限：write 依赖 read 唤醒。完整方案见 Step 2 的读写线程分离。
    loop {
        // 非阻塞取所有待发写出站包
        while let Ok(pkt) = egress_rx.try_recv() {
            if l3conn.write_all(&pkt).is_err() {
                return;
            }
        }
        // 阻塞读一个入站包
        let mut buf = [0u8; 1400];
        match l3conn.read(&mut buf) {
            Ok(0) => return, // EOF
            Ok(n) => {
                ingress.lock().unwrap().push_back(buf[..n].to_vec());
            }
            Err(_) => return,
        }
    }
}
```

- [ ] **Step 2: 在 ec-protocol 的 L3Conn 加 trait 抽象（供 ec-proxy 解耦）**

ec-proxy 不应直接依赖 `L3Conn` 具体类型（测试时要用 mock）。在 `ec-protocol/src/l3conn.rs` 末尾加一个 trait：

```rust
/// 字节流读写抽象，供上层（ec-proxy）解耦 L3Conn 具体类型。
/// L3Conn 已实现 Read/Write，自动满足此 trait。
pub trait L3ConnLike: std::io::Read + std::io::Write + Send {
    // marker trait，无额外方法
}

impl L3ConnLike for L3Conn {}
```

并在 `ec-protocol/src/lib.rs` 导出：
```rust
pub use l3conn::{L3Conn, L3ConnLike};
```
（`L3Conn` 原本就在 l3conn 模块，加 pub re-export）

- [ ] **Step 3: 验证编译**

Run:
```bash
cd D:/dev_code/study/rust_connect/ec-proxy
export PATH="/d/dev_evn/mingw64/bin:/d/dev_evn/Go/bin:$PATH"
export LIBCLANG_PATH="D:/dev_evn/cangjie/third_party/llvm/bin"
export BINDGEN_EXTRA_CLANG_ARGS="-I/d/dev_evn/mingw64/lib/gcc/x86_64-w64-mingw32/14.2.0/include -I/d/dev_evn/mingw64/x86_64-w4-mingw32/include"
cargo build
```
Expected: 编译通过。若报 smoltcp feature 缺失，确认 Cargo.toml 的 features 列表正确。

- [ ] **Step 4: 单元测试（mock L3Conn 验证 Device 喂/取包）**

在 `ec-proxy/src/device.rs` 末尾加：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Read, Write};

    /// 用 Cursor<Vec<u8>> 模拟 L3Conn 的读写。
    struct MockL3 {
        read_buf: Cursor<Vec<u8>>,
        write_buf: Vec<u8>,
    }
    impl Read for MockL3 {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.read_buf.read(buf)
        }
    }
    impl Write for MockL3 {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.write_buf.write(buf)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl L3ConnLike for MockL3 {}

    #[test]
    fn device_receives_packets_from_l3conn() {
        // 模拟 L3Conn 读到两个 IP 包
        let mut mock = MockL3 {
            read_buf: Cursor::new(vec![0x45u8; 20]), // 假 IP 包
            write_buf: vec![],
        };
        // spawn 需要 'static，这里用测试变体直接构造
        // （真实测试在 Task 6 集成测试里做，这里验证 Device 基本逻辑）
        let ingress: IngressQueue = Arc::new(Mutex::new(VecDeque::new()));
        ingress.lock().unwrap().push_back(vec![0x45; 20]);

        let (egress_tx, _egress_rx) = mpsc::channel();
        let mut device = L3ConnDevice {
            ingress,
            egress_tx,
            mtu: 1400,
        };
        // receive 应取出入站包
        let (rx, _tx) = device.receive(Instant::from_millis(0)).expect("应有入站包");
        let pkt = rx.consume(|b| b.to_vec());
        assert_eq!(pkt, vec![0x45; 20]);
        // 第二次 receive 应为 None（队列空了）
        assert!(device.receive(Instant::from_millis(1)).is_none());
    }

    #[test]
    fn device_transmit_sends_to_egress() {
        let ingress: IngressQueue = Arc::new(Mutex::new(VecDeque::new()));
        let (egress_tx, egress_rx) = mpsc::channel();
        let mut device = L3ConnDevice {
            ingress,
            egress_tx,
            mtu: 1400,
        };
        let tx = device.transmit(Instant::from_millis(0)).expect("transmit 应返回 token");
        tx.consume(5, |buf| {
            buf.copy_from_slice(&[1, 2, 3, 4, 5]);
        });
        // 出站包应到达 egress_rx
        let sent = egress_rx.recv().expect("应收到出站包");
        assert_eq!(sent, vec![1, 2, 3, 4, 5]);
    }
}
```

- [ ] **Step 5: 运行测试**

Run:
```bash
cd D:/dev_code/study/rust_connect/ec-proxy
cargo test device -- --nocapture
```
Expected: 2 个测试通过。

---

## Task 3: NetStack（Interface + SocketSet + dial_tcp）

**Files:**
- Modify: `ec-proxy/src/netstack.rs`

**背景**：驱动 smoltcp 的 `Interface`（含 client_ip 作源地址）+ `SocketSet`，提供出站 `dial_tcp`。poll 循环周期性调 `iface.poll(timestamp, device, sockets)`。参考 netstack-smoltcp/src/tcp.rs 的 poll 逻辑，但只做出站（不需要入站 SYN 监听）。

- [ ] **Step 1: 写 NetStack 结构与构造**

替换 `src/netstack.rs` 全部内容（本 Task 先写构造 + poll 循环骨架，dial_tcp 在 Step 3）：

```rust
//! NetStack：smoltcp Interface + SocketSet，提供出站 TCP。
//!
//! 对照 zju-connect 的 gVisor Stack：驱动 TCP/IP 栈，把出站 TCP 流转成 IP 包。
//! poll 循环在 tokio task 里跑，周期性调 iface.poll。

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex as StdMutex};
use std::task::Waker;

use smoltcp::iface::{Config, Interface, SocketHandle, SocketSet};
use smoltcp::phy::Device;
use smoltcp::socket::tcp::{Socket as TcpSocket, SocketBuffer as TcpSocketBuffer, State as TcpState};
use smoltcp::storage::RingBuffer;
use smoltcp::time::{Duration, Instant};
use smoltcp::wire::{HardwareAddress, IpAddress, IpCidr, Ipv4Address};
use tokio::sync::Notify;

use crate::device::L3ConnDevice;
use crate::stream::{NetStream, SharedControl, TcpSocketControl, TcpSocketState};

/// smoltcp 栈。持有 Interface、SocketSet、Device，跑 poll 循环。
pub struct NetStack {
    /// poll 循环停止信号。
    stop: Arc<Notify>,
}

/// poll 循环与 NetStream 共享的 per-socket 控制块映射。
pub(crate) type SocketMap = Arc<StdMutex<HashMap<SocketHandle, SharedControl>>>;

impl NetStack {
    /// 创建并启动 NetStack。
    ///
    /// `device` 是已 spawn 桥接线程的 L3ConnDevice。
    /// `client_ip` 是 RequestIP 拿到的 4 字节 IP（作 smoltcp 源地址）。
    /// `client_port_base` 是出站连接的本地端口起始值（自增分配）。
    ///
    /// 返回 (NetStack, NetStackHandle)，handle 用于 dial_tcp。
    pub fn new(mut device: L3ConnDevice, client_ip: [u8; 4]) -> std::io::Result<(Self, NetStackHandle)> {
        // 创建 Interface（IP medium，绑定 client_ip）
        let mut iface_config = Config::new(HardwareAddress::Ip);
        iface_config.random_seed = rand::random();
        let mut iface = Interface::new(iface_config, &mut device, Instant::now());
        iface.update_ip_addrs(|addrs| {
            addrs.push(IpCidr::new(IpAddress::v4(
                client_ip[0], client_ip[1], client_ip[2], client_ip[3],
            ), 32)).expect("iface client ip");
        });
        // 默认路由：所有流量走这个虚拟网卡
        iface.routes_mut()
            .add_default_ipv4_route(Ipv4Address::new(0, 0, 0, 1))
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::AddrNotAvailable, e))?;
        iface.set_any_ip(true);

        let socket_set = SocketSet::new(vec![]);
        let sockets: SocketMap = Arc::new(StdMutex::new(HashMap::new()));

        let stop = Arc::new(Notify::new());
        let stop_clone = Arc::clone(&stop);

        // dial 请求通道：SOCKS5 → poll 循环
        let (dial_tx, dial_rx) = tokio::sync::mpsc::unbounded_channel::<DialRequest>();

        // 启动 poll 循环（tokio task）
        tokio::spawn(poll_loop(
            iface,
            device,
            socket_set,
            sockets.clone(),
            dial_rx,
            stop_clone,
        ));

        let handle = NetStackHandle {
            dial_tx,
            sockets,
        };
        Ok((Self { stop }, handle))
    }

    /// 停止 poll 循环。
    pub async fn stop(&self) {
        self.stop.notify_waiters();
    }
}

/// 出站拨号请求（SOCKS5 → poll 循环）。
struct DialRequest {
    remote: SocketAddr,
    /// 完成回调：poll 循环建好 socket 后通过它返回 NetStream。
    done: tokio::sync::oneshot::Sender<std::io::Result<(SocketHandle, SharedControl)>>,
}

/// SOCKS5 侧持有的句柄，用于发起 dial。
pub struct NetStackHandle {
    dial_tx: tokio::sync::mpsc::UnboundedSender<DialRequest>,
    sockets: SocketMap,
}

impl NetStackHandle {
    /// 发起出站 TCP 连接，返回 NetStream（AsyncRead/AsyncWrite）。
    pub async fn dial_tcp(&self, remote: SocketAddr) -> std::io::Result<NetStream> {
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        self.dial_tx.send(DialRequest { remote, done: done_tx })
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "netstack 已停止"))?;
        let (handle, control) = done_rx.await
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "dial 无响应"))??;
        Ok(NetStream::new(handle, control))
    }
}
```

- [ ] **Step 2: 写 poll 循环（处理 dial 请求 + socket 状态 + 数据搬运）**

在 `src/netstack.rs` 追加 poll_loop 函数：

```rust
/// poll 循环：驱动 smoltcp Interface，处理 dial 请求和 socket 数据。
///
/// 这是整个栈的核心。参考 netstack-smoltcp/src/tcp.rs handle_socket。
async fn poll_loop(
    mut iface: Interface,
    mut device: L3ConnDevice,
    mut socket_set: SocketSet<'static>,
    sockets: SocketMap,
    mut dial_rx: tokio::sync::mpsc::UnboundedReceiver<DialRequest>,
    stop: Arc<Notify>,
) {
    loop {
        // 1. 处理新的 dial 请求（非阻塞）
        while let Ok(req) = dial_rx.try_recv() {
            match create_outbound_socket(&mut socket_set, req.remote, sockets.clone()) {
                Ok((handle, control)) => {
                    let _ = req.done.send(Ok((handle, control)));
                }
                Err(e) => {
                    let _ = req.done.send(Err(e));
                }
            }
        }

        // 2. 驱动 smoltcp（关键：传入单调时钟）
        let now = Instant::now();
        let _ = iface.poll(now, &mut device, &mut socket_set);

        // 3. 搬运 socket 数据：smoltcp socket ↔ RingBuffer（NetStream 读写）
        let mut to_remove = Vec::new();
        {
            let mut sock_map = sockets.lock().unwrap();
            for (&handle, control_arc) in sock_map.iter() {
                let socket = socket_set.get_mut::<TcpSocket>(handle);
                let mut control = control_arc.lock();

                // socket 关闭 → 标记并唤醒
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
                    let r = socket.recv(|buf| {
                        (control.recv_buffer.enqueue_slice(buf), ())
                    });
                    if r.is_err() {
                        socket.abort();
                        control.recv_state = TcpSocketState::Closed;
                        break;
                    }
                }
                wake(&control.recv_waker);

                // send: send_buffer → socket
                while socket.can_send() && !control.send_buffer.is_empty() {
                    let r = socket.send(|buf| {
                        (control.send_buffer.dequeue_slice(buf), ())
                    });
                    if r.is_err() {
                        socket.abort();
                        control.send_state = TcpSocketState::Closed;
                        break;
                    }
                }
                // SHUT_WR：send_buffer 空了且应用关闭了写半部
                if matches!(control.send_state, TcpSocketState::Close) && control.send_buffer.is_empty() {
                    socket.close();
                    control.send_state = TcpSocketState::Closing;
                }
                wake(&control.send_waker);
            }
        }

        // 清理已关闭的 socket
        for h in to_remove {
            sockets.lock().unwrap().remove(&h);
            socket_set.remove(h);
        }

        // 4. 等待下一轮：有时间就睡，有入站包立即醒
        let has_ingress = device.has_ingress();
        let poll_delay = iface.poll_delay(Instant::now(), &socket_set)
            .unwrap_or(Duration::from_millis(5));
        if has_ingress || poll_delay == Duration::ZERO {
            tokio::task::yield_now().await;
        } else {
            let _ = tokio::time::timeout(
                tokio::time::Duration::from(poll_delay),
                stop.notified(),
            ).await;
            if stop.notified().now_or_never().is_some() {
                break; // 收到停止信号
            }
        }
    }
}

fn wake(waker_slot: &Option<Waker>) {
    if let Some(w) = waker {
        w.wake_by_ref();
    }
}
```

- [ ] **Step 3: 写 create_outbound_socket（dial 核心：TcpSocket::connect）**

在 `src/netstack.rs` 追加：

```rust
/// 创建出站 TCP socket 并发起连接。
///
/// 对照 zju-connect gVisor 的 gonet.DialTCP。
/// smoltcp 的 connect 会产生 SYN 包，经 Device 转发给 L3Conn。
fn create_outbound_socket(
    socket_set: &mut SocketSet<'static>,
    remote: SocketAddr,
    sockets: SocketMap,
) -> std::io::Result<(SocketHandle, SharedControl)> {
    let recv_buf = 0x3FFF * 20; // ~320KB，同 netstack-smoltcp 默认
    let send_buf = recv_buf;

    let mut socket = TcpSocket::new(
        TcpSocketBuffer::new(vec![0u8; recv_buf as usize]),
        TcpSocketBuffer::new(vec![0u8; send_buf as usize]),
    );
    socket.set_keep_alive(Some(Duration::from_secs(28)));
    socket.set_timeout(Some(Duration::from_secs(7200)));

    // smoltcp connect：remote_endpoint=目标，local_endpoint=源（用 client_ip，端口自选）
    let remote_ep = smoltcp::wire::IpEndpoint::new(
        match remote {
            SocketAddr::V4(v4) => smoltcp::wire::IpAddress::Ipv4(smoltcp::wire::Ipv4Address(v4.ip().octets())),
            SocketAddr::V6(_) => {
                return Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "IPv6 暂不支持"));
            }
        },
        remote.port(),
    );
    // local endpoint：源 IP 由 Interface 的 any_ip 自动选，端口用 0 让 smoltcp 分配
    // 但 connect 需要非零 local port，用一个随机高位端口
    let local_port: u16 = (rand::random::<u16>() | 0x8000) + 1; // 32769..65536
    let local_ep = smoltcp::wire::IpListenEndpoint::from(local_port);

    // connect 需要 Context（smoltcp 0.12 API）
    // 注意：connect 在 socket 加入 SocketSet 前调用，传入 local endpoint
    socket
        .connect(smoltcp::iface::Context::new(/* 见下方说明 */), remote_ep, local_ep)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::ConnectionRefused, e))?;

    let handle = socket_set.add(socket);

    let control = Arc::new(spin::Mutex::new(TcpSocketControl {
        send_buffer: RingBuffer::new(vec![0u8; send_buf as usize]),
        send_waker: None,
        recv_buffer: RingBuffer::new(vec![0u8; recv_buf as usize]),
        recv_waker: None,
        recv_state: TcpSocketState::Normal,
        send_state: TcpSocketState::Normal,
    }));

    sockets.lock().unwrap().insert(handle, Arc::clone(&control));
    Ok((handle, control))
}
```

**已验证（smoltcp spike 编译通过）**：smoltcp 0.12 的 `connect` 签名是 `connect(cx: &mut Context, remote, local)`，Context 通过 `iface.context()`（返回 `&mut InterfaceInner`）获取。关键约束：`iface.context()` 借用 `&mut iface`，所以 connect 必须在持有 iface 的 poll_loop 内调用。`create_outbound_socket` 不能是独立函数（拿不到 iface），其逻辑要**内联到 poll_loop 的 dial 处理分支**：

```rust
// poll_loop 内处理 dial 请求（替换 Task 3 Step 2 的 while 循环）：
while let Ok(req) = dial_rx.try_recv() {
    let recv_buf = 0x3FFF * 20usize;
    let mut socket = TcpSocket::new(
        SocketBuffer::new(vec![0u8; recv_buf]),
        SocketBuffer::new(vec![0u8; recv_buf]),
    );
    socket.set_keep_alive(Some(Duration::from_secs(28)));
    socket.set_timeout(Some(Duration::from_secs(7200)));

    let remote_ep = match req.remote {
        SocketAddr::V4(v4) => IpEndpoint::new(
            IpAddress::Ipv4(Ipv4Address(v4.ip().octets())),
            v4.port(),
        ),
        SocketAddr::V6(_) => {
            let _ = req.done.send(Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported, "IPv6 暂不支持")));
            continue;
        }
    };
    let local_port: u16 = (rand::random::<u16>() | 0x8000) + 1;

    // 关键：connect 在此处调用，用 iface.context()，借用 iface
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
                std::io::ErrorKind::ConnectionRefused, e)));
        }
    }
}
```
（`create_outbound_socket` 函数删除，逻辑内联到 poll_loop。Task 3 Step 2 的 poll_loop 把 dial 处理分支替换为上述代码。）

- [ ] **Step 4: 验证编译（预期可能有 smoltcp API 细节需微调）**

Run:
```bash
cd D:/dev_code/study/rust_connect/ec-proxy
cargo build
```
Expected: 编译通过或仅有 smoltcp connect/Context 的 API 适配问题。若 connect 签名不符，参考 Step 3 说明调整（把 connect 移入 poll_loop 内调 `iface.context()`）。

---

## Task 4: NetStream（AsyncRead/AsyncWrite）

**Files:**
- Modify: `ec-proxy/src/stream.rs`

**背景**：单个出站连接的 async 读写封装。与 poll 循环共享 `SharedControl`（RingBuffer + Waker），参考 netstack-smoltcp/src/tcp.rs 的 TcpStream + TcpSocketControl。

- [ ] **Step 1: 写 TcpSocketControl 与 SharedControl**

替换 `src/stream.rs` 全部内容：

```rust
//! NetStream：出站 TCP 连接的 async 读写封装。
//!
//! 与 poll 循环共享 TcpSocketControl（RingBuffer + Waker）。
//! 参考 netstack-smoltcp/src/tcp.rs 的 TcpStream。

use std::sync::Arc;
use std::task::Waker;

use smoltcp::iface::SocketHandle;
use smoltcp::storage::RingBuffer;
use spin::Mutex as SpinMutex;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum TcpSocketState {
    Normal,
    Close,
    Closing,
    Closed,
}

/// per-socket 控制块：poll 循环与 NetStream 之间的数据通道。
pub struct TcpSocketControl {
    pub send_buffer: RingBuffer<'static, u8>,  // 应用 → socket
    pub send_waker: Option<Waker>,
    pub recv_buffer: RingBuffer<'static, u8>,  // socket → 应用
    pub recv_waker: Option<Waker>,
    pub recv_state: TcpSocketState,
    pub send_state: TcpSocketState,
}

pub type SharedControl = Arc<SpinMutex<TcpSocketControl>>;

/// 出站 TCP 连接流。实现 AsyncRead/AsyncWrite。
pub struct NetStream {
    handle: SocketHandle,
    control: SharedControl,
}

impl NetStream {
    pub(crate) fn new(handle: SocketHandle, control: SharedControl) -> Self {
        Self { handle, control }
    }

    /// 对应的 smoltcp socket handle（poll 循环清理用）。
    pub fn handle(&self) -> SocketHandle {
        self.handle
    }
}

impl Drop for NetStream {
    fn drop(&mut self) {
        // 关闭连接：标记 send/recv 为 Close，唤醒 poll 循环处理。
        let mut control = self.control.lock();
        if matches!(control.recv_state, TcpSocketState::Normal) {
            control.recv_state = TcpSocketState::Close;
        }
        if matches!(control.send_state, TcpSocketState::Normal) {
            control.send_state = TcpSocketState::Close;
        }
        // poll 循环在下次轮询时会检测到 Close 并调 socket.close()
    }
}

impl AsyncRead for NetStream {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let mut control = self.control.lock();

        if control.recv_buffer.is_empty() {
            // 已关闭 → EOF
            if matches!(control.recv_state, TcpSocketState::Closed) {
                return std::task::Poll::Ready(Ok(()));
            }
            // 等数据
            if let Some(old) = control.recv_waker.replace(cx.waker().clone()) {
                if !old.will_wake(cx.waker()) {
                    old.wake();
                }
            }
            return std::task::Poll::Pending;
        }

        let n = control.recv_buffer.dequeue_slice(buf.initialize_unfilled());
        buf.advance(n);
        std::task::Poll::Ready(Ok(()))
    }
}

impl AsyncWrite for NetStream {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        let mut control = self.control.lock();

        if !matches!(control.send_state, TcpSocketState::Normal) {
            return std::task::Poll::Ready(Err(std::io::ErrorKind::BrokenPipe.into()));
        }

        if control.send_buffer.is_full() {
            if let Some(old) = control.send_waker.replace(cx.waker().clone()) {
                if !old.will_wake(cx.waker()) {
                    old.wake();
                }
            }
            return std::task::Poll::Pending;
        }

        let n = control.send_buffer.enqueue_slice(buf);
        std::task::Poll::Ready(Ok(n))
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let mut control = self.control.lock();
        if matches!(control.send_state, TcpSocketState::Closed) {
            return std::task::Poll::Ready(Ok(()));
        }
        if matches!(control.send_state, TcpSocketState::Normal) {
            control.send_state = TcpSocketState::Close;
        }
        if let Some(old) = control.send_waker.replace(cx.waker().clone()) {
            old.wake();
        }
        std::task::Poll::Pending
    }
}
```

- [ ] **Step 2: 加 spin 依赖到 Cargo.toml**

在 `ec-proxy/Cargo.toml` 的 `[dependencies]` 加：
```toml
spin = "0.9"
```

- [ ] **Step 3: 验证编译**

Run:
```bash
cd D:/dev_code/study/rust_connect/ec-proxy
cargo build
```
Expected: 编译通过。

---

## Task 5: SOCKS5 server（用 fast-socks5 库）

**Files:**
- Modify: `ec-proxy/Cargo.toml`（加 fast-socks5 依赖）
- Modify: `ec-proxy/src/socks5.rs`
- Modify: `ec-proxy/src/netstack.rs`（NetStackHandle 加 Clone）

**背景**：用 `fast-socks5`（v0.10）处理 SOCKS5 协议（握手、命令解析、错误回复），我们只接管拨号这一步。关键配置：`set_execute_command(false)` 关掉内置拨号（它用系统 TCP），`set_dns_resolve(false)` 关掉内置 DNS。握手后用 `socket.target_addr()` 和 `socket.cmd()` 取目标和命令，自己用 NetStack 拨号。

- [ ] **Step 1: 加 fast-socks5 依赖到 Cargo.toml**

在 `ec-proxy/Cargo.toml` 的 `[dependencies]` 加：
```toml
fast-socks5 = "0.10"
```

- [ ] **Step 2: 给 NetStackHandle 加 Clone（per-connection task 需要）**

在 `src/netstack.rs` 的 `NetStackHandle` 加 Clone 实现：

```rust
impl Clone for NetStackHandle {
    fn clone(&self) -> Self {
        NetStackHandle {
            dial_tx: self.dial_tx.clone(),
            sockets: Arc::clone(&self.sockets),
        }
    }
}
```

- [ ] **Step 3: 写 SOCKS5 server（fast-socks5 + 自定义拨号）**

替换 `src/socks5.rs` 全部内容：

```rust
//! SOCKS5 server（用 fast-socks5 库处理协议，自定义拨号走 NetStack）。
//!
//! fast-socks5 负责：握手、方法协商、命令解析、错误回复。
//! 我们负责：CONNECT 时用 NetStack::dial_tcp 出站拨号（走 VPN 隧道），而非系统 TCP。
//!
//! 对照 zju-connect service/socks.go（用 go-socks5 库 + 自定义 Dialer）。

use fast_socks5::server::{Config, Socks5Server};
use fast_socks5::Socks5Command;
use std::net::SocketAddr;
use tokio::io::AsyncWriteExt;

use crate::netstack::NetStackHandle;

/// 启动 SOCKS5 server。阻塞直到 listener 关闭。
///
/// `listener` 已绑定好端口。`handle` 用于出站拨号（Clone 后分发到各连接 task）。
pub async fn serve_socks5(
    listener: tokio::net::TcpListener,
    handle: NetStackHandle,
) -> std::io::Result<()> {
    // fast-socks5 配置：关掉内置拨号和 DNS（我们接管）。
    let mut config = Config::<fast_socks5::server::DenyAuthentication>::default();
    config.set_execute_command(false); // 不让 fast-socks5 自己用系统 TCP 拨号
    config.set_dns_resolve(false);     // 不让 fast-socks5 自己解析域名

    let server = Socks5Server::<fast_socks5::server::DenyAuthentication>::with_config(config)
        .listener_from_tokio(listener);

    let mut incoming = server.incoming();
    while let Some(sock) = incoming.next().await {
        match sock {
            Ok(mut sock) => {
                let handle = handle.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_connection(&mut sock, &handle).await {
                        log::debug!("[socks5] 连接处理失败: {e}");
                    }
                });
            }
            Err(e) => log::warn!("[socks5] accept 失败: {e}"),
        }
    }
    Ok(())
}

/// 处理单个 SOCKS5 连接：握手 → 拨号 → 双向转发。
///
/// `sock` 是 fast-socks5 的 Socks5Socket（含 listener 产生的 TcpStream）。
async fn handle_connection(
    sock: &mut fast_socks5::server::Socks5Socket<
        tokio::net::TcpStream,
        fast_socks5::server::DenyAuthentication,
    >,
    handle: &NetStackHandle,
) -> std::io::Result<()> {
    // 1. 握手 + 解析请求（不执行内置命令，因为我们 set_execute_command(false)）
    sock.upgrade_to_socks5()
        .await
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;

    // 2. 取命令和目标地址
    let cmd = sock.cmd().cloned();
    let target_addr = sock.target_addr().cloned();

    let cmd = cmd.ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "无命令"))?;
    if cmd != Socks5Command::TCPConnect {
        return Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "仅支持 CONNECT"));
    }

    let target_addr = target_addr
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "无目标地址"))?;

    // 首轮只支持 IP 目标（域名需 DNS 中转，留待后续）。
    // TargetAddr::Ip 直接是 SocketAddr；Domain 转换需要 DNS。
    let target: SocketAddr = match target_addr {
        fast_socks5::util::target_addr::TargetAddr::Ip(addr) => addr,
        fast_socks5::util::target_addr::TargetAddr::Domain(_, _) => {
            log::debug!("[socks5] 域名目标暂不支持（需 DNS 中转）");
            return Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "域名目标暂不支持"));
        }
    };

    log::info!("[socks5] CONNECT {}", target);

    // 3. 用 NetStack 出站拨号（走 VPN 隧道，非系统 TCP）
    let mut remote = match handle.dial_tcp(target).await {
        Ok(s) => s,
        Err(e) => {
            log::warn!("[socks5] 拨号 {} 失败: {e}", target);
            return Err(e);
        }
    };

    // 4. 向客户端发 SOCKS5 成功响应
    //    fast-socks5 的 reply 写在 sock.inner 上。手动写 RFC1928 成功回复。
    sock.get_mut()
        .write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
        .await?;

    // 5. 双向转发：client（sock.inner）↔ remote（NetStream）
    let mut client = sock.get_mut();
    tokio::io::copy_bidirectional(&mut client, &mut remote).await?;

    Ok(())
}
```

**注意**：`fast_socks5::server::Socks5Server` 的构造和 `incoming()` 返回的 `Socks5Socket` 的具体 API（`listener_from_tokio`、`get_mut`）可能因版本略有差异。实现时若 API 不匹配，参考 fast-socks5 的 examples（`src/server.rs:895` 的测试用例）和 docs。核心思路不变：`set_execute_command(false)` + 手动 `upgrade_to_socks5` + 取 target_addr/cmd + 自定义拨号 + reply + copy。

- [ ] **Step 4: 验证编译**

Run:
```bash
cd D:/dev_code/study/rust_connect/ec-proxy
export PATH="/d/dev_evn/mingw64/bin:/d/dev_evn/Go/bin:$PATH"
export LIBCLANG_PATH="D:/dev_evn/cangjie/third_party/llvm/bin"
export BINDGEN_EXTRA_CLANG_ARGS="-I/d/dev_evn/mingw64/lib/gcc/x86_64-w64-mingw32/14.2.0/include -I/d/dev_evn/mingw64/x86_64-w64-mingw32/include"
cargo build
```
Expected: 编译通过。若 fast-socks5 的 `Socks5Server` 构造或 `Socks5Socket` 方法名不符，按编译器提示调整（查 fast-socks5 源码 `server.rs`）。

---

## Task 6: Proxy 顶层 + 端到端 example

**Files:**
- Modify: `ec-proxy/src/proxy.rs`
- Create: `ec-proxy/examples/socks_demo.rs`

- [ ] **Step 1: 写 Proxy 顶层（组装各组件）**

替换 `src/proxy.rs` 全部内容：

```rust
//! Proxy 顶层：组装 L3Conn + NetStack + SOCKS5。
//!
//! 完整流程：登录 → token → RequestIP → L3Conn → NetStack(device) → SOCKS5。

use ec_protocol::{l3conn::L3Conn, token, tunnel};

use crate::device::L3ConnDevice;
use crate::netstack::NetStack;
use crate::socks5;

/// Proxy 配置。
pub struct ProxyConfig {
    /// VPN 服务器，如 "1.2.3.4:44333"。
    pub server: String,
    /// 登录拿到的 TwfID。
    pub twf_id: String,
    /// SOCKS5 监听地址，如 "127.0.0.1:1080"。
    pub socks_bind: String,
}

/// 启动代理。阻塞直到 SOCKS5 server 停止。
pub async fn run(cfg: ProxyConfig) -> std::io::Result<()> {
    // 1. token
    let sid_hex = token::request_token(&cfg.server, &cfg.twf_id)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    let tkn = token::build_token(&sid_hex, &cfg.twf_id)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;

    // 2. RequestIP 拿 client_ip（连接保活持有）
    let ((ip, ip_reverse), _keepalive) = tunnel::request_ip(&cfg.server, &tkn)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    log::info!("client IP: {}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3]);

    // 3. L3Conn
    let l3 = L3Conn::new(&cfg.server, &tkn, &ip_reverse)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;

    // 4. NetStack（L3ConnDevice 桥接 + smoltcp）
    let (device, _bridge) = L3ConnDevice::spawn(l3, 1400);
    let (_stack, handle) = NetStack::new(device, ip)?;

    // 5. SOCKS5
    let listener = tokio::net::TcpListener::bind(&cfg.socks_bind).await?;
    log::info!("SOCKS5 监听 {}", cfg.socks_bind);
    socks5::serve_socks5(listener, handle).await
}
```

- [ ] **Step 2: 写 socks_demo.rs（端到端）**

```rust
//! 端到端：登录 → 隧道 → SOCKS5。
//!
//! 用法：
//!   cargo run --example socks_demo -- \
//!       --server 1.2.3.4:44333 --username username --password password \
//!       --bind 127.0.0.1:1080
//!
//! 启动后另开终端：curl --socks5 127.0.0.1:1080 http://<内网IP> 验证。

use ec_login::{login, LoginConfig, LoginStep};
use ec_proxy::{proxy::{run, ProxyConfig}};

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
        Err(e) => { eprintln!("参数错误: {e}"); std::process::exit(2); }
    };

    // 1. 登录
    println!("[1] 登录...");
    let twf_id = match login(&LoginConfig {
        server: args.server.clone(),
        username: args.username,
        password: args.password,
    }) {
        Ok(LoginStep::Done(twf)) if !twf.is_empty() => twf,
        _ => { eprintln!("登录失败"); std::process::exit(1); }
    };
    println!("    TwfID: {twf_id}");

    // 2. 启动代理（阻塞）
    println!("[2] 启动 SOCKS5 @ {} ...", args.bind);
    if let Err(e) = run(ProxyConfig {
        server: args.server,
        twf_id,
        socks_bind: args.bind,
    }).await {
        eprintln!("代理失败: {e}");
        std::process::exit(1);
    }
}
```

- [ ] **Step 3: ec-login 和 ec-protocol 加 async 支持**

socks_demo 用 `#[tokio::main]`，但 ec-login 的 `login` 是阻塞的（reqwest::blocking）。在 socks_demo 里用 `tokio::task::spawn_blocking` 包裹登录调用。修改 socks_demo 的登录部分：

```rust
    // 登录在阻塞线程跑（reqwest::blocking 不能在 async 上下文）
    let cfg = LoginConfig { server: args.server.clone(), username: args.username, password: args.password };
    let twf_id = match tokio::task::spawn_blocking(move || login(&cfg)).await {
        Ok(Ok(LoginStep::Done(twf))) if !twf.is_empty() => twf,
        _ => { eprintln!("登录失败"); std::process::exit(1); }
    };
```

- [ ] **Step 4: 验证编译**

Run:
```bash
cd D:/dev_code/study/rust_connect/ec-proxy
export PATH="/d/dev_evn/mingw64/bin:/d/dev_evn/Go/bin:$PATH"
export LIBCLANG_PATH="D:/dev_evn/cangjie/third_party/llvm/bin"
export BINDGEN_EXTRA_CLANG_ARGS="-I/d/dev_evn/mingw64/lib/gcc/x86_64-w64-mingw32/14.2.0/include -I/d/dev_evn/mingw64/x86_64-w64-mingw32/include"
cargo build --example socks_demo
```
Expected: 编译通过。

---

## Task 7: 端到端验证 + 集成测试

**Files:**
- Create: `ec-proxy/tests/socks5.rs`

- [ ] **Step 1: 端到端手动验证（连真实服务器）**

Run（终端 1）:
```bash
cd D:/dev_code/study/rust_connect/ec-proxy
export PATH="/d/dev_evn/mingw64/bin:/d/dev_evn/Go/bin:$PATH"
export LIBCLANG_PATH="D:/dev_evn/cangjie/third_party/llvm/bin"
export BINDGEN_EXTRA_CLANG_ARGS="-I/d/dev_evn/mingw64/lib/gcc/x86_64-w64-mingw32/14.2.0/include -I/d/dev_evn/mingw64/x86_64-w64-mingw32/include"
RUST_LOG=info cargo run --example socks_demo -- \
    --server 1.2.3.4:44333 --username username --password password --bind 127.0.0.1:1080
```

Run（终端 2，验证）:
```bash
# 用一个内网 IP 测试（替换为实际内网地址）
curl -v --socks5 127.0.0.1:1080 http://<内网IP>:80 --max-time 10
# 或测 TCP 连通性
curl -v --socks5 127.0.0.1:1080 http://1.1.1.1 --max-time 10
```
Expected:
- 终端 1 日志显示 `[socks5] CONNECT <IP>:<port>`
- 终端 2 收到 HTTP 响应（或至少 TCP 连接建立、数据往返）

**若失败**：优先查 smoltcp 是否产生出站 SYN 包（device 的 egress），L3Conn 是否收到服务端 SYN-ACK 回包（device 的 ingress）。smoltcp connect 的 Context 适配是最可能的坑。

- [ ] **Step 2: 写集成测试（mock L3Conn 验证 SOCKS5 协议层）**

`ec-proxy/tests/socks5.rs`：用 mock NetStack（不连真实服务器）验证 SOCKS5 协议握手和 CONNECT 解析正确。

```rust
//! SOCKS5 协议层测试（不连真实 VPN，验证协议解析）。

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

// 这些测试需要 mock NetStackHandle，比较复杂。
// 首轮优先靠 Task 7 Step 1 的手动端到端验证。
// 协议层单元测试留到架构验证通过后补。

#[tokio::test]
async fn socks5_handshake_reaches_connect() {
    // TODO（架构验证通过后补）：
    // 1. 起一个 mock SOCKS5 server（用真实的 serve_socks5 + mock NetStack）
    // 2. 客户端发 SOCKS5 握手 + CONNECT
    // 3. 断言 dial_tcp 被调用、目标地址正确
}
```

注：SOCKS5 协议层的完整 mock 测试较复杂（需 mock NetStackHandle 的 dial_tcp），首轮靠手动端到端验证。协议正确性已在 Task 5 的代码中体现（对照 RFC 1928）。

- [ ] **Step 3: 验收确认**

对照设计文档第 7 节验收标准：
1. ☐ socks_demo 启动后 SOCKS5 监听 127.0.0.1:1080
2. ☐ curl --socks5 能建立 TCP 连接（至少握手成功）
3. ☐ 能收发数据（curl 收到响应）
4. ☐ 多并发连接工作（curl 连续多次）

---

## 验收标准（阶段 3 完成）

1. `socks_demo` 启动后 SOCKS5 监听 127.0.0.1:1080，日志显示登录成功 + client IP
2. `curl --socks5 127.0.0.1:1080 http://<内网资源IP>` 返回内网内容
3. TCP 连接能维持并收发数据
4. 编译无错误，无 warnings（关键路径）

## 风险与注意事项（实现时重点关注）

1. **smoltcp connect 的 Context 参数**：0.12 的 `connect(cx: &mut Context, ...)` 需要 Context，而 Context 从 Interface 获取。dial 请求处理必须在持有 `&mut Interface` 的 poll_loop 内完成，不能在独立函数里。Task 3 Step 3 的 `create_outbound_socket` 实现时需调整为接收 `&mut Interface`，或把 connect 逻辑内联到 poll_loop。
2. **poll 循环的时间驱动**：`iface.poll(now, ...)` 的 `now` 必须单调递增。用 `Instant::now()`（smoltcp 的 Instant，基于毫秒）。
3. **write 依赖 read 唤醒的局限**：Task 2 的 bridge_loop 单线程版本，写出站包要等入站包唤醒。若发现延迟过大，改为读/写双线程分离（读线程阻塞读 L3Conn，写线程阻塞 recv egress channel）。
4. **smoltcp SocketSet 生命周期**：socket 加入 SocketSet 后拿到 handle，handle 要在 poll_loop 和 NetStream 间共享。注意 remove 时机（socket Closed 后）。
5. **client_port 分配**：出站连接的本地端口用随机高位端口，避免冲突。
6. **MTU 一致性**：Device 的 mtu=1400，smoltcp 会据此分片。若服务器 MTU 不同可能丢包。

## 后续（不在本计划）

- UDP 支持（SOCKS5 UDP ASSOCIATE + smoltcp UdpSocket）
- DNS 中转（域名解析走隧道，支持 socks5-hostname）
- 会话保活（update_session.csp 每 60s）
- L3Conn 出错重连（ec-protocol 已有，但 NetStack 重建需处理）
- TUN 全局代理（阶段 4，与 Tauri GUI）
