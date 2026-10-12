//! 匿名视频通过移动端 Feed 接口获取，不发送登录 Cookie 或 Web 签名。
//!
//! 请求形态参考 ucmao/media-parser 的 `_request_mobile_feed`；只接受与
//! 请求作品 ID 一致的条目，接口推荐的其他作品不能成为解析结果。

use std::time::Duration;

use anyhow::{Context, Result, ensure};
use reqwest::{Client, header};
use serde_json::Value;
use url::Url;

use super::client::{AwemeRef, map_complete_detail_with_quality};
use crate::resolver::{MediaInfo, Platform, url_policy};

const FEED_ENDPOINTS: [&str; 2] = [
    "https://api5-normal-c-hl.amemv.com/aweme/v1/feed/",
    "https://aweme.snssdk.com/aweme/v1/feed/",
];
const FEED_USER_AGENT: &str = "com.ss.android.ugc.aweme/290101 (Linux; U; Android 10; \
    zh_CN; Pixel 4; Build/QQ3A.200805.001; Cronet/TTNetVersion:5f9037be 2023-01-13 \
    QuicVersion:4668bb42 2022-11-21)";
const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(8);
const NODE_TIMEOUT: Duration = Duration::from_secs(10);
const TOTAL_TIMEOUT: Duration = Duration::from_secs(20);

/// Uses the manager's client, which has no Cookie store and does not follow
/// redirects. A feed failure leaves the existing web/share-page fallback usable.
pub(super) async fn resolve(
    http: &Client,
    aweme: &AwemeRef,
    source: &Url,
    quality: &str,
) -> Result<MediaInfo> {
    ensure!(
        !aweme.is_note,
        "douyin mobile feed requires a video reference"
    );
    ensure!(
        !aweme.id.is_empty() && aweme.id.bytes().all(|byte| byte.is_ascii_digit()),
        "douyin mobile feed requires a numeric work id"
    );
    url_policy::validate_platform_url(source, Platform::Douyin)?;
    tokio::time::timeout(TOTAL_TIMEOUT, async {
        let mut last_error = None;
        for endpoint in FEED_ENDPOINTS {
            let attempt = tokio::time::timeout(
                NODE_TIMEOUT,
                fetch_node(http, endpoint, aweme, source, quality),
            )
            .await
            .context("douyin mobile feed node timed out")
            .and_then(|result| result);
            match attempt {
                Ok(media) => return Ok(media),
                Err(error) => last_error = Some(error),
            }
        }
        Err(last_error.context("douyin mobile feed returned no usable video")?)
    })
    .await
    .context("douyin mobile feed timed out")?
}

async fn fetch_node(
    http: &Client,
    endpoint: &str,
    aweme: &AwemeRef,
    source: &Url,
    quality: &str,
) -> Result<MediaInfo> {
    let mut url = Url::parse(endpoint)?;
    url.query_pairs_mut()
        .append_pair("aweme_id", &aweme.id)
        .append_pair("aid", "1128");
    // These are fixed API nodes, not user-controlled share-page hosts. Do not
    // widen the platform page-host allowlist to admit arbitrary amemv/snssdk URLs.
    url_policy::validate_public_url(&url)?;
    let mut response = http
        .get(url)
        .header(header::USER_AGENT, FEED_USER_AGENT)
        .header(header::ACCEPT, "application/json, text/plain, */*")
        .timeout(REQUEST_TIMEOUT)
        .send()
        .await?;
    ensure!(
        response.status().is_success(),
        "douyin mobile feed returned HTTP {}",
        response.status()
    );
    ensure!(
        response
            .content_length()
            .is_none_or(|length| length <= MAX_BODY_BYTES as u64),
        "douyin mobile feed response exceeds size limit"
    );
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        ensure!(
            chunk.len() <= MAX_BODY_BYTES.saturating_sub(body.len()),
            "douyin mobile feed response exceeds size limit"
        );
        body.extend_from_slice(&chunk);
    }
    ensure!(
        !body.is_empty(),
        "douyin mobile feed returned an empty body"
    );
    let payload: Value = serde_json::from_slice(&body)
        .context("douyin mobile feed returned an invalid JSON response")?;
    map_feed_payload_with_quality(&payload, aweme, source, quality)
}

#[cfg(test)]
fn map_feed_payload(payload: &Value, expected: &AwemeRef, source: &Url) -> Result<MediaInfo> {
    map_feed_payload_with_quality(payload, expected, source, "1080P")
}

