# Linux 端移植设计

> 状态：设计完成，待实现
> 日期：2026-08-05
> 前置：Windows 端已完成 PAC + TUN 双模式、保活、安装包

## 目标

将 rust_connect 从 Windows-only 移植到 Linux，第一版实现：
- **PAC 模式**：SOCKS5 代理 + 系统代理设置（gsettings/KDE + 环境变量）
- **TUN 模式**：tun2 虚拟网卡全局代理，setcap 免提权方案

**不在本次范围**：macOS、鸿蒙、安装包自动化（deb/rpm/AppImage 的打包脚本）、Firefox 等浏览器单独代理配置。

## 关键决策（已与用户确认）

| 决策点 | 选择 | 理由 |
|--------|------|------|
| 实现路径 | 方案 A：cfg 隔离 | Windows 特定代码就 3 文件，cfg 足够清晰，YAGNI |
| 移植范围 | PAC + TUN 都要 | 完整功能对齐 |
| TUN 提权 | setcap cap_net_admin | 免每次提权，体验最好 |
| 权限不足处理 | 提示用户手动 setcap | 实现简单，不依赖 pkexec |
| 系统代理 | 环境变量 + GNOME/KDE 检测 | 覆盖主流桌面 |

## 架构：分层移植性评估

```
跨平台免改（已验证 Windows 跑通）：
  ec-login/      登录（reqwest + RSA）          全跨平台
  ec-protocol/   token + 隧道 + L3Conn + 保活    全跨平台
  ec-utls/       Go utls FFI（ffi.rs/lib.rs）    接口跨平台，build.rs 需改
  ec-proxy/      SOCKS5 + smoltcp               全跨平台
  ec-proxy/tun.rs 转发线程 + IPv4 过滤           逻辑跨平台，路由命令需 cfg

需改动：
  构建链          rust-toolchain / .cargo/config / Cargo.toml / build.rs / build.sh
  ec-app/tun_mode.rs   is_admin / relaunch_as_admin / ensure_wintun（Linux 分支）
  ec-app/system_proxy.rs  GNOME/KDE 检测 + 环境变量
  ec-app/config.rs      配置路径（APPDATA -> ~/.config）
  tauri.conf.json       beforeBuildCommand / icon / bundle
```

## 详细设计

### 1. 构建链（P0，编译前提）

#### 1.1 `ec-app/rust-toolchain.toml`
```toml
[toolchain]
channel = "stable"   # 去掉 -x86_64-pc-windows-gnu 后缀
```
- Windows 上 `stable` 频道默认取已安装的 host 工具链（gnu 或 msvc）。
  之前 embed-resource bug 靠 pin gnu 修复；改为 channel=stable 后，Windows 开发者需一次性 `rustup default stable-x86_64-pc-windows-gnu`（不影响已设好的环境）。
- Linux 上 stable 自动取 Linux 工具链。

#### 1.2 `ec-app/.cargo/config.toml` + `ec-utls/.cargo/config.toml`
**删除** `target = "x86_64-pc-windows-gnu"` 行。
- 此行硬编码 target，Linux 上会让 cargo 默认交叉编译到 Windows 而失败。
- 删除后各平台用默认 host target。
- 保留 linker 等 Windows 特定配置到 `[target.x86_64-pc-windows-gnu]` 段（如有）。

#### 1.3 `ec-app/Cargo.toml` 依赖隔离
```toml
# 从 [dependencies] 移除 winreg，改为：
[target.'cfg(windows)'.dependencies]
winreg = "0.55"

# 新增跨平台依赖（配置路径）：
[dependencies]
dirs = "5"   # ~/.config（Linux）/ %APPDATA%（Windows）
```

#### 1.4 Go 构建脚本：新增 `utls-bridge/build.sh`
```bash
#!/bin/bash
set -e
export CGO_ENABLED=1
# Linux 用系统 gcc；CC 可被环境变量覆盖
go build -buildmode=c-shared -ldflags "-s -w" -o ec_utls_bridge.so .
# Linux c-shared 自动生成 ec_utls_bridge.h + ec_utls_bridge.so
# 不需要 gendef/dlltool（那是 mingw 特有）
echo "构建完成：ec_utls_bridge.so + ec_utls_bridge.h"
```
- `build.bat` 保留不动（Windows 用），`build.sh` 给 Linux 用。
- Go 源码（main.go/connection.go）完全跨平台，无需改。

