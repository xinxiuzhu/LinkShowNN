//! Media cover posters with a blurred background, metadata and rich comments.

mod layout;
#[cfg(test)]
mod tests;

#[cfg(test)]
use crate::render::comments::{
    COMMENT_AVATAR, COMMENT_LEADING, COMMENT_LIKE_SIZE, COMMENT_TEXT_WIDTH,
};
use crate::render::{
    comments::{self, MARGIN},
    drawing::{WHITE, blend, circle},
    icons::{ICON_SIZE, draw_stat_icon},
    image::{decode, image_info, oriented_thumbnail, rounded_png},
    limits::{MAX_AVATAR_PIXELS, WIDTH},
    link_qr::{LABEL, Theme},
    text::{BOLD, avatar_initial, draw as draw_font, draw_text, ellipsize, text_width},
};
use crate::resolver::Card;
use ::image::{DynamicImage, Rgb, RgbImage, imageops::FilterType};
use anyhow::{Context, Result};
use layout::{PosterLayout, layout_with_media};
use std::collections::HashMap;

const TEXT_WIDTH: u32 = WIDTH - MARGIN * 2;
const TITLE_SIZE: f32 = 44.0;
const TITLE_LEADING: u32 = 58;
const DETAIL_SIZE: f32 = 17.0;
const DESCRIPTION_SIZE: f32 = 26.0;
const DESCRIPTION_LEADING: u32 = 38;
const POSTER_AVATAR: u32 = 96;
const PALE: [u8; 3] = [238, 239, 241];
const SECONDARY: [u8; 3] = [175, 178, 184];
const ICON_GAP: u32 = 12;

pub(super) fn render(card: &Card, assets: &HashMap<String, Vec<u8>>) -> Result<Vec<u8>> {
    let metadata = card.metadata();
    let cover_url = card.poster_cover_url().context("poster cover is missing")?;
    let bytes = assets
        .get(cover_url)
        .context("poster cover asset is missing")?;
    let info = image_info(bytes)?;
    let comment_media = comments::Media::prepare(&metadata.comments, assets);
    let plan = layout_with_media(card, info, comment_media)?;
    let original = decode(bytes).context("cannot decode poster cover")?;
    let sharp = oriented_thumbnail(
        &original,
        info.orientation,
        plan.cover_width,
        plan.cover_height,
    )
    .into_rgba8();
    drop(original);

    // Blur a small preview first: the CPU and memory cost is independent of the
    // original image's dimensions, and the cover is never replaced by an avatar.
    let tiny = DynamicImage::ImageRgba8(sharp.clone())
        .resize_to_fill(80, plan.height.div_ceil(10), FilterType::Triangle)
        .into_rgba8();
    let blurred = ::image::imageops::blur(&tiny, 7.0);
    let background = ::image::imageops::resize(&blurred, WIDTH, plan.height, FilterType::Triangle);
    let mut canvas = RgbImage::from_pixel(WIDTH, plan.height, Rgb([23, 25, 29]));
    for (x, y, pixel) in background.enumerate_pixels() {
        blend(
            &mut canvas,
            x,
            y,
            [pixel[0], pixel[1], pixel[2]],
            f32::from(pixel[3]) / 255.0,
        );
    }
    let cover_x = (WIDTH - plan.cover_width) / 2;
    let fade_start = plan.cover_height as f32 * 0.60;
    for (x, y, pixel) in sharp.enumerate_pixels() {
        let fade = smoothstep(fade_start, plan.cover_height as f32, y as f32);
        blend(
            &mut canvas,
            cover_x + x,
            y,
            [pixel[0], pixel[1], pixel[2]],
            (1.0 - fade) * f32::from(pixel[3]) / 255.0,
        );
    }
    // Keep the top readable as a photograph; progressively darken the extended
    // image so text and small counters retain contrast even on a white cover.
    for y in 0..plan.height {
        let darkness = if y < plan.title_y {
            0.12 + 0.48 * smoothstep(0.0, plan.title_y as f32, y as f32)
        } else {
            0.60 + 0.26 * smoothstep(plan.title_y as f32, plan.height as f32, y as f32)
        };
        for x in 0..WIDTH {
            blend(&mut canvas, x, y, [12, 14, 18], darkness);
        }
    }
    for (index, line) in plan.title.iter().enumerate() {
        draw_font(
            &mut canvas,
            &BOLD,
            line,
            MARGIN,
            plan.title_y + index as u32 * TITLE_LEADING,
            TITLE_SIZE,
            WHITE,
        );
    }
    for (index, line) in plan.details.iter().enumerate() {
        draw_text(
            &mut canvas,
            line,
            MARGIN,
            plan.details_y + index as u32 * 28,
            DETAIL_SIZE,
            SECONDARY,
        );
    }
    if let Some(id) = &plan.content_id {
        let (x, y) = if plan.content_id_inline {
            (
                WIDTH - MARGIN - text_width(id, DETAIL_SIZE).ceil() as u32,
                plan.details_y,
            )
        } else {
            (MARGIN, plan.details_y + plan.details.len() as u32 * 28)
        };
        draw_text(&mut canvas, id, x, y, DETAIL_SIZE, SECONDARY);
    }
    for (index, line) in plan.description.iter().enumerate() {
        draw_text(
            &mut canvas,
            line,
            MARGIN,
            plan.description_y + index as u32 * DESCRIPTION_LEADING,
            DESCRIPTION_SIZE,
            PALE,
        );
    }
    if !plan.stats.is_empty() {
        let cell_width = TEXT_WIDTH / plan.stats.len() as u32;
        for (index, (kind, number)) in plan.stats.iter().enumerate() {
            let number = ellipsize(number, 28.0, cell_width.saturating_sub(58));
            let number_width = text_width(&number, 28.0).ceil() as u32;
            let total_width = ICON_SIZE + ICON_GAP + number_width;
            let x = MARGIN + index as u32 * cell_width + cell_width.saturating_sub(total_width) / 2;
            draw_stat_icon(&mut canvas, *kind, x, plan.stats_y);
            draw_text(
                &mut canvas,
                &number,
                x + ICON_SIZE + ICON_GAP,
                plan.stats_y + 3,
                28.0,
                WHITE,
            );
            if plan.stat_labels {
                let label = kind.label();
                let label_x = MARGIN
                    + index as u32 * cell_width
                    + cell_width.saturating_sub(text_width(label, 17.0).ceil() as u32) / 2;
                draw_text(
                    &mut canvas,
                    label,
                    label_x,
                    plan.stats_y + ICON_SIZE + 7,
                    17.0,
                    SECONDARY,
                );
            }
        }
    }
    draw_author(&mut canvas, card, assets, &plan);
    if let Some(qr) = &plan.link_qr {
        let x = WIDTH - MARGIN - qr.size();
        qr.draw(&mut canvas, x, plan.author_y, Theme::Poster);
        let label = LABEL;
        let label_x = x + qr
            .size()
            .saturating_sub(text_width(label, 16.0).ceil() as u32)
            / 2;
        draw_text(
            &mut canvas,
            label,
            label_x,
            plan.author_y + qr.size() + 8,
            16.0,
            SECONDARY,
        );
    }
    for (index, line) in plan.author_details.iter().enumerate() {
        draw_text(
            &mut canvas,
            line,
            MARGIN,
            plan.author_details_y + index as u32 * 28,
            DETAIL_SIZE,
            SECONDARY,
        );
    }
    comments::draw(
        &mut canvas,
        assets,
        &plan.comments,
        &plan.comment_media,
        plan.comments_y,
        comments::Palette::POSTER,
    );
    draw_text(
        &mut canvas,
        "Nuono · URL 解析",
        MARGIN,
        plan.footer_y,
        17.0,
        SECONDARY,
    );
    if let Some(platform) = metadata.platform {
        let label = platform.display_name();
        let x = WIDTH - MARGIN - text_width(label, 17.0).ceil() as u32;
        draw_text(&mut canvas, label, x, plan.footer_y, 17.0, SECONDARY);
    }
    rounded_png(canvas)
}

