//! EasyConnect 登录模块（纯 Rust + reqwest）。
//!
//! 严格对照 zju-connect/client/easyconnect/request.go 的 loginAuthAndPsw。
//! 每个 pub 项注释里标出对应的 Go 行号。
//!
//! 这部分走普通 HTTPS（/por/login_auth.csp, /por/login_psw.csp），
//! 不涉及 Sangfor 特殊 TLS 指纹（那只在隧道握手层才需要）。

pub mod error;

pub use error::{LoginError, LoginStep};

use regex::Regex;
use rsa::pkcs1v15::Pkcs1v15Encrypt;
// rsa crate 内部用 num-bigint-dig，并把它重导出为 rsa::BigUint。
// 必须用这个重导出的类型，否则与 rsa 的 API 类型不兼容。
use rsa::{BigUint, RsaPublicKey};
use std::time::Duration;

/// 登录所需配置。对照 Go Client 结构体的关键字段。
#[derive(Debug, Clone)]
pub struct LoginConfig {
    /// 形如 "rvpn.zju.edu.cn:443"，无协议前缀。
    pub server: String,
    pub username: String,
    pub password: String,
}

/// 从 login_auth.csp 响应里提取出的握手参数。
/// 对照 request.go:86-116 提取的那些 XML 标签。
struct AuthParams {
    /// 临时 TwfID（request.go:91）
    twf_id: String,
    /// RSA 模数 N，十六进制字符串（request.go:94）
    rsa_key_hex: String,
    /// RSA 公钥指数 E，十进制（request.go:97-104，缺省 65537）
    rsa_exp: u32,
    /// CSRF 随机码，可能为空（request.go:107-116）
    csrf_code: String,
    /// 是否需要图形验证码（request.go:130-134）
    #[allow(dead_code)]
    need_rand_img: bool,
}

/// 执行登录主流程。
///
/// 对照 request.go:63 loginAuthAndPsw 的完整流程：
///   1) GET /por/login_auth.csp 取临时 TwfID + RSA 公钥 + CSRF
///   2) RSA 加密（password + "_" + csrfCode）
///   3) POST /por/login_psw.csp 提交表单
///   4) 根据响应判定下一步动作
///
/// 成功返回 `LoginStep::Done(twfID)`，失败返回对应的步骤或错误。
pub fn login(cfg: &LoginConfig) -> Result<LoginStep, LoginError> {
    // 构造 HTTP 客户端：必须忽略证书校验。
    // 对照 client.go:66-69  InsecureSkipVerify: true
    let client = reqwest::blocking::Client::builder()
        .danger_accept_invalid_certs(true)
        .timeout(Duration::from_secs(30))
        .build()?;

    // ---- 步骤 1：取临时 TwfID + RSA 参数 ----
    let params = fetch_auth_params(&client, &cfg.server)?;

    // ---- 步骤 2：RSA 加密密码 ----
    // 对照 request.go:107-128
    //   password += "_" + csrfCode  （仅当有 CSRF 时）
    //   encryptedPassword = RSA_EncryptPKCS1v15(password)
    //   encryptedPasswordHex = hex(encryptedPassword)
    let mut plaintext = cfg.password.clone();
    if !params.csrf_code.is_empty() {
        plaintext.push('_');
        plaintext.push_str(&params.csrf_code);
    }
    let encrypted_hex = rsa_encrypt_password(&params.rsa_key_hex, params.rsa_exp, &plaintext)?;

    // ---- 步骤 3：POST 提交登录 ----
    // 对照 request.go:175-202
    // 图形验证码：本模块不处理（need_rand_img=true 时需交互），留空。
    let resp = submit_login(
        &client,
        &cfg.server,
        &params.twf_id,
        &cfg.username,
        &encrypted_hex,
        &params.csrf_code,
    )?;

    // ---- 步骤 4：判定登录结果 ----
    // 对照 request.go:204-240 的判定优先级
    Ok(parse_login_result(&resp))
}

