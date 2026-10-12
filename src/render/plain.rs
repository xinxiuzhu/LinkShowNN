//! Ordinary sharing cards: author header, bounded text, gallery and optional engagement.

mod engagement;

use super::{
    drawing::{WHITE, blend, circle},
    format::format_timestamp,
    image::{decode, encode_png, fit, image_info, oriented_thumbnail},
    limits::{MAX_AVATAR_PIXELS, MAX_HEIGHT, MAX_TOTAL_PIXELS, WIDTH},
    link_qr::{LABEL, LinkQr, Theme},
    text::{self, draw_text, ellipsize, text_width, wrap_text},
};
use crate::resolver::{Card, Platform};
use ::image::{DynamicImage, Rgb, RgbImage, imageops::FilterType, metadata::Orientation};
use anyhow::{Context, Result, ensure};
use std::collections::HashMap;

const PADDING: u32 = 24;
const CONTENT_WIDTH: u32 = WIDTH - PADDING * 2;
const AVATAR_SIZE: u32 = 80;
const BODY_SIZE: f32 = 30.0;
const BODY_LINE_HEIGHT: u32 = 40;
const MAX_BODY_LINES: usize = 36;
const MAX_IMAGES: usize = 10;
const IMAGE_GAP: u32 = 16;

const INK: [u8; 3] = [51, 51, 51];
const MUTED: [u8; 3] = [139, 139, 139];
const BLUE: [u8; 3] = [0, 128, 255];

struct Picture<'a> {
    url: &'a str,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    video: bool,
    orientation: Orientation,
}

struct Layout<'a> {
    header: bool,
    link_qr: Option<LinkQr>,
    body_y: u32,
    body: Vec<String>,
    pictures: Vec<Picture<'a>>,
    engagement: engagement::Layout,
    footer_y: u32,
    footer: Vec<String>,
    height: u32,
}

/// Compose one PNG. Every main picture must be present and decodable: a broken
/// picture is an error so the caller can fall back to the original message.
/// Avatars are decorative and may fall back to an initial instead.
pub(super) fn render(card: &Card, assets: &HashMap<String, Vec<u8>>) -> Result<Vec<u8>> {
    let layout = layout(card, assets)?;
    let mut canvas = RgbImage::from_pixel(WIDTH, layout.height, Rgb(WHITE));
    if layout.header {
        draw_header(&mut canvas, card, assets, layout.link_qr.as_ref());
    }
    for (index, text) in layout.body.iter().enumerate() {
        draw_text(
            &mut canvas,
            text,
            PADDING,
            layout.body_y + index as u32 * BODY_LINE_HEIGHT,
            BODY_SIZE,
            INK,
        );
    }

    // Only one full-resolution image is decoded at a time. Resize before
    // converting to RGBA, so 16-bit sources do not create another large buffer.
    for picture in &layout.pictures {
        let bytes = assets
            .get(picture.url)
            .context("card picture asset is missing")?;
        let original = decode(bytes).context("cannot decode card picture")?;
        let small = oriented_thumbnail(
            &original,
            picture.orientation,
            picture.width,
            picture.height,
        )
        .into_rgba8();
        drop(original);
        for (x, y, pixel) in small.enumerate_pixels() {
            blend(
                &mut canvas,
                picture.x + x,
                picture.y + y,
                [pixel[0], pixel[1], pixel[2]],
                f32::from(pixel[3]) / 255.0,
            );
        }
        if picture.video {
            draw_play_button(&mut canvas, picture);
        }
    }

    layout.engagement.draw(&mut canvas, assets);

    for (index, text) in layout.footer.iter().enumerate() {
        draw_text(
            &mut canvas,
            text,
            PADDING,
            layout.footer_y + index as u32 * 28,
            20.0,
            MUTED,
        );
    }
    encode_png(&canvas)
}

