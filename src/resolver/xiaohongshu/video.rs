//! Bounded playback candidates from the requested note's own video metadata.
//!
//! Configured quality chooses the nearest known rendition, preferring H.264.
//! Other stream types remain playback alternatives, not a guarantee that the
//! platform or the author has not embedded a mark in the video itself.

use std::{borrow::Cow, cmp::Reverse, io};

use serde_json::Value;
use url::Url;

use crate::resolver::url_policy;

const MAX_CANDIDATES: usize = 3;
const MAX_KEY_BYTES: usize = 1024;
const MAX_URL_BYTES: usize = 16 * 1024;
const MAX_MEDIA_V2_BYTES: usize = 256 * 1024;
const MAX_STREAMS: usize = 64;
const MAX_ADDRESS_URLS: usize = 8;

struct Stream<'a> {
    value: &'a Value,
    h264: bool,
    preferred: bool,
    pixels: u64,
    bit_rate: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Dimensions {
    width: u64,
    height: u64,
}

impl Dimensions {
    fn short_edge(self) -> u64 {
        self.width.min(self.height)
    }
}

struct Rendition {
    urls: Vec<String>,
    dimensions: Option<Dimensions>,
    h264: bool,
    web_stream: bool,
    preferred: bool,
    bit_rate: u64,
}

#[derive(Default)]
pub(super) struct Selection {
    pub urls: Vec<String>,
    pub width: Option<u64>,
    pub height: Option<u64>,
    pub notice: Option<String>,
}

impl Selection {
    pub fn quality(&self) -> Option<String> {
        Some(format!("{}P", self.width?.min(self.height?)))
    }
}

pub(super) fn requested_quality(value: &str) -> Option<u64> {
    let value = value.trim().trim_end_matches(['p', 'P']);
    match value.parse::<u64>().ok()? {
        quality @ (360 | 480 | 720 | 1080) => Some(quality),
        _ => None,
    }
}

/// Keep the selected source's own dimensions. An original key without dimensions
/// cannot inherit dimensions from another rendition or from a requested tier.
pub(super) fn select(video: Option<&Value>, requested: Option<u64>) -> Selection {
    let Some(video) = video.filter(|value| value.is_object()) else {
        return Selection::default();
    };
    let mut renditions = renditions(video);
    let has_h264 = renditions.iter().any(|rendition| rendition.h264);
    let has_requested = requested.is_some_and(|requested| {
        renditions.iter().any(|rendition| {
            rendition
                .dimensions
                .is_some_and(|dimensions| dimensions.short_edge() == requested)
        })
    });
    let urls = if let Some(requested) = requested {
        // An available H.264 source is more compatible than a larger HEVC/AV1
        // rendition. Original keys remain bounded fallback sources.
        if has_h264 {
            renditions.retain(|rendition| rendition.h264 || !rendition.web_stream);
        }
        renditions.sort_by_key(|rendition| {
            (
                !rendition.h264,
                rendition.dimensions.is_none(),
                rendition
                    .dimensions
                    .map(|dimensions| dimensions.short_edge().abs_diff(requested))
                    .unwrap_or(u64::MAX),
                rendition
                    .dimensions
                    .is_some_and(|dimensions| dimensions.short_edge() > requested),
                !rendition.preferred,
                Reverse(
                    rendition
                        .dimensions
                        .map(|dimensions| dimensions.width.saturating_mul(dimensions.height))
                        .unwrap_or_default(),
                ),
                Reverse(rendition.bit_rate),
            )
        });
        let mut urls = Vec::with_capacity(MAX_CANDIDATES);
        for rendition in &renditions {
            for url in &rendition.urls {
                add(&mut urls, url);
            }
        }
        urls
    } else {
        // Preserve the original ordering for legacy callers that do not select
        // a tier, while still reporting only the selected URL's known metadata.
        candidates(Some(video))
    };
    let Some(first) = urls.first() else {
        return Selection::default();
    };
    let selected_dimensions = url_dimensions(&renditions, first);
    let mut notice = match (requested, selected_dimensions) {
        (Some(requested), Some(dimensions)) if dimensions.short_edge() != requested => {
            let actual = dimensions.short_edge();
            Some(if has_h264 && has_requested {
                format!("优先获取兼容的 H.264 {actual}P 源（请求 {requested}P，未转码）")
            } else {
                format!("未取得可用 {requested}P 源，本次获取 {actual}P（未转码）")
            })
        }
        (Some(requested), None) => Some(format!(
            "请求 {requested}P；平台未返回所选视频源尺寸，无法确认实际清晰度（未转码）"
        )),
        (None, None) => Some("平台未返回所选视频源尺寸，无法确认实际清晰度".into()),
        _ => None,
    };
    if selected_dimensions.is_some()
        && urls
            .iter()
            .skip(1)
            .any(|url| url_dimensions(&renditions, url) != selected_dimensions)
    {
        match notice.as_mut() {
            Some(notice) => notice.push_str("；备用源清晰度可能不同"),
            None => notice = Some("备用源清晰度可能不同".into()),
        }
    }
    Selection {
        urls,
        width: selected_dimensions.map(|dimensions| dimensions.width),
        height: selected_dimensions.map(|dimensions| dimensions.height),
        notice,
    }
}