/// 步骤 1：GET /por/login_auth.csp，提取握手参数。
///
/// 对照 request.go:65-134。
fn fetch_auth_params(
    client: &reqwest::blocking::Client,
    server: &str,
) -> Result<AuthParams, LoginError> {
    let url = format!("https://{server}/por/login_auth.csp?apiversion=1");
    println!("[login] 请求: {url}");

    // 用构造好的 client 发起一次请求（已配置忽略证书、UA）。
    // 对照 request.go:68  resp, err := c.httpClient.Get(addr)
    let body = client
        .get(&url)
        .send()?
        .text()
        .map_err(|e| LoginError::MissingField(e.to_string()))?;

    println!("[login] 响应长度: {} 字节", body.len());

    // 正则提取各字段。对照 request.go:86-116
    let twf_id = extract_one(&body, r"<TwfID>(.*)</TwfID>")
        .ok_or_else(|| LoginError::MissingField("TwfID".into()))?;

    let rsa_key_hex = extract_one(&body, r"<RSA_ENCRYPT_KEY>(.*)</RSA_ENCRYPT_KEY>")
        .ok_or_else(|| LoginError::MissingField("RSA_ENCRYPT_KEY".into()))?;

    // RSA 指数：缺省 65537（request.go:98-104）
    let rsa_exp = extract_one(&body, r"<RSA_ENCRYPT_EXP>(.*)</RSA_ENCRYPT_EXP>")
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(65537);

    let csrf_code = extract_one(&body, r"<CSRF_RAND_CODE>(.*)</CSRF_RAND_CODE>")
        .unwrap_or_default();
    let need_rand_img = extract_one(&body, r"<RndImg>(.*)</RndImg>")
        .map(|s| s == "1")
        .unwrap_or(false);

    println!(
        "[login] TwfID={}, RSA key 长度={}, exp={}, csrf={}, randImg={}",
        twf_id,
        rsa_key_hex.len(),
        rsa_exp,
        if csrf_code.is_empty() { "(无)" } else { &csrf_code },
        need_rand_img
    );

    Ok(AuthParams {
        twf_id,
        rsa_key_hex,
        rsa_exp,
        csrf_code,
        need_rand_img,
    })
}

/// 步骤 2：RSA-PKCS1v15 加密密码。
///
/// 对照 request.go:118-128：
///   pubKey.N = hex 解析的大整数（注意是 16 进制）
///   pubKey.E = strconv.Atoi（注意是 10 进制）
///   encryptedPassword = rsa.EncryptPKCS1v15(rand, pubKey, password)
///   hex 编码
fn rsa_encrypt_password(
    rsa_key_hex: &str,
    rsa_exp: u32,
    plaintext: &str,
) -> Result<String, LoginError> {
    // N：十六进制大整数（request.go:120-121 modulus.SetString(rsaKey, 16)）
    let n = BigUint::parse_bytes(rsa_key_hex.as_bytes(), 16)
        .ok_or_else(|| LoginError::Rsa(format!("无法解析 RSA 模数(16进制): {rsa_key_hex}")))?;

    // E：十进制公钥指数（request.go:119 strconv.Atoi）。RsaPublicKey::new 需要 BigUint。
    let e = BigUint::from(rsa_exp);

    let pub_key = RsaPublicKey::new(n, e)
        .map_err(|e| LoginError::Rsa(format!("构造 RSA 公钥失败: {e}")))?;

    // PKCS1v15 加密需要 RNG；用 rsa crate 的 OsRng（等价 Go crypto/rand）
    let mut rng = rsa::rand_core::OsRng;
    let cipher = pub_key
        .encrypt(&mut rng, Pkcs1v15Encrypt, plaintext.as_bytes())
        .map_err(|e| LoginError::Rsa(format!("RSA 加密失败: {e}")))?;

    Ok(hex::encode(cipher))
}

/// 步骤 3：POST /por/login_psw.csp 提交登录表单。
///
/// 对照 request.go:175-202：
///   POST /por/login_psw.csp?anti_replay=1&encrypt=1&type=cs
///   Cookie: TWFID={twfID}
///   User-Agent: EasyConnect_windows
///   表单字段：svpn_rand_code / mitm / svpn_req_randcode / svpn_name / svpn_password
fn submit_login(
    client: &reqwest::blocking::Client,
    server: &str,
    twf_id: &str,
    username: &str,
    encrypted_password_hex: &str,
    csrf_code: &str,
) -> Result<String, LoginError> {
    let url = format!("https://{server}/por/login_psw.csp?anti_replay=1&encrypt=1&type=cs");
    println!("[login] 提交登录: {url}");

    // 手动构造 form body，确保字段顺序与编码与 Go url.Values 一致。
    // 对照 request.go:178-184
    let form = format!(
        "svpn_rand_code=&mitm=&svpn_req_randcode={}&svpn_name={}&svpn_password={}",
        urlencoding::encode(csrf_code),
        urlencoding::encode(username),
        urlencoding::encode(encrypted_password_hex),
    );

    let resp = client
        .post(&url)
        .header("Cookie", format!("TWFID={twf_id}"))
        .header("User-Agent", "EasyConnect_windows")
        .header(
            "Content-Type",
            "application/x-www-form-urlencoded",
        )
        .body(form)
        .send()?;

    let text = resp
        .text()
        .map_err(|e| LoginError::MissingField(e.to_string()))?;

    println!("[login] 登录响应长度: {} 字节", text.len());
    Ok(text)
}

