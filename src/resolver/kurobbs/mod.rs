//! 库街区（鸣潮社区）：帖子内容走官方 forum API。

use anyhow::{Context, Result};
use regex::Regex;
use reqwest::header;
use serde_json::Value;
use url::Url;

use crate::{
    resolver::adapter::PlatformAdapter,
    resolver::fetch::{MOBILE_USER_AGENT, PageFetcher},
    resolver::page::strip_html,
    resolver::{MediaInfo, Platform, Resolved},
};

const POST_DETAIL_API: &str = "https://api.kurobbs.com/forum/getPostDetail";

pub struct KurobbsAdapter {
    fetcher: PageFetcher,
}

impl KurobbsAdapter {
    pub fn new(fetcher: PageFetcher) -> Self {
        Self { fetcher }
    }
}

#[async_trait::async_trait]
impl PlatformAdapter for KurobbsAdapter {
    fn platform(&self) -> Platform {
        Platform::Kurobbs
    }

    async fn resolve(&self, url: &Url) -> Result<Resolved> {
        let post_id = post_id(url).context("kurobbs URL has no post id")?;
        let value: Value = self
            .fetcher
            .client()
            .post(POST_DETAIL_API)
            .header(header::REFERER, Platform::Kurobbs.media_referer())
            .header(header::USER_AGENT, MOBILE_USER_AGENT)
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(format!(
                "postId={post_id}&isOnlyPublisher=0&showOrderType=2"
            ))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let post = value.get("data").context("kurobbs post missing")?;
        Ok(Resolved::new(map_post(post, url)))
    }
}

fn post_id(url: &Url) -> Option<String> {
    Regex::new(r"/(?:post|topic)/(\d+)")
        .ok()?
        .captures(url.path())?
        .get(1)
        .map(|value| value.as_str().to_string())
}

fn map_post(post: &Value, source: &Url) -> MediaInfo {
    MediaInfo {
        title: post
            .get("postTitle")
            .and_then(Value::as_str)
            .map(str::to_string),
        description: post
            .get("postContent")
            .and_then(Value::as_str)
            .map(strip_html),
        author: post
            .get("userName")
            .and_then(Value::as_str)
            .map(str::to_string),
        images: post
            .get("postCoverImages")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        source_url: source.to_string(),
        platform: Platform::Kurobbs,
        raw: serde_json::json!({"platform": "kurobbs", "post": post}),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_post_id_from_path() {
        assert_eq!(
            post_id(&Url::parse("https://www.kurobbs.com/post/12345").unwrap()),
            Some("12345".into())
        );
        assert_eq!(
            post_id(&Url::parse("https://www.kurobbs.com/home").unwrap()),
            None
        );
    }

    #[test]
    fn maps_post_and_strips_content_markup() {
        let post = serde_json::json!({
            "postTitle": "标题",
            "postContent": "<p>正文</p>",
            "userName": "作者",
            "postCoverImages": ["https://img.example/a.jpg"],
        });
        let media = map_post(
            &post,
            &Url::parse("https://www.kurobbs.com/post/1").unwrap(),
        );
        assert_eq!(media.title.as_deref(), Some("标题"));
        assert_eq!(media.description.as_deref().map(str::trim), Some("正文"));
        assert_eq!(media.images, vec!["https://img.example/a.jpg"]);
        assert_eq!(media.platform, Platform::Kurobbs);
    }
}
