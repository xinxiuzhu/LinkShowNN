//! Select bounded video sources from anonymous Douyin work metadata.
//!
//! The official playback endpoint can return the platform's version without
//! its download watermark. This does not remove marks embedded by the author.

use serde_json::Value;
use url::Url;

use crate::resolver::url_policy;

const MAX_CANDIDATES: usize = 3;
const MAX_URI_BYTES: usize = 128;
const MAX_ADDRESS_URLS: usize = 8;
const MAX_BIT_RATE_STREAMS: usize = 64;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Codec {
    H264,
    H265,
    Unknown,
}

struct Stream<'a> {
    address: &'a Value,
    codec: Codec,
    short_edge: Option<u64>,
    pixels: u64,
    bit_rate: u64,
}

pub(super) struct Selection {
    pub urls: Vec<String>,
    pub video_quality: Option<String>,
    pub quality_notice: Option<String>,
}

struct Candidate {
    url: String,
    short_edge: Option<u64>,
    requested: bool,
}

pub(super) fn quality_label(value: &str) -> &'static str {
    match value.trim().to_ascii_lowercase().trim_end_matches('p') {
        "360" => "360P",
        "480" => "480P",
        "720" => "720P",
        _ => "1080P",
    }
}

/// Return at most three distinct public video URLs, with a verified API video
/// identifier first and compatible H.264 alternatives ahead of unknown codecs.
#[cfg(test)]
pub(super) fn candidates(video: Option<&Value>) -> Vec<String> {
    candidates_with_quality(video, "1080P").urls
}

pub(super) fn candidates_with_quality(video: Option<&Value>, quality: &str) -> Selection {
    let quality = quality_label(quality);
    let target = quality.trim_end_matches('P').parse::<u64>().unwrap_or(1080);
    let Some(video) = video.filter(|value| value.is_object()) else {
        return selection(Vec::new(), quality, target);
    };
    let mut result = Vec::with_capacity(MAX_CANDIDATES);

    // A work ID is not a video ID. Only use the URI supplied in video address
    // fields; the download field contributes its URI, never its marked URLs.
    let uri = ["play_addr", "play_addr_h264", "download_addr"]
        .into_iter()
        .filter_map(|key| video.get(key)?.get("uri")?.as_str())
        .find(|uri| valid_video_uri(uri));
    if let Some(uri) = uri {
        add(&mut result, &playback_endpoint(uri, quality), None, quality);
    }

    let mut streams: Vec<_> = video
        .get("bit_rate")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(MAX_BIT_RATE_STREAMS)
        .filter_map(|item| {
            let address = item.get("play_addr").filter(|value| value.is_object())?;
            let (width, height) = dimensions(address, item);
            Some(Stream {
                address,
                codec: codec(item)
                    .or_else(|| codec(address))
                    .unwrap_or(Codec::Unknown),
                short_edge: short_edge(width, height),
                pixels: width.saturating_mul(height),
                bit_rate: integer(item, "bit_rate"),
            })
        })
        .collect();
    let base_address = video.get("play_addr");
    let base_codec = codec(video)
        .or_else(|| base_address.and_then(codec))
        .unwrap_or(Codec::Unknown);
    for (address, codec) in [
        (video.get("play_addr_h264"), Codec::H264),
        (
            base_address.filter(|_| base_codec == Codec::H264),
            Codec::H264,
        ),
    ] {
        if let Some(address) = address.filter(|value| value.is_object()) {
            let (width, height) = dimensions(address, video);
            streams.push(Stream {
                address,
                codec,
                short_edge: short_edge(width, height),
                pixels: width.saturating_mul(height),
                bit_rate: integer(address, "bit_rate"),
            });
        }
    }
    streams.sort_by_key(|stream| {
        (
            stream.short_edge.is_none(),
            stream
                .short_edge
                .map(|edge| edge.abs_diff(target))
                .unwrap_or(u64::MAX),
            stream.short_edge.unwrap_or(u64::MAX),
            std::cmp::Reverse(stream.pixels),
            std::cmp::Reverse(stream.bit_rate),
        )
    });

    let mut has_h264 = false;
    for stream in streams.iter().filter(|stream| stream.codec == Codec::H264) {
        has_h264 |= add_address(
            &mut result,
            Some(stream.address),
            stream.short_edge,
            quality,
        );
    }

    if base_codec == Codec::Unknown {
        let dimensions = base_address.map(|address| dimensions(address, video));
        let edge = dimensions.and_then(|(width, height)| short_edge(width, height));
        add_address(&mut result, base_address, edge, quality);
    }
    for stream in streams
        .iter()
        .filter(|stream| stream.codec == Codec::Unknown)
    {
        add_address(
            &mut result,
            Some(stream.address),
            stream.short_edge,
            quality,
        );
    }

    // An available H.264 stream must not be replaced by the first HEVC item
    // merely because the API lists that item first or reports a higher rate.
    if !has_h264 {
        if base_codec == Codec::H265 {
            let dimensions = base_address.map(|address| dimensions(address, video));
            let edge = dimensions.and_then(|(width, height)| short_edge(width, height));
            add_address(&mut result, base_address, edge, quality);
        }
        for stream in streams.iter().filter(|stream| stream.codec == Codec::H265) {
            add_address(
                &mut result,
                Some(stream.address),
                stream.short_edge,
                quality,
            );
        }
        let hevc = video.get("play_addr_265");
        let edge = hevc
            .map(|address| dimensions(address, video))
            .and_then(|(width, height)| short_edge(width, height));
        add_address(&mut result, hevc, edge, quality);
    }
    selection(result, quality, target)
}

