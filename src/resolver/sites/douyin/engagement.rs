//! Optional public work statistics and bounded root-comment candidates.
//!
//! The browser only observes responses the anonymous page itself requested.
//! No signing implementation, login cookie, replies, or global-ranking claim.

use serde_json::Value;
use url::Url;

use crate::{
    download::DownloadCandidate,
    resolver::{Card, CardComment, CardPoster, MediaInfo, ResolvedContent, url_policy},
};

pub(super) const MAX_CANDIDATES: usize = 50;

#[derive(Debug, Clone, Default)]
pub(crate) struct Engagement {
    pub stats: CardPoster,
    pub comments: Vec<CardComment>,
}

impl Engagement {
    pub fn merge(&mut self, other: Self) {
        let stats = &mut self.stats;
        stats.like_count = other.stats.like_count.or(stats.like_count);
        stats.comment_count = other.stats.comment_count.or(stats.comment_count);
        stats.favorite_count = other.stats.favorite_count.or(stats.favorite_count);
        stats.share_count = other.stats.share_count.or(stats.share_count);
        let display = &mut stats.stats_display;
        display.likes = other.stats.stats_display.likes.or(display.likes.take());
        display.comments = other
            .stats
            .stats_display
            .comments
            .or(display.comments.take());
        display.favorites = other
            .stats
            .stats_display
            .favorites
            .or(display.favorites.take());
        display.shares = other.stats.stats_display.shares.or(display.shares.take());
        for mut new in other.comments {
            if let Some(old) = self.comments.iter_mut().find(|old| old.id == new.id) {
                old.avatar_url = new.avatar_url.or(old.avatar_url.take());
                old.published_at = new.published_at.or(old.published_at);
                // API counts are exact. DOM labels can enrich media without
                // overwriting a more precise API snapshot of the same comment.
                if new.like_count_display.is_none() || old.like_count_display.is_some() {
                    old.like_count = new.like_count;
                    old.like_count_display = new.like_count_display;
                }
                old.emotes.append(&mut new.emotes);
                let mut tokens = std::collections::HashSet::new();
                old.emotes.retain(|emote| tokens.insert(emote.text.clone()));
                old.emotes.truncate(8);
                for picture in new.pictures {
                    if old.pictures.len() < 3 && !old.pictures.contains(&picture) {
                        old.pictures.push(picture);
                    }
                }
            } else if self.comments.len() < MAX_CANDIDATES {
                self.comments.push(new);
            }
        }
    }

    pub fn top_comments(&self) -> Vec<CardComment> {
        let mut comments = self.comments.clone();
        // Stable sorting keeps source order on ties, after deduplication by ID.
        comments.retain(|comment| !comment.text.trim().is_empty() || !comment.pictures.is_empty());
        comments.sort_by(|left, right| right.like_count.cmp(&left.like_count));
        comments.truncate(3);
        comments
    }

