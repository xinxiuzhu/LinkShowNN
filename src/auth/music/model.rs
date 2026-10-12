use std::{
    collections::BTreeSet,
    io::Cursor,
    time::{Duration, Instant},
};

use anyhow::{Result, anyhow, ensure};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use url::Url;

const MAX_COOKIE_BYTES: usize = 8192;
const MAX_SECRET_BYTES: usize = 16 * 1024;
const MAX_STATE_BYTES: usize = 16 * 1024;
const MAX_QR_BYTES: usize = 1024 * 1024;
const MAX_QR_SIDE: u32 = 2048;
const MAX_QR_LIFETIME: Duration = Duration::from_secs(15 * 60);
const MAX_UNIX_SECONDS: u64 = 253_402_300_799;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MusicPlatform {
    Ncm,
    QqMusic,
}

impl MusicPlatform {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Ncm => "网易云音乐",
            Self::QqMusic => "QQ 音乐",
        }
    }

    pub(crate) fn session_key(self) -> &'static str {
        match self {
            Self::Ncm => "ncm_qr_session",
            Self::QqMusic => "qqmusic_qr_session",
        }
    }

    pub(crate) fn command_name(self) -> &'static str {
        match self {
            Self::Ncm => "网易云",
            Self::QqMusic => "QQ音乐",
        }
    }

    pub(crate) fn task_name(self) -> &'static str {
        match self {
            Self::Ncm => "ncm-qr-login",
            Self::QqMusic => "qqmusic-qr-login",
        }
    }
}

/// 只用于绘制或发送，不进入日志、文本消息或普通磁盘文件。
#[derive(Clone)]
pub(crate) enum QrImage {
    Url(String),
    Bytes(Vec<u8>),
}

#[derive(Clone)]
pub(crate) struct QrChallenge {
    image: QrImage,
    state: String,
    issued_at: Instant,
    ttl: Duration,
}

impl QrChallenge {
    pub(crate) fn new(image: QrImage, state: String, ttl: Duration) -> Result<Self> {
        ensure!(
            !state.is_empty()
                && state.len() <= MAX_STATE_BYTES
                && !state.chars().any(char::is_control),
            "音乐登录二维码状态无效"
        );
        ensure!(
            !ttl.is_zero() && ttl <= MAX_QR_LIFETIME,
            "音乐登录二维码有效期无效"
        );
        match &image {
            QrImage::Url(raw) => {
                ensure!(
                    raw.len() <= 2048 && !raw.chars().any(char::is_control),
                    "音乐登录二维码无效"
                );
                let url = Url::parse(raw).map_err(|_| anyhow!("音乐登录二维码无效"))?;
                ensure!(
                    url.scheme() == "https"
                        && url.host_str().is_some()
                        && url.username().is_empty()
                        && url.password().is_none()
                        && url.port().is_none_or(|port| port == 443)
                        && url.fragment().is_none(),
                    "音乐登录二维码无效"
                );
            }
            QrImage::Bytes(bytes) => {
                ensure!(
                    !bytes.is_empty() && bytes.len() <= MAX_QR_BYTES,
                    "音乐登录二维码图片无效"
                );
                let format =
                    image::guess_format(bytes).map_err(|_| anyhow!("音乐登录二维码图片无效"))?;
                ensure!(
                    matches!(format, image::ImageFormat::Png | image::ImageFormat::Jpeg),
                    "音乐登录二维码图片无效"
                );
                let (width, height) = image::ImageReader::with_format(Cursor::new(bytes), format)
                    .into_dimensions()
                    .map_err(|_| anyhow!("音乐登录二维码图片无效"))?;
                ensure!(
                    width > 0 && height > 0 && width <= MAX_QR_SIDE && height <= MAX_QR_SIDE,
                    "音乐登录二维码图片无效"
                );
            }
        }
        Ok(Self {
            image,
            state,
            issued_at: Instant::now(),
            ttl,
        })
    }

    pub(crate) fn state(&self) -> &str {
        &self.state
    }

    pub(crate) fn image(&self) -> &QrImage {
        &self.image
    }

    pub(crate) fn expires_in(&self) -> Duration {
        self.ttl.saturating_sub(self.issued_at.elapsed())
    }
}

pub(crate) enum QrPoll {
    WaitingForScan,
    WaitingForConfirmation,
    Expired,
    Refused,
    Success(MusicCredentials),
}

#[async_trait]
pub(crate) trait QrProtocol: Send + Sync {
    async fn generate(&self) -> Result<QrChallenge>;
    async fn poll(&self, qr: &QrChallenge) -> Result<QrPoll>;
}