fn layout<'a>(card: &'a Card, assets: &HashMap<String, Vec<u8>>) -> Result<Layout<'a>> {
    let metadata = card.metadata();
    let link_qr = LinkQr::new(
        metadata
            .original_url
            .as_deref()
            .or(metadata.source_url.as_deref()),
        160,
    );
    let header = metadata.author.is_some()
        || metadata.avatar_url.is_some()
        || metadata.platform.is_some()
        || metadata.published_at.is_some()
        || link_qr.is_some();
    let body_y = if header {
        PADDING + AVATAR_SIZE.max(link_qr.as_ref().map(|qr| qr.size() + 30).unwrap_or(0)) + 16
    } else {
        PADDING
    };
    let (body, text_truncated) =
        wrap_text(&card.body_text(), BODY_SIZE, CONTENT_WIDTH, MAX_BODY_LINES);
    let mut y = body_y + body.len() as u32 * BODY_LINE_HEIGHT;

    let total_images = card.image_urls().count();
    let urls = card.image_urls().take(MAX_IMAGES).collect::<Vec<_>>();
    let mut total_pixels = 0u64;
    let mut picture_dimensions = Vec::with_capacity(urls.len());
    for url in &urls {
        let bytes = assets.get(*url).context("card picture asset is missing")?;
        let info = image_info(bytes)?;
        let (width, height) = info.display_dimensions();
        let pixels = u64::from(width) * u64::from(height);
        total_pixels = total_pixels
            .checked_add(pixels)
            .context("card pixel count overflow")?;
        ensure!(
            total_pixels <= MAX_TOTAL_PIXELS,
            "card pictures exceed total pixel limit"
        );
        picture_dimensions.push(info);
    }

    let mut omissions = Vec::new();
    if text_truncated {
        omissions.push("正文已节选".to_owned());
    }
    if total_images > MAX_IMAGES {
        omissions.push(format!("图片已显示 {MAX_IMAGES} / {total_images} 张"));
    }
    let footer = if omissions.is_empty() {
        Vec::new()
    } else {
        wrap_text(
            &format!("{}；完整内容见原链接", omissions.join("，")),
            20.0,
            CONTENT_WIDTH,
            2,
        )
        .0
    };
    let footer_space = if footer.is_empty() {
        0
    } else {
        20 + footer.len() as u32 * 28
    };
    let engagement = engagement::Section::prepare(card, assets);
    let reserved_engagement = engagement.reserved_height(card);
    let mut pictures = Vec::with_capacity(urls.len());

    if !urls.is_empty() {
        if !body.is_empty() {
            y += 12;
        }
        let available =
            (MAX_HEIGHT - PADDING - footer_space).saturating_sub(y + reserved_engagement);
        ensure!(
            available > 0,
            "card text and comments leave no picture area"
        );
        if urls.len() == 1 {
            let (original_width, original_height) = picture_dimensions[0].display_dimensions();
            let max_width = if original_height > original_width {
                600
            } else {
                CONTENT_WIDTH
            };
            let (width, height) = fit(
                original_width,
                original_height,
                max_width,
                available.min(1800),
            );
            pictures.push(Picture {
                url: urls[0],
                x: PADDING,
                y,
                width,
                height,
                video: metadata.video_cover_url.as_deref() == Some(urls[0]),
                orientation: picture_dimensions[0].orientation,
            });
            y += height;
        } else {
            let cell_width = (CONTENT_WIDTH - IMAGE_GAP) / 2;
            let row_heights = picture_dimensions
                .chunks(2)
                .map(|row| {
                    row.iter()
                        .map(|info| {
                            let (width, height) = info.display_dimensions();
                            fit(width, height, cell_width, 720).1
                        })
                        .max()
                        .unwrap_or(1)
                })
                .collect::<Vec<_>>();
            let gaps = (row_heights.len() as u32 - 1) * IMAGE_GAP;
            let desired: u32 = row_heights.iter().sum();
            let ratio = (available.saturating_sub(gaps) as f64 / f64::from(desired)).min(1.0);
            for (row_index, desired_height) in row_heights.into_iter().enumerate() {
                let row_height = (f64::from(desired_height) * ratio).floor().max(1.0) as u32;
                for column in 0..2 {
                    let index = row_index * 2 + column;
                    if index >= urls.len() {
                        break;
                    }
                    let (original_width, original_height) =
                        picture_dimensions[index].display_dimensions();
                    let (width, height) =
                        fit(original_width, original_height, cell_width, row_height);
                    pictures.push(Picture {
                        url: urls[index],
                        x: PADDING
                            + column as u32 * (cell_width + IMAGE_GAP)
                            + (cell_width - width) / 2,
                        y: y + (row_height - height) / 2,
                        width,
                        height,
                        video: metadata.video_cover_url.as_deref() == Some(urls[index]),
                        orientation: picture_dimensions[index].orientation,
                    });
                }
                y += row_height;
                if row_index + 1 < urls.len().div_ceil(2) {
                    y += IMAGE_GAP;
                }
            }
        }
    }

    let engagement = engagement.layout(card, y, MAX_HEIGHT - PADDING - footer_space);
    let footer_y = engagement.end_y + if footer.is_empty() { 0 } else { 20 };
    let height = (footer_y + footer.len() as u32 * 28 + PADDING).max(64);
    ensure!(
        height <= MAX_HEIGHT,
        "card layout exceeds canvas height limit"
    );
    Ok(Layout {
        header,
        link_qr,
        body_y,
        body,
        pictures,
        engagement,
        footer_y,
        footer,
        height,
    })
}