    pub fn from_detail(detail: &Value) -> Self {
        let counts = &detail["statistics"];
        Self {
            stats: CardPoster {
                like_count: counts["digg_count"].as_u64(),
                comment_count: counts["comment_count"].as_u64(),
                favorite_count: counts["collect_count"].as_u64(),
                share_count: counts["share_count"].as_u64(),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    pub fn from_comments(payload: &Value, expected_id: &str) -> Self {
        if payload["status_code"].as_u64() != Some(0) {
            return Self::default();
        }
        let comments = payload["comments"]
            .as_array()
            .into_iter()
            .flatten()
            .take(MAX_CANDIDATES)
            .filter_map(|value| map_comment(value, expected_id))
            .collect();
        let mut result = Self::default();
        result.merge(Self {
            stats: CardPoster {
                comment_count: payload["total"].as_u64(),
                ..Default::default()
            },
            comments,
        });
        result
    }
}

fn map_comment(value: &Value, expected_id: &str) -> Option<CardComment> {
    let is_root = |value: &Value| value.as_str() == Some("0") || value.as_u64() == Some(0);
    if value["aweme_id"].as_str()? != expected_id
        || !is_root(&value["reply_id"])
        || !is_root(&value["reply_to_reply_id"])
    {
        return None;
    }
    let id = numeric_id(value["cid"].as_str()?)?;
    let author = bounded(value.pointer("/user/nickname")?.as_str()?, 64);
    let text = bounded(value["text"].as_str()?, 2048);
    if author.is_empty() {
        return None;
    }
    // Pictures and emoji URLs are joined from the matching public DOM row;
    // never guess an undocumented image path or rewrite signed CDN URLs.
    let avatar_url = ["avatar_medium", "avatar_thumb", "avatar_larger"]
        .iter()
        .find_map(|key| {
            value["user"][key]["url_list"]
                .as_array()?
                .iter()
                .filter_map(Value::as_str)
                .find_map(image_url)
        });
    Some(CardComment {
        id,
        author,
        author_id: value
            .pointer("/user/sec_uid")
            .and_then(Value::as_str)
            .map(|s| bounded(s, 128)),
        avatar_url,
        text,
        like_count: value["digg_count"].as_u64()?,
        published_at: value["create_time"].as_i64().filter(|time| *time > 0),
        ..Default::default()
    })
}

pub(super) fn numeric_id(value: &str) -> Option<String> {
    (!value.is_empty() && value.len() <= 24 && value.bytes().all(|b| b.is_ascii_digit()))
        .then(|| value.to_owned())
}

pub(super) fn bounded(value: &str, max: usize) -> String {
    value.trim().chars().take(max).collect()
}

pub(super) fn image_url(value: &str) -> Option<String> {
    let value = if value.starts_with("//") {
        format!("https:{value}")
    } else {
        value.to_owned()
    };
    let url = Url::parse(&value).ok()?;
    url_policy::validate_public_url(&url).ok()?;
    Some(url.to_string())
}

/// Parse only a count label, never unrelated prose or a negative/unknown value.
/// The original label is retained by callers; the estimate is for sorting only.
pub(super) fn display_count(text: &str) -> Option<(u64, String)> {
    let label = text.trim();
    if label.is_empty() || label.len() > 32 {
        return None;
    }
    let (number, scale) = if let Some(number) = label.strip_suffix('万') {
        (number, 10_000_u64)
    } else if let Some(number) = label.strip_suffix('亿') {
        (number, 100_000_000_u64)
    } else {
        (label, 1)
    };
    let number = number.replace(',', "");
    let (whole, fraction) = number.split_once('.').unwrap_or((&number, ""));
    if whole.is_empty()
        || !whole.bytes().all(|b| b.is_ascii_digit())
        || !fraction.bytes().all(|b| b.is_ascii_digit())
        || fraction.len() > 4
        || (scale == 1 && !fraction.is_empty())
    {
        return None;
    }
    let mut count = whole.parse::<u64>().ok()?.checked_mul(scale)?;
    if !fraction.is_empty() {
        count = count.checked_add(
            fraction.parse::<u64>().ok()?.checked_mul(scale)? / 10_u64.pow(fraction.len() as u32),
        )?;
    }
    Some((count, label.to_owned()))
}

#[derive(Debug)]
pub(crate) struct DouyinContent {
    pub media: MediaInfo,
    pub engagement: Engagement,
}

impl DouyinContent {
    pub fn new(media: MediaInfo) -> Self {
        let engagement = Engagement::from_detail(&media.raw);
        Self { media, engagement }
    }
}

impl ResolvedContent for DouyinContent {
    fn card(&self) -> Card {
        let card = self.media.card();
        let mut metadata = card.metadata().clone();
        if let Some(poster) = &mut metadata.poster {
            let stats = &self.engagement.stats;
            poster.like_count = stats.like_count;
            poster.comment_count = stats.comment_count;
            poster.favorite_count = stats.favorite_count;
            poster.share_count = stats.share_count;
            poster.stats_display = stats.stats_display.clone();
        }
        metadata.comments = self
            .engagement
            .top_comments()
            .into_iter()
            .filter(|comment| !comment.text.trim().is_empty() || !comment.pictures.is_empty())
            .collect();
        card.with_metadata(metadata)
    }

    fn filter_text(&self) -> String {
        self.media.filter_text()
    }
    fn download_candidate(&self, maximum_duration: u64) -> Option<DownloadCandidate> {
        self.media.download_candidate(maximum_duration)
    }
}

#[cfg(test)]
mod tests {
    use super::super::{client::AwemeRef, desktop};
    use super::*;
    use serde_json::json;
    const ID: &str = "7693165612066999571";
    const HTML: &str = include_str!("../../../../tests/fixtures/douyin-engagement.html");

    fn dom() -> Engagement {
        desktop::map_engagement(
            HTML,
            &AwemeRef {
                id: ID.into(),
                is_note: false,
            },
            &Url::parse(&format!("https://www.douyin.com/video/{ID}")).unwrap(),
        )
        .unwrap()
    }

    fn reply(id: &str, likes: u64) -> Value {
        json!({"cid": id, "aweme_id": ID, "text":"测试[微笑]", "digg_count":likes, "reply_id":"0", "reply_to_reply_id":"0", "create_time":1791420000,
            "user":{"nickname":"公开昵称", "avatar_thumb":{"url_list":["https://p3.douyinpic.com/avatar.png"]}}})
    }

    #[test]
    fn recorded_dom_preserves_display_counts_and_selects_top_three_with_media() {
        let extra = dom();
        assert_eq!(extra.stats.stats_display.likes.as_deref(), Some("20.9万"));
        assert_eq!(extra.stats.stats_display.comments.as_deref(), Some("5783"));
        assert_eq!(
            extra.stats.stats_display.favorites.as_deref(),
            Some("1.4万")
        );
        assert_eq!(extra.stats.stats_display.shares.as_deref(), Some("5.2万"));
        assert!(extra.stats.like_count.is_none());
        assert_eq!(extra.comments.len(), 5);
        let top = extra.top_comments();
        assert_eq!(
            top.iter().map(|c| c.like_count).collect::<Vec<_>>(),
            [547, 351, 42]
        );
        assert!(!top[0].emotes.is_empty());
        assert!(top[1..].iter().all(|c| !c.pictures.is_empty()));
        assert!(extra.comments.iter().any(|c| !c.emotes.is_empty()));
        for c in &extra.comments {
            assert!(!c.text.contains("分享回复"));
            assert!(!c.text.contains("天前·"));
            assert!(!c.text.contains(&format!("{}...", c.author)));
            for emote in &c.emotes {
                assert!(c.text.contains(&emote.text));
            }
        }
    }

    #[test]
    fn merging_api_and_dom_keeps_exact_likes_avatar_and_matching_images() {
        let mut extra = dom();
        let id = extra.top_comments()[1].id.clone();
        extra.merge(Engagement::from_comments(
            &json!({"status_code":0,"total":5785,"comments":[reply(&id, 549),reply(&id, 550)]}),
            ID,
        ));
        extra.merge(dom());
        let top = extra.top_comments();
        assert_eq!(extra.comments.len(), 5);
        assert_eq!(top[0].like_count, 550);
        assert!(top[0].like_count_display.is_none());
        assert_eq!(
            top[0].avatar_url.as_deref(),
            Some("https://p3.douyinpic.com/avatar.png")
        );
        assert!(!top[0].pictures.is_empty());
        assert_eq!(extra.stats.comment_count, Some(5785));
    }

    #[test]
    fn api_rejects_other_works_replies_invalid_counts_and_unsafe_assets() {
        let valid = reply("1", 0);
        let mut invalids = Vec::new();
        for (key, value) in [
            ("aweme_id", json!("other")),
            ("reply_id", json!("9")),
            ("reply_id", json!(9)),
            ("digg_count", json!(-1)),
            ("cid", json!("x")),
        ] {
            let mut bad = valid.clone();
            bad[key] = value;
            invalids.push(bad);
        }
        let mut all = invalids;
        all.push(valid.clone());
        let got = Engagement::from_comments(&json!({"status_code":0,"comments":all}), ID);
        assert_eq!(got.comments.len(), 1);
        assert_eq!(got.comments[0].like_count, 0);
        assert!(
            Engagement::from_comments(&json!({"status_code":8,"comments":[valid]}), ID)
                .comments
                .is_empty()
        );
        for url in [
            "http://127.0.0.1/p.png",
            "file:///private/file",
            "https://user:pass@cdn.test/a.png",
        ] {
            assert!(image_url(url).is_none());
        }
    }

    #[test]
    fn counts_are_bounded_and_rounded_labels_are_not_exact_statistics() {
        assert_eq!(display_count("20.9万"), Some((209000, "20.9万".into())));
        assert_eq!(display_count("1,234"), Some((1234, "1,234".into())));
        for text in [
            "",
            "点赞",
            "-1",
            "1.2",
            "1.2.3万",
            "18446744073709551615亿",
            "作者获赞7571万",
        ] {
            assert!(display_count(text).is_none(), "{text}");
        }
        let empty = Engagement::from_detail(&json!({"author":{"favoriting_count":75710000}}));
        assert!(empty.stats.like_count.is_none());
    }

    #[test]
    fn recommendations_and_wrong_work_cannot_supply_stats_or_comments() {
        let source = Url::parse(&format!("https://www.douyin.com/video/{ID}")).unwrap();
        let expected = AwemeRef {
            id: ID.into(),
            is_note: false,
        };
        let wrong = HTML.replace(
            &format!("data-e2e-aweme-id=\"{ID}\""),
            "data-e2e-aweme-id=\"456\"",
        );
        assert!(desktop::map_engagement(&wrong, &expected, &source).is_err());
        let outside = HTML.replace("data-e2e=\"comment-list\"", "data-e2e=\"recommendations\"");
        assert!(
            desktop::map_engagement(&outside, &expected, &source)
                .unwrap()
                .comments
                .is_empty()
        );
    }
}