/// 故意不实现 Debug / Display / Serialize；只通过校验后的内部接口读写。
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct MusicCredentials {
    platform: MusicPlatform,
    cookie: String,
    user_id: String,
    expires_at: Option<u64>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredCredentials {
    version: u8,
    platform: MusicPlatform,
    cookie: String,
    user_id: String,
    expires_at: Option<u64>,
}

impl MusicCredentials {
    pub(crate) fn new(
        platform: MusicPlatform,
        cookie: String,
        user_id: String,
        expires_at: Option<u64>,
    ) -> Result<Self> {
        ensure!(
            !cookie.is_empty()
                && cookie.len() <= MAX_COOKIE_BYTES
                && !cookie.chars().any(char::is_control),
            "音乐登录凭据无效"
        );
        ensure!(
            !user_id.is_empty()
                && user_id.len() <= 20
                && user_id.bytes().all(|byte| byte.is_ascii_digit())
                && user_id.parse::<u64>().ok().is_some_and(|uid| uid > 0),
            "音乐登录账号无效"
        );
        ensure!(
            expires_at.is_none_or(|expiry| expiry > 0 && expiry <= MAX_UNIX_SECONDS),
            "音乐登录有效期无效"
        );
        let mut seen = BTreeSet::new();
        let mut pairs = Vec::new();
        // A final semicolon is normal in browser Cookie exports; interior empty fields are not.
        let cookie = cookie
            .trim()
            .strip_suffix(';')
            .unwrap_or(cookie.trim())
            .trim();
        for pair in cookie.split(';') {
            let (name, value) = pair
                .trim()
                .split_once('=')
                .ok_or_else(|| anyhow!("音乐登录凭据无效"))?;
            ensure!(
                !name.is_empty() && name.len() <= 128 && name.bytes().all(cookie_token),
                "音乐登录凭据无效"
            );
            ensure!(seen.insert(name) && seen.len() <= 64, "音乐登录凭据无效");
            ensure!(
                !value.is_empty()
                    && value.len() <= 4096
                    && value.bytes().all(|byte| matches!(byte, 0x21..=0x7e)
                        && !matches!(byte, b';' | b',' | b'"' | b'\\')),
                "音乐登录凭据无效"
            );
            pairs.push(format!("{name}={value}"));
        }
        let required = match platform {
            MusicPlatform::Ncm => "MUSIC_U",
            MusicPlatform::QqMusic => "qqmusic_key",
        };
        ensure!(seen.contains(required), "音乐登录凭据不完整");
        Ok(Self {
            platform,
            cookie: pairs.join("; "),
            user_id,
            expires_at,
        })
    }

    pub(crate) fn cookie_header(&self) -> &str {
        &self.cookie
    }
    pub(crate) fn user_id(&self) -> &str {
        &self.user_id
    }
    pub(crate) fn platform(&self) -> MusicPlatform {
        self.platform
    }
    pub(crate) fn expires_at(&self) -> Option<u64> {
        self.expires_at
    }

    pub(crate) fn encode(&self) -> Result<String> {
        let encoded = serde_json::to_string(&StoredCredentials {
            version: 1,
            platform: self.platform,
            cookie: self.cookie.clone(),
            user_id: self.user_id.clone(),
            expires_at: self.expires_at(),
        })
        .map_err(|_| anyhow!("音乐登录凭据编码失败"))?;
        ensure!(encoded.len() <= MAX_SECRET_BYTES, "音乐登录凭据编码失败");
        Ok(encoded)
    }

    pub(crate) fn decode(platform: MusicPlatform, encoded: &str) -> Result<Self> {
        ensure!(encoded.len() <= MAX_SECRET_BYTES, "音乐登录凭据无效");
        let stored: StoredCredentials =
            serde_json::from_str(encoded).map_err(|_| anyhow!("音乐登录凭据无效"))?;
        ensure!(
            stored.version == 1 && stored.platform == platform,
            "音乐登录凭据版本或平台无效"
        );
        Self::new(
            stored.platform,
            stored.cookie,
            stored.user_id,
            stored.expires_at,
        )
    }
}

fn cookie_token(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'!' | b'#'
                | b'$'
                | b'%'
                | b'&'
                | b'\''
                | b'*'
                | b'+'
                | b'-'
                | b'.'
                | b'^'
                | b'_'
                | b'`'
                | b'|'
                | b'~'
        )
}
