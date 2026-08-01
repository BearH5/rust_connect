//! 错误类型与登录步骤判定。
//!
//! 对照 zju-connect request.go:32-35 的 sentinel error：
//!   errSMSRequired / errTOTPRequired / errCertRequired / errNotFound
//!
//! Rust 侧用 enum 表达「下一步动作」，方便 GUI 交互式处理，
//! 而非像 Go 那样用 error 链路触发不同函数。

use thiserror::Error;

/// 登录主流程的「下一步动作」。
/// 对照 request.go:204-226 的 NextAuth 判定逻辑。
#[derive(Debug)]
pub enum LoginStep {
    /// 登录成功，内含授权后的 TwfID。
    Done(String),
    /// 需要短信验证码（NextAuth=2 或 NextService=auth/sms）。
    NeedSms,
    /// 需要 TOTP 验证码（NextAuth=7 或 NextService=auth/token）。
    NeedTotp,
    /// 需要证书（NextAuth=0）。
    NeedCert,
    /// 登录失败，内含服务端原始响应。
    Failed(String),
}

/// 登录过程中的错误。
#[derive(Debug, Error)]
pub enum LoginError {
    #[error("HTTP 请求失败: {0}")]
    Http(#[from] reqwest::Error),

    #[error("响应缺少必要字段: {0}")]
    MissingField(String),

    #[error("RSA 加密失败: {0}")]
    Rsa(String),

    #[error("未实现的认证类型: {0}")]
    NotImplemented(String),

    #[error("登录失败: {0}")]
    LoginFailed(String),

    #[error("需要短信验证码")]
    SmsRequired,

    #[error("需要 TOTP 验证码")]
    TotpRequired,

    #[error("需要证书认证")]
    CertRequired,
}
