# TUN 全局代理模式设计（阶段 5）

> 日期：2026-08-01
> 状态：设计待审阅
> 前置：PAC 系统代理方案已完成并验证（浏览器访问 Jenkins 正常，0.1s）

## 1. 目标

在现有 PAC 方案之外，增加 **TUN 全局代理模式**：创建虚拟网卡接管系统路由，让**所有应用**（包括不走系统代理的）自动通过 VPN 访问内网。用户可在设置页**手动选择** TUN 或 PAC 模式。

同时为跨平台铺路：使用 `tun2` crate（跨平台抽象：Windows=wintun，Linux=/dev/tun，macOS=utun），后续 Linux/macOS 只需换后端无需改业务代码。

## 2. 技术选型

| 项 | 选择 | 理由 |
|---|---|---|
| TUN 库 | `tun2` v4.0.0 | 跨平台抽象，Windows 后端 wintun，API 简单（`configure()` + `create()`） |
| Windows 驱动 | wintun.dll（自动下载） | WireGuard 官方，性能最好，约 100KB |
| 模式切换 | 手动（设置页开关） | 用户明确选择，避免自动切换的隐式行为 |
| 管理员权限 | 连接时检测 + 提权 | wintun 需要管理员权限 |

## 3. 架构

### 3.1 TUN 模式数据流

```
应用（任意）→ [系统路由表] → wintun 虚拟网卡 → L3Conn 写线程 → VPN 服务器
VPN 服务器 → L3Conn 读线程 → wintun 虚拟网卡 → [系统路由表] → 应用
```

**关键简化**：TUN 模式下**不需要 smoltcp 和 SOCKS5**——系统 TCP/IP 栈直接处理 IP 包，L3Conn 只是 IP 包管道。这比 PAC 模式（应用→PAC→SOCKS5→smoltcp→L3Conn）短一层，且覆盖所有应用。

```
PAC 模式：应用 → WinINET PAC → SOCKS5(1080) → smoltcp → L3Conn → VPN
TUN 模式：应用 → 系统路由 → wintun → L3Conn → VPN
```

### 3.2 组件

#### ec-proxy 新增 `tun.rs`（TUN 模式核心）

```rust
/// TUN 模式：L3Conn ↔ tun2 Device 双向转发。
/// 返回 (tun Device, 转发线程句柄)。
pub struct TunBridge {
    device: tun2::Device,   // Read+Write
    // 转发线程
}

impl TunBridge {
    /// 建立 TUN 网卡 + 双向转发。
    /// client_ip: RequestIP 拿到的 IP（作 TUN 接口地址）
    pub fn start(
        l3conn: L3Conn,
        client_ip: [u8; 4],
        routes: &[Route],   // 内网网段（资源 IP 范围）
    ) -> std::io::Result<TunBridge> {
        // 1. 建 TUN 网卡
        let mut config = tun2::configure();
        config
            .name("RustConnect")
            .address(Ipv4Addr::from(client_ip))
            .netmask(Ipv4Addr::new(255, 255, 255, 0))  // /24
            .mtu(1400)
            .up();
        let device = tun2::create(&config)?;

        // 2. 加路由（内网网段 → TUN）
        //    route add <network> mask <mask> <gateway=client_ip> metric 1
        for route in routes {
            run_command("route", &["add", &route.network, "mask", &route.mask, &client_ip_str, "metric", "1"])?;
        }

        // 3. 双向转发：L3Conn 拆两半，各一个 OS 线程
        let (read_half, write_half) = l3conn.split();
        // 读线程：read_half.read() → device.write()
        // 写线程：device.read() → write_half.write()
        // （注意方向：VPN 来的包写进 TUN；TUN 收的包发给 VPN）
    }
}
```

**转发方向**：
- VPN → TUN：`read_half.read(buf)` → `device.write(buf)`（服务端下发的 IP 包进入系统网络栈）
- TUN → VPN：`device.read(buf)` → `write_half.write(buf)`（应用发出的 IP 包发给服务端）

#### ec-app 新增 `tun_mode.rs`（TUN 集成）

