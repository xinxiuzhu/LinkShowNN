//! 网易云音乐：走用户自建的 NeteaseCloudMusicApi 实例，官方接口不开放。

use reqwest::Client;

use anyhow::{Context, Result};
use regex::Regex;
use serde_json::Value;
use url::Url;

use crate::{
    resolver::sites::PlatformAdapter,
    resolver::{MediaInfo, Platform, Resolved},
};

pub struct NcmAdapter {
    http: Client,
    endpoint: String,
}

impl NcmAdapter {
    pub fn new(http: Client, endpoint: String) -> Self {
        Self { http, endpoint }
    }
}

#[async_trait::async_trait]
impl PlatformAdapter for NcmAdapter {
    fn platform(&self) -> Platform {
        Platform::Ncm
    }

    async fn resolve(&self, url: &Url) -> Result<Resolved> {
        let id = song_id(url).context("NCM URL has no song id")?;
        let base = self.endpoint.trim_end_matches('/');
        let http = &self.http;
        let detail: Value = http
            .get(format!("{base}/song/detail?ids={id}"))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let song = detail
            .get("songs")
            .and_then(Value::as_array)
            .and_then(|songs| songs.first())
            .context("NCM song missing")?;
        let audio: Value = http
            .get(format!("{base}/song/url/v1?id={id}&level=standard"))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(Resolved::new(map_song(song, &audio, url)))
    }
}

/// 歌曲 ID 可能在 query 里，也可能在路径上。
fn song_id(url: &Url) -> Option<u64> {
    url.query_pairs()
        .find(|(key, _)| key == "id")
        .and_then(|(_, value)| value.parse::<u64>().ok())
        .or_else(|| {
            Regex::new(r"(?:song/|song\?id=)(\d+)")
                .ok()?
                .captures(url.as_str())?
                .get(1)?
                .as_str()
                .parse()
                .ok()
        })
}

fn map_song(song: &Value, audio: &Value, source: &Url) -> MediaInfo {
    MediaInfo {
        title: song.get("name").and_then(Value::as_str).map(str::to_string),
        description: song
            .get("al")
            .and_then(|album| album.get("name"))
            .and_then(Value::as_str)
            .map(|album| format!("专辑: {album}")),
        cover_url: song
            .get("al")
            .and_then(|album| album.get("picUrl"))
            .and_then(Value::as_str)
            .map(str::to_string),
        audio_url: audio
            .get("data")
            .and_then(Value::as_array)
            .and_then(|items| items.first())
            .and_then(|item| item.get("url"))
            .and_then(Value::as_str)
            .map(str::to_string),
        author: song
            .get("ar")
            .and_then(Value::as_array)
            .and_then(|artists| artists.first())
            .and_then(|artist| artist.get("name"))
            .and_then(Value::as_str)
            .map(str::to_string),
        source_url: source.to_string(),
        platform: Platform::Ncm,
        raw: serde_json::json!({"platform": "ncm", "song": song}),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_song_id_from_query_and_path() {
        assert_eq!(
            song_id(&Url::parse("https://music.163.com/#/song?id=123").unwrap()),
            Some(123)
        );
        assert_eq!(
            song_id(&Url::parse("https://music.163.com/song/456").unwrap()),
            Some(456)
        );
        assert_eq!(
            song_id(&Url::parse("https://music.163.com/artist").unwrap()),
            None
        );
    }

    #[test]
    fn maps_song_payload_with_platform_referer() {
        let song = serde_json::json!({
            "name": "歌名",
            "al": {"name": "专辑名", "picUrl": "https://p.example/cover.jpg"},
            "ar": [{"name": "歌手"}],
        });
        let audio = serde_json::json!({"data": [{"url": "https://m.example/song.mp3"}]});
        let media = map_song(
            &song,
            &audio,
            &Url::parse("https://music.163.com/#/song?id=1").unwrap(),
        );
        assert_eq!(media.title.as_deref(), Some("歌名"));
        assert_eq!(media.description.as_deref(), Some("专辑: 专辑名"));
        assert_eq!(media.author.as_deref(), Some("歌手"));
        assert_eq!(
            media.audio_url.as_deref(),
            Some("https://m.example/song.mp3")
        );
        assert_eq!(media.platform, Platform::Ncm);
    }
}
