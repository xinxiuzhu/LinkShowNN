//! 抖音：优先接口，随后读取公开静态分享页。
//!
use anyhow::{Context, Result, bail};
use serde_json::Value;
use tracing::warn;
use url::Url;

pub mod client;
pub(crate) mod desktop;
pub(crate) mod engagement;
mod playback;
mod video;
use engagement::DouyinContent;

use client::{DouyinClient, map_complete_detail_with_quality, parse_aweme_ref};

use crate::{
    resolver::adapter::PlatformAdapter,
    resolver::fetch::PageFetcher,
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
    include_engagement: bool,
}

impl DouyinAdapter {
    pub fn new(client: DouyinClient, fetcher: PageFetcher, cookie: String) -> Self {
        Self {
            client,
            fetcher,
            cookie,
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
        let mut public_extras = engagement::Engagement::default();
        let mut basic = if aweme.is_note {
            match self.client.resolve_slides(&aweme, &expanded).await {
                Ok(media) => Some(media),
                Err(error) => {
                    warn!(error = %error, "douyin slides API failed, falling back to detail API");
                    None
                }
            }
        } else {
            match self.client.resolve_video(&aweme, &expanded).await {
                Ok(media) => Some(media),
                Err(error) => {
                    warn!(error = %error, "douyin mobile feed failed, falling back to detail API");
                    None
                }
            }
        };
        if basic.is_none() {
            basic = match self.client.resolve(&aweme, &expanded).await {
                Ok(media) => Some(media),
                Err(error) => {
                    warn!(error = %error, "douyin detail API failed, falling back to share page");
                    None
                }
            };
        }
        if basic.is_none() {
            if let Ok(page) = self
                .fetcher
                .get_http_page(&expanded, Platform::Douyin, Some(&self.cookie))
                .await
            {
                basic = map_public_page_with_quality(
                    &page.html,
                    None,
                    &aweme,
                    &page.url,
                    self.client.quality(),
                )
                .ok();
                if basic.is_some() && self.include_engagement {
                    public_extras =
                        desktop::map_engagement(&page.html, &aweme, &page.url).unwrap_or_default();
                }
            }
        }
        if let Some(media) = basic {
            let mut content = DouyinContent::new(media);
            content.engagement.merge(public_extras);
            return Ok(Resolved::new(content));
        }
        bail!("抖音接口或公开分享页未返回可用作品")
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

/// 静态页只接受完整路由数据或经作品 ID 验证的 DOM，不拿验证页元数据冒充作品。
#[cfg(test)]
pub(crate) fn map_public_page(
    html: &str,
    state: Option<&Value>,
    expected: &client::AwemeRef,
    source: &Url,
) -> Result<MediaInfo> {
    map_public_page_with_quality(html, state, expected, source, "1080P")
}

fn map_public_page_with_quality(
    html: &str,
    state: Option<&Value>,
    expected: &client::AwemeRef,
    source: &Url,
    quality: &str,
) -> Result<MediaInfo> {
    let mut media = page_metadata(html, source, Platform::Douyin);
    if enrich_with_quality(html, state, &mut media, quality)
        && media.raw.get("aweme_id").and_then(Value::as_str) == Some(expected.id.as_str())
    {
        return Ok(media);
    }
    let mut media = desktop::map_desktop_page(html, expected, source)?;
    if media.is_video && media.video_url.is_some() {
        media.quality_notice = Some(format!(
            "首选 {}，公开页未提供可选清晰度或源尺寸，使用可用原始源。",
            playback::quality_label(quality)
        ));
    }
    Ok(media)
}

/// 从结构化路由数据或分享页内联状态提取作品。
#[cfg(test)]
fn enrich(html: &str, state: Option<&Value>, media: &mut MediaInfo) -> bool {
    enrich_with_quality(html, state, media, "1080P")
}

fn enrich_with_quality(
    html: &str,
    state: Option<&Value>,
    media: &mut MediaInfo,
    quality: &str,
) -> bool {
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
    let Ok(mapped) = map_complete_detail_with_quality(detail, &expected, &source, quality) else {
        return false;
    };
    *media = mapped;
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_public_page_requires_matching_work_and_retains_its_own_metadata() {
        let html = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/douyin-desktop.html"
        ));
        let source = Url::parse("https://www.douyin.com/video/7693165612066999571").unwrap();
        let expected = parse_aweme_ref(&source).unwrap();
        let media = map_public_page(html, None, &expected, &source).unwrap();
        assert_eq!(media.author.as_deref(), Some("鸣潮"));
        assert!(media.cover_url.is_some());
        let other = client::AwemeRef {
            id: "7693165612066999572".into(),
            is_note: false,
        };
        assert!(map_public_page(html, None, &other, &source).is_err());
    }

    #[test]
    fn verification_page_cannot_be_reported_as_a_successful_work() {
        let source = Url::parse("https://www.douyin.com/video/123").unwrap();
        let expected = parse_aweme_ref(&source).unwrap();
        let html = r#"<title>安全验证</title><meta property="og:image" content="https://p3.douyinpic.com/site-logo.png">"#;
        assert!(map_public_page(html, None, &expected, &source).is_err());
    }

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
    fn structured_router_data_preserves_matching_work_metadata() {
        let html = r#"<script>window._ROUTER_DATA = {"loaderData":{"page":{"videoInfoRes":{"item_list":[{"desc":"旧作品","video":{"play_addr":{"url_list":["https://v26.douyinvod.com/old.mp4"]}}}]}}}}</script>"#;
        let state = serde_json::json!({
            "loaderData": {
                "page": {
                    "videoInfoRes": {
                        "item_list": [{
                            "aweme_id": "123",
                            "desc": "匹配作品",
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
        assert_eq!(media.title.as_deref(), Some("匹配作品"));
        assert_eq!(media.author.as_deref(), Some("作者"));
        assert_eq!(
            media.video_url.as_deref(),
            Some("https://v26.douyinvod.com/new.mp4")
        );
    }

    #[test]
    fn share_page_router_metadata_uses_the_configured_quality() {
        let source = Url::parse("https://www.douyin.com/video/123").unwrap();
        let expected = parse_aweme_ref(&source).unwrap();
        let state = serde_json::json!({"loaderData": {"page": {"videoInfoRes": {"item_list": [{
            "aweme_id": "123", "desc": "作品", "author": {
                "nickname": "作者", "avatar_thumb": {"url_list": ["https://p3.douyinpic.com/avatar.jpeg"]}
            }, "video": {
                "origin_cover": {"url_list": ["https://p3.douyinpic.com/cover.jpeg"]},
                "play_addr": {"uri": "v_actual"}
            }
        }]}}}});
        let media =
            map_public_page_with_quality("", Some(&state), &expected, &source, "360P").unwrap();
        assert!(media.video_url.unwrap().contains("ratio=360p"));
        assert_eq!(media.video_quality.as_deref(), Some("请求360P"));
    }
}
