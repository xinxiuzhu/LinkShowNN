//! Douyin share-link resolution.
//!
//! Public gallery and mobile feed endpoints work without a login session.
//! The web detail endpoint remains a fallback; an anonymous `ttwid` alone does
//! not guarantee access to that endpoint.

use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail, ensure};
use regex::Regex;
use reqwest::{Client, header};
use serde_json::{Value, json};
use url::Url;

use crate::resolver::{MediaInfo, Platform};

const DETAIL_API: &str = "https://www.douyin.com/aweme/v1/web/aweme/detail/";
const SLIDES_API: &str = "https://www.iesdouyin.com/web/api/v2/aweme/slidesinfo/";
const MAX_SLIDES_BODY_BYTES: usize = 4 * 1024 * 1024;
const TTWID_REGISTER_URL: &str = "https://ttwid.bytedance.com/ttwid/union/register/";
const WEB_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
     (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36 Edg/130.0.0.0";
const SLIDES_USER_AGENT: &str = "Mozilla/5.0 (Linux; Android 15; SM-G998B) AppleWebKit/537.36 \
     (KHTML, like Gecko) Chrome/132.0.0.0 Mobile Safari/537.36 Edg/132.0.0.0";

/// Identifies a Douyin post from an already-expanded share URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AwemeRef {
    pub id: String,
    pub is_note: bool,
}

pub fn parse_aweme_ref(url: &Url) -> Option<AwemeRef> {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let regex = RE.get_or_init(|| {
        Regex::new(r"(?i)^(?:www\.)?(?:ies)?douyin\.com$").expect("valid douyin host regex")
    });
    let host = url.host_str()?.trim_end_matches('.').to_ascii_lowercase();
    if !regex.is_match(&host) {
        return None;
    }
    let segments: Vec<&str> = url
        .path_segments()?
        .filter(|part| !part.is_empty())
        .collect();
    let position = segments
        .iter()
        .position(|part| matches!(*part, "video" | "note" | "slides"))?;
    let kind = segments[position];
    let id = segments.get(position + 1)?;
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(AwemeRef {
        id: (*id).to_string(),
        is_note: matches!(kind, "note" | "slides"),
    })
}

/// Caches the anonymous web id so each resolution does not re-register one.
#[derive(Clone)]
pub struct DouyinClient {
    http: Client,
    cookie: String,
    ttwid: Arc<Mutex<Option<String>>>,
    quality: &'static str,
}

impl DouyinClient {
    pub fn new(http: Client, cookie: &str) -> Self {
        let cookie = cookie.trim().to_string();
        let seeded = cookie_value(&cookie, "ttwid");
        Self {
            http,
            cookie,
            ttwid: Arc::new(Mutex::new(seeded)),
            quality: "1080P",
        }
    }

    /// Preferred source resolution; the platform may return a different size.
    pub fn with_quality(mut self, quality: &str) -> Self {
        self.quality = super::playback::quality_label(quality);
        self
    }

