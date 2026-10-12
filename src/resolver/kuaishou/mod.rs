//! 快手分享页：数据在 `window.INIT_STATE` 的 `photo` 对象里。
//!
//! 图集不给完整 URL，只给 CDN 主机加相对路径，需要自己拼接。

use serde_json::Value;

use crate::{
    resolver::adapter::PageAdapter,
    resolver::fetch::{FetchedPage, PageFetcher},
    resolver::page::{captured_json, find_object_with_key, first_nested_url},
    resolver::{MediaInfo, Platform},
};

const STATE_PATTERN: &str = r"(?s)window\.INIT_STATE\s*=\s*(.*?)</script>";

pub struct KuaishouAdapter {
    fetcher: PageFetcher,
}

impl KuaishouAdapter {
    pub fn new(fetcher: PageFetcher) -> Self {
        Self { fetcher }
    }
}

impl PageAdapter for KuaishouAdapter {
    fn platform(&self) -> Platform {
        Platform::Kuaishou
    }

    fn fetcher(&self) -> &PageFetcher {
        &self.fetcher
    }

    fn enrich(&self, page: &FetchedPage, media: &mut MediaInfo) {
        enrich(&page.html, media);
    }
}

fn enrich(html: &str, media: &mut MediaInfo) {
    let Some(state) = captured_json(html, STATE_PATTERN) else {
        return;
    };
    enrich_state(&state, media);
}

fn enrich_state(state: &Value, media: &mut MediaInfo) {
    let Some(photo) = find_object_with_key(state, "photo").and_then(|object| object.get("photo"))
    else {
        return;
    };
    media.title = photo
        .get("caption")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| media.title.clone());
    media.author = photo
        .get("userName")
        .and_then(Value::as_str)
        .map(str::to_string);
    media.video_url = first_nested_url(photo.get("mainMvUrls")).or_else(|| media.video_url.clone());
    media.cover_url = first_nested_url(photo.get("coverUrls")).or_else(|| media.cover_url.clone());
    if let Some(atlas) = photo.get("ext_params").and_then(|value| value.get("atlas")) {
        media.images = atlas_images(atlas);
        if !media.images.is_empty() {
            // 图集可能伴随背景音轨；有效图片列表不能被当成单条视频下载或标记。
            media.video_url = None;
            media.is_video = false;
        }
    }
}

/// 图集的 `list` 是相对路径，须与 `cdnList` 的主机拼成完整 URL。
fn atlas_images(atlas: &Value) -> Vec<String> {
    let Some(cdn) = atlas
        .get("cdnList")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .and_then(|item| item.get("cdn"))
        .and_then(Value::as_str)
    else {
        return Vec::new();
    };
    atlas
        .get("list")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(|path| format!("https://{cdn}/{path}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_video_from_init_state() {
        let html = r#"<script>window.INIT_STATE = {"x":{"photo":{"caption":"作品","userName":"作者","mainMvUrls":[{"url":"https://cdn/video.mp4"}],"coverUrls":[{"url":"https://cdn/cover.jpg"}]}}}</script>"#;
        let mut media = MediaInfo::default();
        enrich(html, &mut media);
        assert_eq!(media.title.as_deref(), Some("作品"));
        assert_eq!(media.author.as_deref(), Some("作者"));
        assert_eq!(media.video_url.as_deref(), Some("https://cdn/video.mp4"));
    }

    #[test]
    fn joins_atlas_paths_onto_cdn_host() {
        let html = r#"<script>window.INIT_STATE = {"x":{"photo":{"caption":"图集","ext_params":{"atlas":{"cdnList":[{"cdn":"img.example"}],"list":["a.jpg","b.jpg"]}}}}}</script>"#;
        let mut media = MediaInfo::default();
        enrich(html, &mut media);
        assert_eq!(
            media.images,
            vec!["https://img.example/a.jpg", "https://img.example/b.jpg"]
        );
    }

    #[test]
    fn atlas_clears_soundtrack_and_open_graph_video_fallback() {
        let state = serde_json::json!({"photo": {
            "mainMvUrls": [{"url": "https://cdn.example/soundtrack.mp3"}],
            "coverUrls": [{"url": "https://img.example/cover.jpg"}],
            "ext_params": {"atlas": {
                "cdnList": [{"cdn": "img.example"}],
                "list": ["a.jpg", "b.jpg"]
            }}
        }});
        let mut media = MediaInfo {
            video_url: Some("https://cdn.example/stale-og.mp4".into()),
            is_video: true,
            ..Default::default()
        };
        enrich_state(&state, &mut media);
        assert_eq!(media.images.len(), 2);
        assert!(media.video_url.is_none());
        assert!(!media.is_video);
        use crate::resolver::ResolvedContent;
        assert!(media.card().metadata().video_cover_url.is_none());
        assert!(media.download_candidate(0).is_none());
    }
}
