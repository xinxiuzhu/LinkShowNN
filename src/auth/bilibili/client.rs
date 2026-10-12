use std::{sync::Arc, time::Duration};

use bpi_rs::{
    ApiEnvelope, BilibiliRequest, BpiClient,
    login::login_action::qr::{CheckQrCodeStatusData, GenerateQrCodeData},
};
use reqwest_012::{Client, RequestBuilder, Response, header::SET_COOKIE};
use serde::de::DeserializeOwned;

use super::model::{Credentials, LoginError, MAX_RESPONSE_BYTES, PollOutcome, QrCode};

const GENERATE_ENDPOINT: &str =
    "https://passport.bilibili.com/x/passport-login/web/qrcode/generate";
const POLL_ENDPOINT: &str = "https://passport.bilibili.com/x/passport-login/web/qrcode/poll";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(12);

#[derive(Clone)]
pub(crate) struct BilibiliQrClient {
    api: Arc<BpiClient>,
}

impl BilibiliQrClient {
    pub(crate) fn new(proxy: Option<&str>) -> Result<Self, LoginError> {
        // 不安装 Cookie jar，不继承用户手工配置或解析器会话，不跟随重定向。
        let mut http = Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(REQUEST_TIMEOUT)
            .redirect(reqwest_012::redirect::Policy::none())
            .no_proxy();
        if let Some(proxy) = proxy.filter(|value| !value.trim().is_empty()) {
            if proxy.len() > 2048 {
                return Err(LoginError::InvalidProxy);
            }
            http = http.proxy(
                reqwest_012::Proxy::all(proxy.trim()).map_err(|_| LoginError::InvalidProxy)?,
            );
        }
        let http = http.build().map_err(|_| LoginError::ClientConfiguration)?;
        let api = BpiClient::builder()
            .reqwest_client(http)
            .build()
            .map_err(|_| LoginError::ClientConfiguration)?;
        Ok(Self { api: Arc::new(api) })
    }

    pub(crate) async fn generate(&self) -> Result<QrCode, LoginError> {
        let bytes = request_bytes(self.generate_request()).await?.0;
        parse_generate(&bytes)
    }

    pub(crate) async fn poll(&self, qr: &QrCode) -> Result<PollOutcome, LoginError> {
        if qr.expires_in().is_zero() {
            return Ok(PollOutcome::Expired);
        }
        let (bytes, cookies) = request_bytes(self.poll_request(qr)).await?;
        parse_poll(&bytes, cookies)
    }

    pub(super) fn generate_request(&self) -> RequestBuilder {
        self.api.get(GENERATE_ENDPOINT).with_bilibili_headers()
    }

    pub(super) fn poll_request(&self, qr: &QrCode) -> RequestBuilder {
        self.api
            .get(POLL_ENDPOINT)
            .with_bilibili_headers()
            .query(&[("qrcode_key", qr.key())])
    }
}

pub(super) async fn request_bytes(
    request: RequestBuilder,
) -> Result<(Vec<u8>, Vec<(String, String)>), LoginError> {
    tokio::time::timeout(REQUEST_TIMEOUT, async {
        let mut response = request.send().await.map_err(|_| LoginError::Transport)?;
        if !response.status().is_success() {
            return Err(LoginError::HttpStatus(response.status().as_u16()));
        }
        let cookies = response_cookies(&response)?;
        if response
            .content_length()
            .is_some_and(|size| size > MAX_RESPONSE_BYTES as u64)
        {
            return Err(LoginError::ResponseTooLarge);
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| LoginError::Transport)? {
            if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
                return Err(LoginError::ResponseTooLarge);
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok((bytes, cookies))
    })
    .await
    .map_err(|_| LoginError::Transport)?
}

fn response_cookies(response: &Response) -> Result<Vec<(String, String)>, LoginError> {
    let headers: Vec<_> = response.headers().get_all(SET_COOKIE).iter().collect();
    if headers.len() > 32
        || headers
            .iter()
            .map(|value| value.as_bytes().len())
            .sum::<usize>()
            > 16 * 1024
    {
        return Err(LoginError::ResponseTooLarge);
    }
    Ok(response
        .cookies()
        .map(|cookie| (cookie.name().to_owned(), cookie.value().to_owned()))
        .collect())
}

fn decode_payload<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, LoginError> {
    if bytes.len() > MAX_RESPONSE_BYTES {
        return Err(LoginError::ResponseTooLarge);
    }
    // bpi-rs 的兼容 envelope 默认缺失 code 为 0；登录流程要求显式成功码。
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| LoginError::InvalidResponse)?;
    let code = value
        .get("code")
        .and_then(serde_json::Value::as_i64)
        .and_then(|code| i32::try_from(code).ok())
        .ok_or(LoginError::InvalidResponse)?;
    if code != 0 {
        return Err(LoginError::Api(code));
    }
    let envelope: ApiEnvelope<T> =
        serde_json::from_value(value).map_err(|_| LoginError::InvalidResponse)?;
    envelope.data.ok_or(LoginError::InvalidResponse)
}

pub(super) fn parse_generate(bytes: &[u8]) -> Result<QrCode, LoginError> {
    let data: GenerateQrCodeData = decode_payload(bytes)?;
    QrCode::new(data.url, data.qrcode_key)
}

pub(super) fn parse_poll(
    bytes: &[u8],
    cookies: Vec<(String, String)>,
) -> Result<PollOutcome, LoginError> {
    let data: CheckQrCodeStatusData = decode_payload(bytes)?;
    // credentials 只能来自官方 HTTPS 响应的 Set-Cookie，忽略响应体中的 cookies / url。
    match data.code {
        86101 => Ok(PollOutcome::WaitingForScan),
        86090 => Ok(PollOutcome::WaitingForConfirmation),
        86038 => Ok(PollOutcome::Expired),
        0 => Credentials::from_pairs(cookies, data.refresh_token).map(PollOutcome::Success),
        code => Err(LoginError::Api(code)),
    }
}
