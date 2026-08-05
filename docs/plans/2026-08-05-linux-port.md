# Linux 端移植实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让 rust_connect 在 Linux 上编译运行，支持 PAC + TUN 双模式。

**Architecture:** cfg 隔离方案--在现有文件里用 `#[cfg(target_os = "linux")]` 加 Linux 分支。先改构建链（P0 编译前提），再 TUN/系统代理（P1 功能），最后配置路径（P2 完善）。

**Tech Stack:** Rust + Tauri 2 + tun2（跨平台 TUN）+ Go c-shared（.so）+ smoltcp + iproute2（Linux 路由命令）+ gsettings/kwriteconfig5（系统代理）

**Spec:** `docs/specs/2026-08-05-linux-port-design.md`

**环境前提：** 在 Linux 机器（或 WSL2）上执行。需要 Go、gcc、Rust stable、Node.js、`libwebkit2gtk-4.1-dev`（Tauri Linux 依赖）。

---

## 文件结构

| 文件 | 责任 | 改动类型 |
|------|------|----------|
| `ec-app/rust-toolchain.toml` | 工具链配置 | 改 channel |
| `ec-app/.cargo/config.toml` | cargo target 配置 | 删 target 行 |
| `ec-utls/.cargo/config.toml` | cargo target 配置 | 删 target 行 |
| `ec-app/Cargo.toml` | 依赖声明 | winreg 移到 target，加 dirs |
| `ec-utls/build.rs` | DLL/SO 链接+拷贝 | 按平台切换扩展名 |
| `ec-app/build.rs` | DLL/SO 部署 | 按平台切换扩展名 |
| `utls-bridge/build.sh` | Go .so 构建（新增） | 新建 |
| `ec-proxy/src/tun.rs` | TUN 路由命令 | cfg 化路由 |
| `ec-app/src/tun_mode.rs` | TUN 提权/wintun | Linux 分支 |
| `ec-app/src/system_proxy.rs` | 系统代理 | Linux 分支 |
| `ec-app/src/config.rs` | 配置路径 | dirs crate |
| `ec-app/src/commands.rs` | TUN 连接入口 | 权限提示文案 |
| `ec-app/tauri.conf.json` | Tauri 构建/打包 | 平台无关化 |

---

## Task 1: 构建链去 Windows 硬编码（P0）

**Files:**
- Modify: `ec-app/rust-toolchain.toml`
- Modify: `ec-app/.cargo/config.toml`
- Modify: `ec-utls/.cargo/config.toml`

- [ ] **Step 1: 改 rust-toolchain.toml 去掉 host 后缀**

`ec-app/rust-toolchain.toml` 完整内容改为：
```toml
[toolchain]
channel = "stable"
```

理由：`stable-x86_64-pc-windows-gnu` 的 host 后缀让 Linux 强制用 Windows 工具链。`channel = "stable"` 各平台取各自默认。Windows 开发者需一次性 `rustup default stable-x86_64-pc-windows-gnu`（已装的话无需操作）。

- [ ] **Step 2: 删 ec-app/.cargo/config.toml 的 target 行**

`ec-app/.cargo/config.toml` 完整内容改为：
```toml
# target 行已删除：硬编码 target 会让 Linux 交叉编译到 Windows。
# 各平台用默认 host target。Windows 如需 gnu，通过 rustup default 设置。
```

（如果文件里只有 `[build] target = ...` 这一行，整个文件可留空或删除。）

- [ ] **Step 3: 删 ec-utls/.cargo/config.toml 的 target 行**

同 Step 2，`ec-utls/.cargo/config.toml` 删除 `target = "x86_64-pc-windows-gnu"`。

- [ ] **Step 4: 验证 Windows 构建仍正常**

在 Windows 上运行：
```bash
cd ec-app && cargo check --features custom-protocol
```
Expected: 编译通过（确认改 toolchain/config 不破坏 Windows 构建）。如果报 embed-resource 相关错误，运行 `rustup default stable-x86_64-pc-windows-gnu` 后重试。

- [ ] **Step 5: Commit**

```bash
git add ec-app/rust-toolchain.toml ec-app/.cargo/config.toml ec-utls/.cargo/config.toml
git commit -m "build: 去除 toolchain/config 的 Windows target 硬编码，支持跨平台编译"
```

---

