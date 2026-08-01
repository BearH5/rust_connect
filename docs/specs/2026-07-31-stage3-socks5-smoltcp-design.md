# 阶段 3：SOCKS5 代理出口（smoltcp 出站层）设计

> 日期：2026-07-31
> 状态：设计待审阅
> 前置：阶段 D-2（L3Conn 收发）已完成并通过验证

## 1. 背景与目标

EasyConnect 的隧道是 **L3（IP 包层）**的：`L3Conn` 收发的是裸 IP 包。
SOCKS5 代理是 **L4（TCP 会话）**：应用通过 SOCKS5 请求连接目标，代理向目标发起 TCP。
两者之间必须有一个**用户态 TCP/IP 栈**做协议转换。

zju-connect 用 **gVisor netstack**（Go）做这个转换：SOCKS5 通过 `gonet.DialTCP` 在 gVisor 栈上**出站拨号**，gVisor 把 TCP 流转成 IP 包经 L3Conn 发出。

**目标**：在 Rust 侧用 **smoltcp**（v0.12，纯 Rust 用户态 TCP/IP 栈）复刻这套机制，实现一个本地 SOCKS5 server，让 `curl --socks5 127.0.0.1:1080 <内网地址>` 能访问内网资源。

**首轮范围**：TCP only（SOCKS5 CONNECT 命令）。UDP/ICMP 留到 TCP 验证通过后再补。

## 2. 关键探索结论（决策依据）

| 调查项 | 结论 | 影响 |
|---|---|---|
| zju-connect 代理栈 | gVisor netstack，SOCKS5 用 `gonet.DialTCP` **出站** | 确认必须支持出站拨号 |
| `netstack-smoltcp` crate | 只有**入站** TcpListener（被动接受 SYN），无出站 DialTCP | **不适用**纯 SOCKS5 出站代理，放弃 |
| smoltcp 原生 | `TcpSocket::connect`（tcp.rs:929）支持出站；`Interface::poll` 驱动栈；`phy::Device` trait 对接 IP 包源 | **采用**，自己写 Device + 出站层 |
| ec-utls/L3Conn | 同步阻塞 I/O（Go DLL） | 必须放独立线程，用 channel 与 smoltcp poll 循环桥接 |

## 3. 架构设计

### 3.1 整体数据流

```
应用程序 (curl)
    │ TCP 连接到 127.0.0.1:1080
    ▼
[SOCKS5 Server] (tokio async)
    │ 解析 CONNECT 目标 → 请求出站 TCP
    ▼
[NetStack 出站层] (smoltcp, tokio task)
    │ TcpSocket::connect(dst) → smoltcp 产生出站 IP 包
    │ 从 smoltcp 读出站 IP 包 → tx channel
    │ rx channel 的入站 IP 包 → 喂给 smoltcp Interface
    ▼
[L3Conn 桥接线程] (std::thread, 同步阻塞)
    │ 从 tx channel 取 IP 包 → L3Conn.write()
    │ L3Conn.read() 的 IP 包 → 推入 rx channel
    ▼
L3Conn ←→ VPN 服务器
```

三层职责清晰分离：
- **SOCKS5 层**：协议解析，连接管理（async）
- **smoltcp 层**：TCP/IP 协议栈，出站拨号（async poll 循环）
- **L3Conn 桥接层**：同步阻塞 I/O 与异步 channel 的适配（独立线程）

### 3.2 三个核心组件

#### 组件 A：`L3ConnDevice`（smoltcp 的 phy::Device 实现）

把 L3Conn 的同步 I/O 包装成 smoltcp 的 `phy::Device` trait。

- 独立 `std::thread` 跑 L3Conn 的读写循环：
  - 读循环：`L3Conn.read()` → 解析 IP 包 → `ingress_tx.send(pkt)`（推入 smoltcp）
  - 写循环：从 `egress_rx.recv()` 取包 → `L3Conn.write(pkt)`
- `L3ConnDevice` 持有 `ingress` 队列（Mutex<VecDeque>）和 `egress` 发送端
- 实现 `phy::Device` 的 `receive()`/`transmit()`：`receive` 从 ingress 队列取包，`transmit` 把包放入 egress channel

参考 `netstack-smoltcp/src/device.rs` 的 `VirtualDevice`（mpsc channel 模型），但反向：它的 Device 消费 channel，我们的 Device 既生产（L3Conn→ingress）又消费（egress→L3Conn）。

#### 组件 B：`NetStack`（smoltcp 出站层）

驱动 smoltcp 的 `Interface` + `SocketSet`，提供出站 `dial_tcp()`。