fn url_dimensions(renditions: &[Rendition], url: &str) -> Option<Dimensions> {
    let mut dimensions = None;
    for rendition in renditions {
        if !rendition.urls.iter().any(|candidate| candidate == url) {
            continue;
        }
        if let Some(current) = rendition.dimensions {
            if dimensions.is_some_and(|known| known != current) {
                // Contradictory dimensions for identical signed URL bytes are
                // not sufficient evidence for an actual quality label.
                return None;
            }
            dimensions = Some(current);
        }
    }
    dimensions
}

fn renditions(video: &Value) -> Vec<Rendition> {
    let mut result = Vec::new();
    origin_rendition(&mut result, video);
    if let Some(media_v2) = media_v2(video.get("mediaV2")) {
        let inner = media_v2.get("video").unwrap_or(&media_v2);
        if let Some(opaque) = inner.get("opaque1") {
            for key in ["hd_screencast_stream", "default_screencast_stream"] {
                let Some(value) = opaque.get(key).and_then(Value::as_str) else {
                    continue;
                };
                let mut urls = Vec::new();
                add(&mut urls, value);
                push_rendition(&mut result, urls, None, false, false, true, 0);
            }
        }
        origin_rendition(&mut result, inner);
        origin_rendition(&mut result, &media_v2);
    }
    for stream in streams(video) {
        let mut urls = Vec::with_capacity(MAX_CANDIDATES);
        add_stream(&mut urls, stream.value);
        push_rendition(
            &mut result,
            urls,
            dimensions(stream.value),
            stream.h264,
            true,
            stream.preferred,
            stream.bit_rate,
        );
    }
    if !marked(video) {
        let mut urls = Vec::new();
        for key in ["masterUrl", "video_url"] {
            if let Some(value) = video.get(key).and_then(Value::as_str) {
                add(&mut urls, value);
            }
        }
        push_rendition(&mut result, urls, dimensions(video), false, false, false, 0);
    }
    result
}

fn origin_rendition(result: &mut Vec<Rendition>, video: &Value) {
    let mut urls = Vec::new();
    add_origin(&mut urls, video);
    let dimensions = video
        .get("consumer")
        .and_then(dimensions)
        .or_else(|| dimensions(video))
        .or_else(|| video.pointer("/media/video").and_then(dimensions));
    push_rendition(result, urls, dimensions, false, false, true, 0);
}

fn push_rendition(
    result: &mut Vec<Rendition>,
    urls: Vec<String>,
    dimensions: Option<Dimensions>,
    h264: bool,
    web_stream: bool,
    preferred: bool,
    bit_rate: u64,
) {
    if !urls.is_empty() {
        result.push(Rendition {
            urls,
            dimensions,
            h264,
            web_stream,
            preferred,
            bit_rate,
        });
    }
}