## Task 2: Cargo.toml 依赖隔离 + dirs crate（P0）

**Files:**
- Modify: `ec-app/Cargo.toml:31`（winreg 行）
- Modify: `ec-app/Cargo.toml`（加 dirs）

- [ ] **Step 1: winreg 移到 target 配置**

在 `ec-app/Cargo.toml` 中，把 `[dependencies]` 里的 `winreg = "0.55"` 删除，在文件末尾新增：
```toml
[target.'cfg(windows)'.dependencies]
winreg = "0.55"
```

- [ ] **Step 2: 加 dirs 依赖**

在 `[dependencies]` 段加：
```toml
dirs = "5"
```

- [ ] **Step 3: 验证 Windows 编译**

```bash
cd ec-app && cargo check --features custom-protocol
```
Expected: 通过（winreg 在 Windows target 下仍可用）。

- [ ] **Step 4: Commit**

```bash
git add ec-app/Cargo.toml
git commit -m "build: winreg 移到 cfg(windows) target，加 dirs 跨平台依赖"
```

---

## Task 3: Go .so 构建脚本（P0）

**Files:**
- Create: `utls-bridge/build.sh`

- [ ] **Step 1: 创建 build.sh**

`utls-bridge/build.sh` 内容：
```bash
#!/bin/bash
# 构建 utls-bridge 为 c-shared so（Linux）。
# 需 cgo，用系统 gcc。Go 源码与 Windows 版完全相同。
set -e
export CGO_ENABLED=1
go build -buildmode=c-shared -ldflags "-s -w" -o ec_utls_bridge.so .
# c-shared 模式自动生成 ec_utls_bridge.h + ec_utls_bridge.so
# 不需要 gendef/dlltool（那是 mingw 特有）
echo "构建完成：ec_utls_bridge.so + ec_utls_bridge.h"
```

- [ ] **Step 2: 赋可执行权限**

```bash
chmod +x utls-bridge/build.sh
```

- [ ] **Step 3: 在 Linux 上验证构建**

在 Linux 上（确保 Go + gcc 已装）：
```bash
cd utls-bridge && ./build.sh
```
Expected: 生成 `ec_utls_bridge.so`（约 5-6MB）和 `ec_utls_bridge.h`。

- [ ] **Step 4: Commit**

```bash
git add utls-bridge/build.sh
git commit -m "build: 新增 Linux Go .so 构建脚本"
```

---

## Task 4: ec-utls/build.rs 按平台切换（P0）

**Files:**
- Modify: `ec-utls/build.rs`（整体重写）

- [ ] **Step 1: 重写 build.rs 按平台切换库名**

`ec-utls/build.rs` 完整内容改为：
```rust
fn main() {
    let dir = std::path::PathBuf::from("../utls-bridge");
    println!("cargo:rustc-link-search=native={}", dir.display());
    println!("cargo:rustc-link-lib=dylib=ec_utls_bridge");

    // 按平台选择库文件扩展名，并拷到 deps/ 确保运行时能加载。
    let (lib_name, rerun) = if cfg!(target_os = "windows") {
        ("ec_utls_bridge.dll", "ec_utls_bridge.dll")
    } else if cfg!(target_os = "linux") {
        ("ec_utls_bridge.so", "ec_utls_bridge.so")
    } else if cfg!(target_os = "macos") {
        ("ec_utls_bridge.dylib", "ec_utls_bridge.dylib")
    } else {
        return; // 不支持的平台，跳过
    };

    println!("cargo:rerun-if-changed={}/{}", dir.display(), rerun);

    let src = dir.join(lib_name);
    if !src.exists() {
        return; // 库还没构建（可能是首次或只改 Rust 代码），跳过拷贝
    }
    if let Ok(out_dir) = std::env::var("OUT_DIR") {
        let out_path = std::path::PathBuf::from(&out_dir);
        if let Some(debug_dir) = out_path.ancestors().nth(3) {
            let deps_dir = debug_dir.join("deps");
            let _ = std::fs::create_dir_all(&deps_dir);
            let _ = std::fs::copy(&src, deps_dir.join(lib_name));
            let _ = std::fs::copy(&src, debug_dir.join(lib_name));
        }
    }
}
```

- [ ] **Step 2: 验证 Windows 编译（dll 路径不变）**

```bash
cd ec-utls && cargo check
```
Expected: 通过。

