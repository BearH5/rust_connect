# 阶段 4：Tauri GUI 设计

> 日期：2026-07-31
> 状态：设计待审阅
> 前置：阶段 1-3（登录/隧道/SOCKS5）已完成并验证（`192.168.1.120:22` SSH banner 通过）

## 1. 目标

把已跑通的核心 VPN 链路（ec-login -> ec-protocol -> ec-proxy）包成可用桌面产品。用 Tauri 2 提供 GUI：用户填写服务器/账号/密码，点「连接」即建立 VPN 隧道并启动本地 SOCKS5 代理，用户配置浏览器/应用走 SOCKS5 访问内网。

**形态**：全功能型——多服务器配置管理、内网资源列表、日志面板、自动重连、开机自启、系统托盘。

## 2. 技术栈

| 层 | 技术 | 说明 |
|---|---|---|
| 前端 | Vue 3 + Vite + TypeScript | 系统 WebView 渲染，Tauri 2 官方推荐 |
| 后端 | Rust + Tauri 2 | 新 crate `ec-app`，编排现有 crate |
| 配置 | JSON 文件 | `%APPDATA%/rust_connect/config.json` |
| 异步 | tokio | VPN 连接是长驻 task，与 Tauri 事件循环共存 |

环境已就绪：rustc 1.95.0、Node v22.12.0、Tauri CLI 2.10.1。

## 3. 架构

三层分离，职责清晰：

```
┌─────────────────────────────────────────────┐
│  Vue 3 前端（Vite 构建，系统 WebView 渲染）    │
│  连接页 / 服务器管理 / 资源页 / 日志页 / 托盘  │
└──────────────┬──────────────────────────────┘
               │ invoke(command) / listen(event)
┌──────────────▼──────────────────────────────┐
│  ec-app（新 crate，Tauri 后端）               │
│  - VpnState（状态机 + tokio task 生命周期）   │
│  - commands: connect/disconnect/profiles...  │
│  - events: progress/status/log/resources     │
│  - ConfigStore（JSON 读写）                  │
└──────────────┬──────────────────────────────┘
               │ 调用
┌──────────────▼──────────────────────────────┐
│  ec-proxy / ec-login / ec-protocol          │
│  （现有 crate，核心 VPN 链路，不改动）        │
└─────────────────────────────────────────────┘
```

设计原则：ec-app 只做编排和状态管理，不重复实现 VPN 逻辑；现有 crate 的 API 不变。

## 4. 核心组件

### 4.1 VpnState 状态机（ec-app 核心）

```
Disconnected ──connect()──> Connecting ──成功──> Connected
    ▲                          │                    │
    │                       失败                  disconnect()
    │                          ▼                    │
    └──────────────────── Error <───────────────────┘
```

```rust
pub enum VpnState {
    Disconnected,
    Connecting,
    Connected { client_ip: [u8;4], socks_bind: String, cancel: tokio::sync::Notify },
    Error(String),
}
```

- 用 `Arc<Mutex<VpnState>>` 存在 Tauri `State<>` 里
- `connect()`：设为 Connecting，spawn tokio task 跑连接流程，成功后设为 Connected（持有 cancel Notify）
- `disconnect()`：取 cancel，notify_waiters()，task 收到后停止 SOCKS5 -> 设回 Disconnected
- task 内部用 `tokio::select! { run_proxy() | cancel.notified() => ... }` 实现可取消

### 4.2 连接流程（connect command 内部）

```
1. spawn_blocking(login)        -> emit "vpn:progress" {stage:"logging_in"}
2. request_token + build_token  -> emit "vpn:progress" {stage:"requesting_token"}
3. request_ip                    -> emit "vpn:progress" {stage:"requesting_ip"}
                                  emit "vpn:status" {client_ip}
4. L3Conn + NetStack + SOCKS5   -> emit "vpn:progress" {stage:"socks_listening"}
                                  emit "vpn:status" {socks_bind, state:"connected"}
   （长驻，select 监听 cancel）
任一步失败 -> emit "vpn:status" {state:"error", message}
```

关键：ec-proxy 的 `run()` 目前把 login 也包进去了（proxy.rs）。需要拆分——ec-app 自己调 ec-login 拿 twfID，再调 ec-proxy 的 `run_with_twfid(twf_id, ...)`（新接口，跳过登录）。这样 ec-app 能控制登录阶段的事件推送。