fn draw_header(
    canvas: &mut RgbImage,
    card: &Card,
    assets: &HashMap<String, Vec<u8>>,
    link_qr: Option<&LinkQr>,
) {
    let metadata = card.metadata();
    let author = metadata
        .author
        .as_deref()
        .filter(|name| !name.trim().is_empty())
        .unwrap_or("分享内容");
    let avatar = metadata
        .avatar_url
        .as_deref()
        .and_then(|url| assets.get(url))
        .and_then(|bytes| {
            let info = image_info(bytes).ok()?;
            let (width, height) = info.display_dimensions();
            if u64::from(width) * u64::from(height) > MAX_AVATAR_PIXELS {
                return None;
            }
            Some((decode(bytes).ok()?, info.orientation))
        });
    draw_avatar(canvas, avatar, author);

    let mut author_right = WIDTH - PADDING;
    if let Some(qr) = link_qr {
        let x = WIDTH - PADDING - qr.size();
        qr.draw(canvas, x, PADDING, Theme::Plain);
        let label = LABEL;
        let label_x = x + qr
            .size()
            .saturating_sub(text_width(label, 16.0).ceil() as u32)
            / 2;
        draw_text(canvas, label, label_x, PADDING + qr.size() + 8, 16.0, MUTED);
        author_right = x - 24;
    }
    if let Some(platform) = metadata.platform {
        let name = platform.display_name();
        let name_width = text_width(name, 24.0).ceil() as u32;
        let name_x = author_right - name_width;
        draw_text(canvas, name, name_x, PADDING + 24, 24.0, INK);
        circle(
            canvas,
            name_x - 14,
            PADDING + 42,
            5.0,
            platform_color(platform),
            1.0,
        );
        author_right = name_x.saturating_sub(34);
    }
    let author_x = PADDING + AVATAR_SIZE + 16;
    let author = ellipsize(author, 30.0, author_right.saturating_sub(author_x));
    let timestamp = metadata.published_at.and_then(format_timestamp);
    draw_text(
        canvas,
        &author,
        author_x,
        if timestamp.is_some() {
            PADDING
        } else {
            PADDING + 20
        },
        30.0,
        BLUE,
    );
    if let Some(timestamp) = timestamp {
        draw_text(canvas, &timestamp, author_x, PADDING + 44, 24.0, MUTED);
    }
}

fn platform_color(platform: Platform) -> [u8; 3] {
    match platform {
        Platform::Bilibili => [251, 114, 153],
        Platform::Douyin => [25, 25, 25],
        Platform::Kuaishou => [255, 87, 34],
        Platform::Xiaohongshu => [255, 36, 66],
        Platform::Twitter => [25, 25, 25],
        Platform::Ncm => [224, 36, 44],
        Platform::QqMusic => [49, 190, 112],
        Platform::Kurobbs => [46, 153, 138],
        Platform::QqChannel => [49, 114, 238],
    }
}

