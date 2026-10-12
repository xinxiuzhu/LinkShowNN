use std::{
    fmt,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::Url;

pub(super) const MAX_RESPONSE_BYTES: usize = 64 * 1024;
const MAX_COOKIE_BYTES: usize = 8192;
const MAX_SECRET_BYTES: usize = 16 * 1024;
pub(super) const QR_LIFETIME: Duration = Duration::from_secs(180);
const COOKIE_NAMES: [&str; 5] = [
    "SESSDATA",
    "bili_jct",
    "DedeUserID",
    "DedeUserID__ckMd5",
    "buvid3",
];

/// 只携带分类错误，不保存 HTTP 错误、接口消息或含凭据的响应。
#[derive(Debug, Error, PartialEq, Eq)]
pub(crate) enum LoginError {
    #[error("无法创建 B 站登录客户端")]
    ClientConfiguration,
    #[error("B 站登录代理配置无效")]
    InvalidProxy,
    #[error("B 站登录请求失败或超时")]
    Transport,
    #[error("B 站登录接口返回 HTTP {0}")]
    HttpStatus(u16),
    #[error("B 站登录响应超过大小限制")]
    ResponseTooLarge,
    #[error("B 站登录接口返回无效数据")]
    InvalidResponse,
    #[error("B 站登录接口返回错误码 {0}")]
    Api(i32),
    #[error("B 站返回的登录二维码无效")]
    InvalidQrCode,
    #[error("B 站登录凭据不完整或无效")]
    InvalidCredentials,
}

#[derive(Clone)]
pub(crate) struct QrCode {
    url: String,
    key: String,
    issued_at: Instant,
}

impl QrCode {
    pub(super) fn new(url: String, key: String) -> Result<Self, LoginError> {
        validate_qr_url(&url, &key)?;
        Ok(Self {
            url,
            key,
            issued_at: Instant::now(),
        })
    }

    pub(crate) fn url(&self) -> &str {
        &self.url
    }

    pub(crate) fn expires_in(&self) -> Duration {
        QR_LIFETIME.saturating_sub(self.issued_at.elapsed())
    }

    pub(super) fn key(&self) -> &str {
        &self.key
    }
}

impl fmt::Debug for QrCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("QrCode")
            .field("url", &"[redacted]")
            .field("key", &"[redacted]")
            .field("expires_in", &self.expires_in())
            .finish()
    }
}

fn validate_qr_url(raw: &str, key: &str) -> Result<(), LoginError> {
    if !(16..=128).contains(&key.len())
        || !key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        || raw.len() > 2048
    {
        return Err(LoginError::InvalidQrCode);
    }
    let url = Url::parse(raw).map_err(|_| LoginError::InvalidQrCode)?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some_and(|port| port != 443)
        || url.fragment().is_some()
    {
        return Err(LoginError::InvalidQrCode);
    }
    // 官方生成接口仍在 passport，二维码页面已迁至 account 域；
    // 严格匹配已验证的域名和路径组合，不接受所有 B 站子域。
    let key_name = match (url.host_str(), url.path()) {
        (Some("passport.bilibili.com"), "/qrcode/h5/login") => "oauthKey",
        (Some("passport.bilibili.com"), "/h5-app/passport/login/scan")
        | (Some("account.bilibili.com"), "/h5/account-h5/auth/scan-web") => "qrcode_key",
        _ => return Err(LoginError::InvalidQrCode),
    };
    let query: Vec<_> = url.query_pairs().collect();
    let matching: Vec<_> = query.iter().filter(|(name, _)| name == key_name).collect();
    if matching.len() != 1 || matching[0].1 != key {
        return Err(LoginError::InvalidQrCode);
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PollOutcome {
    WaitingForScan,
    WaitingForConfirmation,
    Expired,
    Success(Credentials),
}

/// 只供内部解析及宿主私密存储使用，不实现 Serialize / Display。
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Credentials {
    cookies: Vec<(String, String)>,
    refresh_token: String,
    user_id: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredCredentials {
    version: u8,
    cookies: Vec<(String, String)>,
    refresh_token: String,
}

impl Credentials {
    pub(super) fn from_pairs(
        pairs: Vec<(String, String)>,
        refresh_token: String,
    ) -> Result<Self, LoginError> {
        if pairs.len() > 32
            || refresh_token.len() > 4096
            || refresh_token.chars().any(char::is_control)
        {
            return Err(LoginError::InvalidCredentials);
        }
        let mut cookies = Vec::new();
        let mut total_bytes = 0usize;
        for name in COOKIE_NAMES {
            let mut matching = pairs.iter().filter(|(key, _)| key == name);
            let Some((_, value)) = matching.next() else {
                continue;
            };
            if matching.next().is_some()
                || value.is_empty()
                || value.len() > 4096
                || !value.bytes().all(|byte| {
                    matches!(byte, 0x21..=0x7e) && !matches!(byte, b';' | b',' | b'"' | b'\\')
                })
            {
                return Err(LoginError::InvalidCredentials);
            }
            total_bytes += name.len() + value.len() + 2;
            if total_bytes > MAX_COOKIE_BYTES {
                return Err(LoginError::InvalidCredentials);
            }
            cookies.push((name.to_owned(), value.clone()));
        }
        for required in ["SESSDATA", "bili_jct", "DedeUserID"] {
            if !cookies.iter().any(|(key, _)| key == required) {
                return Err(LoginError::InvalidCredentials);
            }
        }
        let user_id = cookies
            .iter()
            .find(|(key, _)| key == "DedeUserID")
            .map(|(_, value)| value.clone())
            .ok_or(LoginError::InvalidCredentials)?;
        if user_id.len() > 20 || user_id.parse::<u64>().ok().filter(|id| *id > 0).is_none() {
            return Err(LoginError::InvalidCredentials);
        }
        Ok(Self {
            cookies,
            refresh_token,
            user_id,
        })
    }

    pub(crate) fn cookie_header(&self) -> String {
        bpi_rs::session::cookie::format_cookie_pairs(&self.cookies)
    }

    #[cfg(test)]
    pub(crate) fn refresh_token(&self) -> &str {
        &self.refresh_token
    }

    pub(crate) fn user_id(&self) -> &str {
        &self.user_id
    }

    pub(crate) fn encode(&self) -> Result<String, LoginError> {
        let encoded = serde_json::to_string(&StoredCredentials {
            version: 1,
            cookies: self.cookies.clone(),
            refresh_token: self.refresh_token.clone(),
        })
        .map_err(|_| LoginError::InvalidCredentials)?;
        if encoded.len() > MAX_SECRET_BYTES {
            return Err(LoginError::InvalidCredentials);
        }
        Ok(encoded)
    }

    pub(crate) fn decode(encoded: &str) -> Result<Self, LoginError> {
        if encoded.len() > MAX_SECRET_BYTES {
            return Err(LoginError::InvalidCredentials);
        }
        let stored: StoredCredentials =
            serde_json::from_str(encoded).map_err(|_| LoginError::InvalidCredentials)?;
        if stored.version != 1 {
            return Err(LoginError::InvalidCredentials);
        }
        Self::from_pairs(stored.cookies, stored.refresh_token)
    }
}

impl fmt::Debug for Credentials {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Credentials")
            .field("cookies", &"[redacted]")
            .field("refresh_token", &"[redacted]")
            .field("user_id", &"[redacted]")
            .finish()
    }
}