fn dimensions(value: &Value) -> Option<Dimensions> {
    let width = integer(value, &["width", "videoWidth", "video_width"]);
    let height = integer(value, &["height", "videoHeight", "video_height"]);
    // Bound malformed metadata; 32K per side accommodates ordinary renditions.
    (width > 0 && height > 0 && width <= 32_768 && height <= 32_768)
        .then_some(Dimensions { width, height })
}

/// Keep the original source and at most two distinct playback alternatives.
/// CDN URLs are opaque: validating them must not rewrite their signed bytes.
pub(super) fn candidates(video: Option<&Value>) -> Vec<String> {
    let Some(video) = video.filter(|value| value.is_object()) else {
        return Vec::new();
    };
    let mut result = Vec::with_capacity(MAX_CANDIDATES);
    add_origin(&mut result, video);

    if let Some(media_v2) = media_v2(video.get("mediaV2")) {
        let inner = media_v2.get("video").unwrap_or(&media_v2);
        if let Some(opaque) = inner.get("opaque1") {
            for key in ["hd_screencast_stream", "default_screencast_stream"] {
                if let Some(value) = opaque.get(key).and_then(Value::as_str) {
                    add(&mut result, value);
                }
            }
        }
        add_origin(&mut result, inner);
        add_origin(&mut result, &media_v2);
    }

    let mut streams = streams(video);
    streams.sort_by(|left, right| {
        (right.h264, right.preferred, right.pixels, right.bit_rate).cmp(&(
            left.h264,
            left.preferred,
            left.pixels,
            left.bit_rate,
        ))
    });

    let mut has_h264 = false;
    for stream in streams.iter().filter(|stream| stream.h264) {
        has_h264 |= add_stream(&mut result, stream.value);
    }

    // Older public pages can expose a direct master URL without stream details.
    // Do not let a marked stream's nested URL leak through a recursive search.
    if !marked(video) {
        for key in ["masterUrl", "video_url"] {
            if let Some(value) = video.get(key).and_then(Value::as_str) {
                add(&mut result, value);
            }
        }
    }

    // Prefer an available H.264 rendition even when an HEVC/AV1 item is bigger.
    if !has_h264 {
        for stream in streams.iter().filter(|stream| !stream.h264) {
            add_stream(&mut result, stream.value);
        }
    }
    result
}

fn streams(video: &Value) -> Vec<Stream<'_>> {
    let stream_map = video
        .get("media")
        .and_then(|media| media.get("stream"))
        .filter(|stream| {
            ["h264", "h265", "av1"]
                .iter()
                .any(|codec| stream.get(*codec).and_then(Value::as_array).is_some())
        })
        .or_else(|| video.get("stream").filter(|stream| stream.is_object()));
    let mut streams = Vec::new();
    let mut remaining = MAX_STREAMS;
    for codec in ["h264", "h265", "av1"] {
        let Some(values) = stream_map
            .and_then(|stream| stream.get(codec))
            .and_then(Value::as_array)
        else {
            continue;
        };
        for value in values.iter().take(remaining) {
            remaining -= 1;
            if !value.is_object() || marked(value) {
                continue;
            }
            let description = description(value);
            streams.push(Stream {
                value,
                h264: codec == "h264",
                preferred: matches!(stream_type(value), Some(258 | 301))
                    || description.contains("X264_MP4"),
                pixels: integer(value, &["width"]).saturating_mul(integer(value, &["height"])),
                bit_rate: integer(value, &["avgBitrate", "avg_bitrate", "bitrate", "bitRate"]),
            });
        }
    }
    streams
}

fn add_origin(result: &mut Vec<String>, video: &Value) {
    for container in [video.get("consumer"), Some(video)].into_iter().flatten() {
        for key in ["originVideoKey", "origin_video_key"] {
            let Some(key) = container.get(key).and_then(Value::as_str) else {
                continue;
            };
            if let Some(value) = origin_url(key) {
                add(result, &value);
                return;
            }
        }
    }
}