#### 1.5 `ec-utls/build.rs` 按平台切换
```rust
fn main() {
    #[cfg(target_os = "windows")]
    {
        println!("cargo:rustc-link-lib=dylib=ec_utls_bridge");
        println!("cargo:rustc-link-search=../utls-bridge");
        // 拷 .dll 到 deps/
        copy_lib("ec_utls_bridge.dll");
    }
    #[cfg(target_os = "linux")]
    {
        println!("cargo:rustc-link-lib=dylib=ec_utls_bridge");
        println!("cargo:rustc-link-search=../utls-bridge");
        // 拷 .so 到 deps/
        copy_lib("ec_utls_bridge.so");
    }
}

fn copy_lib(name: &str) {
    let src = format!("../utls-bridge/{name}");
    if let Ok(out_dir) = std::env::var("OUT_DIR") {
        // ... 拷贝逻辑（复用现有 ancestors().nth(3) 模式）
    }
    println!("cargo:rerun-if-changed=../utls-bridge/{name}");
}
```

#### 1.6 `ec-app/build.rs` 同理
按 target_os 切换拷贝的库名（.dll / .so）。

### 2. TUN 模式 Linux 化（P1）

#### 2.1 `ec-proxy/src/tun.rs` 路由命令 cfg 化

**ifIndex 查询 + 加路由**（替换现有 PowerShell 逻辑）：
```rust
#[cfg(target_os = "linux")]
{
    // Linux: ip link set <name> up; ip addr add <ip>/24 dev <name>; ip route add <net>/24 dev <name>
    let _ = Command::new("ip").args(["link", "set", "RustConnect", "up"])
        .creation_flags_if_windows().status();
    let _ = Command::new("ip")
        .args(["addr", "add", &format!("{}/24", client_ip_str), "dev", "RustConnect"])
        .status();
    for route in routes {
        let _ = Command::new("ip")
            .args(["route", "add", &format!("{}/24", route.network), "dev", "RustConnect"])
            .status();
    }
}
#[cfg(target_os = "windows")]
{
    // 现有 PowerShell Get-NetAdapter + New-NetRoute 逻辑保留
}
```

**清理路由**：
```rust
#[cfg(target_os = "linux")]
{ /* ip route del <net>/24 dev RustConnect */ }
#[cfg(target_os = "windows")]
{ /* 现有 route delete */ }
```

**转发线程 + IPv4 单播过滤**：完全复用，不改。

#### 2.2 `ec-app/src/tun_mode.rs` 提权与 wintun

```rust
pub fn is_admin() -> bool {
    #[cfg(target_os = "linux")]
    {
        // 检查 CAP_NET_ADMIN：读 /proc/self/status 的 CapEff 位
        has_cap_net_admin()
    }
    #[cfg(target_os = "windows")]
    { /* 现有 net session */ }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    { false }
}

#[cfg(target_os = "linux")]
fn has_cap_net_admin() -> bool {
    // 解析 /proc/self/status：CapEff 行的 bit 12 = CAP_NET_ADMIN
    // 或用 caps crate（更清晰但加依赖）
}

#[cfg(target_os = "linux")]
pub fn relaunch_as_admin() -> std::io::Result<bool> {
    // setcap 方案：不重启，返回 false。
    // 调用方（commands.rs）检测到无权限时，提示用户运行 setcap。
    Ok(false)
}

#[cfg(target_os = "linux")]
pub fn ensure_wintun() -> std::io::Result<PathBuf> {
    // Linux 不需要 wintun，返回空路径
    Ok(PathBuf::new())
}
```

**权限不足提示**（commands.rs 的 connect_tun）：
```rust
if !is_admin() {
    return Err(format!(
        "TUN 模式需要 CAP_NET_ADMIN 权限。请运行：\n\
         sudo setcap cap_net_admin+ep {}",
        std::env::current_exe()?.display()
    ));
}
```

### 3. 系统代理 Linux 化（P1）

`ec-app/src/system_proxy.rs` 结构调整：

**跨平台复用**：`generate_pac` / `start_pac_server` / `ip_to_num`（不改）

**Windows 保留**：`save_original` / `enable` / `restore` / `notify_settings_changed`
（现有注册表逻辑包进 `#[cfg(target_os = "windows")]`）

