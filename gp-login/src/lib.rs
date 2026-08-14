//! GlobalProtect 登录模块（纯 Rust + reqwest）。
//!
//! 参考 yuezk/GlobalProtect-openconnect 的 gpapi crate 实现 GP 的三步登录流程：
//!   1) portal prelogin  — POST /global-protect/prelogin.esp，判断认证方式（标准/SAML）
//!   2) portal getconfig — POST /global-protect/getconfig.esp，拿 portal-userauthcookie + gateway 列表
//!   3) gateway login    — POST /ssl-vpn/login.esp，拿 authcookie（拼成 openconnect --cookie 的值）
//!
//! 登录成功后返回的 cookie 交给外部 openconnect --protocol=gp 建立隧道。
//! 这部分走普通 HTTPS，不涉及 GP 隧道协议（gpst）。

pub mod error;

pub use error::GpLoginError;

use std::time::Duration;

/// GP 客户端伪装的 app-version（对应 openconnect gpst.c 的 GPST_VERSION）。
/// 现代 PAN-OS 要求 ≥6.1.4，9.12 发布版的 6.1.2-82 会被拒，这里用 6.3.0-33（master 主干值）。
pub const GP_APP_VERSION: &str = "6.3.0-33";

/// 登录所需配置。
#[derive(Debug, Clone)]
pub struct GpLoginConfig {
    /// 形如 "114.250.31.2:4430"，无协议前缀。
    pub server: String,
    pub username: String,
    pub password: String,
}

/// 登录成功后交给 openconnect 的认证结果。
#[derive(Debug, Clone, serde::Serialize)]
pub struct GpAuthResult {
    /// 选中的 gateway 地址（形如 "114.250.31.2:4430"）。
    pub gateway: String,
    /// openconnect --cookie 的值（authcookie=...&user=...&computer=...）。
    pub cookie: String,
    /// portal 地址（来自 gateway login 响应）。
    pub portal: String,
    /// 用户名（来自 gateway login 响应）。
    pub user: String,
}

/// 客户端环境信息（登录请求里要用），构造一次反复用。
struct ClientEnv {
    /// 本机名（GP computer 字段）。
    computer: String,
    /// 客户端唯一标识（GP host-id 字段，用 uuid v4）。
    host_id: String,
    /// 伪装的操作系统版本字符串（GP os-version 字段）。
    os_version: String,
}

impl ClientEnv {
    fn new() -> Result<Self, GpLoginError> {
        let computer = hostname::get()
            .map_err(|e| GpLoginError::Hostname(e.to_string()))?
            .to_string_lossy()
            .to_string();
        Ok(Self {
            computer,
            host_id: uuid::Uuid::new_v4().to_string().to_uppercase(),
            // 伪装成 Ubuntu 22.04，PAN-OS 不严格校验这个
            os_version: "Ubuntu 22.04.4 LTS".to_string(),
        })
    }
}

/// 构造 HTTP 客户端：忽略自签名证书（GP 服务器常自签名），User-Agent 设为 PAN GlobalProtect。
fn build_http_client() -> Result<reqwest::blocking::Client, GpLoginError> {
    reqwest::blocking::Client::builder()
        .danger_accept_invalid_certs(true)
        .user_agent("PAN GlobalProtect")
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(GpLoginError::from)
}

/// 把 "host:port" 标准化为 "https://host:port"（若无 scheme）。
fn normalize_server(server: &str) -> String {
    let s = server.trim();
    if s.starts_with("http://") || s.starts_with("https://") {
        s.trim_end_matches('/').to_string()
    } else {
        format!("https://{}", s.trim_end_matches('/'))
    }
}