fn draw_avatar(canvas: &mut RgbImage, avatar: Option<(DynamicImage, Orientation)>, author: &str) {
    let center_x = PADDING + AVATAR_SIZE / 2;
    let center_y = PADDING + AVATAR_SIZE / 2;
    circle(
        canvas,
        center_x,
        center_y,
        AVATAR_SIZE as f32 / 2.0,
        [240, 242, 245],
        1.0,
    );
    if let Some((avatar, orientation)) = avatar {
        let mut thumbnail = avatar.resize_to_fill(AVATAR_SIZE, AVATAR_SIZE, FilterType::Triangle);
        thumbnail.apply_orientation(orientation);
        let small = thumbnail.into_rgba8();
        for (x, y, pixel) in small.enumerate_pixels() {
            let dx = x as f32 + 0.5 - AVATAR_SIZE as f32 / 2.0;
            let dy = y as f32 + 0.5 - AVATAR_SIZE as f32 / 2.0;
            let mask = (AVATAR_SIZE as f32 / 2.0 - (dx * dx + dy * dy).sqrt()).clamp(0.0, 1.0);
            blend(
                canvas,
                PADDING + x,
                PADDING + y,
                [pixel[0], pixel[1], pixel[2]],
                mask * f32::from(pixel[3]) / 255.0,
            );
        }
    } else {
        let text = text::units(author, 256)
            .0
            .into_iter()
            .find(|unit| !unit.chars().all(char::is_whitespace))
            .unwrap_or_else(|| "分".into());
        let x = center_x.saturating_sub(text_width(&text, 32.0).round() as u32 / 2);
        draw_text(canvas, &text, x, PADDING + 20, 32.0, [132, 146, 164]);
    }
}

