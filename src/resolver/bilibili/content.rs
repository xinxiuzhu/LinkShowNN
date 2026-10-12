//! B 站的内容形态。
//!
//! 视频有 DASH 分流、分 P 和播放统计，直播有开播状态和人气，收藏夹是条目
//! 列表——这三种形态的字段互不相通，所以各立类型、各自排版，不套用分享页
//! 平台的通用结构。

use super::{
    mapping::normalize_url,
    model::{FavoriteInfo, LiveInfo, VideoInfo},
};

use crate::download::{DownloadCandidate, SkipReason};
use crate::resolver::{Card, CardMetadata, CardPoster, Platform, ResolvedContent};

/// 访问 B 站媒体 CDN 必须带的 Referer，否则返回 403。
const REFERER: &str = "https://www.bilibili.com/";
/// 收藏夹卡片最多列几条。
const FAVORITE_PREVIEW: usize = 10;

/// 稿件与番剧。两者播放接口不同但展示与下载形态一致，共用本类型。
#[derive(Debug, Clone)]
pub struct VideoContent(pub VideoInfo);

impl VideoContent {
    /// 分 P 有独立时长，选中分 P 时应按它判断时长上限。
    fn duration(&self) -> u64 {
        self.0
            .selected_page
            .as_ref()
            .map(|page| page.duration)
            .filter(|duration| *duration > 0)
            .unwrap_or(self.0.duration)
    }

    fn exceeds(&self, maximum_duration: u64) -> bool {
        maximum_duration > 0 && self.duration() > maximum_duration
    }

    fn filename(&self) -> String {
        let id = self
            .0
            .bvid
            .clone()
            .or_else(|| self.0.aid.map(|aid| aid.to_string()))
            .unwrap_or_else(|| "video".to_string());
        match self.0.selected_page.as_ref().map(|page| page.page) {
            Some(page) if page > 1 => format!("bilibili_{id}_p{page}.mp4"),
            _ => format!("bilibili_{id}.mp4"),
        }
    }

    fn stats(&self) -> Option<String> {
        let stats = [
            self.0.view_count.map(|v| format!("播放 {v}")),
            self.0.like_count.map(|v| format!("点赞 {v}")),
            self.0.coin_count.map(|v| format!("投币 {v}")),
            self.0.favorite_count.map(|v| format!("收藏 {v}")),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" / ");
        (!stats.is_empty()).then_some(stats)
    }

    fn source_url(&self) -> Option<String> {
        let mut source = self
            .0
            .bvid
            .as_deref()
            .map(|bvid| format!("https://www.bilibili.com/video/{bvid}"))
            .or_else(|| {
                self.0
                    .aid
                    .map(|aid| format!("https://www.bilibili.com/video/av{aid}"))
            })?;
        if let Some(page) = self.0.selected_page.as_ref().filter(|page| page.page > 1) {
            source.push_str(&format!("?p={}", page.page));
        }
        Some(source)
    }
}

impl ResolvedContent for VideoContent {
    fn card(&self) -> Card {
        let description = &self.0.description;
        let part = self
            .0
            .selected_page
            .as_ref()
            .filter(|page| page.page > 1 && !page.part.trim().is_empty())
            .map(|page| format!("分P{}: {}", page.page, page.part));
        let poster = CardPoster {
            cover_url: (!self.0.cover_url.trim().is_empty()).then(|| self.0.cover_url.clone()),
            title: match part.as_deref() {
                Some(part) => format!("{}\n{part}", self.0.title),
                None => self.0.title.clone(),
            },
            description: (!description.trim().is_empty()).then(|| description.clone()),
            content_id: self
                .0
                .bvid
                .clone()
                .or_else(|| self.0.aid.map(|aid| format!("av{aid}"))),
            author_id: self.0.author_mid.map(|mid| mid.to_string()),
            view_count: self.0.view_count,
            danmaku_count: self.0.danmaku_count,
            comment_count: self.0.comment_count,
            like_count: self.0.like_count,
            coin_count: self.0.coin_count,
            favorite_count: self.0.favorite_count,
            share_count: self.0.share_count,
            video_quality: self.0.video_quality.clone(),
            quality_notice: self.0.quality_notice.clone(),
            stats_display: Default::default(),
        };
        Card::new()
            .with_metadata(CardMetadata {
                platform: Some(Platform::Bilibili),
                author: (!self.0.author.trim().is_empty()).then(|| self.0.author.clone()),
                avatar_url: self.0.avatar_url.clone().or_else(|| {
                    self.0
                        .raw
                        .pointer("/owner/face")
                        .and_then(serde_json::Value::as_str)
                        .filter(|face| !face.trim().is_empty())
                        .map(normalize_url)
                }),
                source_url: self.source_url(),
                video_cover_url: (!self.0.cover_url.is_empty()).then(|| self.0.cover_url.clone()),
                published_at: self.0.published_at,
                poster: (!self.0.cover_url.trim().is_empty()).then_some(poster),
                comments: self.0.comments.clone(),
                ..Default::default()
            })
            .line(self.0.title.clone())
            .line_opt(part)
            .line_opt(self.stats())
            .line_opt((!description.is_empty()).then(|| format!("简介:\n{description}")))
            .image(self.0.cover_url.clone())
    }