```rust
/// 管理员权限检测。
pub fn is_admin() -> bool { /* IsUserAnAdmin() */ }

/// 下载 wintun.dll（不存在时）。
pub fn ensure_wintun() -> std::io::Result<PathBuf> {
    // 从 https://www.wintun.net/builds/wintun-0.14.1.zip 下载，解压 wintun.dll
    // 存到 app 目录（%APPDATA%/rust_connect/wintun.dll 或 exe 同目录）
}

/// 启动 TUN 模式连接。
pub async fn connect_tun(server, twf_id, client_ip, resources) -> Result<(), String> {
    // 1. 检测管理员权限，无则提示（或自动提权重启）
    // 2. 确保 wintun.dll
    // 3. L3Conn::new → TunBridge::start
    // 4. 资源 IP 范围 → 路由
}
```

#### 路由计算（从资源列表）

资源 host 字段如 `192.168.1.40~192.168.1.60;192.168.1.222`：
- 转成路由：`192.168.1.0/24`（网段近似，同 PAC 的简化）
- 精确范围需要多个路由（不同 /24 分别加），跨 /16 更复杂
- 首轮用 /24 网段近似（资源都在 192.168.1.x，一条 `route add 192.168.1.0 mask 255.255.255.0` 够）

### 3.3 设置页模式选择

`Settings` 加字段：
```rust
pub struct Settings {
    // 现有字段...
    pub proxy_mode: ProxyMode,  // TUN / Pac
}
pub enum ProxyMode { Tun, Pac }  // serde: "tun"/"pac"
```

- 默认 `Pac`（无需管理员权限，开箱即用）
- 用户选 TUN 时，连接流程走 `connect_tun`，否则走现有 `connect_vpn`（PAC）
- TUN 模式下**不设置系统代理**（避免与 TUN 双重代理冲突）

### 3.4 wintun.dll 分发

- 构建/运行时从 `https://www.wintun.net/builds/wintun-0.14.1.zip` 下载（build.rs 或首次连接时）
- 存到 `%APPDATA%/rust_connect/wintun.dll`
- tun2 的 Windows PlatformConfig 指定 `wintun_file` 指向该路径
- bundle 时通过 tauri.conf.json resources 包含

## 4. 连接流程（TUN 模式）

```
用户选 TUN 模式 + 点连接
  → 检查管理员权限（无则提示提权）
  → 确保 wintun.dll（下载）
  → login → token → request_ip（拿 client_ip）
  → 拉资源列表（算路由）
  → L3Conn::new（send/recv 隧道）
  → TunBridge::start（建 TUN + 加路由 + 双向转发）
  → emit "vpn:status" {state: "connected", mode: "tun", client_ip}

断开：
  → 停转发线程 + drop TUN device（tun2 自动清理）
  → route delete 内网路由
  → emit "vpn:status" {state: "disconnected"}
```

## 5. 验收标准

1. 设置页可选 TUN/PAC 模式，切换后连接走对应流程
2. TUN 模式连接成功后：
   - `ipconfig` 看到 RustConnect 虚拟网卡（IP = client_ip）
   - `route print` 看到 192.168.1.0/24 → RustConnect 接口
   - 浏览器访问 `http://192.168.1.120:9080/` 正常（不依赖系统代理）
   - **不走系统代理的应用**（如 ping 192.168.1.120 或 nslookup）也能通
3. 断开后路由清理、网卡消失
4. 无管理员权限时连接失败并提示

## 6. 风险与注意事项

1. **管理员权限**：wintun 创建需要管理员。Tauri 进程默认非提权。方案：检测无权限时提示用户"以管理员身份重新运行"，或用 `tauri-plugin` 提权。首轮做提示（最简），自动提权重启留后续。
2. **路由清理**：断开时必须 `route delete`，否则残留路由导致网络异常。tun2 的 drop 清理网卡，但路由是手动加的需手动删。
3. **wintun.dll 下载**：需网络。下载失败时降级提示（PAC 模式仍可用）。
4. **TUN 与 PAC 冲突**：TUN 模式下不设系统代理，避免双重代理。
5. **网段近似**：路由用 /24 近似（同 PAC）。精确到资源 IP 范围需要按 /24 拆分多条路由，首轮 /24 够。
6. **公网流量**：只加内网路由到 TUN，公网走默认路由（DIRECT）——避免被 VPN SHUTDOWN。
7. **跨平台**：tun2 的 API 三平台一致，Linux/macOS 后端自动切换。但路由命令（route/netsh）是平台相关的，需 cfg 隔离。首轮只做 Windows。

## 7. 后续（不在本阶段）

- 自动提权重启（无权限时 UAC 弹窗自动以管理员重启）
- Linux/macOS 后端适配（路由命令 cfg 化）
- 路由精确到资源 IP 范围（多 /24 拆分）
- TUN 与 PAC 的双模式热切换（不重连）
