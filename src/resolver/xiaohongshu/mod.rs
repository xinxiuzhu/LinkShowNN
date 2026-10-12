//! 小红书读取 PC/移动端 HTTP 分享页中的笔记状态。

use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;
use std::time::Duration;
use tracing::{info, warn};
use url::Url;

use crate::resolver::{
    MediaInfo, Platform, Resolved,
    adapter::PlatformAdapter,
    fetch::{FetchedPage, PageFetcher},
    url_policy::{validate_platform_url, validate_public_url},
};

mod content;
mod engagement;
mod state;
#[cfg(test)]
mod transport_tests;
mod video;

use content::XiaohongshuContent;
use engagement::Engagement;

const PC_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

pub struct XiaohongshuAdapter {
    fetcher: PageFetcher,
    cookie: String,
    video_quality: Option<u64>,
    comments_enabled: bool,
}

impl XiaohongshuAdapter {
    pub fn new(fetcher: PageFetcher, cookie: String) -> Self {
        Self {
            fetcher,
            cookie,
            video_quality: Some(1080),
            comments_enabled: true,
        }
    }

    /// 选择平台已提供的源，不通过转码制造所请求的档位。
    pub fn with_quality(mut self, quality: &str) -> Self {
        self.video_quality = video::requested_quality(quality);
        self
    }

    pub fn with_comments_enabled(mut self, enabled: bool) -> Self {
        self.comments_enabled = enabled;
        self
    }
}

#[async_trait::async_trait]
impl PlatformAdapter for XiaohongshuAdapter {
    fn platform(&self) -> Platform {
        Platform::Xiaohongshu
    }

    async fn resolve(&self, url: &Url) -> Result<Resolved> {
        validate_platform_url(url, Platform::Xiaohongshu)?;
        let mut expected_id = state::note_id(url);
        let cookie = (!self.cookie.trim().is_empty()).then_some(self.cookie.as_str());
        let mut preferred: Option<XiaohongshuContent> = None;
        let mut expanded_url = None;
        let mut last_error = None;
        for pc in [true, false] {
            if !pc
                && !self.comments_enabled
                && preferred.as_ref().is_some_and(|content| {
                    !content.media.is_video || content.media.video_url.is_some()
                })
            {
                // 关闭评论时不为已取得的 PC 内容补取移动页；只有缺少
                // 播放源时，移动端请求仍是必要的媒体降级路径。
                break;
            }
            let source = expanded_url.as_ref().unwrap_or(url);
            let response = if pc {
                self.fetcher
                    .get_http_page_with_user_agent(
                        source,
                        Platform::Xiaohongshu,
                        cookie,
                        PC_USER_AGENT,
                    )
                    .await
            } else {
                // 评论补充有独立短预算；失败不能让已取得的笔记丢失。
                match tokio::time::timeout(
                    Duration::from_secs(6),
                    self.fetcher
                        .get_http_page(source, Platform::Xiaohongshu, cookie),
                )
                .await
                {
                    Ok(response) => response,
                    Err(_) => Err(anyhow::anyhow!("小红书移动端公开评论获取超时")),
                }
            };
            let result = match response {
                Ok(page) => (|| {
                    // 请求已指定笔记时，不能接受分享链接跳转到另一篇作品。
                    check_redirect_identity(expected_id.as_deref(), &page.url)?;
                    // 短链第一次展开后的 ID 也约束下一轮 HTTP 请求。
                    if let Some(id) = state::note_id(&page.url) {
                        if expected_id.is_none() {
                            expected_id = Some(id);
                        }
                        expanded_url = Some(page.url.clone());
                    }
                    map_content(
                        &page,
                        expected_id.as_deref(),
                        self.video_quality,
                        self.comments_enabled,
                    )
                })(),
                Err(error) => Err(error),
            };
            match result {
                Ok(content) => match preferred.as_mut() {
                    Some(preferred) => preferred.enrich(content),
                    None => preferred = Some(content),
                },
                Err(error) => {
                    if !pc && preferred.is_some() {
                        warn!(error = %error, "小红书移动端评论补充失败，保留已取得的笔记");
                    }
                    last_error = Some(error);
                }
            }
        }
        // 身份已校验的封面也能生成海报，缺少播放源不使笔记解析失败。
        if let Some(content) = preferred {
            info!(
                engine = "http-pc-mobile",
                platform = "xiaohongshu",
                comments = content.engagement.top_comments().len(),
                "小红书公开笔记与互动信息获取完成"
            );
            return Ok(Resolved::new(content));
        }
        Err(last_error.unwrap_or_else(|| anyhow::anyhow!("分享页未包含笔记状态")))
            .context("小红书公开分享页未返回可用笔记内容")
    }
}

fn check_redirect_identity(expected_id: Option<&str>, final_url: &Url) -> Result<()> {
    validate_platform_url(final_url, Platform::Xiaohongshu)?;
    if let (Some(expected), Some(actual)) = (expected_id, state::note_id(final_url)) {
        ensure!(expected == actual, "小红书分享链接跳转到了另一篇笔记");
    }
    Ok(())
}