**新增 Linux 实现**：
```rust
#[cfg(target_os = "linux")]
mod linux_proxy {
    fn detect_desktop() -> DesktopEnv {
        // 读 XDG_CURRENT_DESKTOP 环境变量
        // gnome/unity/cinnamon -> Gnome
        // kde -> Kde
        // 其他 -> Generic
    }

    pub fn enable(pac_url: &str) -> io::Result<()> {
        match detect_desktop() {
            DesktopEnv::Gnome => {
                // gsettings set org.gnome.system.proxy mode 'auto'
                // gsettings set org.gnome.system.proxy autoconfig-url '<pac_url>'
            }
            DesktopEnv::Kde => {
                // kwriteconfig5 --file kioslaverc --group "Proxy Settings" \
                //   --key "Proxy Type" 4 --key "Proxy Config Script" "<pac_url>"
            }
            DesktopEnv::Generic => {
                // 只设环境变量（影响新进程）：HTTP_PROXY/HTTPS_PROXY/ALL_PROXY
            }
        }
        Ok(())
    }

    pub fn restore() -> io::Result<()> {
        // gsettings set org.gnome.system.proxy mode 'none'
        // 或 kwriteconfig5 反向操作
    }
}
```

**已知限制**（写入文档）：
- 环境变量只影响新启动的进程（curl/git/wget 等），已运行程序不受影响
- Firefox 有独立代理设置，可能不跟随 GNOME/KDE 系统 PAC
- KDE 的 PAC 支持（Proxy Config Script）兼容性不如 GNOME

### 4. 配置路径（P2）

`ec-app/src/config.rs`：
```rust
fn config_dir() -> PathBuf {
    #[cfg(target_os = "windows")]
    { /* 现有 APPDATA 逻辑 */ }
    #[cfg(target_os = "linux")]
    {
        dirs::config_dir()  // ~/.config
            .unwrap_or_else(|| PathBuf::from("."))
            .join("rust_connect")
    }
}
```

### 5. tauri.conf.json 调整

```json
{
  "build": {
    "beforeBuildCommand": "npm --prefix ../ui run build",   // 平台无关
    "beforeDevCommand": "npm --prefix ../ui run dev"
  },
  "bundle": {
    "icon": ["icons/icon.ico", "icons/icon.png"],   // 加 png（Linux 需要）
    "resources": {
      "../utls-bridge/ec_utls_bridge.dll": "ec_utls_bridge.dll"
    }
    // Linux 下 .so 的 resources 配置需用 Tauri 的平台特定 bundle 配置
    // 或在 build 阶段确保 .so 在 exe 同目录（rpath 或 LD_LIBRARY_PATH）
  }
}
```

WebView2Loader.dll 的 resources：Windows-only，配到条件段或 Linux 忽略。

## 验证计划

| 阶段 | 验证项 | 方法 |
|------|--------|------|
| 编译 | Linux 上 `cargo build` 通过 | Ubuntu 22.04 / WSL2 |
| PAC | SOCKS5 代理可连 Jenkins | curl --proxy socks5://127.0.0.1:1080 |
| PAC | 系统代理生效（GNOME） | gsettings get org.gnome.system.proxy |
| TUN | 网卡创建 + 路由 | ip addr / ip route |
| TUN | 全局代理访问 Jenkins | 浏览器直连 |
| TUN | 无 cap 时的提示 | 未 setcap 时运行 |
| 保活 | 闲置 10 分钟不断开 | 连接后等待 |

## 风险与注意事项

1. **embed-resource bug 复发**：改 channel=stable 后，Windows 构建需确认仍用 gnu。缓解：文档注明 Windows 开发者 `rustup default stable-x86_64-pc-windows-gnu`。
2. **Go .so 运行时加载**：Linux 下 .so 需在搜索路径。缓解：build.rs 拷到 deps/，或打包时放 /usr/lib，或设 rpath。
3. **tun2 Linux 后端**：需确认 tun2 4.0.0 在 Linux 上用 /dev/net/tun 的 API 与现有代码兼容（device.split() 等）。
4. **WebKitGTK**：Tauri 2 Linux 依赖 WebKitGTK2，目标机器需安装 `libwebkit2gtk-4.1`。
5. **Linux 系统代理体验**：不如 Windows 全局生效，PAC 支持参差不齐，需文档说明并建议用户配合浏览器插件（如 Proxy SwitchyOmega）。