    pub(super) fn quality(&self) -> &'static str {
        self.quality
    }

    fn cached_ttwid(&self) -> Option<String> {
        self.ttwid.lock().ok().and_then(|value| value.clone())
    }

    fn store_ttwid(&self, value: &str) {
        if let Ok(mut slot) = self.ttwid.lock() {
            *slot = Some(value.to_string());
        }
    }

    async fn register_ttwid(&self) -> Result<String> {
        let url = Url::parse(TTWID_REGISTER_URL)?;
        crate::resolver::url_policy::validate_public_url(&url)?;
        let payload = json!({
            "region": "cn",
            "aid": 1768,
            "needFid": false,
            "service": "www.ixigua.com",
            "migrate_info": {"ticket": "", "source": "node"},
            "cbUrlProtocol": "https",
            "union": true,
        });
        let response = self
            .http
            .post(url)
            .header(header::USER_AGENT, WEB_USER_AGENT)
            .json(&payload)
            .send()
            .await?;
        if !response.status().is_success() {
            bail!("ttwid register returned HTTP {}", response.status());
        }
        let ttwid = response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .find_map(|value| cookie_value(value, "ttwid"))
            .context("ttwid register response has no ttwid cookie")?;
        self.store_ttwid(&ttwid);
        Ok(ttwid)
    }

    /// Builds the detail request cookie, replacing any stale `ttwid` in config.
    fn detail_cookie(&self, ttwid: &str) -> String {
        let mut parts: Vec<String> = self
            .cookie
            .split(';')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .filter(|part| {
                !part
                    .split_once('=')
                    .map(|(name, _)| name.trim().eq_ignore_ascii_case("ttwid"))
                    .unwrap_or(false)
            })
            .map(str::to_string)
            .collect();
        parts.push(format!("ttwid={ttwid}"));
        parts.join("; ")
    }

    async fn fetch_detail(&self, aweme: &AwemeRef, ttwid: &str) -> Result<Value> {
        let mut url = Url::parse(DETAIL_API)?;
        url.query_pairs_mut()
            .append_pair("device_platform", "webapp")
            .append_pair("aid", "6383")
            .append_pair("channel", "channel_pc_web")
            .append_pair("aweme_id", &aweme.id);
        crate::resolver::url_policy::validate_platform_url(&url, Platform::Douyin)?;
        let response = self
            .http
            .get(url)
            .header(header::USER_AGENT, WEB_USER_AGENT)
            .header(header::ACCEPT, "application/json, text/plain, */*")
            .header(header::ACCEPT_LANGUAGE, "zh-CN,zh;q=0.9,en;q=0.8")
            .header(header::REFERER, "https://www.douyin.com/")
            .header(header::COOKIE, self.detail_cookie(ttwid))
            .send()
            .await?;
        if !response.status().is_success() {
            bail!("douyin detail API returned HTTP {}", response.status());
        }
        let body = response.bytes().await?;
        if body.is_empty() {
            bail!("douyin detail API returned an empty body");
        }
        Ok(serde_json::from_slice(&body)?)
    }

    /// The public gallery endpoint accepts an anonymous mobile request; do not
    /// attach the configured login Cookie or a registered web visitor id.
    pub async fn resolve_slides(&self, aweme: &AwemeRef, source_url: &Url) -> Result<MediaInfo> {
        ensure!(
            aweme.is_note,
            "douyin slides API requires a gallery reference"
        );
        let mut url = Url::parse(SLIDES_API)?;
        url.query_pairs_mut()
            .append_pair("aweme_ids", &format!("[{}]", aweme.id))
            .append_pair("request_source", "200");
        crate::resolver::url_policy::validate_platform_url(&url, Platform::Douyin)?;
        let mut response = self
            .http
            .get(url)
            .header(header::USER_AGENT, SLIDES_USER_AGENT)
            .send()
            .await?;
        ensure!(
            response.status().is_success(),
            "douyin slides API returned HTTP {}",
            response.status()
        );
        ensure!(
            response
                .content_length()
                .is_none_or(|length| length <= MAX_SLIDES_BODY_BYTES as u64),
            "douyin slides API response exceeds size limit"
        );
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            ensure!(
                chunk.len() <= MAX_SLIDES_BODY_BYTES.saturating_sub(body.len()),
                "douyin slides API response exceeds size limit"
            );
            body.extend_from_slice(&chunk);
        }
        ensure!(!body.is_empty(), "douyin slides API returned an empty body");
        let payload: Value = serde_json::from_slice(&body)?;
        map_slides_payload(&payload, aweme, source_url)
    }

    pub async fn resolve_video(&self, aweme: &AwemeRef, source_url: &Url) -> Result<MediaInfo> {
        super::video::resolve(&self.http, aweme, source_url, self.quality).await
    }

    /// Resolves a post, retrying once with a fresh `ttwid` when the cached one
    /// has been invalidated.
    pub async fn resolve(&self, aweme: &AwemeRef, source_url: &Url) -> Result<MediaInfo> {
        let mut last_error = None;
        for attempt in 0..2 {
            let ttwid = match (attempt, self.cached_ttwid()) {
                (0, Some(cached)) => cached,
                _ => self.register_ttwid().await?,
            };
            match self.fetch_detail(aweme, &ttwid).await {
                Ok(payload) => match payload.get("aweme_detail") {
                    Some(detail) if detail.is_object() => {
                        return map_complete_detail_with_quality(
                            detail,
                            aweme,
                            source_url,
                            self.quality,
                        );
                    }
                    _ => {
                        if let Some(reason) = filter_reason(&payload) {
                            bail!("douyin content unavailable: {reason}");
                        }
                        last_error = Some(anyhow::anyhow!("douyin detail API returned no content"));
                    }
                },
                Err(error) => last_error = Some(error),
            }
        }
        Err(last_error.unwrap_or_else(|| anyhow::anyhow!("douyin resolution failed")))
    }
}

