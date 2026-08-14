//! getconfig.esp 请求与响应解析。
//!
//! 对照 openconnect gpst.c 第 645-716 行（gpst_get_config）和
//! 第 406-630 行（gpst_parse_config_xml）。

use crate::error::GpTunnelError;
use std::net::Ipv4Addr;
use std::time::Duration;

/// GP 客户端伪装版本（gpst.c GPST_VERSION，master 主干值）。
pub const GP_APP_VERSION: &str = "6.3.0-33";

/// getconfig.esp 返回的隧道配置。
#[derive(Debug, Clone, serde::Serialize)]
pub struct GpTunnelConfig {
    /// 客户端 IPv4（<ip-address>）。
    pub client_ip: [u8; 4],
    /// 掩码（<netmask>）。
    pub netmask: [u8; 4],
    /// MTU（服务器返回 0 时兜底 1400）。
    pub mtu: u16,
    /// 隧道 GET path（<ssl-tunnel-url>，默认 /ssl-tunnel-connect.sslvpn）。
    pub tunnel_url: String,
    /// 包含路由（<access-routes> 的 <member>）。
    pub routes: Vec<String>,
    /// DNS 服务器（<dns> 的 <member>）。
    pub dns: Vec<String>,
    /// rekey 超时秒数（<timeout> - 60），None 表示无 rekey。
    pub rekey_timeout: Option<u64>,
}

/// 请求 getconfig.esp 并解析响应。
///
/// 对照 gpst.c 第 645-672 行：POST ssl-vpn/getconfig.esp，
/// body 含 client-type/protocol-version/app-version/cookie 等。
pub fn fetch_tunnel_config(
    gateway_base: &str, // 形如 "https://114.250.31.2:4430"
    cookie: &str,       // gp-login 拼的 "authcookie=...&user=...&computer=..."
) -> Result<GpTunnelConfig, GpTunnelError> {
    let client = reqwest::blocking::Client::builder()
        .danger_accept_invalid_certs(true)
        .user_agent("PAN GlobalProtect")
        .timeout(Duration::from_secs(30))
        .build()?;

    let body = format!(
        "client-type=1&protocol-version=p1&internal=no&\
         app-version={app_ver}&ipv6-support=yes&clientos=Linux&os-version=Ubuntu-22.04&\
         hmac-algo=sha1,md5,sha256&enc-algo=aes-128-cbc,aes-256-cbc&\
         {cookie}",
        app_ver = urlencoding::encode(GP_APP_VERSION),
    );

    let url = format!("{gateway_base}/ssl-vpn/getconfig.esp");
    log::info!("[gp-tunnel] POST getconfig -> {url}");
    let resp = client
        .post(&url)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(body)
        .send()?;
    let xml = resp.text()?;
    log::debug!(
        "[gp-tunnel] getconfig 响应: {}",
        crate::truncate_for_log(&xml)
    );

    // 检查错误（gpst.c 第 683-695 行：纯文本 "errors getting SSL/VPN config" -> cookie 无效）
    if xml.contains("errors getting SSL/VPN config") {
        return Err(GpTunnelError::GetConfig(
            "cookie 无效（服务器拒绝 getconfig）".into(),
        ));
    }

    parse_config_xml(&xml)
}

/// 解析 getconfig 响应 XML。
///
/// 对照 gpst.c 第 406-630 行 gpst_parse_config_xml。
fn parse_config_xml(xml: &str) -> Result<GpTunnelConfig, GpTunnelError> {
    let doc = roxmltree::Document::parse(xml)
        .map_err(|e| GpTunnelError::Parse(format!("getconfig XML 解析失败: {e}")))?;

    let client_ip = parse_ipv4(&doc, "ip-address").ok_or_else(|| {
        GpTunnelError::GetConfig("getconfig 未返回 <ip-address>".into())
    })?;

    let netmask = parse_ipv4(&doc, "netmask").unwrap_or([255, 255, 255, 0]);

    let mtu = find_text(&doc, "mtu")
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(0);
    let mtu = if mtu == 0 { 1400 } else { mtu }; // GP 常返回 0，兜底 1400

    let tunnel_url = find_text(&doc, "ssl-tunnel-url")
        .unwrap_or_else(|| "/ssl-tunnel-connect.sslvpn".to_string());

    let routes = collect_members(&doc, "access-routes");
    let dns = collect_members(&doc, "dns");

    let rekey_timeout = find_text(&doc, "timeout")
        .and_then(|s| s.parse::<u64>().ok())
        .map(|sec| sec.saturating_sub(60));

    log::info!(
        "[gp-tunnel] getconfig: ip={}, netmask={}, mtu={}, tunnel_url={}, routes={}, dns={}, rekey={:?}",
        Ipv4Addr::from(client_ip),
        Ipv4Addr::from(netmask),
        mtu,
        tunnel_url,
        routes.len(),
        dns.len(),
        rekey_timeout,
    );

    Ok(GpTunnelConfig {
        client_ip,
        netmask,
        mtu,
        tunnel_url,
        routes,
        dns,
        rekey_timeout,
    })
}

// ===================== XML 辅助 =====================

/// 找某标签的文本内容（首个匹配）。
fn find_text(doc: &roxmltree::Document, tag: &str) -> Option<String> {
    doc.descendants()
        .find(|n| n.has_tag_name(tag))
        .and_then(|n| n.text())
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
}

/// 解析某标签为 IPv4（如 <ip-address>10.0.0.1</ip-address>）。
fn parse_ipv4(doc: &roxmltree::Document, tag: &str) -> Option<[u8; 4]> {
    let s = find_text(doc, tag)?;
    s.parse::<Ipv4Addr>().ok().map(|ip| ip.octets())
}

/// 收集某父标签下所有 <member> 的文本（如 <access-routes><member>10.0.0.0/24</member>...）。
fn collect_members(doc: &roxmltree::Document, parent_tag: &str) -> Vec<String> {
    doc.descendants()
        .find(|n| n.has_tag_name(parent_tag))
        .map(|parent| {
            parent
                .descendants()
                .filter(|n| n.has_tag_name("member"))
                .filter_map(|n| n.text().map(|t| t.trim().to_string()))
                .filter(|t| !t.is_empty())
                .collect()
        })
        .unwrap_or_default()
}