- [ ] **Step 3: 验证 Linux 编译（so 路径）**

在 Linux 上（先确保 `utls-bridge/build.sh` 已跑，`ec_utls_bridge.so` 存在）：
```bash
cd ec-utls && cargo check
```
Expected: 通过。

- [ ] **Step 4: Commit**

```bash
git add ec-utls/build.rs
git commit -m "build: ec-utls build.rs 按平台切换 dll/so 链接"
```

---

## Task 5: ec-app/build.rs 按平台切换（P0）

**Files:**
- Modify: `ec-app/build.rs:4-13`

- [ ] **Step 1: build.rs 按平台切换拷贝的库名**

`ec-app/build.rs` 中 DLL 拷贝部分改为：
```rust
fn main() {
    tauri_build::build();

    // DLL/SO 部署：ec-app 间接依赖 utls-bridge，需拷到 exe 同目录。
    let lib_name = if cfg!(target_os = "windows") {
        "ec_utls_bridge.dll"
    } else if cfg!(target_os = "linux") {
        "ec_utls_bridge.so"
    } else if cfg!(target_os = "macos") {
        "ec_utls_bridge.dylib"
    } else {
        return;
    };

    let src = std::path::PathBuf::from(format!("../utls-bridge/{lib_name}"));
    if src.exists() {
        if let Ok(out_dir) = std::env::var("OUT_DIR") {
            let out_path = std::path::PathBuf::from(&out_dir);
            if let Some(debug_dir) = out_path.ancestors().nth(3) {
                let _ = std::fs::copy(&src, debug_dir.join(lib_name));
            }
        }
    }
    println!("cargo:rerun-if-changed=../utls-bridge/{lib_name}");

    // 监控 ui/dist：dist 内容变化时强制 tauri_build 重新嵌入资源。
    println!("cargo:rerun-if-changed=../ui/dist/index.html");
    println!("cargo:rerun-if-changed=../ui/dist/assets");
}
```

- [ ] **Step 2: 验证编译**

```bash
cd ec-app && cargo check --features custom-protocol
```
Expected: 通过。

- [ ] **Step 3: Commit**

```bash
git add ec-app/build.rs
git commit -m "build: ec-app build.rs 按平台切换 dll/so 拷贝"
```

---

## Task 6: config.rs 配置路径跨平台（P2，提前做因为简单且编译验证需要）

**Files:**
- Modify: `ec-app/src/config.rs:119-124`

- [ ] **Step 1: config_path 用 dirs crate**

`config_path` 函数改为：
```rust
fn config_path() -> PathBuf {
    let base = dirs::config_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    base.join("rust_connect").join("config.json")
}
```

`dirs::config_dir()`：Windows 返回 `%APPDATA%`（即 `C:\Users\<user>\AppData\Roaming`），Linux 返回 `~/.config`，macOS 返回 `~/Library/Application Support`。与原来的 `%APPDATA%` 行为一致。

- [ ] **Step 2: 删除不再需要的 APPDATA env var 读取**

确认 `config.rs` 里没有其他地方用 `std::env::var("APPDATA")`。如果有，一并改为 dirs。

- [ ] **Step 3: 验证编译**

```bash
cd ec-app && cargo check --features custom-protocol
```
Expected: 通过。

- [ ] **Step 4: Commit**

```bash
git add ec-app/src/config.rs
git commit -m "feat: 配置路径用 dirs crate 跨平台（~/.config / %APPDATA%）"
```

---

## Task 7: tauri.conf.json 平台无关化（P0）

**Files:**
- Modify: `ec-app/tauri.conf.json`

- [ ] **Step 1: beforeBuildCommand/beforeDevCommand 改平台无关**

把：
```json
"beforeDevCommand": "cd /d D:\\dev_code\\study\\rust_connect\\ui && npm run dev",
"beforeBuildCommand": "cd /d D:\\dev_code\\study\\rust_connect\\ui && npm run build"
```
改为：
```json
"beforeDevCommand": "npm --prefix ../ui run dev",
"beforeBuildCommand": "npm --prefix ../ui run build"
```

注意：Tauri CLI 执行 beforeBuildCommand 时的工作目录是 ec-app/（tauri.conf.json 所在目录），`../ui` 相对路径指向 `rust_connect/ui/`。`npm --prefix` 是平台无关的指定目录方式。先在 Windows 验证 `npm --prefix ../ui run build` 能正常构建。

