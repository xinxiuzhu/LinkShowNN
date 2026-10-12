//! Optional gallery statistics and comments, sharing poster media and row spacing.

use crate::render::{
    comments::{self, CommentLayout, Media, Palette},
    format::count_text,
    icons::{ICON_SIZE, StatKind, draw_stat_icon_with_color, statistics},
    limits::WIDTH,
    text::{draw_text, ellipsize, text_width},
};
use crate::resolver::{Card, Platform};
use ::image::RgbImage;
use std::collections::HashMap;

const MARGIN: u32 = 36;
const CONTENT_WIDTH: u32 = WIDTH - MARGIN * 2;
const SECTION_GAP: u32 = 32;
const STAT_HEIGHT: u32 = 60;
const LABELED_STAT_HEIGHT: u32 = 82;
const COUNT_HEIGHT: u32 = 28;

pub(super) struct Section {
    stats: Vec<(StatKind, String)>,
    stat_labels: bool,
    comment_count: Option<String>,
    media: Media,
}

pub(super) struct Layout {
    section: Section,
    stats_y: u32,
    count_y: u32,
    comments_y: u32,
    pub(super) comments: Vec<CommentLayout>,
    pub(super) end_y: u32,
}

impl Section {
    pub(super) fn prepare(card: &Card, assets: &HashMap<String, Vec<u8>>) -> Self {
        let poster = card.metadata().poster.as_ref();
        let platform = card.metadata().platform;
        let stat_labels = matches!(platform, Some(Platform::Douyin | Platform::Xiaohongshu));
        Self {
            stats: poster
                .map(|poster| statistics(poster, platform))
                .unwrap_or_default(),
            stat_labels,
            comment_count: if stat_labels {
                None
            } else {
                poster.and_then(|poster| {
                    count_text(poster.comment_count, &poster.stats_display.comments)
                })
            },
            media: Media::prepare(&card.metadata().comments, assets),
        }
    }

    fn stats_height(&self) -> u32 {
        if self.stats.is_empty() {
            0
        } else if self.stat_labels {
            LABELED_STAT_HEIGHT
        } else {
            STAT_HEIGHT
        }
    }

    fn counts_height(&self) -> u32 {
        self.stats_height()
            + if self.comment_count.is_some() {
                COUNT_HEIGHT
            } else {
                0
            }
    }

    fn minimum_height(&self, card: &Card) -> u32 {
        let comments_height = comments::minimum_height(&card.metadata().comments, &self.media);
        let height = self.counts_height() + comments_height;
        if height == 0 { 0 } else { SECTION_GAP + height }
    }

    /// Main gallery images retain their order and share remaining height; a
    /// bounded allowance keeps comment pictures visible when available.
    pub(super) fn reserved_height(&self, card: &Card) -> u32 {
        let picture_comments = card
            .metadata()
            .comments
            .iter()
            .take(3)
            .filter(|comment| !comment.pictures.is_empty())
            .count() as u32;
        self.minimum_height(card) + picture_comments * 220
    }

    pub(super) fn layout(self, card: &Card, after_images: u32, maximum_end: u32) -> Layout {
        let minimum = self.minimum_height(card);
        let stats_y = after_images + if minimum == 0 { 0 } else { SECTION_GAP };
        let count_y = stats_y + self.stats_height();
        let comments_y = stats_y + self.counts_height();
        let comments = comments::layout(
            &card.metadata().comments,
            &self.media,
            comments_y,
            maximum_end,
        );
        let end_y = comments
            .last()
            .map(|comment| comment.y + 36 + comment.body.height())
            .unwrap_or(comments_y);
        Layout {
            section: self,
            stats_y,
            count_y,
            comments_y,
            comments,
            end_y,
        }
    }
}

impl Layout {
    pub(super) fn draw(&self, canvas: &mut RgbImage, assets: &HashMap<String, Vec<u8>>) {
        let palette = Palette::GALLERY;
        if !self.section.stats.is_empty() {
            let cell_width = CONTENT_WIDTH / self.section.stats.len() as u32;
            for (index, (kind, number)) in self.section.stats.iter().enumerate() {
                let number = ellipsize(number, 28.0, cell_width.saturating_sub(58));
                let total_width = ICON_SIZE + 12 + text_width(&number, 28.0).ceil() as u32;
                let x =
                    MARGIN + index as u32 * cell_width + cell_width.saturating_sub(total_width) / 2;
                draw_stat_icon_with_color(canvas, *kind, x, self.stats_y, palette.icon);
                draw_text(
                    canvas,
                    &number,
                    x + ICON_SIZE + 12,
                    self.stats_y + 3,
                    28.0,
                    palette.text,
                );
                if self.section.stat_labels {
                    let label = kind.label();
                    let label_x = MARGIN
                        + index as u32 * cell_width
                        + cell_width.saturating_sub(text_width(label, 17.0).ceil() as u32) / 2;
                    draw_text(
                        canvas,
                        label,
                        label_x,
                        self.stats_y + ICON_SIZE + 7,
                        17.0,
                        palette.secondary,
                    );
                }
            }
        }
        if let Some(count) = &self.section.comment_count {
            draw_text(
                canvas,
                &format!("{count}评论"),
                MARGIN,
                self.count_y,
                17.0,
                palette.secondary,
            );
        }
        comments::draw(
            canvas,
            assets,
            &self.comments,
            &self.section.media,
            self.comments_y,
            palette,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolver::{CardPoster, card::CardMetadata};

    #[test]
    fn douyin_gallery_has_one_comment_stat_and_reserves_label_height() {
        let card = Card::new().with_metadata(CardMetadata {
            platform: Some(Platform::Douyin),
            poster: Some(CardPoster {
                like_count: Some(123),
                comment_count: Some(0),
                favorite_count: Some(789),
                share_count: Some(10),
                ..Default::default()
            }),
            ..Default::default()
        });
        let assets = HashMap::new();
        let section = Section::prepare(&card, &assets);
        assert_eq!(
            section.stats,
            [
                (StatKind::Like, "123".into()),
                (StatKind::Comment, "0".into()),
                (StatKind::Favorite, "789".into()),
                (StatKind::Share, "10".into()),
            ]
        );
        assert!(section.comment_count.is_none());
        assert_eq!(section.counts_height(), LABELED_STAT_HEIGHT);
        let plan = section.layout(&card, 1000, 2400);
        assert_eq!(plan.stats_y, 1000 + SECTION_GAP);
        assert_eq!(plan.comments_y, plan.stats_y + LABELED_STAT_HEIGHT);
        assert_eq!(plan.end_y, plan.comments_y);
        let mut canvas = RgbImage::from_pixel(WIDTH, plan.end_y, ::image::Rgb([255; 3]));
        plan.draw(&mut canvas, &assets);
        let label_y = plan.stats_y + ICON_SIZE + 7;
        assert!(
            canvas.enumerate_pixels().any(|(_, y, pixel)| {
                (label_y..label_y + 24).contains(&y) && pixel.0 != [255; 3]
            })
        );
    }

    #[test]
    fn no_statistics_reserve_no_label_height() {
        let card = Card::new().with_metadata(CardMetadata {
            platform: Some(Platform::Douyin),
            poster: Some(CardPoster::default()),
            ..Default::default()
        });
        let section = Section::prepare(&card, &HashMap::new());
        assert_eq!(section.counts_height(), 0);
        assert_eq!(section.reserved_height(&card), 0);
        let plan = section.layout(&card, 100, 2400);
        assert_eq!(plan.end_y, 100);
    }
}