/// 仅使用已绑定请求 ID 的笔记字段，不把验证页或推荐内容的 OG 当作作品。
fn map_content(
    page: &FetchedPage,
    expected_id: Option<&str>,
    requested_quality: Option<u64>,
    comments_enabled: bool,
) -> Result<XiaohongshuContent> {
    check_redirect_identity(expected_id, &page.url)?;
    let final_id = state::note_id(&page.url).context("小红书最终链接未包含笔记 ID")?;
    let initial_state = state::parse_inline(&page.html)?;
    let note = state::requested_note(&initial_state, &final_id)?;
    let engagement =
        Engagement::from_state_with_comments(&initial_state, &final_id, comments_enabled);
    let title = string_field(note, &["title"]);
    let description = string_field(note, &["desc"]);
    ensure!(
        title.is_some() || description.is_some(),
        "小红书笔记未返回标题或正文"
    );
    let user = note.get("user").unwrap_or(note);
    let mut images = Vec::new();
    for image in note
        .get("imageList")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(url) = image_url(image)
            && !images.contains(&url)
        {
            images.push(url);
            if images.len() == 20 {
                break;
            }
        }
    }
    let cover_url = images
        .first()
        .cloned()
        .or_else(|| note.get("cover").and_then(image_url))
        .or_else(|| {
            note.get("video")
                .and_then(|video| video.get("cover"))
                .and_then(image_url)
        });
    let mut media = MediaInfo {
        title,
        description,
        author: string_field(user, &["nickname", "nickName"]),
        avatar_url: string_field(user, &["avatar", "avatarUrl", "image"])
            .and_then(|url| public_url(&url)),
        published_at: note
            .get("time")
            .and_then(Value::as_i64)
            .filter(|time| *time > 0)
            .map(|time| time / 1000),
        images,
        cover_url,
        source_url: page.url.to_string(),
        platform: Platform::Xiaohongshu,
        raw: serde_json::json!({"platform": "xiaohongshu", "note_id": final_id}),
        ..Default::default()
    };
    match note.get("type").and_then(Value::as_str) {
        Some("video") => {
            let selection = video::select(note.get("video"), requested_quality);
            media.video_quality = selection.quality();
            media.quality_notice = selection.notice;
            let mut candidates = selection.urls.into_iter();
            media.video_url = candidates.next();
            media.video_fallback_urls = candidates.collect();
            media.is_video = true;
            // 视频 imageList 是封面尺寸，不应作为图集另外发送。
            media.images.clear();
            ensure!(
                media.cover_url.is_some() || media.video_url.is_some(),
                "小红书视频未返回封面或可用播放地址"
            );
        }
        Some("normal") => {
            media.video_url = None;
            media.video_fallback_urls.clear();
            media.is_video = false;
            ensure!(!media.images.is_empty(), "小红书图文笔记未返回原图");
        }
        _ => bail!("小红书笔记类型未知或内容不完整"),
    }
    Ok(XiaohongshuContent { media, engagement })
}

#[cfg(test)]
fn map_page(page: &FetchedPage, expected_id: Option<&str>) -> Result<MediaInfo> {
    Ok(map_content(page, expected_id, None, true)?.media)
}

fn string_field(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        value
            .get(*key)
            .and_then(Value::as_str)
            .filter(|text| !text.trim().is_empty())
            .map(str::to_string)
    })
}

fn public_url(value: &str) -> Option<String> {
    let value = value.trim();
    let absolute = if value.starts_with("//") {
        format!("https:{value}")
    } else {
        value.to_owned()
    };
    let url = Url::parse(&absolute).ok()?;
    validate_public_url(&url).ok()?;
    Some(absolute)
}

