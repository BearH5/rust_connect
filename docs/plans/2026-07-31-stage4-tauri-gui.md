# 阶段 4：Tauri GUI 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 用 Tauri 2 + Vue 3 把已跑通的 VPN 链路（ec-login/ec-protocol/ec-proxy）包成全功能桌面 GUI，支持多服务器管理、按需连接、资源列表、日志面板、系统托盘。

**Architecture:** 三层：Vue 3 前端（Vite）↔ ec-app（Tauri 2 后端，状态管理+事件推送）↔ 现有 crate（不改动核心逻辑）。按需连接：用户点连接 -> spawn tokio task 跑 login+proxy，进度通过 Tauri event 推送。target 用 x86_64-pc-windows-gnu（已验证 Tauri 2 可编译，复用 ec-utls 的 mingw FFI）。

**Tech Stack:** Tauri 2.11 + Vue 3 + Vite + TypeScript + Pinia。Rust 后端依赖 ec-proxy/ec-login/ec-protocol。

**对应设计文档:** `docs/specs/2026-07-31-stage4-tauri-gui-design.md`

---

## 前置确认（已就绪）

- rustc 1.95.0，x86_64-pc-windows-gnu target 已装
- Node v22.12.0 + npm 10.9.0
- Tauri CLI 2.10.1（cargo tauri）
- Tauri 2.11 依赖用 gnu target 编译通过（已验证）
- ec-proxy 的 `run(ProxyConfig)` 可用；需拆出 `run_with_twfid`

---

## 文件结构

```
rust_connect/
├── ec-app/                       # 新建：Tauri 后端
│   ├── Cargo.toml                # tauri 2, ec-proxy, ec-login, serde, uuid
│   ├── build.rs                  # tauri-build + DLL 部署
│   ├── tauri.conf.json           # 窗口/托盘/权限/前端 devUrl
│   ├── icons/                    # 应用图标（占位）
│   ├── src/
│   │   ├── main.rs               # Tauri Builder + command 注册 + 托盘
│   │   ├── state.rs              # VpnState 状态机 + AppState
│   │   ├── commands.rs           # #[tauri::command] 函数
│   │   ├── config.rs             # ConfigStore (JSON 读写)
│   │   └── vpn.rs                # 连接流程编排
│   └── .cargo/config.toml        # target = x86_64-pc-windows-gnu
├── ui/                           # 新建：Vue 3 前端
│   ├── package.json
│   ├── vite.config.ts
│   ├── tsconfig.json
│   ├── index.html
│   └── src/
│       ├── main.ts
│       ├── App.vue               # 主布局 + 侧边导航
│       ├── views/
│       │   ├── Connect.vue
│       │   ├── Servers.vue
│       │   ├── Resources.vue
│       │   └── Logs.vue
│       └── stores/vpn.ts         # Pinia store
└── （现有 crate 不变，ec-proxy 加 run_with_twfid）
```

---

## Task 1: ec-app Tauri 骨架 + gnu target 验证

**Files:**
- Create: `ec-app/Cargo.toml`, `ec-app/.cargo/config.toml`, `ec-app/build.rs`, `ec-app/tauri.conf.json`, `ec-app/src/main.rs`, `ec-app/icons/`

- [ ] **Step 1: 创建 ec-app 目录与 Cargo.toml**

```toml
[package]
name = "ec-app"
version = "0.1.0"
edition = "2021"
publish = false

[build-dependencies]
tauri-build = { version = "2", features = [] }

[dependencies]
tauri = { version = "2", features = ["tray-icon"] }
ec-proxy = { path = "../ec-proxy" }
ec-login = { path = "../ec-login" }
ec-protocol = { path = "../ec-protocol" }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
uuid = { version = "1", features = ["v4"] }
tokio = { version = "1", features = ["sync", "rt-multi-thread", "macros"] }
log = "0.4"

[profile.release]
panic = "abort"
codegen-units = 1
```

- [ ] **Step 2: .cargo/config.toml 强制 gnu target**

```toml
[build]
target = "x86_64-pc-windows-gnu"
```

- [ ] **Step 3: build.rs**

```rust
fn main() {
    tauri_build::build();
    // DLL 部署：ec-app 间接依赖 utls-bridge.dll
    let src = std::path::PathBuf::from("../utls-bridge/ec_utls_bridge.dll");
    if let Ok(out_dir) = std::env::var("OUT_DIR") {
        let out_path = std::path::PathBuf::from(&out_dir);
        if let Some(debug_dir) = out_path.ancestors().nth(3) {
            if src.exists() {
                let _ = std::fs::copy(&src, debug_dir.join("ec_utls_bridge.dll"));
            }
        }
    }
    println!("cargo:rerun-if-changed=../utls-bridge/ec_utls_bridge.dll");
}
```