### 4.3 配置持久化（ConfigStore）

```rust
pub struct AppConfig {
    pub profiles: Vec<Profile>,
    pub last_profile_id: Option<String>,
    pub settings: Settings,
}
pub struct Profile {
    pub id: String,          // uuid
    pub name: String,
    pub server: String,      // "1.2.3.4:44333"
    pub username: String,
    pub password: String,    // 明文存（后续可加密，YAGNI 先简单）
    pub socks_port: u16,     // 默认 1080
}
pub struct Settings {
    pub auto_reconnect: bool,
    pub auto_start: bool,     // 开机自启
    pub minimize_to_tray: bool,
}
```

路径：`tauri::api::path::app_config_dir()` / `rust_connect/config.json`。serde 序列化。首次运行无文件则用默认空配置。

### 4.4 资源列表

连接成功后，ec-app 调用 ec-protocol 的 `request_token` 时已发过 `/por/rclist.csp` GET。需要解析其 XML 响应拿到资源列表（对照 zju-connect parse.go 的 `parseLineListFromConfig`）。ec-protocol 目前没暴露资源解析——ec-app 自己用正则/etree 解析 rclist 响应（或 ec-protocol 加一个 `parse_resources` 函数）。

资源结构：`{ name, host, port_range }`，通过 `vpn:resources` event 推送。

## 5. Tauri Command/Event 接口

### Commands（前端 invoke）

| command | 参数 | 返回 | 说明 |
|---|---|---|---|
| `connect` | `profile_id: String` | `Result<(), String>` | 按 profile 连接 |
| `disconnect` | - | `Result<(), String>` | 断开 |
| `get_status` | - | `VpnStatus` | 当前状态 |
| `list_profiles` | - | `Vec<Profile>` | 所有配置 |
| `save_profile` | `Profile` | `Result<String, String>` | 新增/更新，返回 id |
| `delete_profile` | `id: String` | `Result<(), String>` | 删除 |
| `get_settings` | - | `Settings` | 全局设置 |
| `save_settings` | `Settings` | `Result<(), String>` | 更新设置 |

### Events（Rust emit -> 前端 listen）

| event | payload | 时机 |
|---|---|---|
| `vpn:progress` | `{stage, message}` | 连接各阶段 |
| `vpn:status` | `{state, client_ip?, socks_bind?, message?}` | 状态变化 |
| `vpn:log` | `{level, message, timestamp}` | 运行日志 |
| `vpn:resources` | `[{name, host, port}]` | 资源列表就绪 |

## 6. 前端页面结构（Vue 3）

单窗口 + 侧边导航：

```
┌──────────────────────────────────┐
│ [连接] [服务器] [资源] [日志]  ──│ 侧边导航
├──────────────────────────────────┤
│                                  │
│         当前页面内容              │
│                                  │
└──────────────────────────────────┘
```

### 连接页（主页）
- 服务器下拉（选 profile）+ 用户名/密码（可覆盖 profile 默认值）+ SOCKS5 端口
- 「连接」/「断开」按钮（根据状态切换）
- 状态卡片：连接状态图标 + 已分配 IP + 连接时长 + SOCKS5 地址（可复制）

### 服务器管理页
- profile 列表（表格：名称/服务器/用户名/操作）
- 新增/编辑（弹窗表单）/删除
- 「设为默认」标记 last_profile

### 资源页
- 连接后展示内网资源列表（名称、IP、端口范围）
- 点击行复制 `IP:端口` 到剪贴板

### 日志页
- 实时滚动日志（时间戳 + 级级 + 消息）
- 级别筛选（info/warn/error）
- 自动滚到底部

### 系统托盘
- 图标：绿色=已连接，灰色=未连接
- 右键菜单：显示窗口 / 断开 / 退出
- 关闭窗口时最小化到托盘（可配）

## 7. 文件结构