    fn filter_text(&self) -> String {
        let mut text = format!("{}\n{}", self.0.title, self.0.description);
        for comment in &self.0.comments {
            text.push('\n');
            text.push_str(&comment.author);
            text.push('\n');
            text.push_str(&comment.text);
        }
        text
    }

    fn download_candidate(&self, maximum_duration: u64) -> Option<DownloadCandidate> {
        if self.exceeds(maximum_duration) {
            return None;
        }
        let video_url = self.0.video_urls.first()?.clone();
        let candidate = DownloadCandidate::video(video_url, self.filename(), REFERER)
            .with_fallback_urls(self.0.video_urls.iter().skip(1).cloned())
            .with_cover(Some(self.0.cover_url.clone()));
        if !self.0.is_dash {
            return Some(candidate);
        }
        // DASH 的视频轨不含音频，缺音轨就不能当完整视频发出去。
        let audio_url = self.0.audio_urls.first()?.clone();
        Some(candidate.with_audio(audio_url))
    }

    fn skip_reason(&self, maximum_duration: u64) -> Option<SkipReason> {
        if self.exceeds(maximum_duration) {
            return Some(SkipReason::DurationExceeded {
                duration: self.duration(),
                maximum: maximum_duration,
            });
        }
        Some(SkipReason::NoPlayableStream)
    }
}

/// 直播间。没有可下载媒体，只报状态。
#[derive(Debug, Clone)]
pub struct LiveContent(pub LiveInfo);

impl ResolvedContent for LiveContent {
    fn card(&self) -> Card {
        let status = if self.0.live_status == 1 {
            "直播中"
        } else {
            "未开播"
        };
        let online = self
            .0
            .online
            .map(|value| value.to_string())
            .unwrap_or_else(|| "未知".to_string());
        Card::new()
            .with_metadata(CardMetadata {
                platform: Some(Platform::Bilibili),
                author: self.0.anchor.clone(),
                source_url: Some(format!("https://live.bilibili.com/{}", self.0.room_id)),
                ..Default::default()
            })
            .line("B站直播")
            .line(self.0.title.clone())
            .line_opt(self.0.anchor.clone().map(|name| format!("主播: {name}")))
            .line(format!("状态: {status} 人气: {online}"))
            .line_opt(self.0.area.clone().map(|area| format!("分区: {area}")))
            .image_opt(self.0.cover_url.clone())
    }

    fn filter_text(&self) -> String {
        self.0.title.clone()
    }
}

/// 收藏夹。列表形态，配图取封面而非逐条图片。
#[derive(Debug, Clone)]
pub struct FavoriteContent(pub FavoriteInfo);

impl ResolvedContent for FavoriteContent {
    fn card(&self) -> Card {
        let mut text = format!("B站收藏夹: {}\n共 {} 个内容", self.0.title, self.0.total);
        for item in self.0.items.iter().take(FAVORITE_PREVIEW) {
            let link = item
                .bvid
                .as_deref()
                .map(|bvid| format!("https://www.bilibili.com/video/{bvid}"))
                .or_else(|| {
                    item.aid
                        .map(|aid| format!("https://www.bilibili.com/video/av{aid}"))
                })
                .unwrap_or_default();
            text.push_str(&format!("\n- {}\n  {link}", item.title));
        }
        let shown = self.0.items.len().min(FAVORITE_PREVIEW) as u64;
        let omitted = self
            .0
            .total
            .max(self.0.items.len() as u64)
            .saturating_sub(shown);
        if omitted > 0 {
            text.push_str(&format!("\n另有 {omitted} 条未展示，请打开收藏夹查看。"));
        }
        Card::new()
            .with_metadata(CardMetadata {
                platform: Some(Platform::Bilibili),
                author: self.0.owner.clone(),
                source_url: Some(format!("https://www.bilibili.com/favlist/{}", self.0.id)),
                ..Default::default()
            })
            .line(text)
            .line_opt(self.0.owner.clone().map(|owner| format!("创建者: {owner}")))
    }