- 持有 `Interface`、`SocketSet`、`L3ConnDevice`
- **poll 循环**（tokio task）：周期性调 `iface.poll(now, &mut device, &mut socket_set)`，处理 socket 状态变化
- `dial_tcp(dst: SocketAddr) -> NetStream`：
  1. 创建 `TcpSocket`，`socket.connect(...)` 发起出站
  2. 加入 SocketSet，拿到 handle
  3. 返回 `NetStream`（封装 handle + control，实现 AsyncRead/AsyncWrite）
- `NetStream`：异步读写，内部通过 poll 循环驱动 socket 收发；数据经 RingBuffer 在 NetStream 与 TcpSocket 间传递（参考 netstack-smoltcp tcp.rs 的 TcpSocketControl 模型）

#### 组件 C：SOCKS5 Server

用 `fast-socks5` 库（v0.10）处理 SOCKS5 协议（握手、命令解析），通过 `NetStack::dial_tcp` 建立隧道连接，双向转发。

- `fast-socks5` 的 `Socks5Server::bind(addr)` 监听
- 关键配置：`Config::default().set_execute_command(false).set_dns_resolve(false)` —— 关掉内置拨号（它用系统 TCP，我们要走 NetStack）和内置 DNS
- `socket.upgrade_to_socks5()` 完成握手后，用 `socket.cmd()` 和 `socket.target_addr()` 取命令和目标地址
- cmd 是 `TCPConnect` 时：用 `NetStack::dial_tcp(target)` 拨号，`socket.reply_success()` 后 `tokio::io::copy` 双向转发
- 用 fast-socks5 处理协议细节（方法协商、地址解析、错误回复），我们只接管拨号这一步

### 3.3 DNS 处理

首轮**不做 DNS 中转**：SOCKS5 的 CONNECT 目标用 IP（`curl --socks5` 支持 IP 目标）。
域名解析留到后续（需要把 DNS 查询通过 UDP 在 smoltcp 里走，或复用 ec-login 的 reqwest 做 HTTP API 解析）。
SOCKS5 协议支持域名类型地址，但首轮我们让 curl 先解析成 IP（`--socks5` 默认本地解析，`--socks5-hostname` 才远程解析）。

## 4. 异步/同步桥接的关键设计

**问题**：L3Conn 是同步阻塞（Go DLL），smoltcp poll 是 async，SOCKS5 是 async。直接混用会阻塞 tokio runtime。

**方案**：L3Conn 放 `std::thread::spawn`（OS 线程，不占 tokio worker），通过 `tokio::sync::mpsc` 与 async 侧通信。

```
std::thread (L3Conn 同步阻塞)        tokio runtime (async)
┌─────────────────────────┐         ┌────────────────────────┐
│ loop {                  │         │                        │
│   pkt = L3Conn.read()   │──ingress_tx──▶ L3ConnDevice      │
│   ingress_tx.send(pkt)  │         │   (smoltcp Device)      │
│ }                       │         │                        │
│                         │         │ poll loop:              │
│ loop {                  │         │   iface.poll(...)       │
│   pkt = egress_rx.recv()│◀─egress_tx── NetStack 出站包      │
│   L3Conn.write(pkt)     │         │                        │
│ }                       │         │                        │
└─────────────────────────┘         └─────────────────────────┘
```

- `ingress_tx`/`ingress_rx`：L3Conn 读到的包 → smoltcp（tokio::sync::mpsc 或 std mpsc + Notify 唤醒）
- `egress_tx`/`egress_rx`：smoltcp 要发的包 → L3Conn
- L3Conn 读循环要能及时唤醒 smoltcp poll（用 tokio Notify 或让 Device 的 receive 返回 pending 直到有包）

**注意**：smoltcp 的 `phy::Device::receive` 是同步的（非 async），但被 `iface.poll()` 在 poll 循环里调用。所以 Device 内部用 `Mutex<VecDeque>` 缓冲，poll 循环靠 `tokio::time::interval` 或 Notify 定时驱动。

## 5. 文件结构

```
rust_connect/
├── ec-protocol/          # 已有（L3Conn 等）
└── ec-proxy/             # 新建：SOCKS5 + smoltcp 出站层
    ├── Cargo.toml        # 依赖 ec-protocol, smoltcp, tokio
    └── src/
        ├── lib.rs        # 模块导出
        ├── device.rs     # L3ConnDevice（smoltcp phy::Device 实现）+ 桥接线程
        ├── netstack.rs   # NetStack（smoltcp Interface/SocketSet + dial_tcp + poll 循环）
        ├── stream.rs     # NetStream（AsyncRead/AsyncWrite，封装 TcpSocket handle）
        ├── socks5.rs     # SOCKS5 server（协议解析 + CONNECT + 双向转发）
        └── proxy.rs      # Proxy 顶层：组装 L3Conn + NetStack + SOCKS5
    └── examples/
        └── socks_demo.rs # 端到端：登录→隧道→SOCKS5，curl --socks5 验证
```

## 6. 任务分解