fn smoothstep(start: f32, end: f32, position: f32) -> f32 {
    let t = ((position - start) / (end - start).max(1.0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn draw_author(
    canvas: &mut RgbImage,
    card: &Card,
    assets: &HashMap<String, Vec<u8>>,
    plan: &PosterLayout,
) {
    let radius = POSTER_AVATAR as f32 / 2.0;
    circle(
        canvas,
        MARGIN + POSTER_AVATAR / 2,
        plan.author_y + POSTER_AVATAR / 2,
        radius,
        [65, 70, 80],
        1.0,
    );
    let avatar = card
        .metadata()
        .avatar_url
        .as_deref()
        .and_then(|url| assets.get(url))
        .and_then(|bytes| {
            let info = image_info(bytes).ok()?;
            if u64::from(info.width) * u64::from(info.height) > MAX_AVATAR_PIXELS {
                return None;
            }
            let original = decode(bytes).ok()?;
            // A square crop commutes with EXIF rotations, keeping memory bounded.
            let mut small =
                original.resize_to_fill(POSTER_AVATAR, POSTER_AVATAR, FilterType::Triangle);
            small.apply_orientation(info.orientation);
            Some(small.into_rgba8())
        });
    if let Some(avatar) = avatar {
        for (x, y, pixel) in avatar.enumerate_pixels() {
            let distance =
                ((x as f32 + 0.5 - radius).powi(2) + (y as f32 + 0.5 - radius).powi(2)).sqrt();
            blend(
                canvas,
                MARGIN + x,
                plan.author_y + y,
                [pixel[0], pixel[1], pixel[2]],
                (radius - distance).clamp(0.0, 1.0) * f32::from(pixel[3]) / 255.0,
            );
        }
    } else {
        let initial = avatar_initial(&plan.author, "分");
        let x = MARGIN + POSTER_AVATAR.saturating_sub(text_width(&initial, 38.0).ceil() as u32) / 2;
        draw_text(canvas, &initial, x, plan.author_y + 24, 38.0, PALE);
    }
    let text_x = MARGIN + POSTER_AVATAR + 32;
    draw_font(
        canvas,
        &BOLD,
        &plan.author,
        text_x,
        plan.author_y + if plan.author_id.is_some() { 1 } else { 24 },
        38.0,
        WHITE,
    );
    if let Some(author_id) = &plan.author_id {
        draw_text(canvas, author_id, text_x, plan.author_y + 61, 26.0, PALE);
    }
}
