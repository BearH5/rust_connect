//! GlobalProtect 登录错误类型。

use thiserror::Error;

/// GP 登录过程中的错误。
#[derive(Debug, Error)]
pub enum GpLoginError {
    #[error("HTTP 请求失败: {0}")]
    Http(#[from] reqwest::Error),

    #[error("XML 解析失败: {0}")]
    Parse(String),

    #[error("该服务器需要 SAML/SSO 登录，当前暂不支持。请改用标准用户名/密码认证的服务器")]
    SamlRequired,

    #[error("认证失败: {0}")]
    AuthFailed(String),

    #[error("portal getconfig 未返回 portal-userauthcookie")]
    NoAuthCookie,

    #[error("获取本机名失败: {0}")]
    Hostname(String),

    #[error("其他错误: {0}")]
    Other(String),
}