fn map_slides_payload(payload: &Value, expected: &AwemeRef, source_url: &Url) -> Result<MediaInfo> {
    ensure!(
        expected.is_note,
        "douyin slides API requires a gallery reference"
    );
    ensure!(
        payload.get("status_code").and_then(Value::as_u64) == Some(0),
        "douyin slides API returned an unsuccessful status"
    );
    if let Some(filters) = payload.get("filter_list").filter(|value| !value.is_null()) {
        ensure!(
            filters.as_array().is_some_and(Vec::is_empty),
            "douyin slides API filtered the requested content"
        );
    }
    let detail = ["item_list", "aweme_details"]
        .into_iter()
        .filter_map(|key| payload.get(key).and_then(Value::as_array))
        .flatten()
        .find(|detail| detail.get("aweme_id").and_then(Value::as_str) == Some(expected.id.as_str()))
        .context("douyin slides API returned no matching gallery")?;
    let media = map_complete_detail(detail, expected, source_url)?;
    ensure!(
        !media.images.is_empty()
            && !media.is_video
            && media.video_url.is_none()
            && media.audio_url.is_none(),
        "douyin slides API returned no gallery images"
    );
    Ok(media)
}

/// API and inline state must each contain their own complete work metadata.
/// Generic page titles/images must never fill missing fields in a partial API
/// result and accidentally suppress the anonymous desktop fallback.
pub(super) fn map_complete_detail(
    detail: &Value,
    expected: &AwemeRef,
    source_url: &Url,
) -> Result<MediaInfo> {
    map_complete_detail_with_quality(detail, expected, source_url, "1080P")
}

pub(super) fn map_complete_detail_with_quality(
    detail: &Value,
    expected: &AwemeRef,
    source_url: &Url,
    quality: &str,
) -> Result<MediaInfo> {
    ensure!(
        detail.get("aweme_id").and_then(Value::as_str) == Some(expected.id.as_str()),
        "douyin detail returned a different or missing work id"
    );
    let media = map_detail_with_quality(detail, source_url, quality);
    ensure!(
        media.title.is_some() && media.author.is_some(),
        "douyin detail has incomplete work text or author"
    );
    ensure!(
        media.cover_url.as_deref().is_some_and(valid_media_url),
        "douyin detail has no valid work cover"
    );
    if media.images.is_empty() {
        ensure!(
            media.is_video && !expected.is_note,
            "douyin detail has no work media"
        );
        ensure!(
            media.avatar_url.as_deref().is_some_and(valid_media_url)
                && media.avatar_url != media.cover_url,
            "douyin video detail has no valid author avatar"
        );
    } else {
        // Gallery metadata is useful without an avatar. Keep every returned
        // image and do not reinterpret its optional soundtrack as a video.
        ensure!(
            media.images.iter().all(|url| valid_media_url(url)),
            "douyin gallery detail has an invalid image URL"
        );
    }
    Ok(media)
}