fn image_url(image: &Value) -> Option<String> {
    if let Some(value) = image.as_str() {
        return public_url(value);
    }
    ["urlDefault", "urlSizeLarge", "url", "imageUrl"]
        .into_iter()
        .find_map(|key| string_field(image, &[key]).and_then(|url| public_url(&url)))
        .or_else(|| {
            image
                .get("infoList")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .find_map(|info| string_field(info, &["url"]).and_then(|url| public_url(&url)))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolver::ResolvedContent;

    const NOTE_ID: &str = "67fa70820000000009021c5b";

    fn page(state: Value) -> FetchedPage {
        FetchedPage {
            url: Url::parse(&format!(
                "https://www.xiaohongshu.com/explore/{NOTE_ID}?xsec_token=fixture"
            ))
            .unwrap(),
            html: format!("<script>window.__INITIAL_STATE__={state}</script>"),
        }
    }

    fn note(kind: &str) -> Value {
        serde_json::json!({
            "noteId": NOTE_ID, "title": "笔记标题", "desc": "笔记正文", "type": kind,
            "time": 1700000000123_i64,
            "user": {"nickname": "作者", "avatar": "https://sns.example/avatar.jpg"},
            "imageList": [
                {"urlDefault": "https://sns.example/cover.jpg", "urlSizeLarge": "https://sns.example/cover-large.jpg"},
                {"infoList": [{"url": "https://sns.example/second.jpg"}]}
            ],
            "video": {"masterUrl": "https://sns.example/video.mp4"},
            "unrelated": {"url": "https://sns.example/unrelated.jpg"}
        })
    }

    #[test]
    fn video_uses_bound_note_metadata_and_keeps_covers_out_of_gallery() {
        let media = map_page(
            &page(serde_json::json!({"note": note("video")})),
            Some(NOTE_ID),
        )
        .unwrap();
        assert_eq!(media.author.as_deref(), Some("作者"));
        assert_eq!(
            media.avatar_url.as_deref(),
            Some("https://sns.example/avatar.jpg")
        );
        assert_eq!(media.published_at, Some(1_700_000_000));
        assert_eq!(
            media.video_url.as_deref(),
            Some("https://sns.example/video.mp4")
        );
        assert!(media.images.is_empty());
        assert!(media.card().metadata().poster.is_some());
        assert!(!media.card().is_gallery());
        assert!(media.source_url.contains("xsec_token=fixture"));
    }

    #[test]
    fn originals_and_backups_reach_the_download_candidate() {
        let mut value = note("video");
        value["video"] = serde_json::json!({
            "consumer": {"originVideoKey": "pre_post/1040g2t_video_key"},
            "media": {"stream": {"h264": [{
                "masterUrl": "https://sns-video.example/compatible.mp4",
                "backupUrls": ["https://sns-video.example/backup.mp4"],
                "width": 1080, "height": 1920
            }]}}
        });
        let expected = video::candidates(Some(&value["video"]));
        let media = map_page(&page(serde_json::json!({"note": value})), Some(NOTE_ID)).unwrap();
        let candidate = media.download_candidate(0).unwrap();
        assert_eq!(candidate.url, expected[0]);
        assert_eq!(candidate.fallback_urls, expected[1..]);
        assert_eq!(candidate.referer, "https://www.xiaohongshu.com/");
    }

    #[test]
    fn normal_note_does_not_download_nested_soundtrack_or_og_video() {
        let mut input = page(serde_json::json!({"note": note("normal")}));
        input
            .html
            .push_str(r#"<meta property="og:video" content="https://sns.example/stale.mp4">"#);
        let media = map_page(&input, Some(NOTE_ID)).unwrap();
        assert_eq!(
            media.images,
            [
                "https://sns.example/cover.jpg",
                "https://sns.example/second.jpg"
            ]
        );
        assert!(media.video_url.is_none());
        assert!(media.video_fallback_urls.is_empty());
        assert!(!media.is_video);
        assert!(media.download_candidate(0).is_none());
        assert!(media.card().is_gallery());
    }

    #[test]
    fn h5_metadata_and_images_are_bound_to_the_requested_note() {
        let input = page(serde_json::json!({
            "recommendations": {"note": note("video")},
            "noteData": {"data": {"noteData": {
                "noteId": NOTE_ID, "title": "移动端笔记", "type": "normal",
                "user": {"nickName": "移动端作者", "avatar": "//sns.example/h5-avatar.jpg"},
                "imageList": [{"url": "//sns.example/h5-image.jpg"}]
            }}}
        }));
        let media = map_page(&input, Some(NOTE_ID)).unwrap();
        assert_eq!(media.title.as_deref(), Some("移动端笔记"));
        assert_eq!(media.author.as_deref(), Some("移动端作者"));
        assert_eq!(media.images, ["https://sns.example/h5-image.jpg"]);
        assert_eq!(
            media.avatar_url.as_deref(),
            Some("https://sns.example/h5-avatar.jpg")
        );
        assert!(media.video_url.is_none());
    }

    #[test]
    fn video_with_only_a_cover_still_produces_a_poster() {
        let mut value = note("video");
        value["video"] = Value::Null;
        let media = map_page(&page(serde_json::json!({"note": value})), Some(NOTE_ID)).unwrap();
        assert!(media.is_video);
        assert!(media.download_candidate(0).is_none());
        assert!(media.card().metadata().poster.is_some());
    }

    #[test]
    fn incomplete_or_private_media_is_not_reported_as_a_successful_note() {
        let mut value = note("normal");
        value["imageList"] = serde_json::json!([
            {"url": "file:///private/image.png"}, {"url": "http://127.0.0.1/private.png"}
        ]);
        let input = page(serde_json::json!({"note": value}));
        assert!(map_page(&input, Some(NOTE_ID)).is_err());
        let mut input = page(serde_json::json!({"recommendations": {"note": note("normal")}}));
        input.html = r#"<meta property="og:title" content="登录后查看">"#.into();
        assert!(map_page(&input, Some(NOTE_ID)).is_err());
    }

    #[test]
    fn a_permalink_cannot_redirect_to_a_different_note() {
        let input = page(serde_json::json!({"note": note("normal")}));
        assert!(map_page(&input, Some("67fa70820000000009021c5c")).is_err());
        let mut input = input;
        input.url = Url::parse("https://www.xiaohongshu.com/login").unwrap();
        assert!(map_page(&input, Some(NOTE_ID)).is_err());
    }
}
