//! 网易云音乐：默认直连官方接口，可选兼容用户自部署 API。

pub(crate) mod client;
mod content;
mod crypto;
mod urls;

use anyhow::{Context, Result};
use reqwest::Client;
use serde_json::Value;
use url::Url;

use crate::resolver::{Platform, Resolved, adapter::PlatformAdapter};
use client::NcmClient;

pub struct NcmAdapter {
    client: NcmClient,
    endpoint: String,
    cookie: String,
}

impl NcmAdapter {
    pub fn new(http: Client, endpoint: String) -> Self {
        Self {
            client: NcmClient::new(http),
            endpoint,
            cookie: String::new(),
        }
    }

    /// 数据库扫码会话只用于官方原生接口，绝不转发给自部署兼容 API。
    pub(crate) fn with_cookie(mut self, cookie: Option<&str>) -> Self {
        self.cookie = cookie.unwrap_or_default().to_owned();
        self
    }

    async fn legacy(&self, id: u64) -> Result<(Value, Result<Value>)> {
        let base = Url::parse(&format!("{}/", self.endpoint.trim().trim_end_matches('/')))
            .context("网易云自部署 API 地址无效")?;
        let detail = self
            .client
            .legacy_get(base.join("song/detail")?, &[("ids", id.to_string())])
            .await?;
        let audio = self
            .client
            .legacy_get(
                base.join("song/url/v1")?,
                &[("id", id.to_string()), ("level", "standard".to_owned())],
            )
            .await;
        Ok((detail, audio))
    }
}

#[async_trait::async_trait]
impl PlatformAdapter for NcmAdapter {
    fn platform(&self) -> Platform {
        Platform::Ncm
    }

    async fn resolve(&self, source: &Url) -> Result<Resolved> {
        let canonical = self.client.song_url(source).await?;
        let id = urls::song_id(&canonical).context("网易云链接未包含歌曲 ID")?;
        let native = self.client.song_detail(id).await;
        let (detail, audio) = match native {
            Ok(detail) => (detail, self.client.song_audio(id, &self.cookie).await),
            Err(error) if self.endpoint.trim().is_empty() => return Err(error),
            Err(_) => self.legacy(id).await?,
        };
        let song = detail
            .get("songs")
            .and_then(Value::as_array)
            .and_then(|songs| {
                songs
                    .iter()
                    .find(|song| song.get("id").and_then(Value::as_u64) == Some(id))
            })
            .context("网易云歌曲不存在或暂不可用")?;
        // 播放接口失败仍保留歌曲信息，错误正文与临时签名不进入卡片或日志。
        Ok(Resolved::new(content::map_song(
            song,
            audio.as_ref().ok(),
            &canonical,
        )))
    }
}

#[cfg(test)]
mod tests;