```
rust_connect/
├── ec-app/                    # 新建：Tauri 后端
│   ├── Cargo.toml             # 依赖 tauri, ec-proxy, ec-login, serde
│   ├── src/
│   │   ├── main.rs            # Tauri app 入口 + command 注册
│   │   ├── state.rs           # VpnState 状态机 + cancel
│   │   ├── commands.rs        # #[tauri::command] 函数
│   │   ├── config.rs          # ConfigStore (JSON 读写)
│   │   └── vpn.rs             # 连接流程编排（调 ec-login/proxy）
│   ├── tauri.conf.json        # Tauri 配置（窗口/托盘/权限）
│   └── icons/                 # 应用图标
├── ui/                        # 新建：Vue 3 前端
│   ├── package.json           # vue, vite, @tauri-apps/api
│   ├── vite.config.ts
│   ├── index.html
│   └── src/
│       ├── main.ts            # Vue 入口
│       ├── App.vue            # 主布局 + 侧边导航
│       ├── views/
│       │   ├── Connect.vue    # 连接页
│       │   ├── Servers.vue    # 服务器管理
│       │   ├── Resources.vue  # 资源页
│       │   └── Logs.vue       # 日志页
│       └── stores/
│           └── vpn.ts         # Pinia 状态（封装 invoke/listen）
└── （现有 crate 不变）
```

## 8. ec-proxy 接口拆分

当前 `ec_proxy::proxy::run(ProxyConfig)` 包含登录。为支持事件推送，拆成两段：

```rust
// ec-proxy 新增：跳过登录，直接用 twfID 建隧道
pub async fn run_with_twfid(server: &str, twf_id: &str, socks_bind: &str) 
    -> std::io::Result<()>
```

ec-app 的 connect 流程：
1. 自己调 `ec_login::login()` -> 拿 twfID（spawn_blocking，因为 reqwest::blocking）
2. emit progress
3. 调 `ec_proxy::run_with_twfid()`（长驻 task，select 监听 cancel）

原 `run()` 保留（example 还用）。

## 9. 验收标准

1. `cargo tauri dev` 启动，显示 GUI 窗口
2. 添加一个 profile（填服务器/账号/密码），点连接，状态变为已连接，显示分配的 client IP
3. `curl --socks5 127.0.0.1:1080 http://192.168.1.120:22` 验证 SOCKS5 通
4. 资源页显示内网资源列表
5. 日志页显示连接过程日志
6. 点断开，状态变为已断开，SOCKS5 停止
7. 关闭窗口最小化到托盘，托盘可退出
8. 重启 app，profile 和设置仍在（配置持久化）

## 10. 风险与注意事项

1. **ec-proxy 的 run() 拆分**：需要把 login 从 run() 里拿出来，加 `run_with_twfid`。注意 request_token 也是 ec-proxy 内部调的，但它在 proxy.rs 里——拆分时 request_token 的进度事件要能推到前端。
2. **async 阻塞**：ec-login 用 reqwest::blocking，必须在 spawn_blocking 里调。ec-proxy 的 run() 是 async，可在 tokio task 直接跑。
3. **cancel 机制**：ec-proxy 的 `serve_socks5` 是死循环，需要接受 cancel 信号。最简方式：SOCKS5 listener 的 accept 用 `tokio::select!` 配合 cancel Notify。
4. **资源解析**：rclist.csp 的 XML 响应需要解析。zju-connect 用 etree（Go），Rust 侧用 `roxmltree` 或正则。ec-protocol 目前不解析资源，ec-app 自己做。
5. **DLL 部署**：ec-app 间接依赖 ec-utls 的 utls-bridge.dll。Tauri 构建产物要包含 DLL（build.rs 拷贝 + tauri.conf.json 的 resources 配置）。
6. **x86_64-pc-windows-gnu target**：现有 crate 都用 gnu target（链接 mingw .a 导入库）。Tauri 2 默认用 MSVC（默认 host）。**决策：先试 gnu target 跑 `cargo tauri dev`，能跑通就用 gnu（零改动复用现有 FFI）；不行再考虑给 utls-bridge.dll 生成 MSVC .lib 切 MSVC**。Task 1 第一步即验证此点。
7. **密码存储**：首轮明文存 JSON（YAGNI）。后续可用系统 keychain。
8. **自动重连**：SHUTDOWN(0x08) 需全新登录，RECONNECTLATER 可重试。首轮实现基本重连（失败后 sleep+重试 login），复杂策略留后续。
