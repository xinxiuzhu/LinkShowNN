//! 分享页类内容的共用形态。
//!
//! 抖音、快手、小红书、Twitter、库街区、QQ 频道都是「标题 + 简介 + 图集 +
//! 可选单条音视频」，共用一个结构是合理的；B 站那种带 DASH 分流和分 P 的形态
//! 不套进来，它在自己的平台模块里另立类型。

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{Card, CardMetadata, CardPoster, Platform, ResolvedContent};
use crate::download::DownloadCandidate;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MediaInfo {
    pub title: Option<String>,
    pub description: Option<String>,
    pub cover_url: Option<String>,
    pub video_url: Option<String>,
    pub audio_url: Option<String>,
    pub images: Vec<String>,
    pub author: Option<String>,
    #[serde(default)]
    pub avatar_url: Option<String>,
    /// Unix 秒；平台返回毫秒时在对应 mapper 转换。
    #[serde(default)]
    pub published_at: Option<i64>,
    /// 即使暂时取不到播放地址，视频作品也可以生成封面海报。
    #[serde(default)]
    pub is_video: bool,
    pub source_url: String,
    /// 解析出该媒体的平台，下载阶段据此决定 CDN Referer。
    pub platform: Platform,
    pub raw: Value,
}

impl ResolvedContent for MediaInfo {
    fn card(&self) -> Card {
        let description = self
            .description
            .as_deref()
            .filter(|text| {
                self.title
                    .as_deref()
                    .is_none_or(|title| title.trim() != text.trim())
            })
            .map(str::to_string);
        let title = self
            .title
            .clone()
            .unwrap_or_else(|| "链接解析结果".to_string());
        let video_cover_url = (self.is_video || self.video_url.is_some())
            .then(|| self.cover_url.clone().filter(|url| !url.trim().is_empty()))
            .flatten();
        // 图集仍按多图布局完整显示，不因同时含视频标志而只留下封面。
        let has_only_cover = self
            .images
            .iter()
            .all(|url| Some(url.as_str()) == video_cover_url.as_deref());
        let poster = (video_cover_url.is_some() && has_only_cover).then(|| CardPoster {
            title: title.clone(),
            description: description.clone(),
            ..Default::default()
        });
        let card = Card::new()
            .with_metadata(CardMetadata {
                platform: Some(self.platform),
                author: self.author.clone(),
                avatar_url: self.avatar_url.clone(),
                published_at: self.published_at,
                source_url: (!self.source_url.is_empty()).then(|| self.source_url.clone()),
                video_cover_url,
                poster,
                comments: Vec::new(),
            })
            .line(title)
            .line_opt(description)
            .line(self.source_url.clone());

        // 封面若已在图集里，跳过以免同一张图发两次。
        let cover = self
            .cover_url
            .clone()
            .filter(|url| !self.images.iter().any(|image| image == url));
        // Card 保留完整内容，渲染/投递层按各自预算截断并说明省略部分。
        card.image_opt(cover).images(self.images.iter().cloned())
    }

    fn filter_text(&self) -> String {
        format!(
            "{}\n{}",
            self.title.clone().unwrap_or_default(),
            self.description.clone().unwrap_or_default()
        )
    }

    fn download_candidate(&self, _maximum_duration: u64) -> Option<DownloadCandidate> {
        let referer = self.platform.media_referer();
        if let Some(url) = self.video_url.as_deref() {
            return Some(
                DownloadCandidate::video(url, "linkshownn_video.mp4", referer)
                    .with_cover(self.cover_url.clone()),
            );
        }
        self.audio_url
            .as_deref()
            .map(|url| DownloadCandidate::audio(url, "linkshownn_audio.mp3", referer))
    }
}

#[cfg(test)]
mod tests {
    use super::MediaInfo;
    use crate::download::model::MediaKind;
    use crate::resolver::{Platform, ResolvedContent};

    #[test]
    fn card_falls_back_to_placeholder_title_and_keeps_source() {
        let media = MediaInfo {
            source_url: "https://www.xiaohongshu.com/explore/1".into(),
            platform: Platform::Xiaohongshu,
            ..MediaInfo::default()
        };
        let text = media.card().text();
        assert!(text.starts_with("链接解析结果"));
        assert!(text.contains("https://www.xiaohongshu.com/explore/1"));
    }

    #[test]
    fn cover_is_not_duplicated_when_already_in_gallery() {
        let media = MediaInfo {
            cover_url: Some("https://cdn.example/1.jpg".into()),
            images: vec![
                "https://cdn.example/1.jpg".into(),
                "https://cdn.example/2.jpg".into(),
            ],
            ..MediaInfo::default()
        };
        assert_eq!(
            media.card().image_urls().collect::<Vec<_>>(),
            ["https://cdn.example/1.jpg", "https://cdn.example/2.jpg"]
        );
    }

    #[test]
    fn video_wins_over_audio_and_carries_platform_referer() {
        let media = MediaInfo {
            video_url: Some("https://cdn.example/v.mp4".into()),
            audio_url: Some("https://cdn.example/a.mp3".into()),
            platform: Platform::Douyin,
            ..MediaInfo::default()
        };
        let candidate = media.download_candidate(0).unwrap();
        assert_eq!(candidate.kind, MediaKind::Video);
        assert_eq!(candidate.referer, Platform::Douyin.media_referer());
    }