- [ ] **Step 4: tauri.conf.json（最小配置，前端先空）**

```json
{
  "$schema": "https://schema.tauri.app/config/2",
  "productName": "RustConnect",
  "version": "0.1.0",
  "identifier": "com.rustconnect.app",
  "build": {
    "frontendDist": "../ui/dist",
    "devUrl": "http://localhost:1420",
    "beforeDevCommand": "npm --prefix ../ui run dev",
    "beforeBuildCommand": "npm --prefix ../ui run build"
  },
  "app": {
    "windows": [{ "title": "RustConnect", "width": 800, "height": 600 }],
    "security": { "csp": null }
  },
  "bundle": {
    "active": true,
    "targets": "all",
    "resources": ["../utls-bridge/ec_utls_bridge.dll"]
  }
}
```

- [ ] **Step 5: 最小 main.rs**

```rust
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    tauri::Builder::default()
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
```

- [ ] **Step 6: 创建占位图标（Tauri 需要）**

在 `ec-app/icons/` 放一个占位 icon.png（32x32）和 icon.ico。可从 Tauri 模板复制或用简单占位。

- [ ] **Step 7: 验证 gnu target 编译**

Run:
```bash
cd D:/dev_code/study/rust_connect/ec-app
export PATH="/d/dev_evn/mingw64/bin:/d/dev_evn/Go/bin:$PATH"
export LIBCLANG_PATH="D:/dev_evn/cangjie/third_party/llvm/bin"
export BINDGEN_EXTRA_CLANG_ARGS="-I/d/dev_evn/mingw64/lib/gcc/x86_64-w64-mingw32/14.2.0/include -I/d/dev_evn/mingw64/x86_64-w64-mingw32/include"
cargo build
```
Expected: 编译通过（Tauri 2 + gnu target + ec-proxy 依赖链）。若链接报 utls-bridge.dll 找不到，确认 build.rs 的 DLL 拷贝路径。

---

## Task 2: ec-proxy 拆分 run_with_twfid

**Files:**
- Modify: `ec-proxy/src/proxy.rs`

**背景**：ec-app 需要自己调 login（控制事件推送），再调 ec-proxy 跳过登录直接建隧道。

- [ ] **Step 1: 在 proxy.rs 加 run_with_twfid**

```rust
/// 用已有的 twfID 建隧道（跳过登录）。供 ec-app 调用。
///
/// 流程：request_token -> build_token -> request_ip -> L3Conn -> NetStack -> SOCKS5。
/// 返回的 JoinHandle 供调用方管理生命周期（cancel 等）。
pub async fn run_with_twfid(
    server: &str,
    twf_id: &str,
    socks_bind: String,
    cancel: Arc<Notify>,
) -> std::io::Result<()> {
    let sid_hex = token::request_token(server, twf_id)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;
    let tkn = token::build_token(&sid_hex, twf_id)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;

    let ((ip, ip_reverse), keepalive_conn) = tunnel::request_ip(server, &tkn)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;
    log::info!("client IP: {}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3]);

    let l3 = L3Conn::new(server, &tkn, &ip_reverse)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;

    let (device, _bridge) = L3ConnDevice::spawn(l3, 1400);
    let (_stack, handle) = NetStack::new(device, ip)?;

    let listener = tokio::net::TcpListener::bind(&socks_bind).await?;
    log::info!("SOCKS5 监听 {}", socks_bind);

    // 可取消的 serve：select 监听 cancel
    let serve = socks5::serve_socks5(listener, handle);
    tokio::select! {
        _ = serve => {}
        _ = cancel.notified() => {
            log::info!("收到 cancel，停止 SOCKS5");
        }
    }
    std::mem::forget(keepalive_conn);
    Ok(())
}
```

注意：`serve_socks5` 返回 `std::io::Result<()>`（死循环直到 listener 关闭）。cancel 时 select 分支返回。需在 proxy.rs 加 `use std::sync::Arc; use tokio::sync::Notify;`。

- [ ] **Step 2: 验证编译**

Run: `cd D:/dev_code/study/rust_connect/ec-proxy && cargo build`
Expected: 编译通过。

---

## Task 3: ec-app 状态机 + ConfigStore