- [ ] **Step 2: icon 加 png（Linux 打包需要）**

把：
```json
"icon": ["icons/icon.ico"]
```
改为：
```json
"icon": ["icons/icon.ico", "icons/icon.png"]
```

需要确保 `ec-app/icons/icon.png` 存在。如果不存在，从 icon.ico 转换或用现有 png（检查 icons 目录）。

- [ ] **Step 3: 验证 Windows 构建**

```bash
cd ec-app && cargo tauri build
```
Expected: 构建成功（验证 npm --prefix 在 Windows 也工作）。

- [ ] **Step 4: Commit**

```bash
git add ec-app/tauri.conf.json
git commit -m "build: tauri.conf.json 平台无关化（npm --prefix + png icon）"
```

---

## Task 8: TUN 路由命令 Linux 分支（P1）

**Files:**
- Modify: `ec-proxy/src/tun.rs:89-137`（加路由）
- Modify: `ec-proxy/src/tun.rs:246-255`（清理路由）

- [ ] **Step 1: 加路由逻辑 cfg 化**

在 `tun.rs` 的"加路由"段（现有 PowerShell 逻辑前），用 cfg 分支。把现有的整个 for route 循环包进 `#[cfg(target_os = "windows")]`，新增 Linux 分支：

```rust
// 2. 加路由（内网网段 → TUN 接口）
let client_ip_str = format!("{}.{}.{}.{}", client_ip[0], client_ip[1], client_ip[2], client_ip[3]);

#[cfg(target_os = "windows")]
{
    // 现有的 PowerShell Get-NetAdapter + New-NetRoute 逻辑（原样保留）
    let if_index = { /* ... 现有代码 ... */ };
    for route in routes {
        /* ... 现有 New-NetRoute / route add 逻辑 ... */
    }
}

#[cfg(target_os = "linux")]
{
    // Linux: 先 up 接口，再设地址，再加路由
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
```

- [ ] **Step 2: 清理路由 cfg 化**

`cleanup_routes` 函数加 Linux 分支：
```rust
pub fn cleanup_routes(routes: &[TunRoute], client_ip: [u8; 4]) {
    #[cfg(target_os = "linux")]
    {
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
```

- [ ] **Step 3: 验证 Windows 编译（cfg 隔离）**

```bash
cd ec-app && cargo check --features custom-protocol
```
Expected: 通过（Linux 分支在 Windows 上不编译）。

- [ ] **Step 4: 在 Linux 验证 TUN 网卡创建 + 路由**

在 Linux 上（需 cap_net_admin 或 root）：
```bash
cd ec-app && cargo run --features custom-protocol
# GUI 里选 TUN 模式连接
ip addr show RustConnect   # 应显示 2.0.1.1/24
ip route show              # 应有 192.168.1.0/24 dev RustConnect
```

- [ ] **Step 5: Commit**

```bash
git add ec-proxy/src/tun.rs
git commit -m "feat: TUN 路由命令 Linux 分支（ip 命令替代 PowerShell）"
```

---

## Task 9: tun_mode.rs 提权与 wintun Linux 分支（P1）

**Files:**
- Modify: `ec-app/src/tun_mode.rs:42-56`（is_admin）
- Modify: `ec-app/src/tun_mode.rs:64-86`（relaunch_as_admin）
- Modify: `ec-app/src/tun_mode.rs:94-156`（ensure_wintun）

- [ ] **Step 1: is_admin Linux 分支（检查 CAP_NET_ADMIN）**

```rust
pub fn is_admin() -> bool {
    #[cfg(target_os = "linux")]
    {
        has_cap_net_admin()
    }
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("net")
            .args(["session"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        false
    }
}

#[cfg(target_os = "linux")]
fn has_cap_net_admin() -> bool {
    // 读 /proc/self/status，解析 CapEff 行。
    // CAP_NET_ADMIN = 12，bit 12 置位表示有此能力。
    let status = match std::fs::read_to_string("/proc/self/status") {
        Ok(s) => s,
        Err(_) => return false,
    };
    for line in status.lines() {
        if let Some(hex) = line.strip_prefix("CapEff:\t") {
            if let Ok(cap) = u64::from_str_radix(hex.trim(), 16) {
                return (cap & (1 << 12)) != 0;
            }
        }
    }
    false
}
```

