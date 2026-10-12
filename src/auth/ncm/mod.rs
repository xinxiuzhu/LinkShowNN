//! 网易云官方客户端扫码协议，二维码会话与数据库持久化由通用控制器负责。

use anyhow::{Result, ensure};
use reqwest::Client;
use serde_json::{Value, json};
use std::time::Duration;

use crate::{
    auth::music::{MusicCredentials, MusicPlatform, QrChallenge, QrImage, QrPoll, QrProtocol},
    resolver::ncm::client::{NcmClient, session_pairs},
};

pub(crate) struct NcmQrClient {
    client: NcmClient,
}

impl NcmQrClient {
    pub(crate) fn new(proxy: Option<&str>) -> Result<Self> {
        let mut builder = Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy();
        if let Some(proxy) = proxy.map(str::trim).filter(|proxy| !proxy.is_empty()) {
            ensure!(proxy.len() <= 2048, "网易云代理配置无效");
            builder = builder.proxy(
                reqwest::Proxy::all(proxy).map_err(|_| anyhow::anyhow!("网易云代理配置无效"))?,
            );
        }
        let http = builder
            .build()
            .map_err(|_| anyhow::anyhow!("网易云客户端初始化失败"))?;
        Ok(Self {
            client: NcmClient::new(http),
        })
    }
}

#[async_trait::async_trait]
impl QrProtocol for NcmQrClient {
    async fn generate(&self) -> Result<QrChallenge> {
        let response = self
            .client
            .eapi("/api/login/qrcode/unikey", json!({"type":3}), "")
            .await?;
        let key = qr_key(&response.body)?;
        let mut url = url::Url::parse("https://music.163.com/login").expect("fixed login URL");
        url.query_pairs_mut().append_pair("codekey", &key);
        QrChallenge::new(QrImage::Url(url.to_string()), key, Duration::from_secs(180))
    }

    async fn poll(&self, qr: &QrChallenge) -> Result<QrPoll> {
        if qr.expires_in().is_zero() {
            return Ok(QrPoll::Expired);
        }
        let response = self
            .client
            .eapi(
                "/api/login/qrcode/client/login",
                json!({"type":3, "key":qr.state()}),
                "",
            )
            .await?;
        match poll_code(&response.body)? {
            800 => Ok(QrPoll::Expired),
            801 => Ok(QrPoll::WaitingForScan),
            802 => Ok(QrPoll::WaitingForConfirmation),
            803 => {
                // 只接收官方 HTTPS Set-Cookie；响应体中的 cookie/url 不作为凭据。
                let cookie = authorized_cookie(response.cookies)?;
                let user_id = self.client.account_id(&cookie).await?;
                Ok(QrPoll::Success(MusicCredentials::new(
                    MusicPlatform::Ncm,
                    cookie,
                    user_id,
                    None,
                )?))
            }
            _ => anyhow::bail!("网易云扫码状态无效，请重新发起登录"),
        }
    }
}

fn qr_key(body: &Value) -> Result<String> {
    ensure!(
        body.get("code").and_then(Value::as_i64) == Some(200),
        "网易云暂时无法生成二维码"
    );
    let key = body
        .get("unikey")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("网易云二维码响应无效"))?;
    ensure!(
        !key.is_empty()
            && key.len() <= 256
            && key
                .bytes()
                .all(|byte| byte.is_ascii_graphic() && !matches!(byte, b'"' | b'\\' | b';')),
        "网易云二维码响应无效"
    );
    Ok(key.to_owned())
}

fn poll_code(body: &Value) -> Result<i64> {
    body.get("code")
        .and_then(Value::as_i64)
        .ok_or_else(|| anyhow::anyhow!("网易云扫码响应无效"))
}

fn authorized_cookie(cookies: Vec<(String, String)>) -> Result<String> {
    let cookie = cookies
        .into_iter()
        .filter(|(key, _)| matches!(key.as_str(), "MUSIC_U" | "__csrf" | "MUSIC_A"))
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join("; ");
    let pairs = session_pairs(&cookie)?;
    ensure!(
        pairs.contains_key("MUSIC_U"),
        "网易云扫码成功响应缺少登录凭据"
    );
    Ok(cookie)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qr_key_requires_explicit_success_and_safe_bounded_key() {
        assert_eq!(
            qr_key(&json!({"code":200, "unikey":"safe-key"})).unwrap(),
            "safe-key"
        );
        for body in [
            json!({"unikey":"key"}),
            json!({"code":500, "unikey":"key"}),
            json!({"code":200, "unikey":"\nsecret"}),
            json!({"code":200, "unikey":"x".repeat(257)}),
        ] {
            assert!(qr_key(&body).is_err());
        }
    }

    #[test]
    fn missing_poll_status_cannot_become_success() {
        assert!(poll_code(&json!({"message":"ok"})).is_err());
        assert_eq!(poll_code(&json!({"code":801})).unwrap(), 801);
    }

    #[test]
    fn authorization_only_accepts_safe_login_header_fields() {
        let cookie = authorized_cookie(vec![
            ("MUSIC_U".into(), "test-token".into()),
            ("__csrf".into(), "test-csrf".into()),
            ("tracking".into(), "ignored".into()),
        ])
        .unwrap();
        assert_eq!(cookie, "MUSIC_U=test-token; __csrf=test-csrf");
        assert!(authorized_cookie(vec![]).is_err());
        assert!(authorized_cookie(vec![("MUSIC_U".into(), "bad\r\nvalue".into())]).is_err());
        assert!(
            authorized_cookie(vec![
                ("MUSIC_U".into(), "one".into()),
                ("MUSIC_U".into(), "two".into())
            ])
            .is_err()
        );
    }

    /// Explicit public-network probe; never reads config, credentials or a database.
    #[tokio::test]
    #[ignore = "contacts official anonymous QR endpoints; invoke explicitly"]
    async fn official_anonymous_qr_generate_and_waiting_poll() {
        let proxy = std::env::var("NUONO_MEDIA_PROBE_PROXY").ok();
        let client = NcmQrClient::new(proxy.as_deref()).unwrap();
        let qr = client.generate().await.unwrap();
        assert!(matches!(qr.image(), QrImage::Url(_)));
        assert!(matches!(
            client.poll(&qr).await.unwrap(),
            QrPoll::WaitingForScan
        ));
        println!("网易云官方匿名二维码：生成成功，轮询等待扫码；未登录、未保存凭据");
    }
}
