//! Twitter/X：走 oEmbed 端点，只拿得到作者和嵌入 HTML，没有媒体直链。

use reqwest::Client;

use anyhow::Result;
use serde_json::Value;
use url::Url;

use crate::{
    resolver::adapter::PlatformAdapter,
    resolver::page::strip_html,
    resolver::{MediaInfo, Platform, Resolved},
};

pub struct TwitterAdapter {
    http: Client,
    endpoint: String,
}

impl TwitterAdapter {
    pub fn new(http: Client, endpoint: String) -> Self {
        Self { http, endpoint }
    }
}

#[async_trait::async_trait]
impl PlatformAdapter for TwitterAdapter {
    fn platform(&self) -> Platform {
        Platform::Twitter
    }

    async fn resolve(&self, url: &Url) -> Result<Resolved> {
        let value: Value = self
            .http
            .get(&self.endpoint)
            .query(&[("url", url.as_str()), ("omit_script", "1")])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(Resolved::new(map_oembed(&value, url)))
    }
}

fn map_oembed(value: &Value, source: &Url) -> MediaInfo {
    let author = value
        .get("author_name")
        .and_then(Value::as_str)
        .map(str::to_string);
    MediaInfo {
        title: author.clone(),
        description: value.get("html").and_then(Value::as_str).map(strip_html),
        author,
        source_url: source.to_string(),
        platform: Platform::Twitter,
        raw: serde_json::json!({"platform": "twitter", "response": value}),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_embed_markup_into_description() {
        let value = serde_json::json!({
            "author_name": "某人",
            "html": "<blockquote><p>推文<br>正文</p></blockquote>",
        });
        let media = map_oembed(&value, &Url::parse("https://x.com/a/status/1").unwrap());
        assert_eq!(media.author.as_deref(), Some("某人"));
        let description = media.description.unwrap();
        assert!(!description.contains('<'));
        assert!(description.contains("推文"));
        assert_eq!(media.platform, Platform::Twitter);
    }
}