fn origin_url(key: &str) -> Option<String> {
    if let Ok(url) = Url::parse(key) {
        if matches!(url.scheme(), "http" | "https") {
            return validated_url(key);
        }
        return None;
    }
    if key.is_empty()
        || key.len() > MAX_KEY_BYTES
        || !key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'_' | b'.'))
        || key
            .split('/')
            .any(|part| part.is_empty() || matches!(part, "." | ".."))
    {
        return None;
    }
    let value = format!("https://sns-video-bd.xhscdn.com/{key}");
    validated_url(&value)
}

fn media_v2(value: Option<&Value>) -> Option<Cow<'_, Value>> {
    match value? {
        Value::String(raw) if raw.len() <= MAX_MEDIA_V2_BYTES => {
            let parsed = serde_json::from_str::<Value>(raw).ok()?;
            parsed.is_object().then_some(Cow::Owned(parsed))
        }
        value @ Value::Object(_) => {
            // Check the encoded bound without allocating another large JSON blob.
            serde_json::to_writer(SizeLimit(MAX_MEDIA_V2_BYTES), value).ok()?;
            Some(Cow::Borrowed(value))
        }
        _ => None,
    }
}

struct SizeLimit(usize);

impl io::Write for SizeLimit {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 = self
            .0
            .checked_sub(bytes.len())
            .ok_or_else(|| io::Error::other("mediaV2 exceeds the encoded size limit"))?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn integer(value: &Value, keys: &[&str]) -> u64 {
    keys.iter()
        .find_map(|key| {
            value
                .get(*key)
                .and_then(|value| value.as_u64().or_else(|| value.as_str()?.parse().ok()))
        })
        .unwrap_or_default()
}

fn stream_type(value: &Value) -> Option<u64> {
    value
        .get("streamType")
        .or_else(|| value.get("stream_type"))
        .and_then(|value| value.as_u64().or_else(|| value.as_str()?.parse().ok()))
}

fn description(value: &Value) -> String {
    value
        .get("streamDesc")
        .or_else(|| value.get("stream_desc"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .chars()
        .take(512)
        .collect::<String>()
        .to_ascii_uppercase()
}

fn marked(value: &Value) -> bool {
    let description = description(value);
    matches!(stream_type(value), Some(259 | 309))
        || description.contains("WATERMARK")
        || description.contains("WATER_MARK")
        || description.contains("带水印")
}

fn add_stream(result: &mut Vec<String>, stream: &Value) -> bool {
    let mut remaining = MAX_ADDRESS_URLS;
    let mut valid = false;
    if let Some(value) = stream
        .get("masterUrl")
        .or_else(|| stream.get("master_url"))
        .and_then(Value::as_str)
    {
        remaining -= 1;
        valid |= add(result, value);
    }
    for key in ["backupUrls", "backup_urls", "backupUrl", "backup_url"] {
        if let Some(values) = stream.get(key).and_then(Value::as_array) {
            for value in values.iter().take(remaining) {
                remaining -= 1;
                if let Some(value) = value.as_str() {
                    valid |= add(result, value);
                }
            }
        } else if let Some(value) = stream.get(key).and_then(Value::as_str)
            && remaining > 0
        {
            remaining -= 1;
            valid |= add(result, value);
        }
    }
    valid
}

fn add(result: &mut Vec<String>, value: &str) -> bool {
    let Some(value) = validated_url(value) else {
        return false;
    };
    if result.len() < MAX_CANDIDATES && !result.contains(&value) {
        result.push(value);
    }
    true
}

fn validated_url(value: &str) -> Option<String> {
    if value.is_empty() || value.len() > MAX_URL_BYTES || value != value.trim() {
        return None;
    }
    let url = Url::parse(value).ok()?;
    url_policy::validate_public_url(&url).ok()?;
    if url.fragment().is_some() {
        return None;
    }
    let host = url.host_str()?.trim_end_matches('.').to_ascii_lowercase();
    if host == "localhost" || host.ends_with(".localhost") || host.ends_with(".local") {
        return None;
    }
    if let Some(url::Host::Ipv6(ip)) = url.host()
        && let Some(mapped) = ip.to_ipv4_mapped()
    {
        let mapped = Url::parse(&format!("https://{mapped}/")).ok()?;
        url_policy::validate_public_url(&mapped).ok()?;
    }
    let path = url.path().to_ascii_lowercase();
    if [".mp3", ".m4a", ".aac", ".wav", ".flac", ".ogg", ".opus"]
        .iter()
        .any(|suffix| path.ends_with(suffix))
        || url.query_pairs().any(|(key, value)| {
            key.eq_ignore_ascii_case("watermark") && matches!(value.as_ref(), "1" | "true")
        })
    {
        return None;
    }
    Some(value.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn configured_tiers_choose_the_matching_h264_rendition_for_both_orientations() {
        let video = json!({"stream": {"h264": [
            {"width": 1080, "height": 1920, "masterUrl": "https://cdn.example/1080.mp4"},
            {"width": 720, "height": 1280, "masterUrl": "https://cdn.example/720.mp4"},
            {"width": 854, "height": 480, "masterUrl": "https://cdn.example/480.mp4"},
            {"width": "360", "height": "640", "masterUrl": "https://cdn.example/360.mp4"}
        ]}});
        for quality in [360, 480, 720, 1080] {
            let selection = select(Some(&video), requested_quality(&format!("{quality}P")));
            assert_eq!(
                selection.urls[0],
                format!("https://cdn.example/{quality}.mp4")
            );
            assert_eq!(selection.quality(), Some(format!("{quality}P")));
            assert!(selection.urls.len() <= MAX_CANDIDATES);
        }
    }

    #[test]
    fn missing_lower_tier_reports_the_selected_source_without_transcoding() {
        let master = "https://cdn.example/720.mp4?sign=A%2Fb%2Bz&key=1&key=2";
        let video = json!({"stream": {"h264": [{
            "width": 720, "height": 1280, "masterUrl": master,
            "backupUrls": ["https://backup.example/720.mp4"]
        }]}});
        let selection = select(Some(&video), Some(480));
        assert_eq!(selection.urls, [master, "https://backup.example/720.mp4"]);
        assert_eq!(selection.quality().as_deref(), Some("720P"));
        let notice = selection.notice.as_deref().unwrap();
        assert!(notice.contains("480P") && notice.contains("720P") && notice.contains("未转码"));
        assert!(!notice.contains("备用源"));
    }

    #[test]
    fn compatible_source_wins_over_a_matching_original_and_hevc() {
        let video = json!({
            "width": 1080, "height": 1920,
            "consumer": {"originVideoKey": "pre_post/original"},
            "stream": {
                "h264": [{"width": 720, "height": 1280, "masterUrl": "https://cdn.example/720.mp4"}],
                "h265": [{"width": 1080, "height": 1920, "masterUrl": "https://cdn.example/hevc.mp4"}]
            }
        });
        let selection = select(Some(&video), Some(1080));
        assert_eq!(selection.urls[0], "https://cdn.example/720.mp4");
        assert_eq!(selection.quality().as_deref(), Some("720P"));
        assert!(selection.notice.as_deref().unwrap().contains("H.264"));
        assert!(!selection.urls.iter().any(|url| url.ends_with("hevc.mp4")));
    }

    #[test]
    fn unknown_original_does_not_borrow_another_streams_dimensions() {
        let video = json!({
            "consumer": {"originVideoKey": "pre_post/unknown_original"},
            "stream": {"h264": [{"width": 1080, "height": 1920, "masterUrl": "https://cdn.example/1080.mp4"}]}
        });
        let selection = select(Some(&video), None);
        assert_eq!(
            selection.urls[0],
            "https://sns-video-bd.xhscdn.com/pre_post/unknown_original"
        );
        assert_eq!(selection.quality(), None);
        assert!(
            selection
                .notice
                .as_deref()
                .unwrap()
                .contains("无法确认实际清晰度")
        );
        let video = json!({"consumer": {"originVideoKey": "pre_post/unknown_original"}});
        let selection = select(Some(&video), Some(1080));
        assert_eq!(selection.quality(), None);
        assert!(selection.notice.as_deref().unwrap().contains("请求 1080P"));
    }

    #[test]
    fn original_dimensions_are_not_inferred_from_cover_or_other_renditions() {
        let video = json!({
            "consumer": {"originVideoKey": "pre_post/known_original"},
            "media": {"video": {"width": 1080, "height": 1920}}
        });
        let selection = select(Some(&video), Some(360));
        assert_eq!(selection.quality().as_deref(), Some("1080P"));
        assert!(selection.notice.as_deref().unwrap().contains("360P"));
        let video = json!({
            "consumer": {"originVideoKey": "pre_post/unknown_original"},
            "cover": {"width": 1080, "height": 1920}
        });
        assert_eq!(select(Some(&video), Some(1080)).quality(), None);
    }

    #[test]
    fn configured_selection_keeps_public_unmarked_bounded_sources() {
        let video = json!({"stream": {"h264": [
            {"width": 480, "height": 854, "streamType": 259, "masterUrl": "https://cdn.example/marked.mp4"},
            {"width": 480, "height": 854, "masterUrl": "http://127.0.0.1/private.mp4"},
            {"width": 720, "height": 1280, "masterUrl": "https://cdn.example/720.mp4", "backupUrls": [
                "https://backup.example/720a.mp4", "https://backup.example/720b.mp4", "https://backup.example/720c.mp4"
            ]}
        ]}});
        let selection = select(Some(&video), Some(480));
        assert_eq!(
            selection.urls,
            [
                "https://cdn.example/720.mp4",
                "https://backup.example/720a.mp4",
                "https://backup.example/720b.mp4"
            ]
        );
        assert_eq!(selection.quality().as_deref(), Some("720P"));
    }

    #[test]
    fn equally_close_tiers_prefer_the_lower_short_edge() {
        let video = json!({"stream": {"h264": [
            {"width": 840, "height": 1493, "masterUrl": "https://cdn.example/840.mp4"},
            {"width": 600, "height": 1067, "masterUrl": "https://cdn.example/600.mp4"}
        ]}});
        let selection = select(Some(&video), Some(720));
        assert_eq!(selection.quality().as_deref(), Some("600P"));
        assert_eq!(selection.urls[0], "https://cdn.example/600.mp4");
    }

    #[test]
    fn mobile_original_precedes_marked_mini_app_and_unknown_hevc_streams() {
        let video = json!({
            "consumer": {"originVideoKey": "pre_post/1040g2t_video_key"},
            "media": {"stream": {
                "h264": [{"streamType": 259, "streamDesc": "MINI_APP", "masterUrl": "https://sns-video-v2.xhscdn.com/marked.mp4"}],
                "h265": [{"streamType": 520, "masterUrl": "https://sns-video-v2.xhscdn.com/hevc.mp4"}]
            }}
        });
        assert_eq!(
            candidates(Some(&video)),
            [
                "https://sns-video-bd.xhscdn.com/pre_post/1040g2t_video_key",
                "https://sns-video-v2.xhscdn.com/hevc.mp4"
            ]
        );
    }

    #[test]
    fn pc_x264_and_its_backup_are_kept_without_rewriting_signature() {
        let master = "https://sns-video-v2.xhscdn.com/stream/258.mp4?sign=A%2Fb%2Bz&key=1&key=2";
        let video = json!({"consumer": {}, "media": {"stream": {
            "h264": [{"streamType": 258, "streamDesc": "X264_MP4", "width": 720, "height": 1280,
                "masterUrl": master, "backupUrls": ["https://sns-video-qc.xhscdn.com/backup.mp4"]}],
            "h265": [{"streamType": 520, "width": 1080, "height": 1920, "masterUrl": "https://sns-video-v2.xhscdn.com/hevc.mp4"}]
        }}});
        assert_eq!(
            candidates(Some(&video)),
            [master, "https://sns-video-qc.xhscdn.com/backup.mp4"]
        );
    }

    #[test]
    fn preferred_h264_streams_sort_by_pixels_then_bit_rate_before_unknown_types() {
        let video = json!({"stream": {"h264": [
            {"streamType": 600, "width": 2160, "height": 3840, "masterUrl": "https://cdn.example/unknown.mp4"},
            {"streamType": 258, "width": 720, "height": 1280, "avgBitrate": 4000, "masterUrl": "https://cdn.example/small.mp4"},
            {"streamType": 301, "width": 1080, "height": 1920, "avgBitrate": 1000, "masterUrl": "https://cdn.example/large-slow.mp4"},
            {"streamType": 301, "width": 1080, "height": 1920, "avgBitrate": 3000, "masterUrl": "https://cdn.example/large-fast.mp4"}
        ]}});
        assert_eq!(
            candidates(Some(&video)),
            [
                "https://cdn.example/large-fast.mp4",
                "https://cdn.example/large-slow.mp4",
                "https://cdn.example/small.mp4"
            ]
        );
    }

    #[test]
    fn media_v2_supports_string_object_and_internal_original_key() {
        for media in [
            json!({"video": {"opaque1": {"hd_screencast_stream": "https://cdn.example/hd.mp4", "default_screencast_stream": "https://cdn.example/default.mp4"}}}),
            json!(
                r#"{"video":{"opaque1":{"hd_screencast_stream":"https://cdn.example/hd.mp4","default_screencast_stream":"https://cdn.example/default.mp4"}}}"#
            ),
        ] {
            let video = json!({"mediaV2": media});
            assert_eq!(
                candidates(Some(&video)),
                [
                    "https://cdn.example/hd.mp4",
                    "https://cdn.example/default.mp4"
                ]
            );
        }
        let video = json!({"mediaV2": {"video": {"consumer": {"origin_video_key": "pre_post/actual_key"}}}});
        assert_eq!(
            candidates(Some(&video)),
            ["https://sns-video-bd.xhscdn.com/pre_post/actual_key"]
        );
    }

    #[test]
    fn origin_keys_are_actual_bounded_paths_or_public_urls() {
        for key in [
            "",
            "../video",
            "pre_post/../video",
            "pre_post/./video",
            "/pre_post/video",
            "//evil.test/video",
            "video?x=1",
            "video#x",
            "video%2f..",
            "pre_post\\video",
            "https://user:pass@cdn.example/video.mp4",
            "http://127.0.0.1/v.mp4",
            "javascript:alert(1)",
        ] {
            assert!(origin_url(key).is_none(), "{key}");
        }
        assert!(origin_url(&"a".repeat(MAX_KEY_BYTES + 1)).is_none());
        let signed = "http://cdn.example/original.mp4?sig=A%2Fb%3D";
        let video = json!({"origin_video_key": signed});
        assert_eq!(candidates(Some(&video)), [signed]);
        assert!(candidates(None).is_empty());
        assert!(candidates(Some(&Value::Null)).is_empty());
    }

    #[test]
    fn marked_streams_are_not_revived_as_generic_master_urls() {
        let video = json!({"media": {"stream": {"h264": [
            {"streamType": 259, "masterUrl": "https://cdn.example/a.mp4"},
            {"streamType": 309, "masterUrl": "https://cdn.example/b.mp4"},
            {"streamType": 258, "streamDesc": "X264_MP4_WITH_WATERMARK", "masterUrl": "https://cdn.example/c.mp4"}
        ]}}});
        assert!(candidates(Some(&video)).is_empty());
        let video = json!({"streamType": 259, "masterUrl": "https://cdn.example/marked.mp4"});
        assert!(candidates(Some(&video)).is_empty());
    }

    #[test]
    fn unknown_h264_is_a_playback_fallback_and_hevc_requires_no_valid_h264() {
        let mut video = json!({"stream": {
            "h264": [{"streamType": 600, "masterUrl": "https://cdn.example/h264.mp4"}],
            "h265": [{"streamType": 520, "masterUrl": "https://cdn.example/hevc.mp4"}],
            "av1": [{"streamType": 700, "masterUrl": "https://cdn.example/av1.mp4"}]
        }});
        assert_eq!(candidates(Some(&video)), ["https://cdn.example/h264.mp4"]);
        video["stream"]["h264"][0]["masterUrl"] = json!("http://127.0.0.1/private.mp4");
        assert_eq!(
            candidates(Some(&video)),
            [
                "https://cdn.example/hevc.mp4",
                "https://cdn.example/av1.mp4"
            ]
        );
    }

    #[test]
    fn rejects_audio_private_and_credential_urls_but_keeps_legacy_direct_video() {
        for value in [
            "https://cdn.example/audio.MP3",
            "https://cdn.example/audio.m4a",
            "http://192.168.1.1/v.mp4",
            "http://[::ffff:127.0.0.1]/v.mp4",
            "http://localhost/v.mp4",
            "https://internal.local/v.mp4",
            "https://user:pass@cdn.example/v.mp4",
            "file:///v.mp4",
            "https://cdn.example/v.mp4?watermark=1",
        ] {
            let video = json!({"masterUrl": value});
            assert!(candidates(Some(&video)).is_empty(), "{value}");
        }
        let video = json!({"video_url": "https://cdn.example/legacy.mp4?sig=unchanged"});
        assert_eq!(
            candidates(Some(&video)),
            ["https://cdn.example/legacy.mp4?sig=unchanged"]
        );
    }

    #[test]
    fn deduplicates_original_screencast_and_stream_sources() {
        let shared = "https://cdn.example/shared.mp4";
        let video = json!({
            "consumer": {"originVideoKey": shared},
            "mediaV2": {"video": {"opaque1": {"hd_screencast_stream": shared, "default_screencast_stream": "https://cdn.example/default.mp4"}}},
            "stream": {"h264": [{"streamType": 258, "masterUrl": shared, "backupUrls": ["https://cdn.example/backup.mp4", "https://cdn.example/fourth.mp4"]}]}
        });
        assert_eq!(
            candidates(Some(&video)),
            [
                shared,
                "https://cdn.example/default.mp4",
                "https://cdn.example/backup.mp4"
            ]
        );
    }

    #[test]
    fn media_v2_stream_and_address_scans_are_bounded() {
        for media in [
            json!("x".repeat(MAX_MEDIA_V2_BYTES + 1)),
            json!({"extra": "x".repeat(MAX_MEDIA_V2_BYTES + 1)}),
        ] {
            let video = json!({"mediaV2": media, "masterUrl": "https://cdn.example/direct.mp4"});
            assert_eq!(candidates(Some(&video)), ["https://cdn.example/direct.mp4"]);
        }
        let mut streams = vec![json!({"streamType": 259}); MAX_STREAMS];
        streams.push(json!({"streamType": 258, "masterUrl": "https://cdn.example/too-late.mp4"}));
        let video = json!({"stream": {"h264": streams, "h265": [{"masterUrl": "https://cdn.example/also-too-late.mp4"}]}});
        assert!(candidates(Some(&video)).is_empty());

        let mut urls = vec![Value::Null; MAX_ADDRESS_URLS - 1];
        urls.push(json!("https://cdn.example/too-late.mp4"));
        let video = json!({"stream": {"h264": [{"masterUrl": "http://127.0.0.1/private.mp4", "backupUrls": urls}]}});
        assert!(candidates(Some(&video)).is_empty());
    }
}
