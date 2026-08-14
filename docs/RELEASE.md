# 发版与自动更新

## 架构

- **客户端**：`tauri-plugin-updater`。启动 3 秒后静默检查更新，有新版本时界面顶部显示横幅（版本号 + 更新说明 + 进度条），用户确认后下载安装。Windows 上 NSIS 安装器装完自动重启应用（`currentUser` 模式，免 UAC）。
- **服务端**：GitHub Releases 静态 `latest.json`。客户端 endpoint 双通道：GitHub 直连 + gh-proxy 镜像（国内兜底）。
- **签名**：minisign 密钥对。`tauri build` 对安装包签名生成 `.sig`，客户端用 `tauri.conf.json` 里的公钥校验，防篡改。

## 发版流程

```bash
# 1. 改版本号（唯一来源：ec-app/Cargo.toml）
#    version = "0.x.y"

# 2. 提交并打 tag
git add -A
git commit -m "release v0.x.y"
git tag v0.x.y
git push origin main --tags

# 3. GitHub Actions 自动：构建(Windows+Linux) -> 签名 -> 发 Release -> 生成 latest.json（含镜像前缀）
#    客户端下次启动即检测到新版本
```

## 签名密钥管理（重要）

- 私钥：`~/.tauri/rust_connect.key`（无密码），已存入 GitHub Secrets（`TAURI_SIGNING_PRIVATE_KEY`）。
- 公钥：`~/.tauri/rust_connect.key.pub`，已写入 `ec-app/tauri.conf.json` 的 `plugins.updater.pubkey`。
- **请备份私钥到安全位置（密码管理器/离线存储）**。私钥丢失后，已安装用户将永远无法收到更新（只能换公钥发新版挽回）。
- 私钥绝不能提交进仓库（`.gitignore` 已加 `*.key` 防御）。

## 本地构建安装包（调试用）

```bash
cd rust_connect
export TAURI_SIGNING_PRIVATE_KEY="$(cat ~/.tauri/rust_connect.key)"
export TAURI_SIGNING_PRIVATE_KEY_PASSWORD=""   # 密钥无密码，置空防交互 prompt
./ui/node_modules/.bin/tauri build
# 产物：ec-app/target/release/bundle/nsis/RustConnect_x.y.z_x64-setup.exe (+.sig)
#       ec-app/target/release/bundle/msi/RustConnect_x.y.z_x64_en-US.msi (+.sig)
```

注意 `tauri build` 必须从仓库根目录跑（tauri CLI 在子目录 ec-app/ 里找 tauri.conf.json）。

## 镜像前缀

`latest.json` 内的下载 url 在 CI 里统一加 `https://gh-proxy.com/` 前缀（gh-proxy 只代理请求不重写 JSON 内容，必须在生成时写对，否则国内用户取到 JSON 却下载不动）。镜像失效时改 `.github/workflows/release.yml` 里 `mirror-updates` job 的 `MIRROR_PREFIX`（置空 = GitHub 直链）。

## 平台说明

- **Windows**：NSIS `currentUser` 安装（`%LOCALAPPDATA%`，免管理员）。更新走 NSIS `/P /UPDATE /R`（进度条 + 装完自动重启）。MSI 也产出但仅供手动安装（MSI 是 perMachine，更新会弹 UAC）。
- **Linux**：`.deb` / `.AppImage` + `.rpm`。
- 资源文件（`ec_utls_bridge.dll/.so`、`WebView2Loader.dll`）通过平台配置文件 `tauri.windows.conf.json` / `tauri.linux.conf.json` 声明（`bundle.windows` 不支持 resources 字段）。