fn map_feed_payload_with_quality(
    payload: &Value,
    expected: &AwemeRef,
    source: &Url,
    quality: &str,
) -> Result<MediaInfo> {
    ensure!(
        !expected.is_note,
        "douyin mobile feed requires a video reference"
    );
    ensure!(
        payload.get("status_code").and_then(Value::as_u64) == Some(0),
        "douyin mobile feed returned an unsuccessful status"
    );
    if let Some(filters) = payload.get("filter_list").filter(|value| !value.is_null()) {
        ensure!(
            filters.as_array().is_some_and(Vec::is_empty),
            "douyin mobile feed filtered the requested content"
        );
    }
    ensure!(
        payload.get("filter_detail").is_none_or(Value::is_null),
        "douyin mobile feed filtered the requested content"
    );
    let detail = payload
        .get("aweme_list")
        .and_then(Value::as_array)
        .and_then(|list| {
            list.iter().find(|detail| {
                detail.get("aweme_id").and_then(Value::as_str) == Some(expected.id.as_str())
            })
        })
        .context("douyin mobile feed returned no matching video")?;
    ensure!(
        detail.get("filter_detail").is_none_or(Value::is_null),
        "douyin mobile feed filtered the requested video"
    );
    let has_images = [
        "/images",
        "/image_list",
        "/image_infos",
        "/original_images",
        "/image_post_info/images",
        "/image_post_info/image_list",
    ]
    .into_iter()
    .any(|path| {
        detail
            .pointer(path)
            .and_then(Value::as_array)
            .is_some_and(|list| !list.is_empty())
    });
    ensure!(
        !has_images
            && !matches!(
                detail.get("aweme_type").and_then(Value::as_u64),
                Some(2 | 68)
            ),
        "douyin mobile feed returned a gallery instead of a video"
    );
    let media = map_complete_detail_with_quality(detail, expected, source, quality)?;
    ensure!(
        media.is_video && media.images.is_empty() && media.audio_url.is_none(),
        "douyin mobile feed returned no video media"
    );
    let video_url = media
        .video_url
        .as_deref()
        .context("douyin mobile feed returned no video playback URL")?;
    url_policy::validate_public_url(&Url::parse(video_url)?)
        .context("douyin mobile feed returned an invalid video playback URL")?;
    Ok(media)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn source() -> Url {
        Url::parse("https://www.douyin.com/video/123").unwrap()
    }

    fn expected() -> AwemeRef {
        AwemeRef {
            id: "123".into(),
            is_note: false,
        }
    }

    fn detail(id: &str) -> Value {
        json!({
            "aweme_id": id,
            "aweme_type": 0,
            "desc": "作品标题\n完整文案 #标签",
            "create_time": 1700000000,
            "author": {
                "nickname": "作者",
                "avatar_medium": {"url_list": ["https://p3.douyinpic.com/avatar.jpeg"]},
                "sec_uid": "unneeded-author-field"
            },
            "video": {
                "origin_cover": {"url_list": ["https://p3.douyinpic.com/original.jpeg"]},
                "cover": {"url_list": ["https://p3.douyinpic.com/thumbnail.jpeg"]},
                "play_addr": {"url_list": ["https://v26.douyinvod.com/video.mp4"]},
                "bit_rate": [{
                    "is_h265": 1,
                    "play_addr": {"url_list": ["https://v26.douyinvod.com/lower-quality.mp4"]}
                }]
            },
            "statistics": {"digg_count": 42, "comment_count": 7, "collect_count": 3, "share_count": 2},
            "unneeded_feed_field": "do-not-retain"
        })
    }

    fn payload(detail: Value) -> Value {
        json!({"status_code": 0, "aweme_list": [detail]})
    }

    #[test]
    fn feed_metadata_uses_the_configured_official_ratio() {
        let mut value = detail("123");
        value["video"]["play_addr"]["uri"] = json!("v_actual");
        let media =
            map_feed_payload_with_quality(&payload(value), &expected(), &source(), "720P").unwrap();
        assert!(media.video_url.unwrap().contains("ratio=720p"));
        assert_eq!(media.video_quality.as_deref(), Some("请求720P"));
    }

    #[test]
    fn selects_requested_work_and_retains_metadata_statistics_and_original_play_address() {
        let value = json!({"status_code": 0, "aweme_list": [detail("456"), detail("123")]});
        let media = map_feed_payload(&value, &expected(), &source()).unwrap();
        assert_eq!(media.title.as_deref(), Some("作品标题"));
        assert_eq!(media.description.as_deref(), Some("完整文案 #标签"));
        assert_eq!(media.author.as_deref(), Some("作者"));
        assert_eq!(media.published_at, Some(1700000000));
        assert_eq!(
            media.cover_url.as_deref(),
            Some("https://p3.douyinpic.com/original.jpeg")
        );
        assert_eq!(
            media.avatar_url.as_deref(),
            Some("https://p3.douyinpic.com/avatar.jpeg")
        );
        assert_eq!(
            media.video_url.as_deref(),
            Some("https://v26.douyinvod.com/video.mp4")
        );
        assert!(media.is_video && media.images.is_empty());
        assert_eq!(media.raw["aweme_id"], "123");
        assert_eq!(media.raw["statistics"]["digg_count"], 42);
        assert_eq!(media.raw["statistics"]["comment_count"], 7);
        assert_eq!(media.raw["statistics"]["collect_count"], 3);
        assert_eq!(media.raw["statistics"]["share_count"], 2);
        assert!(media.raw.get("author").is_none());
        assert!(media.raw.get("video").is_none());
        assert!(media.raw.get("unneeded_feed_field").is_none());
    }

    #[test]
    fn missing_id_and_recommended_other_work_cannot_be_used_as_the_requested_video() {
        let mut missing = detail("123");
        missing.as_object_mut().unwrap().remove("aweme_id");
        for value in [
            json!({"status_code": 0, "aweme_list": []}),
            payload(detail("456")),
            payload(missing),
            json!({"status_code": 0, "item_list": [detail("123")]}),
        ] {
            assert!(map_feed_payload(&value, &expected(), &source()).is_err());
        }
    }

    #[test]
    fn rejects_unsuccessful_or_filtered_content_even_with_complete_metadata() {
        for value in [
            json!({"aweme_list": [detail("123")]}),
            json!({"status_code": 1, "aweme_list": [detail("123")]}),
            json!({"status_code": 0, "filter_list": [{"aweme_id": "123"}], "aweme_list": [detail("123")]}),
            json!({"status_code": 0, "filter_detail": {"filter_reason": "status_self_see"}, "aweme_list": [detail("123")]}),
        ] {
            assert!(map_feed_payload(&value, &expected(), &source()).is_err());
        }
        let mut filtered = detail("123");
        filtered["filter_detail"] = json!({"filter_reason": "status_deleted"});
        assert!(map_feed_payload(&payload(filtered), &expected(), &source()).is_err());
    }

    #[test]
    fn gallery_soundtracks_are_never_mistaken_for_a_video() {
        for image_path in ["images", "image_list", "image_infos", "original_images"] {
            let mut gallery = detail("123");
            gallery[image_path] = json!([{"url_list": ["https://p3.douyinpic.com/image.jpeg"]}]);
            assert!(map_feed_payload(&payload(gallery), &expected(), &source()).is_err());
        }
        let mut gallery = detail("123");
        gallery["image_post_info"] =
            json!({"images": [{"url_list": ["https://p3.douyinpic.com/image.jpeg"]}]});
        assert!(map_feed_payload(&payload(gallery), &expected(), &source()).is_err());
        for kind in [2, 68] {
            let mut gallery = detail("123");
            gallery["aweme_type"] = json!(kind);
            assert!(map_feed_payload(&payload(gallery), &expected(), &source()).is_err());
        }
        let mut note = expected();
        note.is_note = true;
        assert!(map_feed_payload(&payload(detail("123")), &note, &source()).is_err());
    }

    #[test]
    fn complete_metadata_and_public_playback_url_are_required() {
        for field in ["desc", "author", "video"] {
            let mut partial = detail("123");
            partial.as_object_mut().unwrap().remove(field);
            assert!(map_feed_payload(&payload(partial), &expected(), &source()).is_err());
        }
        for bad_url in [
            "http://127.0.0.1/video.mp4",
            "http://192.168.1.1/video.mp4",
            "https://user:password@v26.douyinvod.com/video.mp4",
            "file:///tmp/video.mp4",
        ] {
            let mut partial = detail("123");
            partial["video"]["play_addr"]["url_list"] = json!([bad_url]);
            let alternative =
                map_feed_payload(&payload(partial.clone()), &expected(), &source()).unwrap();
            assert_eq!(
                alternative.video_url.as_deref(),
                Some("https://v26.douyinvod.com/lower-quality.mp4")
            );
            partial["video"]["bit_rate"] = json!([]);
            assert!(map_feed_payload(&payload(partial), &expected(), &source()).is_err());
        }
        let mut partial = detail("123");
        partial["video"]
            .as_object_mut()
            .unwrap()
            .remove("play_addr");
        partial["video"]["bit_rate"] = json!([]);
        assert!(map_feed_payload(&payload(partial), &expected(), &source()).is_err());
        let mut partial = detail("123");
        partial["author"]["avatar_medium"]["url_list"] = json!(["http://127.0.0.1/avatar.jpeg"]);
        assert!(map_feed_payload(&payload(partial), &expected(), &source()).is_err());
        let mut partial = detail("123");
        partial["video"]["origin_cover"]["url_list"] = json!(["http://127.0.0.1/cover.jpeg"]);
        assert!(map_feed_payload(&payload(partial), &expected(), &source()).is_err());
    }
}