### Task 1: ec-proxy crate 骨架
- `Cargo.toml`：依赖 `ec-protocol`（path）、`smoltcp = "0.12"`、`tokio`（full）、`log`/`env_logger`
- smoltcp features：`proto-ipv4`、`socket-tcp`（首轮只需 IPv4 + TCP）
- `src/lib.rs` 模块声明

### Task 2: L3ConnDevice + 桥接线程
- 实现 smoltcp `phy::Device` trait
- `L3ConnDevice::spawn(l3conn) -> (device, shutdown)`：启动 OS 线程跑 L3Conn 读写循环
- Device 内部 `Mutex<VecDeque<Vec<u8>>>` 缓冲入站包，`Sender<Vec<u8>>` 发出站包
- 验证：单元测试喂/取 IP 包（不连真实服务器）

### Task 3: NetStack（poll 循环 + dial_tcp）
- `NetStack::new(device, client_ip) -> Self`：建 Interface（设 client_ip 为地址）、SocketSet、启动 poll 循环 task
- `dial_tcp(dst: SocketAddr) -> io::Result<NetStream>`：创建 TcpSocket、connect、入 SocketSet
- poll 循环：`loop { iface.poll(now, device, socket_set); 处理 socket 状态; tokio::time::sleep; }`
- 验证：dial 一个已知 IP，看是否产生出站 SYN 包（用 Device 的 egress 队列检查）

### Task 4: NetStream（AsyncRead/AsyncWrite）
- 封装 SocketHandle + 与 poll 循环共享的 control（RingBuffer + Waker，参考 netstack-smoltcp tcp.rs）
- `AsyncRead`：从 recv_buffer 读，空则注册 waker
- `AsyncWrite`：写入 send_buffer，满则注册 waker；poll 循环负责 send_buffer → TcpSocket

### Task 5: SOCKS5 server
- `serve_socks5(listener, netstack) -> io::Result<()>`
- 协议：方法协商（无认证/用户密码）→ CONNECT 解析 → `netstack.dial_tcp(dst)` → 成功响应 → 双向 copy
- 验证：连本地 SOCKS5，CONNECT 一个 IP，看是否触发 dial

### Task 6: Proxy 顶层 + 端到端 example
- `Proxy::start(server, twf, bind_addr)`：登录→token→RequestIP→L3Conn→NetStack→SOCKS5
- `socks_demo.rs`：跑完整链路，SOCKS5 监听 127.0.0.1:1080
- 验证：`curl --socks5 127.0.0.1:1080 http://<内网IP>` 能访问

## 7. 验收标准

1. `socks_demo` 启动后 SOCKS5 监听 127.0.0.1:1080
2. `curl --socks5 127.0.0.1:1080 http://<内网资源IP>` 返回内网内容（HTTP 200 或内网页面）
3. 能维持 TCP 连接收发数据（不只是握手成功）
4. 多个并发 SOCKS5 连接都能工作（SocketSet 管理多个 socket）

## 8. 风险与注意事项

1. **smoltcp poll 的时间驱动**：`iface.poll(timestamp, ...)` 需要单调递增的 `Instant`。用 `tokio::time::Instant::now()` 转 smoltcp 的 `Instant`。
2. **TCP 窗口/缓冲**：smoltcp 的 TcpSocket 收发缓冲区大小影响吞吐。参考 netstack-smoltcp 默认 `0x3FFF * 20`（~320KB）。
3. **L3Conn read 阻塞唤醒**：L3Conn 没有超时，读到数据才返回。桥接线程的 read 循环天然阻塞在 L3Conn.read() 上，这是可接受的（OS 线程），但要确保新数据能及时唤醒 smoltcp poll。
4. **连接关闭语义**：SOCKS5 连接关闭 → NetStream drop → 通知 poll 循环关闭对应 TcpSocket。参考 netstack-smoltcp TcpStream::Drop。
5. **MTU**：zju-connect gvisor 用 1400（stack.go:33），smoltcp Device 的 capabilities.mtu 要与之匹配，避免分片问题。
6. **smoltcp API 复杂度**：smoltcp 的 Device trait 在 0.12 有 GAT（generic associated types）变化（`type RxToken<'a>`），需注意生命周期。
7. **client_ip 来源**：smoltcp Interface 要绑定 client_ip（RequestIP 拿到的），这样出站包的源 IP 才正确，服务端才能识别。
8. **DNS 暂不处理**：首轮 curl 用 `--socks5`（本地解析），不用 `--socks5-hostname`（远程解析）。

## 9. 后续（不在本阶段）

- UDP 支持（SOCKS5 UDP ASSOCIATE + smoltcp UdpSocket）
- DNS 中转（域名解析走隧道）
- 会话保活（update_session.csp）
- TUN 全局代理（阶段 4，与 Tauri GUI 一起）
