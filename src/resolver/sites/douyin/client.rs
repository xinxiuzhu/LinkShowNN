//! Douyin share-link resolution.
//!
//! The web detail API remains useful with a working web session. Anonymous
//! `ttwid` registration alone does not guarantee access: current endpoints may
//! also require visitor signatures. The adapter has a separate public-page path.

use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail, ensure};
use regex::Regex;
use reqwest::{Client, header};
use serde_json::{Value, json};
use url::Url;

use crate::resolver::{MediaInfo, Platform};

const DETAIL_API: &str = "https://www.douyin.com/aweme/v1/web/aweme/detail/";
const TTWID_REGISTER_URL: &str = "https://ttwid.bytedance.com/ttwid/union/register/";
const WEB_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
     (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36 Edg/130.0.0.0";

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
}

impl DouyinClient {
    pub fn new(http: Client, cookie: &str) -> Self {
        let cookie = cookie.trim().to_string();
        let seeded = cookie_value(&cookie, "ttwid");
        Self {
            http,
            cookie,
            ttwid: Arc::new(Mutex::new(seeded)),
        }
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
                        return map_complete_detail(detail, aweme, source_url);
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

/// API and inline state must each contain their own complete work metadata.
/// Generic page titles/images must never fill missing fields in a partial API
/// result and accidentally suppress the anonymous desktop fallback.
pub(super) fn map_complete_detail(
    detail: &Value,
    expected: &AwemeRef,
    source_url: &Url,
) -> Result<MediaInfo> {
    ensure!(
        detail.get("aweme_id").and_then(Value::as_str) == Some(expected.id.as_str()),
        "douyin detail returned a different or missing work id"
    );
    let media = map_detail(detail, source_url);
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
pub fn map_detail(detail: &Value, source_url: &Url) -> MediaInfo {
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
    let video_url = if images.is_empty() {
        first_url(video.and_then(|value| value.get("play_addr")))
            .map(|url| url.replace("playwm", "play"))
    } else {
        None
    };

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
            Some("https://v26-web.douyinvod.com/play/a.mp4")
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
