//! 抖音：API 优先；静态分享页没有作品时，用独立匿名 Chromium 会话读取公开页面。
//! 当前 Obscura 执行抖音完整页面存在原生 iframe 崩溃，不能在宿主里运行该回退。

use anyhow::{Context, Result, bail};
use serde_json::Value;
use tracing::warn;
use url::Url;

pub(crate) mod anonymous_browser;
pub mod client;
pub(crate) mod desktop;
pub(crate) mod engagement;
use engagement::DouyinContent;

use client::{DouyinClient, map_complete_detail, parse_aweme_ref};

use crate::{
    resolver::fetch::PageFetcher,
    resolver::sites::PlatformAdapter,
    resolver::{
        MediaInfo, Platform, Resolved,
        page::{captured_json, page_metadata},
    },
};

const SHORT_HOST: &str = "v.douyin.com";
const ROUTER_PATTERN: &str = r"(?s)window\._ROUTER_DATA\s*=\s*(.*?)</script>";

pub struct DouyinAdapter {
    client: DouyinClient,
    fetcher: PageFetcher,
    cookie: String,
    anonymous_browser: Option<anonymous_browser::AnonymousBrowser>,
    include_engagement: bool,
}

impl DouyinAdapter {
    pub fn new(
        client: DouyinClient,
        fetcher: PageFetcher,
        cookie: String,
        anonymous_browser: Option<anonymous_browser::AnonymousBrowser>,
    ) -> Self {
        Self {
            client,
            fetcher,
            cookie,
            anonymous_browser,
            include_engagement: true,
        }
    }
    pub fn with_engagement(mut self, enabled: bool) -> Self {
        self.include_engagement = enabled;
        self
    }
}

#[async_trait::async_trait]
impl PlatformAdapter for DouyinAdapter {
    fn platform(&self) -> Platform {
        Platform::Douyin
    }

    async fn resolve(&self, url: &Url) -> Result<Resolved> {
        let expanded = self.expand(url).await?;
        let aweme = parse_aweme_ref(&expanded).context("抖音链接未找到作品 ID")?;
        let mut basic = match self.client.resolve(&aweme, &expanded).await {
            Ok(media) => Some(media),
            Err(error) => {
                warn!(error = %error, "douyin detail API failed, falling back to share page");
                None
            }
        };
        if basic.is_none() {
            if let Ok(page) = self
                .fetcher
                .get_http_page(&expanded, Platform::Douyin, Some(&self.cookie))
                .await
            {
                let mut media = page_metadata(&page.html, &page.url, Platform::Douyin);
                if enrich(&page.html, page.state.as_ref(), &mut media)
                    && media.raw.get("aweme_id").and_then(Value::as_str) == Some(aweme.id.as_str())
                {
                    basic = Some(media);
                }
            }
        }
        // Successful APIs retain their download/gallery data. A separate
        // anonymous visit can still supplement video statistics and comments.
        if basic.is_some()
            && (aweme.is_note || !self.include_engagement || self.anonymous_browser.is_none())
        {
            return Ok(Resolved::new(DouyinContent::new(basic.unwrap())));
        }
        if let Some(browser) = &self.anonymous_browser {
            match browser.fetch(&aweme, self.include_engagement).await {
                Ok(page) => {
                    let media = match basic {
                        Some(mut media) => {
                            if media.is_video && media.video_url.is_none() {
                                media.video_url =
                                    desktop::map_desktop_page(&page.html, &aweme, &expanded)
                                        .ok()
                                        .and_then(|page| page.video_url);
                            }
                            media
                        }
                        None => desktop::map_desktop_page(&page.html, &aweme, &expanded)?,
                    };
                    let mut content = DouyinContent::new(media);
                    content.engagement.merge(page.engagement);
                    tracing::info!(
                        platform = "douyin",
                        engine = "chromium-anonymous",
                        comments = content.engagement.top_comments().len(),
                        has_video_stream = content.media.video_url.is_some(),
                        "抖音公开作品信息获取完成"
                    );
                    return Ok(Resolved::new(content));
                }
                Err(error) if basic.is_none() => return Err(error),
                Err(_) => {
                    tracing::debug!("Douyin anonymous extras unavailable; keeping API work data")
                }
            }
        }
        if let Some(media) = basic {
            return Ok(Resolved::new(DouyinContent::new(media)));
        }
        bail!("抖音接口与分享页未返回作品；可启用抖音匿名浏览器读取公开信息")
    }
}

impl DouyinAdapter {
    /// 展开 `v.douyin.com` 短链，命中作品 ID 即停止。
    async fn expand(&self, url: &Url) -> Result<Url> {
        if !matches!(url.host_str(), Some(host) if host.eq_ignore_ascii_case(SHORT_HOST)) {
            return Ok(url.clone());
        }
        self.fetcher
            .expand_short_url(url, Platform::Douyin, |current| {
                parse_aweme_ref(current).is_some()
            })
            .await
    }
}