fn selection(candidates: Vec<Candidate>, quality: &str, target: u64) -> Selection {
    let (video_quality, quality_notice) = match candidates.first() {
        Some(candidate) if candidate.requested => (
            Some(format!("请求{quality}")),
            Some(format!(
                "{quality} 为首选档位，平台可能返回其他画质，实际尺寸以下载源为准。"
            )),
        ),
        Some(candidate) => match candidate.short_edge {
            Some(edge) => (
                Some(format!("{edge}P")),
                (edge != target)
                    .then(|| format!("首选 {quality}，已选择最接近的 {edge}P 可用源。")),
            ),
            None => (
                None,
                Some(format!(
                    "首选 {quality}，平台未返回源尺寸，实际画质尚未确认。"
                )),
            ),
        },
        None => (None, None),
    };
    Selection {
        urls: candidates
            .into_iter()
            .map(|candidate| candidate.url)
            .collect(),
        video_quality,
        quality_notice,
    }
}

fn dimensions(address: &Value, fallback: &Value) -> (u64, u64) {
    let pair = (integer(address, "width"), integer(address, "height"));
    if pair.0 > 0 && pair.1 > 0 {
        pair
    } else {
        (integer(fallback, "width"), integer(fallback, "height"))
    }
}

fn short_edge(width: u64, height: u64) -> Option<u64> {
    (width > 0 && height > 0).then(|| width.min(height))
}

fn integer(value: &Value, key: &str) -> u64 {
    value.get(key).and_then(Value::as_u64).unwrap_or_default()
}

fn codec(value: &Value) -> Option<Codec> {
    if let Some(marker) = value.get("is_h265") {
        if marker.as_bool() == Some(false) || marker.as_u64() == Some(0) {
            return Some(Codec::H264);
        }
        if marker.as_bool() == Some(true) || marker.as_u64() == Some(1) {
            return Some(Codec::H265);
        }
    }
    match value
        .get("codec_type")?
        .as_str()?
        .to_ascii_lowercase()
        .as_str()
    {
        "h264" | "avc" | "avc1" => Some(Codec::H264),
        "h265" | "hevc" | "hev1" | "hvc1" => Some(Codec::H265),
        _ => None,
    }
}

fn valid_video_uri(uri: &str) -> bool {
    !uri.is_empty()
        && uri.len() <= MAX_URI_BYTES
        && uri
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        && !uri.to_ascii_lowercase().starts_with("http")
        && !uri.to_ascii_lowercase().contains("mp3")
}

fn playback_endpoint(uri: &str, quality: &str) -> String {
    let mut url = Url::parse("https://www.iesdouyin.com/aweme/v1/play/")
        .expect("the fixed official playback endpoint is valid");
    url.query_pairs_mut()
        .append_pair("video_id", uri)
        .append_pair("ratio", &quality.to_ascii_lowercase())
        .append_pair("line", "0")
        .append_pair("is_play_url", "1")
        .append_pair("watermark", "0")
        .append_pair("source", "PackSourceEnum_PUBLISH");
    url.into()
}