    fn filter_text(&self) -> String {
        self.0.title.clone()
    }
}

/// 动态、专栏、opus 等纯图文内容。
#[derive(Debug, Clone)]
pub struct RawContent {
    pub title: String,
    pub content: String,
    pub cover_url: Option<String>,
    pub metadata: CardMetadata,
}

impl ResolvedContent for RawContent {
    fn card(&self) -> Card {
        Card::new()
            .with_metadata(self.metadata.clone())
            .line(self.title.clone())
            .line(self.content.clone())
            .image_opt(self.cover_url.clone())
    }

    fn filter_text(&self) -> String {
        format!("{}\n{}", self.title, self.content)
    }
}

#[cfg(test)]
mod tests {
    use super::super::model::{FavoriteInfo, FavoriteItem, LiveInfo, VideoInfo, VideoPage};
    use super::{FavoriteContent, LiveContent, RawContent, VideoContent};
    use crate::download::SkipReason;
    use crate::resolver::ResolvedContent;

    fn dash_video() -> VideoInfo {
        VideoInfo {
            bvid: Some("BV1xx411c7mD".into()),
            title: "标题".into(),
            cover_url: "https://cdn.example/cover.jpg".into(),
            duration: 100,
            is_dash: true,
            video_urls: vec!["https://cdn.example/v.m4s".into()],
            audio_urls: vec!["https://cdn.example/a.m4s".into()],
            ..VideoInfo::default()
        }
    }

    #[test]
    fn dash_candidate_pairs_audio_and_names_by_bvid() {
        let candidate = VideoContent(dash_video()).download_candidate(480).unwrap();
        assert_eq!(
            candidate.audio_url.as_deref(),
            Some("https://cdn.example/a.m4s")
        );
        assert_eq!(candidate.filename, "bilibili_BV1xx411c7mD.mp4");
        assert_eq!(candidate.referer, "https://www.bilibili.com/");
    }

    #[test]
    fn dash_candidate_preserves_video_backups_and_selected_audio_track() {
        let info = VideoInfo {
            video_urls: vec![
                "https://cdn.example/v.m4s".into(),
                "https://backup-one.example/v.m4s".into(),
                "https://backup-two.example/v.m4s".into(),
            ],
            audio_urls: vec![
                "https://cdn.example/a.m4s".into(),
                "https://backup-one.example/a.m4s".into(),
            ],
            ..dash_video()
        };
        let candidate = VideoContent(info).download_candidate(480).unwrap();
        assert_eq!(candidate.url, "https://cdn.example/v.m4s");
        assert_eq!(
            candidate.fallback_urls,
            [
                "https://backup-one.example/v.m4s",
                "https://backup-two.example/v.m4s"
            ]
        );
        assert_eq!(
            candidate.audio_url.as_deref(),
            Some("https://cdn.example/a.m4s")
        );
    }

    #[test]
    fn progressive_candidate_preserves_backups_without_separate_audio() {
        let info = VideoInfo {
            is_dash: false,
            video_urls: vec![
                "https://cdn.example/v.mp4".into(),
                "https://backup.example/v.mp4".into(),
            ],
            audio_urls: Vec::new(),
            ..dash_video()
        };
        let content = VideoContent(info);
        let candidate = content.download_candidate(100).unwrap();
        assert_eq!(candidate.url, "https://cdn.example/v.mp4");
        assert_eq!(candidate.fallback_urls, ["https://backup.example/v.mp4"]);
        assert!(candidate.audio_url.is_none());
        assert!(content.download_candidate(99).is_none());
        assert_eq!(
            content.skip_reason(99),
            Some(SkipReason::DurationExceeded {
                duration: 100,
                maximum: 99,
            })
        );
    }

    #[test]
    fn dash_without_audio_track_is_not_downloadable() {
        let info = VideoInfo {
            video_urls: vec![
                "https://cdn.example/v.m4s".into(),
                "https://backup.example/v.m4s".into(),
            ],
            audio_urls: Vec::new(),
            ..dash_video()
        };
        let content = VideoContent(info);
        assert!(content.download_candidate(480).is_none());
        assert_eq!(content.skip_reason(480), Some(SkipReason::NoPlayableStream));
    }

