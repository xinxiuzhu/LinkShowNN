//! 小红书内容与公开互动信息的展示装配。

use crate::{
    download::{DownloadCandidate, SkipReason},
    resolver::{Card, CardPoster, MediaInfo, ResolvedContent},
};

use super::engagement::Engagement;

#[derive(Debug)]
pub(crate) struct XiaohongshuContent {
    pub media: MediaInfo,
    pub engagement: Engagement,
}

impl XiaohongshuContent {
    /// 两次取页已校验为同一笔记。优先保留桌面端兼容的播放源与完整正文，
    /// 移动端只补缺失内容、统计和匿名可见评论。
    pub fn enrich(&mut self, other: Self) {
        self.media.title = self.media.title.take().or(other.media.title);
        self.media.description = self.media.description.take().or(other.media.description);
        self.media.cover_url = self.media.cover_url.take().or(other.media.cover_url);
        self.media.author = self.media.author.take().or(other.media.author);
        self.media.avatar_url = self.media.avatar_url.take().or(other.media.avatar_url);
        self.media.published_at = self.media.published_at.or(other.media.published_at);
        if self.media.is_video && other.media.is_video {
            self.media.video_duration_seconds = self
                .media
                .video_duration_seconds
                .or(other.media.video_duration_seconds);
        }
        if self.media.is_video && self.media.video_url.is_none() && other.media.is_video {
            self.media.video_url = other.media.video_url;
            self.media.video_fallback_urls = other.media.video_fallback_urls;
            self.media.video_quality = other.media.video_quality;
            self.media.quality_notice = other.media.quality_notice;
        }
        self.engagement.merge(other.engagement);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mobile_cover_can_complete_a_pc_video_without_replacing_its_playback() {
        let mut pc = XiaohongshuContent {
            media: MediaInfo {
                is_video: true,
                title: Some("PC 标题".into()),
                video_url: Some("https://sns.example/h264.mp4".into()),
                video_quality: Some("720P".into()),
                quality_notice: Some("请求 1080P，本次获取 720P".into()),
                ..Default::default()
            },
            engagement: Engagement::default(),
        };
        pc.enrich(XiaohongshuContent {
            media: MediaInfo {
                is_video: true,
                title: Some("H5 标题".into()),
                description: Some("移动端补充简介".into()),
                cover_url: Some("https://sns.example/cover.jpg".into()),
                video_url: Some("https://sns.example/hevc.mp4".into()),
                video_duration_seconds: Some(481),
                video_quality: Some("1080P".into()),
                quality_notice: Some("移动源提示".into()),
                ..Default::default()
            },
            engagement: Engagement::default(),
        });
        assert_eq!(
            pc.download_candidate(0).unwrap().url,
            "https://sns.example/h264.mp4"
        );
        let card = pc.card();
        let poster = card.metadata().poster.as_ref().unwrap();
        assert_eq!(poster.title, "PC 标题");
        assert_eq!(poster.description.as_deref(), Some("移动端补充简介"));
        assert_eq!(poster.video_quality.as_deref(), Some("720P"));
        assert_eq!(
            poster.quality_notice.as_deref(),
            Some("请求 1080P，本次获取 720P")
        );
        assert!(!card.is_gallery());
        assert!(pc.download_candidate(480).is_none());
        assert_eq!(
            pc.skip_reason(480),
            Some(SkipReason::DurationExceeded {
                duration: 481,
                maximum: 480,
            })
        );
        assert!(pc.skip_reason(0).is_none());
    }

    #[test]
    fn mobile_playback_and_its_quality_complete_a_pc_cover_together() {
        let mut pc = XiaohongshuContent {
            media: MediaInfo {
                is_video: true,
                cover_url: Some("https://sns.example/cover.jpg".into()),
                ..Default::default()
            },
            engagement: Engagement::default(),
        };
        pc.enrich(XiaohongshuContent {
            media: MediaInfo {
                is_video: true,
                video_url: Some("https://sns.example/hevc.mp4".into()),
                video_fallback_urls: vec!["https://backup.example/hevc.mp4".into()],
                video_quality: Some("1080P".into()),
                quality_notice: Some("请求 480P，本次获取 1080P".into()),
                ..Default::default()
            },
            engagement: Engagement::default(),
        });
        assert_eq!(pc.media.video_quality.as_deref(), Some("1080P"));
        assert_eq!(
            pc.media.quality_notice.as_deref(),
            Some("请求 480P，本次获取 1080P")
        );
        assert_eq!(
            pc.media.video_fallback_urls,
            ["https://backup.example/hevc.mp4"]
        );
    }
}

impl ResolvedContent for XiaohongshuContent {
    fn card(&self) -> Card {
        let card = self.media.card();
        let mut metadata = card.metadata().clone();
        if metadata.poster.is_some() || self.engagement.stats != CardPoster::default() {
            let poster = metadata.poster.get_or_insert_with(CardPoster::default);
            let stats = &self.engagement.stats;
            poster.like_count = stats.like_count;
            poster.comment_count = stats.comment_count;
            poster.favorite_count = stats.favorite_count;
            poster.share_count = stats.share_count;
            poster.stats_display = stats.stats_display.clone();
        }
        metadata.comments = self.engagement.top_comments();
        card.with_metadata(metadata)
    }

    fn filter_text(&self) -> String {
        self.media.filter_text()
    }

    fn download_candidate(&self, maximum_duration: u64) -> Option<DownloadCandidate> {
        self.media.download_candidate(maximum_duration)
    }

    fn skip_reason(&self, maximum_duration: u64) -> Option<SkipReason> {
        self.media.skip_reason(maximum_duration)
    }
}