/// 执行登录主流程，返回交给 openconnect 的认证结果。
///
/// 步骤：
///   1. portal prelogin（判断 SAML）
///   2. portal getconfig（拿 portal-userauthcookie + gateway 列表）
///   3. gateway login（拿 authcookie，拼成 openconnect --cookie）
pub fn login(cfg: &GpLoginConfig) -> Result<GpAuthResult, GpLoginError> {
    let base = normalize_server(&cfg.server);
    let env = ClientEnv::new()?;
    let client = build_http_client()?;

    // ---- 步骤 1：portal prelogin（判断认证方式）----
    log::info!("[gp-login] 步骤1: portal prelogin -> {}", base);
    let prelogin_body = format!(
        "tmp=tmp&clientVer=4100&clientos=Linux&os-version={os}&host-id={hid}&ipv6-support=yes",
        os = urlencoding::encode(&env.os_version),
        hid = urlencoding::encode(&env.host_id),
    );
    let prelogin_resp = client
        .post(format!("{base}/global-protect/prelogin.esp"))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(prelogin_body)
        .send()
        .map_err(GpLoginError::from)?;
    let prelogin_xml = prelogin_resp.text().map_err(GpLoginError::from)?;
    log::debug!("[gp-login] prelogin 响应: {}", truncate_for_log(&prelogin_xml));

    // 判断 SAML：同时有 saml-auth-method 和 saml-request 即为 SAML
    if let Some((_, _)) = parse_saml_markers(&prelogin_xml)? {
        return Err(GpLoginError::SamlRequired);
    }

    // ---- 步骤 2：portal getconfig（拿 portal-userauthcookie + gateway 列表）----
    log::info!("[gp-login] 步骤2: portal getconfig -> {}", base);
    let getconfig_body = format!(
        "user={user}&passwd={passwd}&passcode=&portal-userauthcookie=empty&portal-prelogonuserauthcookie=empty&inputStr=&clientVer=4100&clientos=Linux&clientgpversion={gpver}&computer={computer}&os-version={os}&host-id={hid}&ipv6-support=yes&cfg-hash=&future-config=&csc-digest=&config-digest=&csc-support=yes&swg-auth-token=0&swg-nonce=0&ok=Login",
        user = urlencoding::encode(&cfg.username),
        passwd = urlencoding::encode(&cfg.password),
        gpver = urlencoding::encode(GP_APP_VERSION),
        computer = urlencoding::encode(&env.computer),
        os = urlencoding::encode(&env.os_version),
        hid = urlencoding::encode(&env.host_id),
    );
    let getconfig_resp = client
        .post(format!("{base}/global-protect/getconfig.esp"))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(getconfig_body)
        .send()
        .map_err(GpLoginError::from)?;
    let getconfig_xml = getconfig_resp.text().map_err(GpLoginError::from)?;
    log::debug!(
        "[gp-login] getconfig 响应: {}",
        truncate_for_log(&getconfig_xml)
    );

    // 解析 portal-userauthcookie（关键 token）和 gateway 列表
    let (portal_cookie, gateways) = parse_portal_config(&getconfig_xml)?;
    if portal_cookie.is_empty() {
        // getconfig 失败：可能是认证错误。尝试从响应里提取错误信息
        if let Some(msg) = parse_error_message(&getconfig_xml) {
            return Err(GpLoginError::AuthFailed(msg));
        }
        return Err(GpLoginError::NoAuthCookie);
    }
    log::info!(
        "[gp-login] 拿到 portal-userauthcookie (len={}), gateway 数={}",
        portal_cookie.len(),
        gateways.len()
    );

    // 选 gateway：优先用 portal 返回的 external gateway；列表空则 fallback 用 portal server 本身
    let gateway = if let Some(gw) = gateways.first() {
        gw.clone()
    } else {
        // server 形如 "https://114.250.31.2:4430"，gateway login 用同样的地址
        cfg.server.clone()
    };
    log::info!("[gp-login] 选中 gateway: {}", gateway);

    // ---- 步骤 3：gateway login（拿 authcookie）----
    // gateway base 可能和 portal 不同，标准化
    let gw_base = normalize_server(&gateway);
    log::info!("[gp-login] 步骤3: gateway login -> {}", gw_base);
    let login_body = format!(
        "user={user}&passwd=&prelogin-cookie=&portal-userauthcookie={pc}&portal-prelogonuserauthcookie=&prot=https:&jnlpReady=jnlpReady&ok=Login&direct=yes&clientVer=4100&clientos=Linux&clientgpversion={gpver}&computer={computer}&os-version={os}&server={server}&host-id={hid}&ipv6-support=yes",
        user = urlencoding::encode(&cfg.username),
        pc = urlencoding::encode(&portal_cookie),
        gpver = urlencoding::encode(GP_APP_VERSION),
        computer = urlencoding::encode(&env.computer),
        os = urlencoding::encode(&env.os_version),
        // server 字段放 gateway 的 host（去掉 scheme）
        server = urlencoding::encode(&strip_scheme(&gw_base)),
        hid = urlencoding::encode(&env.host_id),
    );
    let login_resp = client
        .post(format!("{gw_base}/ssl-vpn/login.esp"))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(login_body)
        .send()
        .map_err(GpLoginError::from)?;
    let login_xml = login_resp.text().map_err(GpLoginError::from)?;
    log::debug!(
        "[gp-login] gateway login 响应: {}",
        truncate_for_log(&login_xml)
    );

    // 解析 jnlp <argument> 数组
    let args = parse_jnlp_arguments(&login_xml)?;
    // arg[1]=authcookie, arg[3]=portal, arg[4]=user, arg[7]=domain
    // 索引不足或值为 (null)/-1/空 则跳过
    let authcookie = arg_at(&args, 1);
    if authcookie.is_empty() {
        // login 失败：尝试提取错误信息
        if let Some(msg) = parse_error_message(&login_xml) {
            return Err(GpLoginError::AuthFailed(msg));
        }
        return Err(GpLoginError::AuthFailed(format!(
            "gateway login 未返回 authcookie，原始响应: {}",
            truncate_for_log(&login_xml)
        )));
    }
    let portal = arg_at(&args, 3);
    // user 为空时回退到登录用的用户名
    let user = {
        let u = arg_at(&args, 4);
        if u.is_empty() { cfg.username.clone() } else { u }
    };
    let domain = arg_at(&args, 7);

    // 拼成 openconnect --cookie 的值
    let mut cookie = format!("authcookie={}", urlencoding::encode(&authcookie));
    cookie.push_str("&persistent-cookie=");
    if !portal.is_empty() {
        cookie.push_str(&format!("&portal={}", urlencoding::encode(&portal)));
    }
    cookie.push_str(&format!("&user={}", urlencoding::encode(&user)));
    if !domain.is_empty() {
        cookie.push_str(&format!("&domain={}", urlencoding::encode(&domain)));
    }
    cookie.push_str(&format!("&computer={}", urlencoding::encode(&env.computer)));

    log::info!(
        "[gp-login] 登录成功: user={}, portal={}, cookie len={}",
        user,
        portal,
        cookie.len()
    );

    Ok(GpAuthResult {
        gateway: gateway.clone(),
        cookie,
        portal,
        user,
    })
}