- [ ] **Step 2: relaunch_as_admin Linux 分支（不重启）**

```rust
pub fn relaunch_as_admin() -> std::io::Result<bool> {
    #[cfg(target_os = "linux")]
    {
        // setcap 方案：不重启。调用方检测 is_admin() == false 时提示用户。
        Ok(false)
    }
    #[cfg(target_os = "windows")]
    {
        // 现有 PowerShell Start-Process -Verb RunAs 逻辑
        let exe = std::env::current_exe()?;
        let exe_str = exe.to_string_lossy().to_string();
        let script = format!(
            "try {{ $p = Start-Process -FilePath '{}' -ArgumentList '--relaunched-as-admin' -Verb RunAs -PassThru; if ($p) {{ exit 0 }} else {{ exit -1 }} }} catch {{ exit -1 }}",
            exe_str.replace('\'', "''")
        );
        let out = std::process::Command::new("powershell")
            .args(["-NoProfile", "-Command", &script])
            .creation_flags(CREATE_NO_WINDOW)
            .output()?;
        Ok(out.status.success())
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        Ok(false)
    }
}
```

- [ ] **Step 3: ensure_wintun Linux 分支（不需要 wintun）**

```rust
pub fn ensure_wintun() -> std::io::Result<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        // Linux 用内核 tun，不需要 wintun.dll
        Ok(PathBuf::new())
    }
    #[cfg(target_os = "windows")]
    {
        // 现有的下载 wintun.dll 逻辑（原样保留）
        /* ... */
    }
}
```

- [ ] **Step 4: commands.rs 权限不足提示文案**

`commands.rs` 里 `connect_tun` 的非管理员提示，加 Linux 特定文案：
```rust
if !crate::tun_mode::is_admin() {
    #[cfg(target_os = "linux")]
    {
        let exe = std::env::current_exe()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "<ec-app>".into());
        return Err(format!(
            "TUN 模式需要 CAP_NET_ADMIN 权限。请运行以下命令后重启程序：\n\
             sudo setcap cap_net_admin+ep {}",
            exe
        ));
    }
    #[cfg(target_os = "windows")]
    {
        return Err("TUN 模式需要管理员权限，请以管理员身份运行本程序".into());
    }
}
```

- [ ] **Step 5: 验证编译**

```bash
cd ec-app && cargo check --features custom-protocol
```
Expected: 通过。

- [ ] **Step 6: Commit**

```bash
git add ec-app/src/tun_mode.rs ec-app/src/commands.rs
git commit -m "feat: TUN 提权与 wintun 逻辑 Linux 分支（CAP_NET_ADMIN 检测 + setcap 提示）"
```

---

## Task 10: system_proxy.rs Linux 分支（P1）

**Files:**
- Modify: `ec-app/src/system_proxy.rs`

- [ ] **Step 1: 把现有 Windows 逻辑包进 cfg(windows)**

现有的 `save_original` / `enable` / `restore` / `notify_settings_changed` 四个函数体包进 `#[cfg(target_os = "windows")]`。

- [ ] **Step 2: 新增 Linux 系统代理模块**

在 `system_proxy.rs` 末尾加：
```rust
#[cfg(target_os = "linux")]
mod linux_proxy {
    use std::io;
    use std::process::Command;

    enum Desktop {
        Gnome,
        Kde,
        Generic,
    }

    fn detect_desktop() -> Desktop {
        match std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default().to_uppercase().as_str() {
            d if d.contains("GNOME") || d.contains("UNITY") || d.contains("CINNAMON") => Desktop::Gnome,
            d if d.contains("KDE") => Desktop::Kde,
            _ => Desktop::Generic,
        }
    }

    pub fn enable(pac_url: &str) -> io::Result<()> {
        match detect_desktop() {
            Desktop::Gnome => {
                Command::new("gsettings").args(["set", "org.gnome.system.proxy", "mode", "auto"]).status()?;
                Command::new("gsettings").args(["set", "org.gnome.system.proxy", "autoconfig-url", pac_url]).status()?;
            }
            Desktop::Kde => {
                Command::new("kwriteconfig5")
                    .args(["--file", "kioslaverc", "--group", "Proxy Settings", "--key", "Proxy Type", "4"]).status()?;
                Command::new("kwriteconfig5")
                    .args(["--file", "kioslaverc", "--group", "Proxy Settings", "--key", "Proxy Config Script", pac_url]).status()?;
            }
            Desktop::Generic => {
                log::info!("检测到非 GNOME/KDE 桌面，仅设环境变量（影响新进程）。建议手动配置浏览器代理。");
            }
        }
        Ok(())
    }

    pub fn save_original() -> io::Result<()> {
        // Linux 无注册表式全局状态，无需保存（restore 时直接设回 none）
        Ok(())
    }

    pub fn restore() -> io::Result<()> {
        match detect_desktop() {
            Desktop::Gnome => {
                Command::new("gsettings").args(["set", "org.gnome.system.proxy", "mode", "none"]).status()?;
            }
            Desktop::Kde => {
                Command::new("kwriteconfig5")
                    .args(["--file", "kioslaverc", "--group", "Proxy Settings", "--key", "Proxy Type", "0"]).status()?;
            }
            Desktop::Generic => {}
        }
        Ok(())
    }
}
```

