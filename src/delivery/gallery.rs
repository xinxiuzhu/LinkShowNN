//! 原图投递计划。图片字节由上游有界抓取，再经 SDK 编码；此处只负责发消息。

use anyhow::{Result, ensure};
use nuono_plugin::{MessageSegment, SendableMessage};

use crate::resolver::Card;

/// 图集消息最多包含八张原图；获取和发送都应使用这个上限。
pub(crate) const MAX_GALLERY_IMAGES: usize = 8;
const MAX_TEXT_CHARS: usize = 4000;

/// 图集使用文字和原图，不把头像、封面或评论配图加入原图投递预算。
pub(crate) fn gallery_text(card: &Card) -> String {
    let metadata = card.metadata();
    let mut paragraphs = vec![clip_text(&card.body_text(), 1800)];
    let mut author = Vec::new();
    if let Some(name) = metadata
        .author
        .as_deref()
        .filter(|name| !name.trim().is_empty())
    {
        author.push(format!("作者：{}", clip_text(name, 128)));
    }
    if let Some(published) = metadata.published_at.and_then(|timestamp| {
        let date = chrono::DateTime::<chrono::Utc>::from_timestamp(timestamp, 0)?;
        let timezone = chrono::FixedOffset::east_opt(8 * 60 * 60)?;
        Some(
            date.with_timezone(&timezone)
                .format("%Y-%m-%d %H:%M:%S")
                .to_string(),
        )
    }) {
        author.push(format!("发布时间：{published}"));
    }
    if !author.is_empty() {
        paragraphs.push(author.join("\n"));
    }
    if let Some(poster) = &metadata.poster {
        let display = &poster.stats_display;
        let mut counts = Vec::new();
        for (label, exact, shown) in [
            ("点赞", poster.like_count, &display.likes),
            ("评论", poster.comment_count, &display.comments),
            ("收藏", poster.favorite_count, &display.favorites),
            ("分享", poster.share_count, &display.shares),
        ] {
            if let Some(count) = exact
                .map(|count| count.to_string())
                .or_else(|| shown.clone())
            {
                counts.push(format!("{label} {}", clip_text(&count, 40)));
            }
        }
        if !counts.is_empty() {
            paragraphs.push(counts.join(" · "));
        }
    }
    let public = &metadata.public_details;
    let mut public_lines = Vec::new();
    for (label, value) in [
        ("粉丝", &public.author_followers_display),
        ("获赞", &public.author_likes_display),
        ("大家都在搜", &public.suggested_search),
    ] {
        if let Some(value) = value.as_deref().filter(|value| !value.trim().is_empty()) {
            public_lines.push(format!("{label}：{}", clip_text(value, 120)));
        }
    }
    if !public_lines.is_empty() {
        paragraphs.push(public_lines.join("\n"));
    }
    let mut comments = metadata.comments.iter().collect::<Vec<_>>();
    comments.retain(|comment| !comment.text.trim().is_empty() || !comment.pictures.is_empty());
    comments.sort_by(|left, right| right.like_count.cmp(&left.like_count));
    for comment in comments.into_iter().take(3) {
        let name = if comment.author.trim().is_empty() {
            "评论者".to_owned()
        } else {
            clip_text(&comment.author, 64)
        };
        let likes = comment
            .like_count_display
            .as_deref()
            .map(|count| clip_text(count, 40))
            .unwrap_or_else(|| comment.like_count.to_string());
        let mut body = clip_text(&comment.text, 480);
        if !comment.pictures.is_empty() {
            if !body.is_empty() {
                body.push(' ');
            }
            body.push_str(&format!("[含 {} 张配图]", comment.pictures.len()));
        }
        paragraphs.push(format!("{name}（赞 {likes}）：\n{body}"));
    }
    let source = metadata.source_url.as_deref().unwrap_or("");
    let body = paragraphs
        .into_iter()
        .filter(|text| !text.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    bounded_text(&body, source)
}

fn bounded_text(body: &str, source: &str) -> String {
    if source.is_empty() {
        return clip_text(body, MAX_TEXT_CHARS);
    }
    // Move only standalone source lines to the end, reserving their budget.
    let body = body
        .lines()
        .filter(|line| line.trim() != source.trim())
        .collect::<Vec<_>>()
        .join("\n");
    let budget = MAX_TEXT_CHARS.saturating_sub(source.chars().count() + 2);
    let body = clip_text(body.trim(), budget);
    if body.is_empty() {
        source.to_owned()
    } else {
        format!("{body}\n\n{source}")
    }
}

fn clip_text(text: &str, limit: usize) -> String {
    if limit == 0 {
        return String::new();
    }
    let mut chars = text.chars();
    let mut result: String = chars.by_ref().take(limit).collect();
    if chars.next().is_some() {
        result.pop();
        result.push('…');
    }
    result
}

pub(crate) struct GalleryDelivery {
    text: String,
    images: Vec<MessageSegment>,
    total: usize,
    source_url: String,
}

#[cfg(test)]
mod tests;

impl GalleryDelivery {
    /// `images` 必须来自 `ImageSource::bytes(...).into_segment()`，保持作品原顺序。
    /// `total` 是原作品的图片数，不能用成功获取的数量改变合并转发阈值。
    pub(crate) fn new(
        text: String,
        images: Vec<MessageSegment>,
        total: usize,
        source_url: &str,
    ) -> Result<Self> {
        ensure!(!images.is_empty(), "图集没有可发送的原图");
        ensure!(images.len() <= MAX_GALLERY_IMAGES, "图集图片超过发送上限");
        ensure!(total >= images.len(), "图集原始数量小于已获取数量");
        ensure!(
            images.iter().all(|image| matches!(image,
                MessageSegment::Image { file, url: None }
                    if file.starts_with("base64://") && file.len() > "base64://".len()
                        && file.len() <= "base64://".len()
                            + nuono_plugin::media::MAX_IMAGE_BYTES.div_ceil(3) * 4
            )),
            "图集图片必须先获取字节，再使用 SDK 图片编码"
        );
        Ok(Self {
            text,
            images,
            total,
            source_url: source_url.to_owned(),
        })
    }

    pub(super) fn uses_forward(&self) -> bool {
        self.total > 3
    }

    pub(super) fn into_direct(self, unsupported_forward: bool) -> SendableMessage {
        let text = self.message_text(unsupported_forward);
        let mut segments = Vec::with_capacity(self.images.len() + 1);
        if !text.is_empty() {
            segments.push(MessageSegment::text(text));
        }
        segments.extend(self.images);
        segments.into()
    }

    pub(super) fn into_nodes(self, name: &str, bot_id: &str) -> Vec<serde_json::Value> {
        let text = self.message_text(false);
        let mut contents = vec![vec![MessageSegment::text(text)]];
        contents.extend(self.images.into_iter().map(|image| vec![image]));
        contents
            .into_iter()
            .map(|content| {
                // The SDK node encoder recursively preserves image wire segments.
                // Its optional numeric uin cannot preserve an exact string identity;
                // insert the actual routed bot ID without a lossy numeric conversion.
                let mut node = MessageSegment::node(content).to_json();
                node["data"]["name"] = serde_json::json!(name);
                node["data"]["uin"] = serde_json::json!(bot_id);
                node
            })
            .collect()
    }

    fn message_text(&self, unsupported_forward: bool) -> String {
        let Some(notice) = self.notice(unsupported_forward) else {
            return bounded_text(&self.text, &self.source_url);
        };
        let body = self
            .text
            .lines()
            .filter(|line| self.source_url.is_empty() || line.trim() != self.source_url.trim())
            .collect::<Vec<_>>()
            .join("\n");
        // Reserve omission/fallback notices as well as the original link, so a
        // long title or comment cannot hide the eight-image limit explanation.
        let source_budget =
            self.source_url.chars().count() + usize::from(!self.source_url.is_empty()) * 2;
        let body_budget = MAX_TEXT_CHARS.saturating_sub(source_budget + notice.chars().count() + 2);
        let text = format!("{}\n\n{notice}", clip_text(body.trim(), body_budget));
        bounded_text(&text, &self.source_url)
    }

    fn notice(&self, unsupported_forward: bool) -> Option<String> {
        let mut lines = Vec::new();
        if unsupported_forward {
            lines.push(format!(
                "当前平台不支持合并转发，发送 {} 张图片。",
                self.images.len()
            ));
        }
        if self.total > MAX_GALLERY_IMAGES {
            lines.push(format!(
                "图集共 {} 张，最多展示前 {} 张，其余图片请查看原链接。",
                self.total, MAX_GALLERY_IMAGES
            ));
        }
        if self.images.len() < self.total.min(MAX_GALLERY_IMAGES) {
            lines.push("部分图片获取失败，请查看原链接。".to_owned());
        }
        if lines.is_empty() {
            return None;
        }
        Some(lines.join("\n"))
    }
}
