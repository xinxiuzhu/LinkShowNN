//! 小红书分享页：数据在 `window.__INITIAL_STATE__`。
//!
//! 优先使用浏览器执行后的状态；静态 payload 是 JS 字面量而非严格 JSON，
//! 含裸 `undefined`，先替换成 `null` 才能解析。需要登录 Cookie 才能拿到完整笔记内容。

use serde_json::Value;

use crate::{
    resolver::fetch::{FetchedPage, PageFetcher},
    resolver::page::{captured_json, find_object_with_key, find_string},
    resolver::sites::PageAdapter,
    resolver::{MediaInfo, Platform},
};

const STATE_PATTERN: &str = r"(?s)window\.__INITIAL_STATE__\s*=\s*(.*?)\s*</script>";

pub struct XiaohongshuAdapter {
    fetcher: PageFetcher,
    cookie: String,
}

impl XiaohongshuAdapter {
    pub fn new(fetcher: PageFetcher, cookie: String) -> Self {
        Self { fetcher, cookie }
    }
}

impl PageAdapter for XiaohongshuAdapter {
    fn platform(&self) -> Platform {
        Platform::Xiaohongshu
    }

    fn page_cookie(&self) -> Option<&str> {
        Some(&self.cookie)
    }

    fn fetcher(&self) -> &PageFetcher {
        &self.fetcher
    }

    fn enrich(&self, page: &FetchedPage, media: &mut MediaInfo) {
        enrich(&page.html, page.state.as_ref(), media);
    }
}

fn enrich(html: &str, state: Option<&Value>, media: &mut MediaInfo) {
    let inline_state = state
        .is_none()
        .then(|| captured_json(&html.replace("undefined", "null"), STATE_PATTERN))
        .flatten();
    let Some(state) = state.or(inline_state.as_ref()) else {
        return;
    };
    let note = find_object_with_key(state, "imageList").unwrap_or(state);
    media.title = string_field(note, &["title"]).or_else(|| media.title.clone());
    media.description = string_field(note, &["desc"]).or_else(|| media.description.clone());
    let user = note.get("user").unwrap_or(note);
    media.author = string_field(user, &["nickname", "nickName"]);
    media.avatar_url =
        string_field(user, &["avatar", "avatarUrl", "image"]).filter(|value| is_image_url(value));
    // 小红书笔记状态的 time 为 Unix 毫秒；不把 lastUpdateTime 当首次发布时间。
    media.published_at = note
        .get("time")
        .and_then(Value::as_i64)
        .filter(|time| *time > 0)
        .map(|time| time / 1000);
    // 限定笔记 imageList，每张图片只选一个尺寸；不能递归收集头像或视频地址。
    media.images = note
        .get("imageList")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(image_url)
        .take(20)
        .collect();
    media.cover_url = media
        .images
        .first()
        .cloned()
        .or_else(|| media.cover_url.clone());
    media.video_url = note
        .get("video")
        .and_then(|video| find_string(video, &["masterUrl", "video_url"]))
        .or_else(|| string_field(note, &["video_url"]))
        .or_else(|| media.video_url.clone());
    media.is_video = note.get("type").and_then(Value::as_str) == Some("video");
    if note.get("type").and_then(Value::as_str) == Some("normal") {
        // 明确的图文笔记优先于 OG 元数据，避免把背景音轨或分享页视频当作品。
        media.video_url = None;
        media.is_video = false;
    }
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

fn is_image_url(value: &str) -> bool {
    value.starts_with("https://") || value.starts_with("http://")
}

fn image_url(image: &Value) -> Option<String> {
    ["urlDefault", "urlSizeLarge", "url"]
        .into_iter()
        .find_map(|key| string_field(image, &[key]).filter(|value| is_image_url(value)))
        .or_else(|| {
            image
                .get("infoList")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .find_map(|info| string_field(info, &["url"]).filter(|value| is_image_url(value)))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_state_containing_bare_undefined() {
        let html = r#"<script>window.__INITIAL_STATE__ = {"note":{"title":"笔记","nickname":"作者","imageList":[{"urlDefault":"https://sns.example/a.jpg"}],"extra":undefined}} </script>"#;
        let mut media = MediaInfo::default();
        enrich(html, None, &mut media);
        assert_eq!(media.title.as_deref(), Some("笔记"));
        assert_eq!(media.author.as_deref(), Some("作者"));
        assert_eq!(media.images, vec!["https://sns.example/a.jpg"]);
        assert_eq!(
            media.cover_url.as_deref(),
            Some("https://sns.example/a.jpg")
        );
    }

    #[test]
    fn prefers_executed_state_over_stale_inline_state() {
        let html = r#"<script>window.__INITIAL_STATE__ = {"note":{"title":"旧笔记","imageList":[{"urlDefault":"https://sns.example/old.jpg"}]}}</script>"#;
        let state = serde_json::json!({
            "note": {
                "title": "动态笔记",
                "nickname": "作者",
                "imageList": [{"urlDefault": "https://sns.example/new.jpg"}]
            }
        });
        let mut media = MediaInfo::default();
        enrich(html, Some(&state), &mut media);
        assert_eq!(media.title.as_deref(), Some("动态笔记"));
        assert_eq!(media.author.as_deref(), Some("作者"));
        assert_eq!(media.images, vec!["https://sns.example/new.jpg"]);
    }

    #[test]
    fn keeps_post_images_separate_from_avatar_and_video_urls() {
        let state = serde_json::json!({"note": {
            "title": "笔记标题", "desc": "笔记正文", "type": "video",
            "time": 1700000000123_i64,
            "user": {"nickname": "作者", "avatar": "https://sns.example/avatar.jpg"},
            "imageList": [
                {"urlDefault": "https://sns.example/cover.jpg", "urlSizeLarge": "https://sns.example/cover-large.jpg"},
                {"infoList": [{"url": "https://sns.example/second.jpg"}]}
            ],
            "video": {"masterUrl": "https://sns.example/video.mp4"},
            "unrelated": {"url": "https://sns.example/unrelated.jpg"}
        }});
        let mut media = MediaInfo::default();
        enrich("", Some(&state), &mut media);
        assert_eq!(media.description.as_deref(), Some("笔记正文"));
        assert_eq!(media.author.as_deref(), Some("作者"));
        assert_eq!(
            media.avatar_url.as_deref(),
            Some("https://sns.example/avatar.jpg")
        );
        assert_eq!(media.published_at, Some(1_700_000_000));
        assert_eq!(
            media.images,
            [
                "https://sns.example/cover.jpg",
                "https://sns.example/second.jpg"
            ]
        );
        assert_eq!(
            media.video_url.as_deref(),
            Some("https://sns.example/video.mp4")
        );
        assert!(media.is_video);
    }

    #[test]
    fn image_note_clears_video_metadata_from_the_share_page() {
        let state = serde_json::json!({"note": {
            "type": "normal",
            "imageList": [{"urlDefault": "https://sns.example/image.jpg"}],
            "video": {"masterUrl": "https://sns.example/soundtrack.mp3"}
        }});
        let mut media = MediaInfo {
            video_url: Some("https://sns.example/stale-og.mp4".into()),
            is_video: true,
            ..Default::default()
        };
        enrich("", Some(&state), &mut media);
        assert_eq!(media.images, ["https://sns.example/image.jpg"]);
        assert!(media.video_url.is_none());
        assert!(!media.is_video);
        use crate::resolver::ResolvedContent;
        assert!(media.card().metadata().video_cover_url.is_none());
        assert!(media.download_candidate(0).is_none());
    }
}