fn valid_media_url(value: &str) -> bool {
    Url::parse(value)
        .is_ok_and(|url| crate::resolver::url_policy::validate_public_url(&url).is_ok())
}

fn filter_reason(payload: &Value) -> Option<String> {
    let entry = payload.get("filter_detail").or_else(|| {
        payload
            .get("filter_list")
            .and_then(Value::as_array)
            .and_then(|list| list.first())
    })?;
    entry
        .get("detail_msg")
        .or_else(|| entry.get("filter_reason"))
        .or_else(|| entry.get("reason"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn first_url(value: Option<&Value>) -> Option<String> {
    value?
        .get("url_list")?
        .as_array()?
        .iter()
        .filter_map(Value::as_str)
        .find(|candidate| candidate.starts_with("http"))
        .map(str::to_string)
}

/// Keep a caption's first line as the poster heading and its remaining lines as
/// body text. This also preserves the complete caption in the text-only card.
pub(super) fn split_caption(caption: &str) -> (Option<String>, Option<String>) {
    let caption = caption.trim();
    if caption.is_empty() {
        return (None, None);
    }
    let (title, description) = caption.split_once('\n').unwrap_or((caption, ""));
    (
        Some(title.trim().to_owned()),
        (!description.trim().is_empty()).then(|| description.trim().to_owned()),
    )
}

/// Maps `aweme_detail` into the resolver's stable media model.
#[cfg(test)]
pub fn map_detail(detail: &Value, source_url: &Url) -> MediaInfo {
    map_detail_with_quality(detail, source_url, "1080P")
}

pub(super) fn map_detail_with_quality(
    detail: &Value,
    source_url: &Url,
    quality: &str,
) -> MediaInfo {
    let video = detail.get("video");
    let author = detail.get("author");
    let images: Vec<String> = detail
        .get("images")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|item| {
                    item.get("url_list")?
                        .as_array()?
                        .iter()
                        .filter_map(Value::as_str)
                        .rfind(|candidate| candidate.starts_with("http"))
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default();

    // Slide posts expose an audio-only `play_addr`; treating it as the video
    // would download a soundtrack instead of the post.
    let selection = super::playback::candidates_with_quality(
        if images.is_empty() { video } else { None },
        quality,
    );
    let mut video_urls = selection.urls.into_iter();
    let video_url = video_urls.next();
    let video_fallback_urls = video_urls.collect();

    let cover_url = first_url(video.and_then(|value| value.get("origin_cover")))
        .or_else(|| first_url(video.and_then(|value| value.get("cover"))))
        .or_else(|| first_url(video.and_then(|value| value.get("dynamic_cover"))))
        .or_else(|| images.first().cloned());
    let is_video = images.is_empty() && video.is_some_and(Value::is_object);
    let (title, description) = split_caption(
        detail
            .get("desc")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    );

    MediaInfo {
        title,
        description,
        cover_url,
        video_url,
        video_fallback_urls,
        video_quality: selection.video_quality,
        quality_notice: selection.quality_notice,
        audio_url: None,
        images,
        author: author
            .and_then(|author| author.get("nickname"))
            .and_then(Value::as_str)
            .filter(|text| !text.trim().is_empty())
            .map(str::to_string),
        avatar_url: ["avatar_medium", "avatar_thumb", "avatar_larger"]
            .into_iter()
            .find_map(|key| first_url(author.and_then(|author| author.get(key)))),
        published_at: detail
            .get("create_time")
            .and_then(Value::as_i64)
            .filter(|time| *time > 0),
        is_video,
        // Aweme 的 video.duration 和顶层 duration 均为毫秒；不读取 music.duration。
        video_duration_seconds: is_video
            .then(|| {
                video
                    .and_then(|video| video.get("duration"))
                    .and_then(Value::as_u64)
                    .filter(|duration| *duration > 0)
                    .or_else(|| {
                        detail
                            .get("duration")
                            .and_then(Value::as_u64)
                            .filter(|duration| *duration > 0)
                    })
                    .map(|milliseconds| milliseconds.div_ceil(1000))
            })
            .flatten(),
        source_url: source_url.to_string(),
        platform: Platform::Douyin,
        raw: json!({"platform": "douyin", "aweme_id": detail.get("aweme_id"), "statistics": {
            "digg_count": detail.pointer("/statistics/digg_count").and_then(Value::as_u64),
            "comment_count": detail.pointer("/statistics/comment_count").and_then(Value::as_u64),
            "collect_count": detail.pointer("/statistics/collect_count").and_then(Value::as_u64),
            "share_count": detail.pointer("/statistics/share_count").and_then(Value::as_u64),
        }}),
    }
}

fn cookie_value(cookie: &str, name: &str) -> Option<String> {
    cookie
        .split(';')
        .map(str::trim)
        .filter_map(|part| part.split_once('='))
        .find(|(key, _)| key.trim().eq_ignore_ascii_case(name))
        .map(|(_, value)| value.trim().to_string())
        .filter(|value| !value.is_empty())
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolver::content::ResolvedContent;

    #[test]
    fn aweme_video_milliseconds_limit_media_without_using_soundtrack_duration() {
        let source = Url::parse("https://www.douyin.com/video/123").unwrap();
        for detail in [
            json!({"video": {"duration": 480001, "play_addr": {"uri": "v_actual"}}}),
            json!({"duration": 480001, "video": {"play_addr": {"uri": "v_actual"}}}),
        ] {
            let media = map_detail(&detail, &source);
            assert_eq!(media.video_duration_seconds, Some(481));
            assert!(media.download_candidate(480).is_none());
            assert_eq!(
                media.skip_reason(480),
                Some(crate::download::SkipReason::DurationExceeded {
                    duration: 481,
                    maximum: 480,
                })
            );
            assert!(media.download_candidate(481).is_some());
            assert!(media.skip_reason(481).is_none());
        }
        let unknown = map_detail(
            &json!({"video": {"play_addr": {"uri": "v_actual"}}, "music": {"duration": 900000}}),
            &source,
        );
        assert!(unknown.video_duration_seconds.is_none());
        assert!(unknown.download_candidate(1).is_some());
        let gallery = map_detail(
            &json!({
                "images": [{"url_list": ["https://p3.douyinpic.com/image.jpeg"]}],
                "video": {"duration": 900000, "play_addr": {"uri": "v_soundtrack"}},
                "duration": 900000
            }),
            &source,
        );
        assert!(gallery.video_duration_seconds.is_none());
        assert!(gallery.download_candidate(480).is_none());
        assert!(gallery.skip_reason(480).is_none());
    }

    #[test]
    fn client_and_detail_mapper_apply_the_preferred_quality() {
        let client = DouyinClient::new(Client::new(), "").with_quality("480p");
        assert_eq!(client.quality(), "480P");
        let source = Url::parse("https://www.douyin.com/video/123").unwrap();
        let media = map_detail_with_quality(
            &json!({
                "aweme_id": "123",
                "video": {"play_addr": {"uri": "v_actual", "width": 576, "height": 1024}}
            }),
            &source,
            client.quality(),
        );
        assert!(media.video_url.unwrap().contains("ratio=480p"));
        assert_eq!(media.video_quality.as_deref(), Some("请求480P"));
        assert!(media.quality_notice.is_some());

        let gallery = map_detail_with_quality(
            &json!({
                "images": [{"url_list": ["https://p3.douyinpic.com/image.jpeg"]}],
                "video": {"play_addr": {"uri": "v_soundtrack"}}
            }),
            &source,
            "360P",
        );
        assert!(gallery.video_url.is_none());
        assert!(gallery.video_quality.is_none());
        assert!(gallery.quality_notice.is_none());
    }

    #[test]
    fn caption_keeps_body_and_hashtags_out_of_the_poster_heading() {
        let caption = "共鸣者档案前瞻 | 莉莉\n共鸣属性：衍射\n武器类型：佩枪\n#鸣潮 #鸣潮莉莉";
        let media = map_detail(
            &json!({"desc": caption}),
            &Url::parse("https://www.douyin.com/video/123").unwrap(),
        );
        assert_eq!(media.title.as_deref(), Some("共鸣者档案前瞻 | 莉莉"));
        assert_eq!(
            media.description.as_deref(),
            Some("共鸣属性：衍射\n武器类型：佩枪\n#鸣潮 #鸣潮莉莉")
        );
        assert!(media.card().text().contains(caption));
        assert_eq!(
            split_caption("  单行标题  "),
            (Some("单行标题".into()), None)
        );
        assert_eq!(split_caption(" \n "), (None, None));
    }

    #[test]
    fn incomplete_video_details_do_not_suppress_the_anonymous_fallback() {
        let source = Url::parse("https://www.douyin.com/video/123").unwrap();
        let expected = parse_aweme_ref(&source).unwrap();
        let complete = json!({
            "aweme_id": "123",
            "desc": "完整作品文案",
            "author": {
                "nickname": "作者",
                "avatar_thumb": {"url_list": ["https://p3.douyinpic.com/avatar.jpeg"]}
            },
            "video": {"origin_cover": {"url_list": ["https://p3.douyinpic.com/cover.jpeg"]}}
        });
        assert!(map_complete_detail(&complete, &expected, &source).is_ok());
        assert!(map_complete_detail(&json!({"aweme_id": "123"}), &expected, &source).is_err());
        for field in ["aweme_id", "desc", "author", "video"] {
            let mut partial = complete.clone();
            partial.as_object_mut().unwrap().remove(field);
            assert!(
                map_complete_detail(&partial, &expected, &source).is_err(),
                "missing {field}"
            );
        }
        let mut partial = complete.clone();
        partial["author"]
            .as_object_mut()
            .unwrap()
            .remove("avatar_thumb");
        assert!(map_complete_detail(&partial, &expected, &source).is_err());
        let mut partial = complete;
        partial["video"]["origin_cover"]["url_list"] =
            json!(["https://user:secret@cdn.example/cover.jpeg"]);
        assert!(map_complete_detail(&partial, &expected, &source).is_err());
    }

    #[test]
    fn complete_gallery_keeps_images_without_requiring_a_video_avatar() {
        let source = Url::parse("https://www.douyin.com/note/123").unwrap();
        let expected = parse_aweme_ref(&source).unwrap();
        let media = map_complete_detail(
            &json!({
                "aweme_id": "123",
                "desc": "图集文案",
                "author": {"nickname": "作者"},
                "images": [
                    {"url_list": ["https://p3.douyinpic.com/first.jpeg"]},
                    {"url_list": ["https://p3.douyinpic.com/second.jpeg"]}
                ],
                "video": {"play_addr": {"url_list": ["https://v26.douyinvod.com/soundtrack.mp3"]}}
            }),
            &expected,
            &source,
        )
        .unwrap();
        assert_eq!(media.images.len(), 2);
        assert!(!media.is_video);
        assert!(media.video_url.is_none());
        assert!(media.avatar_url.is_none());
    }

    fn slides_detail(id: &str) -> Value {
        json!({
            "aweme_id": id,
            "desc": "公开图集\n保留完整文案",
            "author": {"nickname": "公开作者"},
            "images": [
                {"url_list": ["https://p3.douyinpic.com/first.jpeg"]},
                {"url_list": ["https://p3.douyinpic.com/second.jpeg"]}
            ],
            "video": {"play_addr": {"url_list": ["https://v26.douyinvod.com/soundtrack.mp3"]}},
            "statistics": {"digg_count": 42, "comment_count": 7}
        })
    }

    #[test]
    fn slides_response_selects_the_requested_work_from_either_response_list() {
        let source = Url::parse("https://www.douyin.com/note/123").unwrap();
        let expected = parse_aweme_ref(&source).unwrap();
        for key in ["item_list", "aweme_details"] {
            let mut payload = json!({"status_code": 0, "filter_list": []});
            payload[key] = json!([slides_detail("456"), slides_detail("123")]);
            let media = map_slides_payload(&payload, &expected, &source).unwrap();
            assert_eq!(media.raw["aweme_id"], "123");
            assert_eq!(media.title.as_deref(), Some("公开图集"));
            assert_eq!(media.description.as_deref(), Some("保留完整文案"));
            assert_eq!(media.author.as_deref(), Some("公开作者"));
            assert_eq!(media.images.len(), 2);
            assert_eq!(media.raw["statistics"]["digg_count"], 42);
            assert_eq!(media.raw["statistics"]["comment_count"], 7);
            assert!(!media.is_video);
            assert!(media.video_url.is_none() && media.audio_url.is_none());
        }
        let payload = json!({
            "status_code": 0,
            "item_list": [],
            "aweme_details": [slides_detail("123")]
        });
        assert!(map_slides_payload(&payload, &expected, &source).is_ok());
    }

    #[test]
    fn slides_response_rejects_unavailable_or_mismatched_content() {
        let source = Url::parse("https://www.douyin.com/note/123").unwrap();
        let expected = parse_aweme_ref(&source).unwrap();
        for payload in [
            json!({}),
            json!({"status_code": 0}),
            json!({"status_code": 0, "aweme_details": []}),
            json!({"status_code": 0, "aweme_details": [slides_detail("456")]}),
            json!({"status_code": 1, "aweme_details": [slides_detail("123")]}),
            json!({"status_code": "0", "aweme_details": [slides_detail("123")]}),
            json!({"status_code": 0, "filter_list": [{"aweme_id": "123"}], "aweme_details": [slides_detail("123")]}),
            json!({"status_code": 0, "filter_list": false, "aweme_details": [slides_detail("123")]}),
        ] {
            assert!(map_slides_payload(&payload, &expected, &source).is_err());
        }
    }

    #[test]
    fn slides_response_preserves_metadata_and_media_validation() {
        let source = Url::parse("https://www.douyin.com/note/123").unwrap();
        let expected = parse_aweme_ref(&source).unwrap();
        for field in ["author", "desc", "images"] {
            let mut detail = slides_detail("123");
            detail.as_object_mut().unwrap().remove(field);
            let payload = json!({"status_code": 0, "aweme_details": [detail]});
            assert!(map_slides_payload(&payload, &expected, &source).is_err());
        }
        let mut detail = slides_detail("123");
        detail["images"][0]["url_list"] = json!(["http://127.0.0.1/private.jpeg"]);
        let payload = json!({"status_code": 0, "aweme_details": [detail]});
        assert!(map_slides_payload(&payload, &expected, &source).is_err());
        let video = AwemeRef {
            id: "123".into(),
            is_note: false,
        };
        let payload = json!({"status_code": 0, "aweme_details": [slides_detail("123")]});
        assert!(map_slides_payload(&payload, &video, &source).is_err());
    }

    #[test]
    fn poster_prefers_static_original_cover_over_animated_preview() {
        let media = map_detail(
            &json!({
                "video": {
                    "origin_cover": {"url_list": ["https://p3.douyinpic.com/original.jpg"]},
                    "cover": {"url_list": ["https://p3.douyinpic.com/cover.jpg"]},
                    "dynamic_cover": {"url_list": ["https://p3.douyinpic.com/preview.webp"]}
                }
            }),
            &Url::parse("https://www.douyin.com/video/123").unwrap(),
        );
        assert_eq!(
            media.cover_url.as_deref(),
            Some("https://p3.douyinpic.com/original.jpg")
        );
    }

    #[test]
    fn parses_video_and_note_references() {
        let video = parse_aweme_ref(
            &Url::parse("https://www.iesdouyin.com/share/video/7676025808796152186/?region=CN")
                .unwrap(),
        )
        .unwrap();
        assert_eq!(video.id, "7676025808796152186");
        assert!(!video.is_note);

        let note = parse_aweme_ref(
            &Url::parse("https://www.douyin.com/note/7123456789012345678").unwrap(),
        )
        .unwrap();
        assert!(note.is_note);

        assert!(
            parse_aweme_ref(&Url::parse("https://douyin.com.evil.test/video/1").unwrap()).is_none()
        );
        assert!(
            parse_aweme_ref(&Url::parse("https://www.douyin.com/video/abc").unwrap()).is_none()
        );
    }

    #[test]
    fn maps_video_detail_into_media() {
        let detail = json!({
            "desc": "  测试标题  ",
            "author": {
                "nickname": "作者",
                "avatar_thumb": {"url_list": ["https://p3.douyinpic.com/avatar.jpeg"]}
            },
            "create_time": 1700000000,
            "video": {
                "play_addr": {"url_list": ["https://v26-web.douyinvod.com/playwm/a.mp4"]},
                "dynamic_cover": {"url_list": ["https://p3.douyinpic.com/cover.jpeg"]}
            }
        });
        let media = map_detail(
            &detail,
            &Url::parse("https://www.douyin.com/video/7676025808796152186").unwrap(),
        );
        assert_eq!(media.title.as_deref(), Some("测试标题"));
        assert_eq!(media.author.as_deref(), Some("作者"));
        assert_eq!(
            media.avatar_url.as_deref(),
            Some("https://p3.douyinpic.com/avatar.jpeg")
        );
        assert_eq!(media.published_at, Some(1_700_000_000));
        assert!(media.is_video);
        assert_eq!(
            media.video_url.as_deref(),
            Some("https://v26-web.douyinvod.com/playwm/a.mp4")
        );
        assert_eq!(
            media.cover_url.as_deref(),
            Some("https://p3.douyinpic.com/cover.jpeg")
        );
        assert!(media.images.is_empty());
    }

    #[test]
    fn slide_posts_use_images_and_drop_audio_play_addr() {
        let detail = json!({
            "desc": "图文",
            "images": [
                {"url_list": ["https://p3.douyinpic.com/small-1.jpeg", "https://p3.douyinpic.com/large-1.jpeg"]},
                {"url_list": ["https://p3.douyinpic.com/large-2.jpeg"]}
            ],
            "video": {"play_addr": {"url_list": ["https://v26-web.douyinvod.com/audio.mp3"]}}
        });
        let media = map_detail(
            &detail,
            &Url::parse("https://www.douyin.com/note/7123456789012345678").unwrap(),
        );
        assert!(media.video_url.is_none());
        assert!(!media.is_video);
        assert_eq!(
            media.images,
            vec![
                "https://p3.douyinpic.com/large-1.jpeg".to_string(),
                "https://p3.douyinpic.com/large-2.jpeg".to_string(),
            ]
        );
        assert_eq!(
            media.cover_url.as_deref(),
            Some("https://p3.douyinpic.com/large-1.jpeg")
        );
    }

    #[test]
    fn detail_cookie_replaces_stale_ttwid() {
        let client = DouyinClient::new(Client::new(), "sessionid=abc; ttwid=old");
        assert_eq!(client.cached_ttwid().as_deref(), Some("old"));
        let cookie = client.detail_cookie("fresh");
        assert_eq!(cookie, "sessionid=abc; ttwid=fresh");
    }

    #[test]
    fn reports_filtered_content_reason() {
        let payload = json!({"filter_detail": {"detail_msg": "作品不见了"}});
        assert_eq!(filter_reason(&payload).as_deref(), Some("作品不见了"));
        assert!(filter_reason(&json!({"aweme_detail": {}})).is_none());
    }
}