/// The boolean records a valid address even when it is already in the list or
/// the list is full, so codec selection does not depend on deduplication.
fn add_address(
    result: &mut Vec<Candidate>,
    address: Option<&Value>,
    short_edge: Option<u64>,
    quality: &str,
) -> bool {
    let Some(urls) = address
        .and_then(|value| value.get("url_list"))
        .and_then(Value::as_array)
    else {
        return false;
    };
    let mut valid = false;
    for value in urls.iter().take(MAX_ADDRESS_URLS).filter_map(Value::as_str) {
        valid |= add(result, value, short_edge, quality);
    }
    valid
}

fn add(result: &mut Vec<Candidate>, value: &str, short_edge: Option<u64>, quality: &str) -> bool {
    let Some(value) = normalize_with_quality(value, quality) else {
        return false;
    };
    if result.len() < MAX_CANDIDATES && !result.iter().any(|candidate| candidate.url == value) {
        let requested = Url::parse(&value)
            .ok()
            .is_some_and(|url| official_endpoint(&url));
        result.push(Candidate {
            url: value,
            short_edge,
            requested,
        });
    }
    true
}

#[cfg(test)]
fn normalize(value: &str) -> Option<String> {
    normalize_with_quality(value, "1080P")
}

fn normalize_with_quality(value: &str, quality: &str) -> Option<String> {
    let value = value.trim();
    let mut url = Url::parse(value).ok()?;
    url_policy::validate_public_url(&url).ok()?;
    let host = url.host_str()?.trim_end_matches('.').to_ascii_lowercase();
    if host == "localhost" || host.ends_with(".localhost") || host.ends_with(".local") {
        return None;
    }
    if let Some(url::Host::Ipv6(ip)) = url.host() {
        if let Some(ip) = ip.to_ipv4_mapped() {
            let mapped = Url::parse(&format!("http://{ip}/")).ok()?;
            url_policy::validate_public_url(&mapped).ok()?;
        }
    }
    let path = url.path().to_ascii_lowercase();
    if [".mp3", ".m4a", ".aac", ".wav", ".flac", ".ogg", ".opus"]
        .iter()
        .any(|suffix| path.ends_with(suffix))
        || host.contains("ies-music")
    {
        return None;
    }

    if !official_endpoint(&url) {
        if url
            .query_pairs()
            .any(|(key, value)| key.eq_ignore_ascii_case("watermark") && value == "1")
        {
            return None;
        }
        // CDN signatures and unrelated occurrences of "playwm" are opaque.
        return Some(value.to_owned());
    }

    let marked_path = url.path() == "/aweme/v1/playwm/";
    if marked_path {
        url.set_path("/aweme/v1/play/");
    }
    let mut changed_query = false;
    let mut pairs = Vec::new();
    let ratio = quality_label(quality).to_ascii_lowercase();
    let mut has_ratio = false;
    let mut has_watermark = false;
    for raw_pair in url.query().into_iter().flat_map(|query| query.split('&')) {
        let Some((key, value)) = url::form_urlencoded::parse(raw_pair.as_bytes()).next() else {
            pairs.push(raw_pair.to_owned());
            continue;
        };
        if key.eq_ignore_ascii_case("watermark") {
            has_watermark = true;
            if value != "0" {
                pairs.push("watermark=0".to_owned());
                changed_query = true;
            } else {
                pairs.push(raw_pair.to_owned());
            }
        } else if key.eq_ignore_ascii_case("ratio") {
            has_ratio = true;
            if value != ratio {
                pairs.push(format!("ratio={ratio}"));
                changed_query = true;
            } else {
                pairs.push(raw_pair.to_owned());
            }
        } else {
            pairs.push(raw_pair.to_owned());
        }
    }
    if !has_ratio {
        pairs.push(format!("ratio={ratio}"));
        changed_query = true;
    }
    if !has_watermark {
        pairs.push("watermark=0".to_owned());
        changed_query = true;
    }
    if changed_query {
        url.set_query(Some(&pairs.join("&")));
    } else if !marked_path {
        return Some(value.to_owned());
    }
    Some(url.into())
}