    #[test]
    fn audio_only_media_produces_audio_candidate() {
        let media = MediaInfo {
            audio_url: Some("https://cdn.example/a.mp3".into()),
            platform: Platform::Ncm,
            ..MediaInfo::default()
        };
        assert_eq!(media.download_candidate(0).unwrap().kind, MediaKind::Audio);
    }

    #[test]
    fn referer_follows_platform_not_share_page_host() {
        // 抖音分享页 host 是 iesdouyin.com，但 CDN 只接受主站 Referer。
        let media = MediaInfo {
            video_url: Some("https://v26-web.douyinvod.com/clip.mp4".into()),
            source_url: "https://www.iesdouyin.com/share/video/7676025808796152186/".into(),
            platform: Platform::Douyin,
            ..MediaInfo::default()
        };
        assert_eq!(
            media.download_candidate(0).unwrap().referer,
            "https://www.douyin.com/"
        );
    }

    #[test]
    fn video_candidate_carries_cover_url_for_thumbnail_download() {
        // 封面要先被下载层落成本地文件才能当 thumb，所以候选必须带上它的 URL。
        let media = MediaInfo {
            video_url: Some("https://cdn.example/v.mp4".into()),
            cover_url: Some("https://cdn.example/c.jpg".into()),
            platform: Platform::Douyin,
            ..MediaInfo::default()
        };
        let candidate = media.download_candidate(0).unwrap();
        assert_eq!(
            candidate.cover_url.as_deref(),
            Some("https://cdn.example/c.jpg")
        );
    }

    #[test]
    fn video_card_keeps_metadata_without_a_downloadable_stream() {
        let media = MediaInfo {
            title: Some("作品".into()),
            description: Some("作品".into()),
            author: Some("作者".into()),
            avatar_url: Some("https://cdn.example/avatar.jpg".into()),
            published_at: Some(1_700_000_000),
            is_video: true,
            cover_url: Some("https://cdn.example/cover.jpg".into()),
            source_url: "https://www.douyin.com/video/1".into(),
            platform: Platform::Douyin,
            ..MediaInfo::default()
        };
        let card = media.card();
        assert_eq!(card.metadata().author.as_deref(), Some("作者"));
        assert_eq!(card.metadata().published_at, Some(1_700_000_000));
        assert_eq!(card.metadata().video_cover_url, media.cover_url);
        let poster = card.metadata().poster.as_ref().unwrap();
        assert_eq!(poster.title, "作品");
        assert!(poster.description.is_none());
        assert!(poster.like_count.is_none());
        assert_eq!(card.body_text(), "作品");
        assert!(card.text().contains(&media.source_url));
        assert!(media.download_candidate(0).is_none());
    }

    #[test]
    fn gallery_and_audio_covers_are_not_marked_as_video() {
        let media = MediaInfo {
            cover_url: Some("https://cdn.example/cover.jpg".into()),
            audio_url: Some("https://cdn.example/audio.mp3".into()),
            ..MediaInfo::default()
        };
        assert!(media.card().metadata().video_cover_url.is_none());
        assert!(media.card().metadata().poster.is_none());
        let video = MediaInfo {
            video_url: Some("https://cdn.example/video.mp4".into()),
            ..media
        };
        assert!(video.card().metadata().video_cover_url.is_some());
        assert!(video.card().metadata().poster.is_some());
    }

    #[test]
    fn video_without_cover_and_galleries_keep_their_existing_layout() {
        let video = MediaInfo {
            is_video: true,
            ..MediaInfo::default()
        };
        assert!(video.card().metadata().poster.is_none());
        let gallery = MediaInfo {
            is_video: true,
            cover_url: Some("https://cdn.example/cover.jpg".into()),
            images: vec!["https://cdn.example/gallery.jpg".into()],
            ..MediaInfo::default()
        };
        assert!(gallery.card().metadata().poster.is_none());
        assert_eq!(gallery.card().image_urls().count(), 2);
    }

    #[test]
    fn video_cover_repeated_in_image_list_still_uses_poster() {
        let cover = "https://cdn.example/cover.jpg";
        let video = MediaInfo {
            title: Some("小红书视频".into()),
            is_video: true,
            cover_url: Some(cover.into()),
            images: vec![cover.into()],
            platform: Platform::Xiaohongshu,
            ..MediaInfo::default()
        };
        let card = video.card();
        assert_eq!(card.metadata().poster.as_ref().unwrap().title, "小红书视频");
        assert_eq!(card.image_urls().collect::<Vec<_>>(), [cover]);
    }

    #[test]
    fn card_preserves_long_description_and_twenty_images_for_rendering() {
        let description = format!("{}末尾正文", "长简介".repeat(800));
        let images: Vec<_> = (0..20)
            .map(|index| format!("https://cdn.example/{index}.jpg"))
            .collect();
        let media = MediaInfo {
            title: Some("完整图文".into()),
            description: Some(description.clone()),
            cover_url: images.first().cloned(),
            images: images.clone(),
            source_url: "https://www.xiaohongshu.com/explore/1".into(),
            platform: Platform::Xiaohongshu,
            ..MediaInfo::default()
        };
        let card = media.card();
        assert_eq!(card.body_text(), format!("完整图文\n{description}"));
        assert_eq!(card.image_urls().collect::<Vec<_>>(), images);
        assert_eq!(media.filter_text(), format!("完整图文\n{description}"));
    }
}