// ===================== XML 解析辅助 =====================

/// 检测 SAML 标记：同时存在 <saml-auth-method> 和 <saml-request> 返回 (method, request)。
fn parse_saml_markers(xml: &str) -> Result<Option<(String, String)>, GpLoginError> {
    let doc = roxmltree::Document::parse(xml)
        .map_err(|e| GpLoginError::Parse(format!("prelogin XML 解析失败: {e}")))?;
    let method = find_text(&doc, "saml-auth-method");
    let request = find_text(&doc, "saml-request");
    match (method, request) {
        (Some(m), Some(r)) if !m.is_empty() && !r.is_empty() => Ok(Some((m, r))),
        _ => Ok(None),
    }
}

/// 从 portal getconfig 响应解析 (portal-userauthcookie, gateway 列表)。
fn parse_portal_config(xml: &str) -> Result<(String, Vec<String>), GpLoginError> {
    let doc = match roxmltree::Document::parse(xml) {
        Ok(d) => d,
        Err(_) => {
            // getconfig 偶尔返回非 XML（如纯文本错误），返回空让上层判断
            return Ok((String::new(), Vec::new()));
        }
    };
    let cookie = find_text(&doc, "portal-userauthcookie").unwrap_or_default();

    // gateway 列表：<gateways><external><list><entry name="gw-fqdn">...
    // 也可能 <gateways><list><entry name="...">
    let mut gateways = Vec::new();
    for entry in doc.descendants().filter(|n| n.has_tag_name("entry")) {
        if let Some(name) = entry.attribute("name") {
            // entry name 通常是 gateway 的 FQDN:port 或 FQDN
            if !name.is_empty() && !gateways.contains(&name.to_string()) {
                gateways.push(name.to_string());
            }
        }
    }
    Ok((cookie, gateways))
}

/// 从 jnlp 响应解析 <argument> 数组（openconnect 需要的 authcookie 等按位置取）。
fn parse_jnlp_arguments(xml: &str) -> Result<Vec<String>, GpLoginError> {
    let doc = roxmltree::Document::parse(xml)
        .map_err(|e| GpLoginError::Parse(format!("gateway login XML 解析失败: {e}")))?;
    let args: Vec<String> = doc
        .descendants()
        .filter(|n| n.has_tag_name("argument"))
        .filter_map(|n| n.text().map(|t| t.to_string()))
        .collect();
    Ok(args)
}

/// 取 args[i]，索引不足或值为哨兵（空/`(null)`/`-1`/`(empty_domain)`/其 URL 编码）时返回空 String。
fn arg_at(args: &[String], idx: usize) -> String {
    let Some(v) = args.get(idx) else {
        return String::new();
    };
    let v = v.trim();
    if v.is_empty()
        || v == "(null)"
        || v == "-1"
        || v.eq_ignore_ascii_case("(empty_domain)")
        || v.eq_ignore_ascii_case("%28empty_domain%29")
    {
        String::new()
    } else {
        v.to_string()
    }
}

/// 在 XML 文档里找某标签的文本内容（首个匹配）。
fn find_text(doc: &roxmltree::Document, tag: &str) -> Option<String> {
    doc.descendants()
        .find(|n| n.has_tag_name(tag))
        .and_then(|n| n.text())
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
}

/// 尝试从响应 XML 提取错误消息（<msg> 或 <respMsg> 或 <status>）。
fn parse_error_message(xml: &str) -> Option<String> {
    let doc = roxmltree::Document::parse(xml).ok()?;
    for tag in &["msg", "respMsg", "message", "status"] {
        if let Some(t) = find_text(&doc, tag) {
            return Some(t);
        }
    }
    None
}

// ===================== 字符串辅助 =====================

/// 去掉 URL 的 scheme 前缀（"https://x:4430" -> "x:4430"）。
fn strip_scheme(url: &str) -> String {
    url.replace("https://", "").replace("http://", "")
}

/// 日志截断（避免超长 XML 刷屏）。
fn truncate_for_log(s: &str) -> String {
    const MAX: usize = 500;
    if s.len() <= MAX {
        s.to_string()
    } else {
        format!("{}...(共{}字节)", &s[..MAX], s.len())
    }
}