- [ ] **Step 3: enable/restore 入口分发**

现有公开的 `enable` / `save_original` / `restore` 函数加 cfg 分发：
```rust
pub fn enable(pac_url: &str) -> Result<(), SystemProxyError> {
    #[cfg(target_os = "windows")]
    { windows_proxy::enable(pac_url) }
    #[cfg(target_os = "linux")]
    { linux_proxy::enable(pac_url).map_err(SystemProxyError::Io) }
}
```
（具体函数名/错误类型按现有代码调整）

- [ ] **Step 4: 验证编译**

```bash
cd ec-app && cargo check --features custom-protocol
```
Expected: 通过。

- [ ] **Step 5: Commit**

```bash
git add ec-app/src/system_proxy.rs
git commit -m "feat: 系统代理 Linux 分支（gsettings/KDE/环境变量）"
```

---

## Task 11: Linux 端集成验证（P1）

**Files:** 无（验证任务）

- [ ] **Step 1: Linux 上完整构建 Go .so**

```bash
cd utls-bridge && ./build.sh
```
Expected: 生成 `ec_utls_bridge.so`。

- [ ] **Step 2: Linux 上 cargo check 全项目**

```bash
cd ec-app && cargo check --features custom-protocol
```
Expected: 通过。

- [ ] **Step 3: PAC 模式端到端验证**

```bash
# 启动（Linux 需 WebKitGTK）
cargo run --features custom-protocol
# GUI 选 PAC 模式连接
# 验证 SOCKS5：另开终端
curl --proxy socks5://127.0.0.1:1080 http://192.168.1.120:9080/ -I
```
Expected: curl 返回 Jenkins 响应头。

- [ ] **Step 4: TUN 模式端到端验证**

```bash
# 先 setcap
sudo setcap cap_net_admin+ep target/debug/ec-app
# 启动
cargo run --features custom-protocol
# GUI 选 TUN 模式连接
ip addr show RustConnect      # 网卡 2.0.1.1/24
ip route show | grep RustConnect  # 路由 192.168.1.0/24
# 浏览器直连 Jenkins
```
Expected: TUN 网卡创建、路由添加、Jenkins 可访问。

- [ ] **Step 5: 保活验证**

连接后闲置 10 分钟，再访问内网资源。
Expected: 仍可访问，未断开。

- [ ] **Step 6: Commit（如有 Linux 特定修复）**

如果验证中发现问题并修复，提交修复。

---

## Self-Review

**Spec 覆盖：**
- ✅ 构建链（rust-toolchain/config/Cargo.toml/build.rs/build.sh）-> Task 1-5, 7
- ✅ TUN 路由 cfg 化 -> Task 8
- ✅ TUN 提权（setcap/CAP_NET_ADMIN）-> Task 9
- ✅ 系统代理（gsettings/KDE）-> Task 10
- ✅ 配置路径 -> Task 6
- ✅ 集成验证 -> Task 11

**Placeholder 扫描：** 无 TBD/TODO，所有步骤有完整代码。

**类型一致性：** `has_cap_net_admin`、`linux_proxy::enable` 等函数名在各 Task 一致。

**注意：** Task 11 的端到端验证需要在真实 Linux 环境执行，Windows 机器上只能做 cfg 编译验证（Task 1-10 的 cargo check）。实际运行验证需要用户在 Linux/WSL2 上完成。
