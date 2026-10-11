//! 展示卡片：平台排版的产物，与具体聊天协议无关。
//!
//! 平台模块只描述「要展示什么」，不碰 OneBot segment 的 JSON 形状；协议编码
//! 由装配层负责。这样平台排版可以离线断言，不需要构造事件上下文。

use super::Platform;
use serde::{Deserialize, Serialize};

/// 评论正文中可替换为图片的完整表情标记；素材失效时保留原文字。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardCommentEmote {
    pub text: String,
    pub url: String,
    /// 平台的大号表情，按相对字号展示，不将远端 size 当作像素值。
    pub large: bool,
}

/// 匿名接口可见的根评论；不包含楼中楼，也不代表全站点赞排名。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardComment {
    pub id: String,
    pub author: String,
    pub author_id: Option<String>,
    pub avatar_url: Option<String>,
    pub text: String,
    pub emotes: Vec<CardCommentEmote>,
    /// 评论附图，与正文内的小表情分开展示。
    pub pictures: Vec<String>,
    pub like_count: u64,
    /// 页面只有缩略值时保留原显示值；like_count 此时仅作排序估值。
    pub like_count_display: Option<String>,
    /// Unix 时间戳，单位秒。
    pub published_at: Option<i64>,
}

/// 网页只提供缩略统计时按原文展示，不把“万”级数值伪装成精确计数。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CardStatsDisplay {
    pub likes: Option<String>,
    pub comments: Option<String>,
    pub favorites: Option<String>,
    pub shares: Option<String>,
}

/// 视频海报的结构化内容。统计缺失时保持为空，不将未知值显示成零。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CardPoster {
    pub title: String,
    pub description: Option<String>,
    pub content_id: Option<String>,
    pub author_id: Option<String>,
    pub view_count: Option<u64>,
    pub danmaku_count: Option<u64>,
    pub comment_count: Option<u64>,
    pub like_count: Option<u64>,
    pub coin_count: Option<u64>,
    pub favorite_count: Option<u64>,
    pub share_count: Option<u64>,
    pub stats_display: CardStatsDisplay,
}

/// 渲染所需的展示信息。缺失字段保持为空，不用抓取时间替代发布时间。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CardMetadata {
    pub platform: Option<Platform>,
    pub author: Option<String>,
    pub avatar_url: Option<String>,
    /// Unix 时间戳，单位秒。
    pub published_at: Option<i64>,
    pub source_url: Option<String>,
    /// 视频封面：海报主视觉；普通图文布局仅这张图叠加播放标记。
    pub video_cover_url: Option<String>,
    /// 视频可用封面海报排版；原文仍由 Card 保留以供过滤和发送回退。
    pub poster: Option<CardPoster>,
    /// 当前匿名可见候选中按赞降序取至多三条；取不到时为空。
    pub comments: Vec<CardComment>,
}

/// 卡片里的一张图。附带来源说明便于日志定位。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardImage {
    pub url: String,
}

impl CardImage {
    pub fn new(url: impl Into<String>) -> Self {
        Self { url: url.into() }
    }
}

/// 一条解析结果的展示形态：一段正文加若干配图。
///
/// 正文分段保留，因为部分平台需要「标题 / 统计 / 简介」之间有稳定的空行，
/// 拼接策略留给装配层。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Card {
    lines: Vec<String>,
    images: Vec<CardImage>,
    metadata: CardMetadata,
}

impl Card {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_metadata(mut self, metadata: CardMetadata) -> Self {
        self.metadata = metadata;
        self
    }

    pub fn metadata(&self) -> &CardMetadata {
        &self.metadata
    }

    /// 追加一段正文。空白内容自动忽略，省掉每个平台各写一遍 `is_empty` 判断。
    pub fn line(mut self, text: impl Into<String>) -> Self {
        let text = text.into();
        if !text.trim().is_empty() {
            self.lines.push(text);
        }
        self
    }

    pub fn line_opt(self, text: Option<String>) -> Self {
        match text {
            Some(text) => self.line(text),
            None => self,
        }
    }

    pub fn image(mut self, url: impl Into<String>) -> Self {
        let url = url.into();
        if !url.trim().is_empty() {
            self.images.push(CardImage::new(url));
        }
        self
    }

    pub fn image_opt(self, url: Option<String>) -> Self {
        match url {
            Some(url) => self.image(url),
            None => self,
        }
    }

    pub fn images(mut self, urls: impl IntoIterator<Item = String>) -> Self {
        for url in urls {
            self = self.image(url);
        }
        self
    }

    pub fn image_urls(&self) -> impl Iterator<Item = &str> {
        self.images.iter().map(|image| image.url.as_str())
    }

    /// 用换行拼成单段文本，供协议编码和过滤复用。
    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    /// 图片正文不重复附加来源链接；文本回退仍由 `text()` 完整保留。
    pub fn body_text(&self) -> String {
        self.lines
            .iter()
            .flat_map(|line| line.split('\n'))
            .filter(|line| {
                self.metadata
                    .source_url
                    .as_deref()
                    .is_none_or(|source| line.trim() != source.trim())
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::{Card, CardMetadata};

    #[test]
    fn drops_blank_lines_and_images() {
        let card = Card::new()
            .line("标题")
            .line("   ")
            .line_opt(None)
            .image("")
            .image("https://cdn.example/a.jpg");
        assert_eq!(card.text(), "标题");
        assert_eq!(
            card.image_urls().collect::<Vec<_>>(),
            ["https://cdn.example/a.jpg"]
        );
    }

    #[test]
    fn joins_lines_in_append_order() {
        let card = Card::new().line("一").line("二").line("三");
        assert_eq!(card.text(), "一\n二\n三");
    }

    #[test]
    fn rendered_body_omits_only_the_standalone_source_line() {
        let card = Card::new()
            .with_metadata(CardMetadata {
                source_url: Some("https://example.test/post/1".into()),
                ..Default::default()
            })
            .line("正文提及 https://example.test/post/1")
            .line("补充\nhttps://example.test/post/1")
            .line("https://example.test/post/1");
        assert_eq!(
            card.body_text(),
            "正文提及 https://example.test/post/1\n补充"
        );
        assert_eq!(card.text().lines().count(), 4);
    }
}