fn draw_play_button(canvas: &mut RgbImage, picture: &Picture<'_>) {
    let smaller_side = picture.width.min(picture.height);
    if smaller_side < 32 {
        return;
    }
    let radius = (smaller_side as f32 / 6.0).clamp(12.0, 46.0);
    let center_x = picture.x + picture.width / 2;
    let center_y = picture.y + picture.height / 2;
    circle(canvas, center_x, center_y, radius, [0, 0, 0], 0.20);
    let left = center_x as f32 - radius * 0.25;
    let right = center_x as f32 + radius * 0.48;
    let half_height = radius * 0.48;
    for y in (center_y as f32 - half_height).floor() as u32
        ..=(center_y as f32 + half_height).ceil() as u32
    {
        for x in left.floor() as u32..=right.ceil() as u32 {
            let horizontal = (right - (x as f32 + 0.5)) / (right - left);
            let vertical = ((y as f32 + 0.5) - center_y as f32).abs();
            let coverage = (horizontal * half_height - vertical + 0.5).clamp(0.0, 1.0);
            if x as f32 + 0.5 >= left {
                blend(canvas, x, y, WHITE, coverage * 0.9);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::{
        image::{dimensions, reader},
        limits::{MAX_DIMENSION, MAX_IMAGE_BYTES, MAX_PNG_BYTES},
    };
    use crate::resolver::card::CardMetadata;
    use unicode_segmentation::UnicodeSegmentation;

    fn photo(width: u32, height: u32) -> Vec<u8> {
        let mut image = RgbImage::new(width, height);
        for (x, y, pixel) in image.enumerate_pixels_mut() {
            let sky = y as f32 / height as f32;
            *pixel = Rgb([
                (192.0 + sky * 35.0) as u8,
                (221.0 + sky * 18.0) as u8,
                (245.0 - sky * 26.0) as u8,
            ]);
            let dx = x as f32 / width as f32 - 0.72;
            let dy = y as f32 / height as f32 - 0.28;
            if dx * dx + dy * dy < 0.009 {
                *pixel = Rgb([255, 231, 157]);
            }
            if y as f32 / height as f32 > 0.67 - (x as f32 / width as f32 * 4.3).sin() * 0.08 {
                *pixel = Rgb([125, 169, 167]);
            }
            if y as f32 / height as f32 > 0.82 - (x as f32 / width as f32 * 5.6 + 1.0).sin() * 0.07
            {
                *pixel = Rgb([56, 112, 112]);
            }
        }
        encode_png(&image).unwrap()
    }

    fn fixture() -> (Card, HashMap<String, Vec<u8>>) {
        let card = Card::new()
            .with_metadata(CardMetadata {
                platform: Some(Platform::Douyin),
                author: Some("Nuono 图文解析".into()),
                avatar_url: Some("avatar".into()),
                published_at: Some(1_791_641_167),
                source_url: Some("https://www.douyin.com/video/example".into()),
                video_cover_url: Some("cover".into()),
                ..Default::default()
            })
            .line("把旅途里的好天气，收藏在一张照片里。")
            .line("#周末出走 #风景记录")
            .line("https://www.douyin.com/video/example")
            .image("cover");
        let assets = HashMap::from([
            ("cover".into(), photo(600, 800)),
            ("avatar".into(), photo(120, 120)),
        ]);
        (card, assets)
    }

    #[test]
    fn header_body_and_video_cover_form_one_png() {
        let (card, assets) = fixture();
        let plan = layout(&card, &assets).unwrap();
        assert!(plan.header);
        let qr = plan.link_qr.as_ref().unwrap();
        assert!(plan.body_y > PADDING + qr.size() + 24);
        assert_eq!(plan.pictures[0].width, 600);
        assert_eq!(plan.pictures[0].height, 800);
        assert!(plan.pictures[0].video);
        assert!(plan.body.iter().all(|line| !line.contains("douyin.com")));
        let png = render(&card, &assets).unwrap();
        let image = ::image::load_from_memory(&png).unwrap().into_rgb8();
        assert_eq!(image.dimensions(), (WIDTH, plan.height));
        assert_eq!(*image.get_pixel(0, 0), Rgb(WHITE));
        assert!(
            image
                .enumerate_pixels()
                .any(|(x, y, pixel)| x >= 120 && y < 70 && pixel[2] > 220 && pixel[0] < 100)
        );
        assert!(
            image
                .enumerate_pixels()
                .any(|(x, y, pixel)| x >= 640 && y < 104 && pixel[0] < 100)
        );
        if let Some(directory) = std::env::var_os("NUONO_CARD_PREVIEW_DIR") {
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(
                std::path::Path::new(&directory).join("linkshownn-card-preview.png"),
                png,
            )
            .unwrap();
        }
    }

    #[test]
    fn long_text_and_large_gallery_are_bounded_and_disclose_omissions() {
        let mut card = Card::new().line("长文测试，正文可以自然换行。".repeat(1000));
        let mut assets = HashMap::new();
        let small = photo(40, 160);
        for index in 0..14 {
            let url = format!("photo-{index}");
            card = card.image(url.clone());
            if index < MAX_IMAGES {
                assets.insert(url, small.clone());
            }
        }
        let plan = layout(&card, &assets).unwrap();
        assert_eq!(plan.pictures.len(), MAX_IMAGES);
        assert!(plan.height <= MAX_HEIGHT);
        assert_eq!(plan.body.len(), MAX_BODY_LINES);
        assert!(plan.footer.join("").contains("正文已节选"));
        assert!(plan.footer.join("").contains("10 / 14"));
        for picture in &plan.pictures {
            assert!(picture.y + picture.height <= plan.footer_y);
            assert!(picture.x + picture.width <= WIDTH - PADDING);
            assert!((f64::from(picture.width) / f64::from(picture.height) - 0.25).abs() < 0.01);
        }
        let png = render(&card, &assets).unwrap();
        assert!(png.len() < MAX_PNG_BYTES);
    }

    #[test]
    fn missing_or_corrupt_picture_fails_but_bad_avatar_uses_placeholder() {
        let (card, mut assets) = fixture();
        assets.insert("avatar".into(), vec![0, 1, 2]);
        assert!(render(&card, &assets).is_ok());
        assets.insert("cover".into(), vec![0, 1, 2]);
        assert!(render(&card, &assets).is_err());
        assets.remove("cover");
        assert!(render(&card, &assets).is_err());
    }

    // A valid small PNG header with inflated dimensions proves that limits are
    // enforced before trying to inflate the image, without allocating a bomb.
    fn inflated_header(width: u32, height: u32) -> Vec<u8> {
        let mut png = photo(1, 1);
        png[16..20].copy_from_slice(&width.to_be_bytes());
        png[20..24].copy_from_slice(&height.to_be_bytes());
        let mut crc = !0u32;
        for byte in &png[12..29] {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                crc = (crc >> 1) ^ if crc & 1 != 0 { 0xedb8_8320 } else { 0 };
            }
        }
        png[29..33].copy_from_slice(&(!crc).to_be_bytes());
        png
    }

    #[test]
    fn image_area_dimensions_and_total_pixels_are_checked_before_decode() {
        let card = Card::new().image("large");
        let assets = HashMap::from([("large".into(), inflated_header(7000, 4000))]);
        assert!(format!("{:#}", layout(&card, &assets).err().unwrap()).contains("pixel limit"));
        assert!(dimensions(&inflated_header(MAX_DIMENSION + 1, 1)).is_err());
        let mut card = Card::new();
        let mut assets = HashMap::new();
        for index in 0..4 {
            let url = index.to_string();
            card = card.image(url.clone());
            assets.insert(url, inflated_header(4000, 4000));
        }
        assert!(
            format!("{:#}", layout(&card, &assets).err().unwrap()).contains("total pixel limit")
        );
        assert!(reader(&vec![0; MAX_IMAGE_BYTES + 1]).is_err());
    }

    #[test]
    fn wrapping_handles_chinese_words_long_urls_and_missing_glyphs() {
        let text = format!(
            "中文换行 English words\nhttps://example.com/{}\n未知字形：\u{10ffff}",
            "abcdef".repeat(30)
        );
        let (lines, truncated) = wrap_text(&text, BODY_SIZE, CONTENT_WIDTH, 20);
        assert!(!truncated);
        assert!(
            lines
                .iter()
                .all(|line| text_width(line, BODY_SIZE) <= CONTENT_WIDTH as f32)
        );
        assert!(lines.join("").contains("English words"));
        assert!(lines.join("").contains('□'));
        assert_eq!(format_timestamp(0).unwrap(), "1970-01-01 08:00:00");
        assert!(text_width(&ellipsize(&"长作者名字".repeat(100), 30.0, 300), 30.0) <= 300.0);
    }

    #[test]
    fn emoji_wrapping_and_ellipsis_keep_complete_sequences() {
        let source = "🌙👍🏽👩‍💻🇨🇳👨‍👩‍👧‍👦❤️";
        let (lines, truncated) = wrap_text(source, 30.0, 60, 10);
        assert!(!truncated);
        assert_eq!(lines.concat(), source);
        assert!(lines.iter().all(|line| text_width(line, 30.0) <= 60.0));
        assert!(lines.iter().all(|line| line.graphemes(true).count() <= 2));
        let clipped = ellipsize(source, 30.0, 70);
        assert!(clipped.ends_with('…'));
        assert!(source.starts_with(clipped.trim_end_matches('…')));
        assert!(text_width(&clipped, 30.0) <= 70.0);
        assert!(!clipped.contains('□'));
    }

    #[test]
    fn plain_card_draws_unicode_moon_in_color_without_remote_assets() {
        let png = render(&Card::new().line("小姐演唱。🌙"), &HashMap::new()).unwrap();
        let canvas = ::image::load_from_memory(&png).unwrap().to_rgb8();
        let yellow = canvas
            .pixels()
            .filter(|pixel| {
                u16::from(pixel[0]) > u16::from(pixel[2]) + 80
                    && u16::from(pixel[1]) > u16::from(pixel[2]) + 50
            })
            .count();
        assert!(
            yellow > 20,
            "the moon must be colored pixels, not a missing-glyph box"
        );
    }

    #[test]
    fn gallery_keeps_different_aspect_ratios_complete() {
        let (first, mut assets) = fixture();
        let card = first.image("landscape").image("square");
        assets.insert("landscape".into(), photo(600, 300));
        assets.insert("square".into(), photo(240, 240));
        let plan = layout(&card, &assets).unwrap();
        assert_eq!(plan.pictures.len(), 3);
        assert_eq!(plan.pictures[1].width, plan.pictures[1].height * 2);
        assert_eq!(plan.pictures[2].width, plan.pictures[2].height);
        if let Some(directory) = std::env::var_os("NUONO_CARD_PREVIEW_DIR") {
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(
                std::path::Path::new(&directory).join("linkshownn-card-gallery-preview.png"),
                render(&card, &assets).unwrap(),
            )
            .unwrap();
        }
    }

    #[test]
    fn mixed_gallery_keeps_main_images_and_appends_stats_and_rich_comments() {
        use crate::resolver::card::{CardComment, CardCommentEmote, CardPoster};
        let metadata = CardMetadata {
            platform: Some(Platform::Douyin),
            author: Some("图集作者".into()),
            video_cover_url: Some("first".into()),
            poster: Some(CardPoster {
                like_count: Some(123),
                comment_count: Some(456),
                favorite_count: Some(789),
                share_count: Some(10),
                ..Default::default()
            }),
            comments: (0..3)
                .map(|index| CardComment {
                    id: index.to_string(),
                    author: format!("评论者{index}"),
                    text: "正文[微笑]".into(),
                    like_count: 30 - index,
                    emotes: vec![CardCommentEmote {
                        text: "[微笑]".into(),
                        url: "emote".into(),
                        large: false,
                    }],
                    pictures: vec!["comment-picture".into()],
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        };
        let card = Card::new()
            .with_metadata(metadata)
            .line("图集正文")
            .image("first")
            .image("second")
            .image("third");
        let assets = HashMap::from([
            ("first".into(), photo(240, 160)),
            ("second".into(), photo(160, 240)),
            ("third".into(), photo(200, 200)),
            (
                "emote".into(),
                encode_png(&RgbImage::from_pixel(32, 32, Rgb([0, 255, 0]))).unwrap(),
            ),
            (
                "comment-picture".into(),
                encode_png(&RgbImage::from_pixel(200, 100, Rgb([255, 0, 0]))).unwrap(),
            ),
        ]);
        let plan = layout(&card, &assets).unwrap();
        assert_eq!(
            plan.pictures
                .iter()
                .map(|picture| picture.url)
                .collect::<Vec<_>>(),
            ["first", "second", "third"]
        );
        assert_eq!(plan.engagement.comments.len(), 3);
        assert!(
            plan.pictures.last().unwrap().y + plan.pictures.last().unwrap().height
                < plan.engagement.comments[0].y
        );
        for pair in plan.engagement.comments.windows(2) {
            assert_eq!(
                pair[1].y - (pair[0].y + 36 + pair[0].body.height()),
                crate::render::comments::COMMENT_GAP
            );
        }
        let png = crate::render::compose(&card, &assets).unwrap();
        let canvas = ::image::load_from_memory(&png).unwrap().into_rgb8();
        // Shared comments retain both inline image emotes and attached pictures.
        let first_comment_y = plan.engagement.comments[0].y;
        assert!(
            canvas
                .enumerate_pixels()
                .any(|(_, y, pixel)| y > first_comment_y && pixel.0 == [0, 255, 0])
        );
        assert!(
            canvas
                .enumerate_pixels()
                .any(|(_, y, pixel)| y > first_comment_y && pixel.0 == [255, 0, 0])
        );
        assert_eq!(
            *canvas.get_pixel(0, 0),
            Rgb(WHITE),
            "mixed gallery must use the plain layout"
        );
        assert_eq!(canvas.dimensions(), (WIDTH, plan.height));
        if let Some(directory) = std::env::var_os("NUONO_CARD_PREVIEW_DIR") {
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(
                std::path::Path::new(&directory).join("linkshownn-gallery-comments-preview.png"),
                png,
            )
            .unwrap();
        }
    }

    #[test]
    fn tall_gallery_reserves_all_three_comment_texts_and_remains_bounded() {
        use crate::resolver::card::CardComment;
        let mut card = Card::new()
            .with_metadata(CardMetadata {
                comments: (0..3)
                    .map(|index| CardComment {
                        id: index.to_string(),
                        author: format!("评论者{index}"),
                        text: "很长的评论正文。".repeat(80),
                        pictures: vec!["comment-picture".into()],
                        ..Default::default()
                    })
                    .collect(),
                ..Default::default()
            })
            .line("很长的图集正文。".repeat(1000));
        let mut assets = HashMap::from([("comment-picture".into(), photo(120, 240))]);
        for index in 0..14 {
            let url = format!("photo-{index}");
            card = card.image(&url);
            if index < MAX_IMAGES {
                assets.insert(url, photo(40, 160));
            }
        }
        let plan = layout(&card, &assets).unwrap();
        assert_eq!(plan.pictures.len(), MAX_IMAGES);
        assert_eq!(plan.engagement.comments.len(), 3);
        assert!(plan.engagement.comments.last().unwrap().y < plan.footer_y);
        assert!(plan.height <= MAX_HEIGHT);
        assert!(plan.footer.join("").contains("10 / 14"));
        assert!(render(&card, &assets).unwrap().len() <= MAX_PNG_BYTES);
    }

    #[test]
    fn jpeg_exif_rotates_the_thumbnail_without_distorting_its_aspect_ratio() {
        let mut source = RgbImage::new(2, 3);
        for (pixel, color) in source.pixels_mut().zip([
            [255, 0, 0],
            [0, 255, 0],
            [0, 0, 255],
            [255, 255, 0],
            [255, 0, 255],
            [0, 255, 255],
        ]) {
            *pixel = Rgb(color);
        }
        let original = DynamicImage::ImageRgb8(source);
        let thumbnail = oriented_thumbnail(&original, Orientation::Rotate90, 6, 4).into_rgb8();
        assert_eq!(thumbnail.dimensions(), (6, 4));
        assert_eq!(*thumbnail.get_pixel(0, 0), Rgb([255, 0, 255]));
        assert_eq!(*thumbnail.get_pixel(5, 0), Rgb([255, 0, 0]));

        let mut jpeg = Vec::new();
        ::image::codecs::jpeg::JpegEncoder::new(&mut jpeg)
            .encode_image(&original)
            .unwrap();
        // APP1 with one little-endian TIFF orientation tag (6 = 90° clockwise).
        let exif = [
            b'E', b'x', b'i', b'f', 0, 0, b'I', b'I', 42, 0, 8, 0, 0, 0, 1, 0, 0x12, 1, 3, 0, 1, 0,
            0, 0, 6, 0, 0, 0, 0, 0, 0, 0,
        ];
        let mut marker = vec![0xff, 0xe1];
        marker.extend_from_slice(&((exif.len() + 2) as u16).to_be_bytes());
        marker.extend_from_slice(&exif);
        jpeg.splice(2..2, marker);
        assert_eq!(
            image_info(&jpeg).unwrap().orientation,
            Orientation::Rotate90
        );
        assert_eq!(dimensions(&jpeg).unwrap(), (3, 2));
        let card = Card::new().image("rotated");
        let assets = HashMap::from([("rotated".into(), jpeg)]);
        let plan = layout(&card, &assets).unwrap();
        assert_eq!(plan.pictures[0].width, CONTENT_WIDTH);
        assert_eq!(plan.pictures[0].height, CONTENT_WIDTH * 2 / 3);
        let output = render(&card, &assets).unwrap();
        assert!(::image::load_from_memory(&output).is_ok());
    }
}