/// 步骤 4：根据响应判定下一步动作。
///
/// 严格对照 request.go:204-240 的判定优先级（顺序很重要）：
///   1) auth/sms 或 NextAuth=2 → NeedSms
///   2) auth/token 或 NextAuth=7 → NeedTotp
///   3) NextAuth=0 → NeedCert
///   4) NextAuth=-1 或无 NextAuth → 继续
///   5) 不含 Result=1 → Failed
///   6) 含 TwfID → Done
fn parse_login_result(resp: &str) -> LoginStep {
    // 对照 request.go:204
    if resp.contains("<NextService>auth/sms</NextService>") || resp.contains("<NextAuth>2</NextAuth>") {
        return LoginStep::NeedSms;
    }
    // 对照 request.go:210
    if resp.contains("<NextService>auth/token</NextService>") || resp.contains("<NextAuth>7</NextAuth>") {
        return LoginStep::NeedTotp;
    }
    // 对照 request.go:216
    if resp.contains("<NextAuth>0</NextAuth>") {
        return LoginStep::NeedCert;
    }
    // 对照 request.go:222-226
    if !resp.contains("<NextAuth>-1</NextAuth>") && resp.contains("<NextAuth>") {
        return LoginStep::Failed(format!("未实现的认证类型: {resp}"));
    }
    // 对照 request.go:228-230
    if !resp.contains("<Result>1</Result>") {
        return LoginStep::Failed(resp.to_string());
    }
    // 对照 request.go:232-238：提取授权后的 TwfID
    if let Some(new_twf) = extract_one(resp, r"<TwfID>(.*)</TwfID>") {
        return LoginStep::Done(new_twf);
    }
    LoginStep::Done(String::new())
}

/// 用正则提取第一个捕获组。对照 Go 的 FindSubmatch[1]。
fn extract_one(text: &str, pattern: &str) -> Option<String> {
    let re = Regex::new(pattern).ok()?;
    re.captures(text)
        .and_then(|c| c.get(1).map(|m| m.as_str().to_string()))
}

/// 内网资源条目（简化版，供 GUI 展示）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct Resource {
    pub name: String,
    pub host: String,
    pub port: String,
}

/// 拉取内网资源列表。
///
/// 对照 zju-connect request.go 的 requestResources + parse.go 的 parseResources。
/// 发 GET /por/rclist.csp（带 TWFID cookie），用 roxmltree 解析 `<Rc>` 标签的 name/host/port。
pub fn fetch_resources(server: &str, twf_id: &str) -> Result<Vec<Resource>, LoginError> {
    let client = reqwest::blocking::Client::builder()
        .danger_accept_invalid_certs(true)
        .timeout(Duration::from_secs(30))
        .build()?;

    let url = format!("https://{server}/por/rclist.csp");
    let body = client
        .get(&url)
        .header("Cookie", format!("TWFID={twf_id}"))
        .header("User-Agent", "EasyConnect_windows")
        .send()?
        .text()
        .map_err(|e| LoginError::MissingField(e.to_string()))?;

    // 用 roxmltree 解析 XML，遍历 <Resource><Rcs><Rc .../></Rcs></Resource>。
    // 对照 parse.go:60-113。只取 name/host/port 三个展示字段。
    let doc = roxmltree::Document::parse(&body)
        .map_err(|e| LoginError::MissingField(format!("XML 解析失败: {e}")))?;

    let mut resources = Vec::new();
    for rc in doc.descendants().filter(|n| n.has_tag_name("Rc")) {
        let name = rc.attribute("name").unwrap_or("").to_string();
        let host = rc.attribute("host").unwrap_or("").to_string();
        let port = rc.attribute("port").unwrap_or("").to_string();
        if !host.is_empty() {
            resources.push(Resource { name, host, port });
        }
    }
    Ok(resources)
}

/// 会话保活：调 /por/update_session.csp 防止服务端空闲超时关闭会话。
///
/// 对照 zju-connect request.go:482-535 requestUpdateSession + client.go sessionKeepAliveLoop。
/// 服务端空闲策略严格时（如本测试服务器），不保活的话会话会被关闭，
/// 表现为 recv/send 连接失败（broken pipe）。
///
/// 成功响应包含 `<Message>success</Message>` 和 `<ErrorCode>1</ErrorCode>`。
pub fn keep_session_alive(server: &str, twf_id: &str) -> Result<(), LoginError> {
    let client = reqwest::blocking::Client::builder()
        .danger_accept_invalid_certs(true)
        .timeout(Duration::from_secs(15))
        .build()?;

    let url = format!("https://{server}/por/update_session.csp?twfid={twf_id}&apiversion=1");
    let resp = client
        .get(&url)
        .header("User-Agent", "EasyConnect_windows")
        .send()?;

    if resp.status() != reqwest::StatusCode::OK {
        return Err(LoginError::MissingField(format!(
            "update_session: 状态码 {}",
            resp.status()
        )));
    }

    let body = resp
        .text()
        .map_err(|e| LoginError::MissingField(e.to_string()))?;

    // 成功时 body 含 <Message>success</Message><ErrorCode>1</ErrorCode>
    if !body.contains("<Message>success</Message>") || !body.contains("<ErrorCode>1</ErrorCode>") {
        return Err(LoginError::MissingField(format!(
            "update_session: 意外响应: {}",
            body.chars().take(200).collect::<String>()
        )));
    }
    Ok(())
}
