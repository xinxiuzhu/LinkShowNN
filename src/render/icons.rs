//! Bundled statistics icons shared by poster and gallery layouts.

pub(super) const ICON_SIZE: u32 = 42;
const COMMENT_LIKE_SIZE: u32 = 16;
const ICON: [u8; 3] = [180, 183, 190];
use crate::render::drawing::blend;
use ::image::{RgbImage, imageops::FilterType};
use std::sync::LazyLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum StatKind {
    Like,
    Coin,
    Favorite,
    Share,
    Comment,
}

impl StatKind {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Like => "点赞",
            Self::Coin => "投币",
            Self::Favorite => "收藏",
            Self::Share => "分享",
            Self::Comment => "评论",
        }
    }
}

// These trusted, bundled SVGs are parsed once. Keep their curves and even-odd
// cutouts intact; only use alpha so the poster controls the monochrome tint.
static STAT_ICONS: LazyLock<[resvg::tiny_skia::Pixmap; 5]> = LazyLock::new(|| {
    [
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/icons/bilibili/like.svg"
        ))
        .as_slice(),
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/icons/bilibili/coin.svg"
        ))
        .as_slice(),
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/icons/bilibili/star.svg"
        ))
        .as_slice(),
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/icons/bilibili/share.svg"
        ))
        .as_slice(),
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/icons/common/comment.svg"
        ))
        .as_slice(),
    ]
    .map(|svg| {
        let tree = resvg::usvg::Tree::from_data(svg, &resvg::usvg::Options::default())
            .expect("bundled statistic icon must be a valid SVG");
        let bounds = tree.root().abs_bounding_box();
        // Normalize visible bounds rather than viewBox: the original like icon
        // uses 36 units while the others use 28, with different internal padding.
        let scale = (ICON_SIZE - 2) as f32 / bounds.width().max(bounds.height());
        let x = (ICON_SIZE as f32 - bounds.width() * scale) / 2.0 - bounds.x() * scale;
        let y = (ICON_SIZE as f32 - bounds.height() * scale) / 2.0 - bounds.y() * scale;
        let transform = resvg::tiny_skia::Transform::from_row(scale, 0.0, 0.0, scale, x, y);
        let mut pixmap = resvg::tiny_skia::Pixmap::new(ICON_SIZE, ICON_SIZE)
            .expect("fixed icon dimensions must be valid");
        resvg::render(&tree, transform, &mut pixmap.as_mut());
        pixmap
    })
});

pub(super) static COMMENT_LIKE_ICON: LazyLock<::image::RgbaImage> = LazyLock::new(|| {
    let original =
        ::image::RgbaImage::from_raw(ICON_SIZE, ICON_SIZE, STAT_ICONS[0].data().to_vec())
            .expect("bundled statistic icon has fixed RGBA dimensions");
    ::image::imageops::resize(
        &original,
        COMMENT_LIKE_SIZE,
        COMMENT_LIKE_SIZE,
        FilterType::Lanczos3,
    )
});

pub(super) fn draw_stat_icon(canvas: &mut RgbImage, kind: StatKind, x: u32, y: u32) {
    draw_stat_icon_with_color(canvas, kind, x, y, ICON);
}

pub(super) fn draw_stat_icon_with_color(
    canvas: &mut RgbImage,
    kind: StatKind,
    x: u32,
    y: u32,
    color: [u8; 3],
) {
    let index = match kind {
        StatKind::Like => 0,
        StatKind::Coin => 1,
        StatKind::Favorite => 2,
        StatKind::Share => 3,
        StatKind::Comment => 4,
    };
    for (offset, pixel) in STAT_ICONS[index].pixels().iter().enumerate() {
        blend(
            canvas,
            x + offset as u32 % ICON_SIZE,
            y + offset as u32 / ICON_SIZE,
            color,
            f32::from(pixel.alpha()) / 255.0,
        );
    }
}

/// Unknown statistics are omitted; rounded public labels keep their original text.
pub(super) fn statistics(
    poster: &crate::resolver::CardPoster,
    platform: Option<crate::resolver::Platform>,
) -> Vec<(StatKind, String)> {
    use crate::render::format::{compact_count, count_text};
    let second = if matches!(
        platform,
        Some(crate::resolver::Platform::Douyin | crate::resolver::Platform::Xiaohongshu)
    ) {
        (
            StatKind::Comment,
            count_text(poster.comment_count, &poster.stats_display.comments),
        )
    } else {
        (StatKind::Coin, poster.coin_count.map(compact_count))
    };
    [
        (
            StatKind::Like,
            count_text(poster.like_count, &poster.stats_display.likes),
        ),
        second,
        (
            StatKind::Favorite,
            count_text(poster.favorite_count, &poster.stats_display.favorites),
        ),
        (
            StatKind::Share,
            count_text(poster.share_count, &poster.stats_display.shares),
        ),
    ]
    .into_iter()
    .filter_map(|(kind, count)| count.map(|count| (kind, count)))
    .collect()
}
