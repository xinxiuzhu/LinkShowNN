//! 有界官方 HTTP 请求。Cookie 只用于固定的网易云 HTTPS 地址。

use anyhow::{Context, Result, bail, ensure};
use reqwest::{Client, RequestBuilder, header};
use serde_json::{Value, json};
use std::{collections::BTreeMap, time::Duration};
use url::Url;

use super::crypto;

const OFFICIAL_BASE: &str = "https://interface3.music.163.com";
const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const MAX_COOKIE_BYTES: usize = 16 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const APP_VERSION: &str = "3.1.17.204416";

#[derive(Clone)]
pub(crate) struct NcmClient {
    http: Client,
    base: String,
}

pub(crate) struct NcmResponse {
    pub(crate) body: Value,
    pub(crate) cookies: Vec<(String, String)>,
}

impl NcmClient {
    pub(crate) fn new(http: Client) -> Self {
        Self {
            http,
            base: OFFICIAL_BASE.to_owned(),
        }
    }

    pub(super) async fn song_url(&self, source: &Url) -> Result<Url> {
        super::urls::canonical_song_url(&self.http, source).await
    }

    pub(crate) async fn song_detail(&self, id: u64) -> Result<Value> {
        let response = self
            .request(
                self.http
                    .post(format!("{}/api/v3/song/detail", self.base))
                    .header(header::REFERER, "https://music.163.com/")
                    .form(&[("c", json!([{"id": id, "v": 0}]).to_string())]),
            )
            .await?;
        ensure!(
            response.body.get("code").and_then(Value::as_i64) == Some(200),
            "网易云歌曲详情暂不可用"
        );
        Ok(response.body)
    }

    pub(crate) async fn song_audio(&self, id: u64, cookie: &str) -> Result<Value> {
        let response = self
            .eapi(
                "/api/song/enhance/player/url/v1",
                json!({"ids": format!("[{id}]"), "level": "standard", "encodeType": "mp3"}),
                cookie,
            )
            .await?;
        ensure!(
            response.body.get("code").and_then(Value::as_i64) == Some(200),
            "网易云播放地址暂不可用"
        );
        Ok(response.body)
    }

    pub(crate) async fn account_id(&self, cookie: &str) -> Result<String> {
        let response = self
            .eapi("/api/nuser/account/get", json!({}), cookie)
            .await?;
        ensure!(
            response.body.get("code").and_then(Value::as_i64) == Some(200),
            "网易云登录凭据暂不可用"
        );
        response
            .body
            .pointer("/account/id")
            .or_else(|| response.body.pointer("/profile/userId"))
            .and_then(Value::as_u64)
            .filter(|id| *id > 0)
            .map(|id| id.to_string())
            .context("网易云登录凭据未返回有效账号")
    }

    pub(crate) async fn eapi(
        &self,
        path: &str,
        mut body: Value,
        cookie: &str,
    ) -> Result<NcmResponse> {
        ensure!(
            path.starts_with("/api/") && !path.contains('?'),
            "网易云请求路径无效"
        );
        let cookies = session_pairs(cookie)?;
        let mut device = json!({"os": "pc", "appver": APP_VERSION, "deviceId": "linkshownn", "requestId": uuid::Uuid::new_v4().simple().to_string()});
        for (name, value) in &cookies {
            device[name] = Value::String(value.clone());
        }
        body["header"] = Value::String(device.to_string());
        body["e_r"] = Value::String("false".to_owned());
        let encoded = crypto::encode(path, &body);
        let mut cookie_header = format!("os=pc; appver={APP_VERSION}");
        for (name, value) in cookies {
            cookie_header.push_str(&format!("; {name}={value}"));
        }
        self.request(
            self.http
                .post(format!(
                    "{}{}",
                    self.base,
                    path.replacen("/api/", "/eapi/", 1)
                ))
                .header(header::REFERER, "https://music.163.com/")
                .header(header::USER_AGENT, "NeteaseMusicDesktop/3.1.17.204416")
                .header(header::COOKIE, cookie_header)
                .form(&[("params", encoded)]),
        )
        .await
    }

    pub(super) async fn legacy_get(&self, url: Url, query: &[(&str, String)]) -> Result<Value> {
        Ok(self.request(self.http.get(url).query(query)).await?.body)
    }

    async fn request(&self, request: RequestBuilder) -> Result<NcmResponse> {
        tokio::time::timeout(REQUEST_TIMEOUT, async {
            let mut response = request
                .send()
                .await
                .map_err(|_| anyhow::anyhow!("网易云网络请求失败"))?;
            ensure!(
                response.status().is_success(),
                "网易云接口返回 HTTP {}",
                response.status().as_u16()
            );
            ensure!(
                !response
                    .content_length()
                    .is_some_and(|size| size > MAX_RESPONSE_BYTES as u64),
                "网易云响应过大"
            );
            let headers: Vec<_> = response
                .headers()
                .get_all(header::SET_COOKIE)
                .iter()
                .collect();
            ensure!(
                headers.len() <= 32
                    && headers
                        .iter()
                        .map(|header| header.as_bytes().len())
                        .sum::<usize>()
                        <= MAX_COOKIE_BYTES,
                "网易云 Cookie 响应过大"
            );
            let cookies = response
                .cookies()
                .filter(|cookie| matches!(cookie.name(), "MUSIC_U" | "__csrf" | "MUSIC_A"))
                .map(|cookie| (cookie.name().to_owned(), cookie.value().to_owned()))
                .collect();
            let mut bytes = Vec::new();
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|_| anyhow::anyhow!("网易云响应读取失败"))?
            {
                ensure!(
                    bytes.len().saturating_add(chunk.len()) <= MAX_RESPONSE_BYTES,
                    "网易云响应过大"
                );
                bytes.extend_from_slice(&chunk);
            }
            let body = serde_json::from_slice(&bytes)
                .map_err(|_| anyhow::anyhow!("网易云响应格式无效"))?;
            Ok(NcmResponse { body, cookies })
        })
        .await
        .map_err(|_| anyhow::anyhow!("网易云请求超时"))?
    }

    #[cfg(test)]
    pub(super) fn with_base(mut self, base: String) -> Self {
        self.base = base;
        self
    }
}

pub(crate) fn session_pairs(header: &str) -> Result<BTreeMap<String, String>> {
    ensure!(
        header.len() <= MAX_COOKIE_BYTES && !header.contains(['\r', '\n']),
        "网易云会话格式无效"
    );
    let mut result = BTreeMap::new();
    for pair in header
        .split(';')
        .map(str::trim)
        .filter(|part| !part.is_empty())
    {
        let Some((name, value)) = pair.split_once('=') else {
            bail!("网易云会话格式无效");
        };
        if !matches!(name, "MUSIC_U" | "__csrf" | "MUSIC_A") {
            continue;
        }
        ensure!(
            !value.is_empty()
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_graphic()
                        && !matches!(byte, b';' | b',' | b'"' | b'\\')),
            "网易云会话格式无效"
        );
        ensure!(
            result.insert(name.to_owned(), value.to_owned()).is_none(),
            "网易云会话字段重复"
        );
    }
    Ok(result)
}