**Files:**
- Create: `ec-app/src/state.rs`, `ec-app/src/config.rs`

- [ ] **Step 1: state.rs - VpnState 状态机**

```rust
use std::sync::Arc;
use tokio::sync::Notify;

#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "state")]
pub enum VpnStatus {
    Disconnected,
    Connecting,
    Connected { client_ip: String, socks_bind: String },
    Error { message: String },
}

pub struct AppState {
    pub vpn: std::sync::Mutex<Option<VpnSession>>,
    pub config: std::sync::Mutex<crate::config::ConfigStore>,
}

pub struct VpnSession {
    pub cancel: Arc<Notify>,
    pub status: VpnStatus,
}
```

- [ ] **Step 2: config.rs - ConfigStore（JSON 读写）**

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub id: String,
    pub name: String,
    pub server: String,
    pub username: String,
    pub password: String,
    pub socks_port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Settings {
    pub auto_reconnect: bool,
    pub auto_start: bool,
    pub minimize_to_tray: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AppConfig {
    pub profiles: Vec<Profile>,
    pub last_profile_id: Option<String>,
    pub settings: Settings,
}

pub struct ConfigStore {
    pub config: AppConfig,
    pub path: std::path::PathBuf,
}

impl ConfigStore {
    pub fn load() -> Self { /* 读 app_config_dir/rust_connect/config.json，不存在则默认 */ }
    pub fn save(&self) -> std::io::Result<()> { /* serde_json 写文件 */ }
}
```

- [ ] **Step 3: 验证编译**

Run: `cd D:/dev_code/study/rust_connect/ec-app && cargo build`

---

## Task 4: ec-app commands + 连接流程

**Files:**
- Create: `ec-app/src/commands.rs`, `ec-app/src/vpn.rs`
- Modify: `ec-app/src/main.rs`

- [ ] **Step 1: vpn.rs - 连接流程编排**

```rust
use std::sync::Arc;
use tokio::sync::Notify;
use tauri::{AppHandle, Emitter};

/// 连接流程：login -> run_with_twfid，通过 event 推送进度。
pub async fn connect_vpn(app: AppHandle, server: String, username: String, password: String, socks_port: u16, cancel: Arc<Notify>) -> Result<(), String> {
    // 1. login（spawn_blocking，reqwest::blocking）
    let _ = app.emit("vpn:progress", serde_json::json!({"stage": "logging_in", "message": "登录中..."}));
    let twf_id = tokio::task::spawn_blocking(move || {
        ec_login::login(&ec_login::LoginConfig { server: server.clone(), username, password })
    }).await.map_err(|e| e.to_string())?
      .map_err(|e| e.to_string())?;
    let twf_id = match twf_id {
        ec_login::LoginStep::Done(t) if !t.is_empty() => t,
        other => return Err(format!("登录未完成: {:?}", other)),
    };
    let _ = app.emit("vpn:progress", serde_json::json!({"stage": "tunnel", "message": "建立隧道..."}));

    // 2. run_with_twfid（select 监听 cancel）
    let socks_bind = format!("127.0.0.1:{socks_port}");
    let _ = app.emit("vpn:status", serde_json::json!({"state": "connecting"}));
    ec_proxy::proxy::run_with_twfid(&server, &twf_id, socks_bind.clone(), cancel).await
        .map_err(|e| e.to_string())?;
    let _ = app.emit("vpn:status", serde_json::json!({"state": "connected", "socks_bind": socks_bind}));
    Ok(())
}
```

- [ ] **Step 2: commands.rs - Tauri command 函数**

实现 `connect`、`disconnect`、`get_status`、`list_profiles`、`save_profile`、`delete_profile`、`get_settings`、`save_settings`。每个 `#[tauri::command]`，操作 `AppState`。

- [ ] **Step 3: main.rs - 注册 command + 托盘**

```rust
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod state; mod config; mod commands; mod vpn;

use state::AppState;

fn main() {
    tauri::Builder::default()
        .manage(AppState { /* 初始化 */ })
        .invoke_handler(tauri::generate_handler![
            commands::connect, commands::disconnect, commands::get_status,
            commands::list_profiles, commands::save_profile, commands::delete_profile,
            commands::get_settings, commands::save_settings,
        ])
        .setup(|app| { /* 托盘图标 */ Ok(()) })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
```

- [ ] **Step 4: 验证编译**

Run: `cd D:/dev_code/study/rust_connect/ec-app && cargo build`

---

## Task 5: Vue 3 前端骨架

**Files:**
- Create: `ui/package.json`, `ui/vite.config.ts`, `ui/index.html`, `ui/src/main.ts`, `ui/src/App.vue`

- [ ] **Step 1: 初始化 ui 项目**

```bash
cd D:/dev_code/study/rust_connect/ui
npm init -y
npm install vue @tauri-apps/api pinia
npm install -D vite @vitejs/plugin-vue typescript vue-tsc
```

- [ ] **Step 2: vite.config.ts（端口 1420 对应 tauri.conf.json）**

```ts
import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'
export default defineConfig({
  plugins: [vue()],
  server: { port: 1420 },
  build: { outDir: 'dist' },
})
```

- [ ] **Step 3: App.vue 主布局（侧边导航 + router-view）**

简单 tab 切换（连接/服务器/资源/日志），用 `v-if` 或简单状态切换，不引 vue-router（YAGNI，4 个 tab 用条件渲染够）。

- [ ] **Step 4: stores/vpn.ts（Pinia，封装 invoke + listen）**

```ts
import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
// connect/disconnect/get_status/list_profiles 等 action
// listen('vpn:progress'/'vpn:status') 更新 state
```

- [ ] **Step 5: 4 个 view 组件骨架**

Connect.vue / Servers.vue / Resources.vue / Logs.vue，各放占位内容。

- [ ] **Step 6: 验证 cargo tauri dev**

Run: `cd D:/dev_code/study/rust_connect/ec-app && cargo tauri dev`
Expected: Tauri 窗口弹出，显示 Vue 前端（占位内容）。

---

## Task 6: 前端各页面实现

**Files:**
- Modify: `ui/src/views/*.vue`, `ui/src/stores/vpn.ts`

- [ ] **Step 1: Connect.vue** - 服务器下拉 + 用户名密码 + SOCKS端口 + 连接按钮 + 状态卡片
- [ ] **Step 2: Servers.vue** - profile 列表 + 新增/编辑/删除
- [ ] **Step 3: Resources.vue** - 资源列表展示（监听 vpn:resources event）
- [ ] **Step 4: Logs.vue** - 实时日志（监听 vpn:log event）
- [ ] **Step 5: 系统托盘** - main.rs setup 里建 tray icon + 菜单

每个 view 通过 `useVpnStore()` 调 invoke/listen。

---

## Task 7: 端到端验证

- [ ] **Step 1: 启动 app**

```bash
cd D:/dev_code/study/rust_connect/ec-app
export PATH="/d/dev_evn/mingw64/bin:/d/dev_evn/Go/bin:$PATH"
export LIBCLANG_PATH="D:/dev_evn/cangjie/third_party/llvm/bin"
export BINDGEN_EXTRA_CLANG_ARGS="-I/d/dev_evn/mingw64/lib/gcc/x86_64-w64-mingw32/14.2.0/include -I/d/dev_evn/mingw64/x86_64-w64-mingw32/include"
cargo tauri dev
```

- [ ] **Step 2: GUI 操作验证**
1. 服务器管理页添加 profile（1.2.3.4:44333 / username / password）
2. 连接页选 profile，点连接，观察状态变为已连接 + 显示 SOCKS5 地址
3. 另开终端：`curl --socks5 127.0.0.1:1080 http://192.168.1.120:22` 验证通
4. 资源页看内网资源列表
5. 日志页看连接日志
6. 点断开，状态变已断开
7. 关窗最小化到托盘

---

## 验收标准

1. `cargo tauri dev` 启动，GUI 窗口正常显示
2. 添加 profile -> 连接 -> 已连接状态 + client IP 显示
3. `curl --socks5` 验证 VPN 隧道通
4. 资源页、日志页正常
5. 断开、托盘、配置持久化（重启后 profile 还在）

## 风险

1. **gnu target + Tauri**：已验证依赖可编译。若 dev/run 时 WebView2 或 DLL 加载有问题，考虑切 MSVC（需重生成 utls-bridge 的 .lib）。
2. **reqwest::blocking 在 Tauri**：必须在 spawn_blocking 调用，否则阻塞 tokio runtime。
3. **cancel 机制**：serve_socks5 需配合 select，listener 关闭后 serve 返回。
4. **DLL 部署**：dev 模式 DLL 在 target/debug，需 build.rs 拷贝；release 打包用 tauri.conf.json resources。
5. **图标**：Tauri 编译需要 icon.ico/png，用占位图标。