    #[test]
    fn selected_page_duration_and_index_drive_limit_and_filename() {
        let info = VideoInfo {
            duration: 100,
            selected_page: Some(VideoPage {
                page: 3,
                part: "第三话".into(),
                duration: 900,
                ..VideoPage::default()
            }),
            is_dash: false,
            audio_urls: Vec::new(),
            ..dash_video()
        };
        let content = VideoContent(info);
        assert!(content.download_candidate(480).is_none());
        assert_eq!(
            content.skip_reason(480),
            Some(SkipReason::DurationExceeded {
                duration: 900,
                maximum: 480,
            })
        );
        let candidate = content.download_candidate(0).unwrap();
        assert_eq!(candidate.filename, "bilibili_BV1xx411c7mD_p3.mp4");
        assert!(content.card().text().contains("分P3: 第三话"));
        assert_eq!(
            content.card().metadata().poster.as_ref().unwrap().title,
            "标题\n分P3: 第三话"
        );
    }

    #[test]
    fn live_and_favorite_have_no_download() {
        let live = LiveContent(LiveInfo {
            title: "直播标题".into(),
            live_status: 1,
            online: Some(1234),
            ..LiveInfo::default()
        });
        assert!(live.download_candidate(0).is_none());
        assert!(live.card().text().contains("直播中"));

        let favorite = FavoriteContent(FavoriteInfo {
            title: "我的收藏".into(),
            total: 2,
            items: vec![FavoriteItem {
                title: "条目".into(),
                bvid: Some("BVitem".into()),
                ..FavoriteItem::default()
            }],
            ..FavoriteInfo::default()
        });
        let text = favorite.card().text();
        assert!(text.contains("共 2 个内容"));
        assert!(text.contains("https://www.bilibili.com/video/BVitem"));
    }

    #[test]
    fn video_metadata_keeps_author_and_cover_when_stream_is_unavailable() {
        let info = VideoInfo {
            author: "UP主".into(),
            raw: serde_json::json!({"owner": {"face": "//cdn.example/avatar.jpg"}}),
            video_urls: Vec::new(),
            ..dash_video()
        };
        let content = VideoContent(info);
        let card = content.card();
        assert_eq!(card.metadata().author.as_deref(), Some("UP主"));
        assert_eq!(
            card.metadata().avatar_url.as_deref(),
            Some("https://cdn.example/avatar.jpg")
        );
        assert_eq!(
            card.metadata().video_cover_url.as_deref(),
            Some("https://cdn.example/cover.jpg")
        );
        assert_eq!(
            card.metadata().source_url.as_deref(),
            Some("https://www.bilibili.com/video/BV1xx411c7mD")
        );
        assert!(card.metadata().published_at.is_none());
        let poster = card.metadata().poster.as_ref().unwrap();
        assert!(poster.view_count.is_none());
        assert!(poster.comment_count.is_none());
        assert!(poster.share_count.is_none());
        assert!(content.download_candidate(0).is_none());
    }

    #[test]
    fn video_comments_reach_card_metadata_and_keyword_filter() {
        let comment = crate::resolver::card::CardComment {
            id: "123".into(),
            author: "评论昵称".into(),
            text: "评论关键词".into(),
            like_count: 30,
            ..Default::default()
        };
        let content = VideoContent(VideoInfo {
            comments: vec![comment.clone()],
            ..dash_video()
        });
        assert_eq!(content.card().metadata().comments, [comment]);
        assert!(content.filter_text().contains("评论昵称\n评论关键词"));
        // 文本回退仍保持原视频描述，评论只是海报的可选增强。
        assert!(!content.card().text().contains("评论关键词"));
    }

    #[test]
    fn video_description_and_raw_body_remain_complete_for_rendering() {
        let description = format!("{}视频尾部", "长简介".repeat(300));
        let video = VideoContent(VideoInfo {
            description: description.clone(),
            ..dash_video()
        });
        assert_eq!(
            video.card().body_text(),
            format!("标题\n简介:\n{description}")
        );
        assert_eq!(
            video
                .card()
                .metadata()
                .poster
                .as_ref()
                .unwrap()
                .description
                .as_deref(),
            Some(description.as_str())
        );

        let body = format!("{}动态尾部", "长正文".repeat(1000));
        let raw = RawContent {
            title: "动态".into(),
            content: body.clone(),
            cover_url: None,
            metadata: Default::default(),
        };
        assert_eq!(raw.card().body_text(), format!("动态\n{body}"));
        assert!(raw.card().metadata().video_cover_url.is_none());
    }

    #[test]
    fn favorite_preview_explicitly_reports_unshown_items() {
        let favorite = FavoriteContent(FavoriteInfo {
            total: 50,
            items: (0..12)
                .map(|index| FavoriteItem {
                    title: format!("第{index}条"),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        });
        let text = favorite.card().text();
        assert!(text.contains("第9条"));
        assert!(!text.contains("第10条"));
        assert!(text.contains("另有 40 条未展示"));
    }
}
