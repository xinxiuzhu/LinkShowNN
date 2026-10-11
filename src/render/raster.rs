//! Bounded, offline card composition. Network access stays in the async asset loader.

use std::{collections::HashMap, io::Cursor, sync::LazyLock};

use ab_glyph::{Font, FontArc, PxScale, ScaleFont, point};
use anyhow::{Context, Result, bail, ensure};
use chrono::{DateTime, FixedOffset, Utc};
use image::{
    DynamicImage, ImageDecoder, ImageEncoder, ImageFormat, ImageReader, Limits, Rgb, RgbImage,
    codecs::png::{CompressionType, FilterType as PngFilterType, PngEncoder},
    imageops::FilterType,
    metadata::Orientation,
};

use crate::resolver::{Platform, card::Card};

#[path = "poster.rs"]
mod poster;

const WIDTH: u32 = 800;
const MAX_HEIGHT: u32 = 4096;
const PADDING: u32 = 24;
const CONTENT_WIDTH: u32 = WIDTH - PADDING * 2;
const AVATAR_SIZE: u32 = 80;
const BODY_SIZE: f32 = 30.0;
const BODY_LINE_HEIGHT: u32 = 40;
const MAX_BODY_LINES: usize = 36;
const MAX_TEXT_CHARS: usize = 8000;
const MAX_IMAGES: usize = 10;
const IMAGE_GAP: u32 = 16;
const MAX_IMAGE_BYTES: usize = 12 * 1024 * 1024;
const MAX_ASSET_BYTES: usize = 48 * 1024 * 1024;
const MAX_DIMENSION: u32 = 12_000;
const MAX_IMAGE_PIXELS: u64 = 24_000_000;
const MAX_TOTAL_PIXELS: u64 = 60_000_000;
const MAX_AVATAR_PIXELS: u64 = 4_000_000;
const MAX_PNG_BYTES: usize = nuono_plugin::media::MAX_IMAGE_BYTES;

const WHITE: [u8; 3] = [255, 255, 255];
const INK: [u8; 3] = [51, 51, 51];
const MUTED: [u8; 3] = [139, 139, 139];
const BLUE: [u8; 3] = [0, 128, 255];

static FONT: LazyLock<FontArc> = LazyLock::new(|| {
    FontArc::try_from_slice(include_bytes!("../../assets/fonts/NotoSansSC-Regular.otf"))
        .expect("bundled Noto Sans SC must be a valid font")
});

struct Picture<'a> {
    url: &'a str,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    video: bool,
    orientation: Orientation,
}

#[derive(Clone, Copy)]
struct ImageInfo {
    width: u32,
    height: u32,
    orientation: Orientation,
}

impl ImageInfo {
    fn display_dimensions(self) -> (u32, u32) {
        if swaps_axes(self.orientation) {
            (self.height, self.width)
        } else {
            (self.width, self.height)
        }
    }
}

struct Layout<'a> {
    header: bool,
    body_y: u32,
    body: Vec<String>,
    pictures: Vec<Picture<'a>>,
    footer_y: u32,
    footer: Vec<String>,
    height: u32,
}