/// 页面兜底优先读执行后的路由状态，没有时再解析旧分享页内联状态。
fn enrich(html: &str, state: Option<&Value>, media: &mut MediaInfo) -> bool {
    let inline_state = state
        .is_none()
        .then(|| captured_json(html, ROUTER_PATTERN))
        .flatten();
    let Some(state) = state.or(inline_state.as_ref()) else {
        return false;
    };
    let detail = state
        .get("loaderData")
        .and_then(Value::as_object)
        .and_then(|pages| {
            pages.values().find_map(|page| {
                page.get("videoInfoRes")?
                    .get("item_list")?
                    .as_array()?
                    .first()
            })
        });
    let Some(detail) = detail else {
        return false;
    };
    let Ok(source) = Url::parse(&media.source_url) else {
        return false;
    };
    let Some(expected) = parse_aweme_ref(&source) else {
        return false;
    };
    let Ok(mapped) = map_complete_detail(detail, &expected, &source) else {
        return false;
    };
    *media = mapped;
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enriches_from_inlined_router_data() {
        let html = r#"<script>window._ROUTER_DATA = {"loaderData":{"video_(id)/page":{"videoInfoRes":{"item_list":[{"aweme_id":"123","desc":"作品","author":{"nickname":"作者","avatar_thumb":{"url_list":["https://p3.douyinpic.com/avatar.jpeg"]}},"create_time":1700000000,"video":{"origin_cover":{"url_list":["https://p3.douyinpic.com/cover.jpeg"]},"play_addr":{"url_list":["https://v26.douyinvod.com/a.mp4"]}}}]}}}}</script>"#;
        let mut media = MediaInfo {
            source_url: "https://www.iesdouyin.com/share/video/123/".into(),
            platform: Platform::Douyin,
            ..MediaInfo::default()
        };
        enrich(html, None, &mut media);
        assert_eq!(media.title.as_deref(), Some("作品"));
        assert_eq!(media.author.as_deref(), Some("作者"));
        assert_eq!(
            media.avatar_url.as_deref(),
            Some("https://p3.douyinpic.com/avatar.jpeg")
        );
        assert_eq!(media.published_at, Some(1_700_000_000));
        assert!(media.is_video);
        assert!(
            media
                .video_url
                .as_deref()
                .is_some_and(|url| url.contains("douyinvod.com"))
        );
    }

    #[test]
    fn leaves_media_untouched_when_page_has_no_data() {
        let mut media = MediaInfo {
            title: Some("原标题".into()),
            platform: Platform::Douyin,
            ..MediaInfo::default()
        };
        enrich("<html></html>", None, &mut media);
        assert_eq!(media.title.as_deref(), Some("原标题"));
        assert!(media.video_url.is_none());
    }

    #[test]
    fn generic_page_fields_cannot_complete_partial_router_data() {
        let source = Url::parse("https://www.iesdouyin.com/share/video/123/").unwrap();
        let shell = r#"<title>抱歉出错了，请尝试在抖音内观看</title><meta property="og:image" content="https://p3.douyinpic.com/site-icon.png">"#;
        let complete = serde_json::json!({
            "aweme_id": "123",
            "desc": "真实作品",
            "author": {
                "nickname": "作者",
                "avatar_thumb": {"url_list": ["https://p3.douyinpic.com/avatar.jpeg"]}
            },
            "video": {"origin_cover": {"url_list": ["https://p3.douyinpic.com/cover.jpeg"]}}
        });
        for field in ["desc", "video"] {
            let mut partial = complete.clone();
            partial.as_object_mut().unwrap().remove(field);
            let state = serde_json::json!({"loaderData": {"page": {"videoInfoRes": {"item_list": [partial]}}}});
            let mut media = page_metadata(shell, &source, Platform::Douyin);
            assert!(
                !enrich(shell, Some(&state), &mut media),
                "page shell filled missing {field}"
            );
            assert_eq!(
                media.title.as_deref(),
                Some("抱歉出错了，请尝试在抖音内观看")
            );
        }
        let state = serde_json::json!({"loaderData": {"page": {"videoInfoRes": {"item_list": [complete]}}}});
        let mut media = page_metadata(shell, &source, Platform::Douyin);
        assert!(enrich(shell, Some(&state), &mut media));
        assert_eq!(media.title.as_deref(), Some("真实作品"));
        assert_eq!(
            media.cover_url.as_deref(),
            Some("https://p3.douyinpic.com/cover.jpeg")
        );
        assert!(media.description.is_none());
    }

    #[test]
    fn prefers_executed_state_over_stale_inline_state() {
        let html = r#"<script>window._ROUTER_DATA = {"loaderData":{"page":{"videoInfoRes":{"item_list":[{"desc":"旧作品","video":{"play_addr":{"url_list":["https://v26.douyinvod.com/old.mp4"]}}}]}}}}</script>"#;
        let state = serde_json::json!({
            "loaderData": {
                "page": {
                    "videoInfoRes": {
                        "item_list": [{
                            "aweme_id": "123",
                            "desc": "动态作品",
                            "author": {
                                "nickname": "作者",
                                "avatar_thumb": {"url_list": ["https://p3.douyinpic.com/avatar.jpeg"]}
                            },
                            "video": {
                                "origin_cover": {"url_list": ["https://p3.douyinpic.com/cover.jpeg"]},
                                "play_addr": {
                                    "url_list": ["https://v26.douyinvod.com/new.mp4"]
                                }
                            }
                        }]
                    }
                }
            }
        });
        let mut media = MediaInfo {
            source_url: "https://www.iesdouyin.com/share/video/123/".into(),
            platform: Platform::Douyin,
            ..MediaInfo::default()
        };
        enrich(html, Some(&state), &mut media);
        assert_eq!(media.title.as_deref(), Some("动态作品"));
        assert_eq!(media.author.as_deref(), Some("作者"));
        assert_eq!(
            media.video_url.as_deref(),
            Some("https://v26.douyinvod.com/new.mp4")
        );
    }
}
