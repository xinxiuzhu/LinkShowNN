//! Media poster layout and display-count formatting.

use super::{
    DESCRIPTION_LEADING, DESCRIPTION_SIZE, DETAIL_SIZE, MARGIN, POSTER_AVATAR, TEXT_WIDTH,
    TITLE_LEADING, TITLE_SIZE,
};
use crate::render::{
    comments::{self, CommentLayout},
    format::{compact_count, count_text, format_timestamp},
    icons::{StatKind, statistics},
    image::{ImageInfo, fit},
    limits::{MAX_HEIGHT, WIDTH},
    link_qr::LinkQr,
    text::{BOLD, clip_font, ellipsize, text_width, wrap_font, wrap_text},
};
use crate::resolver::{Card, Platform};
use anyhow::{Context, Result, ensure};

pub(super) struct PosterLayout {
    pub(super) cover_width: u32,
    pub(super) cover_height: u32,
    pub(super) title_y: u32,
    pub(super) title: Vec<String>,
    pub(super) details_y: u32,
    pub(super) details: Vec<String>,
    pub(super) content_id: Option<String>,
    pub(super) content_id_inline: bool,
    pub(super) description_y: u32,
    pub(super) description: Vec<String>,
    pub(super) stats_y: u32,
    pub(super) stats: Vec<(StatKind, String)>,
    pub(super) stat_labels: bool,
    pub(super) author_y: u32,
    pub(super) author: String,
    pub(super) author_id: Option<String>,
    pub(super) link_qr: Option<LinkQr>,
    pub(super) author_details_y: u32,
    pub(super) author_details: Vec<String>,
    pub(super) comments_y: u32,
    pub(super) comments: Vec<CommentLayout>,
    pub(super) comment_media: comments::Media,
    pub(super) footer_y: u32,
    pub(super) height: u32,
}

#[cfg(test)]
pub(super) fn layout(card: &Card, cover: ImageInfo) -> Result<PosterLayout> {
    layout_with_media(card, cover, comments::Media::default())
}