/// Compose one PNG. Every main picture must be present and decodable: a broken
/// picture is an error so the caller can fall back to the original message.
/// Avatars are decorative and may fall back to an initial instead.
pub(super) fn render(card: &Card, assets: &HashMap<String, Vec<u8>>) -> Result<Vec<u8>> {
    let asset_bytes = assets
        .values()
        .try_fold(0usize, |total, bytes| total.checked_add(bytes.len()))
        .context("card asset byte count overflow")?;
    ensure!(
        asset_bytes <= MAX_ASSET_BYTES,
        "card assets exceed byte limit"
    );

    if card.metadata().poster.is_some() && card.metadata().video_cover_url.is_some() {
        return poster::render(card, assets);
    }

    let layout = layout(card, assets)?;
    let mut canvas = RgbImage::from_pixel(WIDTH, layout.height, Rgb(WHITE));
    if layout.header {
        draw_header(&mut canvas, card, assets);
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
    let header = metadata.author.is_some()
        || metadata.avatar_url.is_some()
        || metadata.platform.is_some()
        || metadata.published_at.is_some();
    let body_y = if header {
        PADDING + AVATAR_SIZE + 16
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
    let mut pictures = Vec::with_capacity(urls.len());

    if !urls.is_empty() {
        if !body.is_empty() {
            y += 12;
        }
        let available = MAX_HEIGHT - PADDING - footer_space - y;
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
            let ratio = ((available - gaps) as f64 / f64::from(desired)).min(1.0);
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

    let footer_y = y + if footer.is_empty() { 0 } else { 20 };
    let height = (footer_y + footer.len() as u32 * 28 + PADDING).max(64);
    ensure!(
        height <= MAX_HEIGHT,
        "card layout exceeds canvas height limit"
    );
    Ok(Layout {
        header,
        body_y,
        body,
        pictures,
        footer_y,
        footer,
        height,
    })
}

fn fit(width: u32, height: u32, max_width: u32, max_height: u32) -> (u32, u32) {
    let scale =
        (f64::from(max_width) / f64::from(width)).min(f64::from(max_height) / f64::from(height));
    (
        (f64::from(width) * scale).floor().max(1.0) as u32,
        (f64::from(height) * scale).floor().max(1.0) as u32,
    )
}

fn reader(bytes: &[u8]) -> Result<ImageReader<Cursor<&[u8]>>> {
    ensure!(
        bytes.len() <= MAX_IMAGE_BYTES,
        "card image exceeds byte limit"
    );
    let mut reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    ensure!(
        matches!(
            reader.format(),
            Some(ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::WebP | ImageFormat::Gif)
        ),
        "unsupported card image format"
    );
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_DIMENSION);
    limits.max_image_height = Some(MAX_DIMENSION);
    limits.max_alloc = Some(128 * 1024 * 1024);
    reader.limits(limits);
    Ok(reader)
}

fn dimensions(bytes: &[u8]) -> Result<(u32, u32)> {
    Ok(image_info(bytes)?.display_dimensions())
}

fn image_info(bytes: &[u8]) -> Result<ImageInfo> {
    let mut decoder = reader(bytes)?
        .into_decoder()
        .context("invalid card image header")?;
    let (width, height) = decoder.dimensions();
    ensure!(width > 0 && height > 0, "empty card image");
    ensure!(
        u64::from(width) * u64::from(height) <= MAX_IMAGE_PIXELS,
        "card image exceeds pixel limit"
    );
    let orientation = decoder
        .orientation()
        .context("invalid card image orientation")?;
    Ok(ImageInfo {
        width,
        height,
        orientation,
    })
}

fn decode(bytes: &[u8]) -> Result<DynamicImage> {
    // Keep the area check in this function too: avatars do not pass through
    // the main-picture layout preflight.
    dimensions(bytes)?;
    reader(bytes)?.decode().context("invalid card image data")
}

fn swaps_axes(orientation: Orientation) -> bool {
    matches!(
        orientation,
        Orientation::Rotate90
            | Orientation::Rotate270
            | Orientation::Rotate90FlipH
            | Orientation::Rotate270FlipH
    )
}

fn oriented_thumbnail(
    image: &DynamicImage,
    orientation: Orientation,
    width: u32,
    height: u32,
) -> DynamicImage {
    let (raw_width, raw_height) = if swaps_axes(orientation) {
        (height, width)
    } else {
        (width, height)
    };
    let mut thumbnail = image.resize_exact(raw_width, raw_height, FilterType::Triangle);
    // Apply EXIF after reducing the image, so 90-degree rotations allocate at
    // most a thumbnail-sized buffer rather than another full-resolution image.
    thumbnail.apply_orientation(orientation);
    thumbnail
}

fn draw_header(canvas: &mut RgbImage, card: &Card, assets: &HashMap<String, Vec<u8>>) {
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
    if let Some(platform) = metadata.platform {
        let name = platform.display_name();
        let name_width = text_width(name, 24.0).ceil() as u32;
        let name_x = WIDTH - PADDING - name_width;
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

fn format_timestamp(timestamp: i64) -> Option<String> {
    let date = DateTime::<Utc>::from_timestamp(timestamp, 0)?;
    let timezone = FixedOffset::east_opt(8 * 60 * 60)?;
    Some(
        date.with_timezone(&timezone)
            .format("%Y-%m-%d %H:%M:%S")
            .to_string(),
    )
}

fn platform_color(platform: Platform) -> [u8; 3] {
    match platform {
        Platform::Bilibili => [251, 114, 153],
        Platform::Douyin => [25, 25, 25],
        Platform::Kuaishou => [255, 87, 34],
        Platform::Xiaohongshu => [255, 36, 66],
        Platform::Twitter => [25, 25, 25],
        Platform::Ncm => [224, 36, 44],
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
        let initial = author
            .chars()
            .find(|character| !character.is_whitespace())
            .unwrap_or('分');
        let text = visible_character(initial).unwrap_or('分').to_string();
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

fn circle(
    canvas: &mut RgbImage,
    center_x: u32,
    center_y: u32,
    radius: f32,
    color: [u8; 3],
    alpha: f32,
) {
    let extent = radius.ceil() as u32;
    for y in center_y.saturating_sub(extent)..=(center_y + extent).min(canvas.height() - 1) {
        for x in center_x.saturating_sub(extent)..=(center_x + extent).min(canvas.width() - 1) {
            let dx = x as f32 + 0.5 - center_x as f32;
            let dy = y as f32 + 0.5 - center_y as f32;
            let coverage = (radius - (dx * dx + dy * dy).sqrt()).clamp(0.0, 1.0);
            blend(canvas, x, y, color, alpha * coverage);
        }
    }
}

fn blend(canvas: &mut RgbImage, x: u32, y: u32, color: [u8; 3], alpha: f32) {
    if x >= canvas.width() || y >= canvas.height() || alpha <= 0.0 {
        return;
    }
    let pixel = canvas.get_pixel_mut(x, y);
    for channel in 0..3 {
        pixel[channel] = (f32::from(pixel[channel]) * (1.0 - alpha)
            + f32::from(color[channel]) * alpha)
            .round() as u8;
    }
}

/// `ab_glyph` scales by ascender + descender; CSS sizes use em. Convert so a
/// 30px Chinese glyph has the same apparent size as a browser's 30px text.
fn font_scale(size: f32) -> PxScale {
    PxScale::from(size * FONT.height_unscaled() / FONT.units_per_em().unwrap_or(1000.0))
}

fn visible_character(character: char) -> Option<char> {
    match character {
        '\r' | '\u{200b}' | '\u{200c}' | '\u{200d}' | '\u{fe0e}' | '\u{fe0f}' => None,
        '\t' => Some(' '),
        '\n' => Some('\n'),
        character if character.is_control() => None,
        character if FONT.glyph_id(character).0 == 0 => Some('□'),
        character => Some(character),
    }
}

fn text_width(text: &str, size: f32) -> f32 {
    let scaled = FONT.as_scaled(font_scale(size));
    let mut previous = None;
    let mut width = 0.0;
    for character in text.chars().filter_map(visible_character) {
        let id = scaled.glyph_id(character);
        if let Some(previous) = previous {
            width += scaled.kern(previous, id);
        }
        width += scaled.h_advance(id);
        previous = Some(id);
    }
    width
}

fn draw_text(canvas: &mut RgbImage, text: &str, x: u32, y: u32, size: f32, color: [u8; 3]) {
    let scaled = FONT.as_scaled(font_scale(size));
    let mut pen = x as f32;
    let mut previous = None;
    for character in text.chars().filter_map(visible_character) {
        let id = scaled.glyph_id(character);
        if let Some(previous) = previous {
            pen += scaled.kern(previous, id);
        }
        let glyph = id.with_scale_and_position(font_scale(size), point(pen, y as f32 + size));
        if let Some(outline) = FONT.outline_glyph(glyph) {
            let bounds = outline.px_bounds();
            outline.draw(|glyph_x, glyph_y, coverage| {
                let target_x = bounds.min.x as i64 + i64::from(glyph_x);
                let target_y = bounds.min.y as i64 + i64::from(glyph_y);
                if target_x >= 0 && target_y >= 0 {
                    blend(canvas, target_x as u32, target_y as u32, color, coverage);
                }
            });
        }
        pen += scaled.h_advance(id);
        previous = Some(id);
    }
}

fn ellipsize(text: &str, size: f32, max_width: u32) -> String {
    let mut text = text
        .chars()
        .take(256)
        .filter_map(visible_character)
        .map(|character| if character == '\n' { ' ' } else { character })
        .collect::<String>();
    if text_width(&text, size) <= max_width as f32 && text.chars().count() < 256 {
        return text;
    }
    let suffix = "…";
    while !text.is_empty() && text_width(&text, size) + text_width(suffix, size) > max_width as f32
    {
        text.pop();
    }
    if text_width(suffix, size) <= max_width as f32 {
        text.push_str(suffix);
    }
    text
}

fn wrap_text(text: &str, size: f32, max_width: u32, max_lines: usize) -> (Vec<String>, bool) {
    let mut chars = text.chars();
    let bounded = chars
        .by_ref()
        .take(MAX_TEXT_CHARS)
        .filter_map(visible_character)
        .collect::<String>();
    let mut truncated = chars.next().is_some();
    let mut lines = Vec::new();
    let paragraphs = bounded.split('\n').collect::<Vec<_>>();
    for (paragraph_index, paragraph) in paragraphs.iter().enumerate() {
        let mut current = String::new();
        for token in tokens(paragraph) {
            if !current.is_empty()
                && text_width(&(current.clone() + &token), size) > max_width as f32
            {
                lines.push(current.trim_end().to_owned());
                current.clear();
                if lines.len() >= max_lines {
                    truncated = true;
                    break;
                }
            }
            // An unbroken URL or English identifier can be wider than the card.
            // Split only such oversized tokens, otherwise keep words together.
            for character in token.chars() {
                if current.is_empty() && character.is_whitespace() {
                    continue;
                }
                current.push(character);
                if text_width(&current, size) > max_width as f32 {
                    current.pop();
                    if !current.is_empty() {
                        lines.push(std::mem::take(&mut current));
                    }
                    if lines.len() >= max_lines {
                        truncated = true;
                        break;
                    }
                    current.push(character);
                }
            }
            if lines.len() >= max_lines {
                break;
            }
        }
        if lines.len() >= max_lines {
            break;
        }
        if !current.is_empty() || paragraph.is_empty() {
            lines.push(current.trim_end().to_owned());
        }
        if lines.len() >= max_lines && paragraph_index + 1 < paragraphs.len() {
            truncated = true;
            break;
        }
    }
    while lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    if truncated && let Some(last) = lines.last_mut() {
        *last = ellipsize(&(last.clone() + "…"), size, max_width);
    }
    (lines, truncated)
}

fn tokens(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut word = String::new();
    for character in text.chars() {
        if character.is_ascii() && !character.is_whitespace() {
            word.push(character);
        } else {
            if !word.is_empty() {
                tokens.push(std::mem::take(&mut word));
            }
            tokens.push(character.to_string());
        }
    }
    if !word.is_empty() {
        tokens.push(word);
    }
    tokens
}

fn encode_png(canvas: &RgbImage) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    PngEncoder::new_with_quality(&mut output, CompressionType::Fast, PngFilterType::Adaptive)
        .write_image(
            canvas.as_raw(),
            canvas.width(),
            canvas.height(),
            image::ExtendedColorType::Rgb8,
        )?;
    if output.len() > MAX_PNG_BYTES {
        bail!("rendered card exceeds output byte limit");
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolver::card::CardMetadata;

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
        assert_eq!(plan.body_y, 120);
        assert_eq!(plan.pictures[0].width, 600);
        assert_eq!(plan.pictures[0].height, 800);
        assert!(plan.pictures[0].video);
        assert!(plan.body.iter().all(|line| !line.contains("douyin.com")));
        let png = render(&card, &assets).unwrap();
        let image = image::load_from_memory(&png).unwrap().into_rgb8();
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
        image::codecs::jpeg::JpegEncoder::new(&mut jpeg)
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
        assert!(image::load_from_memory(&output).is_ok());
    }
}