fn official_endpoint(url: &Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    ["douyin.com", "iesdouyin.com", "amemv.com", "snssdk.com"]
        .iter()
        .any(|root| host == *root || host.ends_with(&format!(".{root}")))
        && matches!(url.port(), None | Some(80) | Some(443))
        && matches!(url.path(), "/aweme/v1/play/" | "/aweme/v1/playwm/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn uri_selects_official_clean_endpoint_and_download_only_supplies_its_identifier() {
        let video = json!({"play_addr": {"uri": "v0300_ABC-123", "url_list": ["https://v26.douyinvod.com/original.mp4"]}});
        let urls = candidates(Some(&video));
        let primary = Url::parse(&urls[0]).unwrap();
        assert_eq!(primary.host_str(), Some("www.iesdouyin.com"));
        assert_eq!(primary.path(), "/aweme/v1/play/");
        let params: std::collections::HashMap<_, _> = primary.query_pairs().into_owned().collect();
        assert_eq!(params["video_id"], "v0300_ABC-123");
        assert_eq!(params["ratio"], "1080p");
        assert_eq!(params["watermark"], "0");
        assert_eq!(urls[1], "https://v26.douyinvod.com/original.mp4");

        let video = json!({"download_addr": {"uri": "v_valid", "url_list": ["https://v26.douyinvod.com/marked.mp4"]}});
        let urls = candidates(Some(&video));
        assert_eq!(urls.len(), 1);
        assert!(urls[0].contains("video_id=v_valid"));
    }

    #[test]
    fn rejects_malformed_or_audio_identifiers_without_losing_valid_api_addresses() {
        for uri in [
            "",
            " ",
            "v/id",
            "https://evil.test/v",
            "HTTP_example",
            "MP3audio",
            "v%2fid",
            "视频",
            "v?id=3",
            "v&watermark=1",
        ] {
            let video = json!({"play_addr": {"uri": uri, "url_list": ["https://v26.douyinvod.com/video.mp4"]}});
            assert_eq!(
                candidates(Some(&video)),
                vec!["https://v26.douyinvod.com/video.mp4"],
                "{uri}"
            );
        }
        assert!(valid_video_uri(&"a".repeat(MAX_URI_BYTES)));
        assert!(!valid_video_uri(&"a".repeat(MAX_URI_BYTES + 1)));
        assert!(candidates(None).is_empty());
        assert!(candidates(Some(&Value::Null)).is_empty());
    }

    #[test]
    fn official_marked_endpoint_normalization_preserves_other_raw_query_fields() {
        let source = "https://www.iesdouyin.com/aweme/v1/playwm/?video_id=v123&ratio=720p&watermark=1&token=a%2Fb%20c&line=0";
        let url = normalize(source).unwrap();
        assert_eq!(
            url,
            "https://www.iesdouyin.com/aweme/v1/play/?video_id=v123&ratio=1080p&watermark=0&token=a%2Fb%20c&line=0"
        );
        assert_eq!(
            normalize(
                "https://aweme.snssdk.com/aweme/v1/playwm/?video_id=v123&token=a%2Fb%20c&&flag&"
            )
            .unwrap(),
            "https://aweme.snssdk.com/aweme/v1/play/?video_id=v123&token=a%2Fb%20c&&flag&&ratio=1080p&watermark=0"
        );
        assert_eq!(
            normalize("https://www.douyin.com/aweme/v1/play/?watermark=1&ratio=720p").unwrap(),
            "https://www.douyin.com/aweme/v1/play/?watermark=0&ratio=1080p"
        );
    }

    #[test]
    fn normal_signed_cdns_and_unrelated_playwm_strings_are_opaque() {
        for source in [
            "https://playwm.cdn.example/video/playwm/file.mp4?sign=a%2Fb%20c&ratio=720p&token=playwm",
            "https://v26.douyinvod.com/aweme/v1/playwm/?sign=a%2Fb&ratio=720p",
            "https://evil-iesdouyin.com/aweme/v1/playwm/?ratio=720p",
            "https://www.iesdouyin.com.evil.example/aweme/v1/playwm/?ratio=720p",
            "https://www.iesdouyin.com/other/playwm/?ratio=720p",
            "https://www.iesdouyin.com:8443/aweme/v1/playwm/?ratio=720p",
        ] {
            assert_eq!(normalize(source).as_deref(), Some(source));
        }
        assert!(
            normalize("https://v26.douyinvod.com/video.mp4?watermark=1&token=signed").is_none()
        );
        assert!(normalize("https://evil-iesdouyin.com/aweme/v1/playwm/?watermark=1").is_none());
    }

    #[test]
    fn rejects_private_credentials_non_http_and_audio_urls() {
        for source in [
            "http://127.0.0.1/video.mp4",
            "http://192.168.1.3/video.mp4",
            "http://[::1]/video.mp4",
            "http://[::ffff:127.0.0.1]/video.mp4",
            "http://localhost/video.mp4",
            "http://device.local/video.mp4",
            "https://user:pass@www.douyin.com/aweme/v1/play/",
            "file:///tmp/video.mp4",
            "javascript:alert(1)",
            "https://v26.douyinvod.com/audio.MP3?token=1",
            "https://v26.douyinvod.com/audio.m4a",
            "https://sf3.ies-music.example/clip?id=1",
        ] {
            assert!(normalize(source).is_none(), "{source}");
        }
    }

    #[test]
    fn explicit_h264_is_sorted_by_resolution_then_rate_ahead_of_unknown_and_hevc() {
        let video = json!({
            "play_addr": {"url_list": ["https://v26.douyinvod.com/unknown.mp4"]},
            "bit_rate": [
                {"is_h265": 1, "bit_rate": 99999999, "play_addr": {"width": 3840, "height": 2160, "url_list": ["https://v26.douyinvod.com/hevc.mp4"]}},
                {"bit_rate": 99999999, "play_addr": {"width": 3840, "height": 2160, "url_list": ["https://v26.douyinvod.com/unknown-stream.mp4"]}},
                {"is_h265": 0, "bit_rate": 9000, "play_addr": {"width": 640, "height": 360, "url_list": ["https://v26.douyinvod.com/h264-low.mp4"]}},
                {"is_h265": 0, "bit_rate": 1000, "play_addr": {"width": 1920, "height": 1080, "url_list": ["https://v26.douyinvod.com/h264-high.mp4"]}}
            ]
        });
        assert_eq!(
            candidates(Some(&video)),
            vec![
                "https://v26.douyinvod.com/h264-high.mp4",
                "https://v26.douyinvod.com/h264-low.mp4",
                "https://v26.douyinvod.com/unknown.mp4"
            ]
        );
    }

    #[test]
    fn original_play_address_stays_ahead_of_hevc_only_bitrate_alternatives() {
        let video = json!({
            "play_addr": {"uri": "v_video", "url_list": ["https://v26.douyinvod.com/original.mp4"]},
            "bit_rate": [{"is_h265": 1, "bit_rate": 100, "play_addr": {"url_list": ["https://v26.douyinvod.com/hevc.mp4"]}}]
        });
        let urls = candidates(Some(&video));
        assert!(urls[0].contains("video_id=v_video"));
        assert_eq!(urls[1], "https://v26.douyinvod.com/original.mp4");
        assert_eq!(urls[2], "https://v26.douyinvod.com/hevc.mp4");
    }

    #[test]
    fn fallback_urls_are_deduplicated_and_bounded_and_invalid_h264_does_not_hide_hevc() {
        let video = json!({
            "play_addr_h264": {"url_list": ["http://127.0.0.1/private.mp4"]},
            "play_addr": {"url_list": ["https://v26.douyinvod.com/a.mp4", "https://v26.douyinvod.com/a.mp4", "https://v26.douyinvod.com/b.mp4"]},
            "bit_rate": [{"is_h265": 1, "play_addr": {"url_list": ["https://v26.douyinvod.com/c.mp4", "https://v26.douyinvod.com/d.mp4"]}}]
        });
        assert_eq!(
            candidates(Some(&video)),
            vec![
                "https://v26.douyinvod.com/a.mp4",
                "https://v26.douyinvod.com/b.mp4",
                "https://v26.douyinvod.com/c.mp4"
            ]
        );
    }

    #[test]
    fn configured_ratios_are_requested_without_claiming_actual_source_dimensions() {
        let video = json!({"width": 576, "height": 1024, "play_addr": {"uri": "v_actual", "url_list": ["https://v26.douyinvod.com/original.mp4"]}});
        for quality in ["360P", "480P", "720P", "1080P"] {
            let selection = candidates_with_quality(Some(&video), quality);
            let primary = Url::parse(&selection.urls[0]).unwrap();
            let pairs: std::collections::HashMap<_, _> =
                primary.query_pairs().into_owned().collect();
            assert_eq!(pairs["video_id"], "v_actual");
            assert_eq!(pairs["ratio"], quality.to_ascii_lowercase());
            assert_eq!(pairs["watermark"], "0");
            assert_eq!(selection.video_quality, Some(format!("请求{quality}")));
            assert!(
                selection
                    .quality_notice
                    .unwrap()
                    .contains("平台可能返回其他画质")
            );
            assert_eq!(selection.urls[1], "https://v26.douyinvod.com/original.mp4");
        }
    }

    #[test]
    fn quality_normalization_only_changes_the_official_playback_request() {
        let source = "https://www.iesdouyin.com/aweme/v1/playwm/?video_id=v123&ratio=1080p&watermark=1&token=a%2Fb%20c";
        assert_eq!(
            normalize_with_quality(source, "720P").unwrap(),
            "https://www.iesdouyin.com/aweme/v1/play/?video_id=v123&ratio=720p&watermark=0&token=a%2Fb%20c"
        );
        let cdn = "https://v26.douyinvod.com/video.mp4?sign=a%2Fb%20c&ratio=1080p";
        assert_eq!(normalize_with_quality(cdn, "360P").as_deref(), Some(cdn));
        assert_eq!(quality_label(" 480p "), "480P");
        assert_eq!(quality_label("invalid"), "1080P");
    }

    #[test]
    fn closest_known_h264_sources_precede_base_address_and_hevc() {
        let video = json!({
            "play_addr_h264": {"width": 1920, "height": 1080, "url_list": ["https://v26.douyinvod.com/base.mp4"]},
            "bit_rate": [
                {"is_h265": 1, "play_addr": {"width": 1280, "height": 720, "url_list": ["https://v26.douyinvod.com/hevc.mp4"]}},
                {"is_h265": 0, "bit_rate": 1000, "play_addr": {"width": 640, "height": 360, "url_list": ["https://v26.douyinvod.com/360.mp4"]}},
                {"is_h265": 0, "bit_rate": 1000, "play_addr": {"width": 854, "height": 480, "url_list": ["https://v26.douyinvod.com/480.mp4"]}},
                {"is_h265": 0, "bit_rate": 1000, "play_addr": {"width": 720, "height": 1280, "url_list": ["https://v26.douyinvod.com/720.mp4"]}},
                {"is_h265": 0, "bit_rate": 9999, "play_addr": {"url_list": ["https://v26.douyinvod.com/unknown.mp4"]}}
            ]
        });
        for (quality, filename) in [
            ("360P", "360"),
            ("480P", "480"),
            ("720P", "720"),
            ("1080P", "base"),
        ] {
            let selection = candidates_with_quality(Some(&video), quality);
            assert_eq!(
                selection.urls[0],
                format!("https://v26.douyinvod.com/{filename}.mp4")
            );
            assert_eq!(selection.video_quality.as_deref(), Some(quality));
            assert!(selection.quality_notice.is_none());
            assert!(selection.urls.iter().all(|url| !url.contains("hevc")));
        }
    }

    #[test]
    fn unavailable_quality_uses_nearest_known_size_and_reports_it() {
        let video = json!({"bit_rate": [
            {"is_h265": 0, "play_addr": {"width": 576, "height": 1024, "url_list": ["https://v26.douyinvod.com/lower.mp4"]}},
            {"is_h265": 0, "play_addr": {"width": 1536, "height": 864, "url_list": ["https://v26.douyinvod.com/higher.mp4"]}}
        ]});
        let selection = candidates_with_quality(Some(&video), "720P");
        assert_eq!(selection.urls[0], "https://v26.douyinvod.com/lower.mp4");
        assert_eq!(selection.video_quality.as_deref(), Some("576P"));
        assert!(selection.quality_notice.unwrap().contains("首选 720P"));

        let unknown = json!({"play_addr": {"url_list": ["https://v26.douyinvod.com/unknown.mp4"]}});
        let selection = candidates_with_quality(Some(&unknown), "480P");
        assert!(selection.video_quality.is_none());
        assert!(
            selection
                .quality_notice
                .unwrap()
                .contains("平台未返回源尺寸")
        );
    }
}