pub(super) fn layout_with_media(
    card: &Card,
    cover: ImageInfo,
    comment_media: comments::Media,
) -> Result<PosterLayout> {
    let metadata = card.metadata();
    let poster = metadata
        .poster
        .as_ref()
        .context("poster metadata is missing")?;
    let (source_width, source_height) = cover.display_dimensions();
    let (cover_width, cover_height) = fit(source_width, source_height, WIDTH, 1400);
    let title = wrap_font(&BOLD, &poster.title, TITLE_SIZE, TEXT_WIDTH, 4);
    let title_y = cover_height.max(250) + 34;
    let details_y = title_y + title.len() as u32 * TITLE_LEADING + 14;
    let mut details = Vec::new();
    if let Some(date) = metadata.published_at.and_then(format_timestamp) {
        details.push(date.chars().take(16).collect::<String>());
    }
    if let Some(count) = poster.view_count {
        details.push(format!("{}播放", compact_count(count)));
    }
    if let Some(quality) = poster
        .video_quality
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        details.push(format!("清晰度 {quality}"));
    }
    if let Some(notice) = poster
        .quality_notice
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        details.push(notice.to_owned());
    }
    // Douyin and Xiaohongshu comments have their own statistic cell; keep Bilibili's
    // existing detail line and coin cell unchanged.
    let stat_labels = matches!(
        metadata.platform,
        Some(Platform::Douyin | Platform::Xiaohongshu)
    );
    if !stat_labels {
        if let Some(count) = count_text(poster.comment_count, &poster.stats_display.comments) {
            details.push(format!("{count}评论"));
        } else if let Some(count) = poster.danmaku_count {
            details.push(format!("{}弹幕", compact_count(count)));
        }
    }
    let details = wrap_text(&details.join("   "), DETAIL_SIZE, TEXT_WIDTH, 3).0;
    let content_id = poster
        .content_id
        .as_deref()
        .filter(|id| !id.trim().is_empty())
        .map(|id| ellipsize(id, DETAIL_SIZE, TEXT_WIDTH));
    let content_id_inline = content_id.as_ref().is_some_and(|id| {
        details.len() <= 1
            && text_width(
                details.first().map(String::as_str).unwrap_or(""),
                DETAIL_SIZE,
            ) + 28.0
                + text_width(id, DETAIL_SIZE)
                <= TEXT_WIDTH as f32
    });
    let detail_rows = details.len().max(usize::from(content_id_inline))
        + usize::from(content_id.is_some() && !content_id_inline);
    let description = poster
        .description
        .as_deref()
        .map(|text| wrap_text(text, DESCRIPTION_SIZE, TEXT_WIDTH, 6).0)
        .unwrap_or_default();
    let description_y = details_y + detail_rows as u32 * 28 + 24;
    let stats = statistics(poster, metadata.platform);
    let stats_y = description_y + description.len() as u32 * DESCRIPTION_LEADING + 42;
    let author_y = stats_y
        + if stats.is_empty() {
            8
        } else if stat_labels {
            118
        } else {
            96
        };
    let link_qr = LinkQr::new(
        metadata
            .original_url
            .as_deref()
            .or(metadata.source_url.as_deref()),
        160,
    );
    let author_x = MARGIN + POSTER_AVATAR + 32;
    let author_width =
        WIDTH - MARGIN - author_x - link_qr.as_ref().map(|qr| qr.size() + 24).unwrap_or(0);
    let author = clip_font(
        &BOLD,
        metadata
            .author
            .as_deref()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or("分享内容"),
        38.0,
        author_width,
    );
    let author_id = poster
        .author_id
        .as_deref()
        .filter(|id| !id.trim().is_empty())
        .map(|id| ellipsize(&format!("UID: {id}"), 26.0, author_width));
    let public = &metadata.public_details;
    let mut author_stats = Vec::new();
    if let Some(count) = public
        .author_followers_display
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        author_stats.push(format!("粉丝 {}", count.trim()));
    }
    if let Some(count) = public
        .author_likes_display
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        author_stats.push(format!("获赞 {}", count.trim()));
    }
    let mut author_details = if author_stats.is_empty() {
        Vec::new()
    } else {
        wrap_text(&author_stats.join("   "), DETAIL_SIZE, TEXT_WIDTH, 1).0
    };
    if let Some(query) = public
        .suggested_search
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        author_details.extend(
            wrap_text(
                &format!("大家都在搜：{}", query.trim()),
                DETAIL_SIZE,
                TEXT_WIDTH,
                2,
            )
            .0,
        );
    }
    let author_row_height = link_qr
        .as_ref()
        .map(|qr| qr.size() + 30)
        .unwrap_or(0)
        .max(POSTER_AVATAR);
    let author_details_y = author_y + author_row_height + 24;
    let author_end = if author_details.is_empty() {
        author_y + author_row_height
    } else {
        author_details_y + author_details.len() as u32 * 28
    };
    let comments_y = author_end + 48;
    let comments = comments::layout(
        &metadata.comments,
        &comment_media,
        comments_y,
        MAX_HEIGHT - 106,
    );
    let comment_y = comments
        .last()
        .map(|comment| comment.y + 36 + comment.body.height());
    let height = if comments.is_empty() {
        author_end + 130
    } else {
        comment_y.expect("nonempty comments have an end position") + 106
    }
    .max(1120);
    ensure!(height <= MAX_HEIGHT, "poster exceeds canvas height limit");
    Ok(PosterLayout {
        cover_width,
        cover_height,
        title_y,
        title,
        details_y,
        details,
        content_id,
        content_id_inline,
        description_y,
        description,
        stats_y,
        stats,
        stat_labels,
        author_y,
        author,
        author_id,
        link_qr,
        author_details_y,
        author_details,
        comments_y,
        comments,
        comment_media,
        footer_y: height - 60,
        height,
    })
}
