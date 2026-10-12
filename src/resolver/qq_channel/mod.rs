//! QQ 频道分享页：视频地址只出现在裸 CDN 链接里，没有内联状态可用。

use regex::Regex;

use crate::{
    resolver::adapter::PageAdapter,
    resolver::fetch::{FetchedPage, PageFetcher},
    resolver::page::decode_html,
    resolver::{MediaInfo, Platform},
};

const VIDEO_PATTERN: &str =
    r#"(?i)https?://qchannelvideo\.photo\.qq\.com/[^"'<>\\\s]+?\.mp4(?:\?[^"'<>\\\s]+)?"#;

pub struct QqChannelAdapter {
    fetcher: PageFetcher,
}

impl QqChannelAdapter {
    pub fn new(fetcher: PageFetcher) -> Self {
        Self { fetcher }
    }
}

impl PageAdapter for QqChannelAdapter {
    fn platform(&self) -> Platform {
        Platform::QqChannel
    }

    fn fetcher(&self) -> &PageFetcher {
        &self.fetcher
    }

    fn enrich(&self, page: &FetchedPage, media: &mut MediaInfo) {
        enrich(&page.html, media);
    }
}

fn enrich(html: &str, media: &mut MediaInfo) {
    media.video_url = Regex::new(VIDEO_PATTERN)
        .ok()
        .and_then(|regex| regex.find(html))
        .map(|value| decode_html(value.as_str()))
        .or_else(|| media.video_url.clone());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolver::page::page_metadata;
    use url::Url;

    #[test]
    fn extracts_inline_cdn_video_url() {
        let html = r#"<meta property="og:title" content="频道视频"> https://qchannelvideo.photo.qq.com/a.mp4"#;
        let mut media = page_metadata(
            html,
            &Url::parse("https://pd.qq.com/s/1").unwrap(),
            Platform::QqChannel,
        );
        enrich(html, &mut media);
        assert_eq!(media.title.as_deref(), Some("频道视频"));
        assert_eq!(
            media.video_url.as_deref(),
            Some("https://qchannelvideo.photo.qq.com/a.mp4")
        );
    }
}
