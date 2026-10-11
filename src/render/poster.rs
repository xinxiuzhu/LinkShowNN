//! Video posters use the actual video cover, with a blurred continuation behind
//! structured metadata. Rendering stays offline and shares the card limits.

use super::*;
use crate::resolver::card::CardPoster;

#[path = "comment_body.rs"]
mod comment_body;

const MARGIN: u32 = 36;
const TEXT_WIDTH: u32 = WIDTH - MARGIN * 2;
const TITLE_SIZE: f32 = 44.0;
const TITLE_LEADING: u32 = 58;
const DETAIL_SIZE: f32 = 17.0;
const DESCRIPTION_SIZE: f32 = 26.0;
const DESCRIPTION_LEADING: u32 = 38;
const POSTER_AVATAR: u32 = 96;
const PALE: [u8; 3] = [238, 239, 241];
const SECONDARY: [u8; 3] = [175, 178, 184];
const ICON: [u8; 3] = [180, 183, 190];
const ICON_SIZE: u32 = 42;
const ICON_GAP: u32 = 12;
const COMMENT_AVATAR: u32 = 42;
const COMMENT_TEXT_X: u32 = MARGIN + COMMENT_AVATAR + 16;
const COMMENT_TEXT_WIDTH: u32 = WIDTH - MARGIN - COMMENT_TEXT_X;
const COMMENT_SIZE: f32 = 22.0;
const COMMENT_LEADING: u32 = 32;
const COMMENT_LIKE_SIZE: u32 = 16;
const COMMENT_GAP: u32 = 48;
const COMMENT_LIKES_GAP: u32 = 20;

static BOLD: LazyLock<FontArc> = LazyLock::new(|| {
    FontArc::try_from_slice(include_bytes!("../../assets/fonts/NotoSansSC-Bold.otf"))
        .expect("bundled Noto Sans SC Bold must be a valid font")
});

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StatKind {
    Like,
    Coin,
    Favorite,
    Share,
}

struct PosterLayout {
    cover_width: u32,
    cover_height: u32,
    title_y: u32,
    title: Vec<String>,
    details_y: u32,
    details: Vec<String>,
    content_id: Option<String>,
    content_id_inline: bool,
    description_y: u32,
    description: Vec<String>,
    stats_y: u32,
    stats: Vec<(StatKind, String)>,
    author_y: u32,
    author: String,
    author_id: Option<String>,
    comments_y: u32,
    comments: Vec<CommentLayout>,
    comment_media: comment_body::Media,
    footer_y: u32,
    height: u32,
}

struct CommentLayout {
    y: u32,
    author: String,
    avatar_url: Option<String>,
    body: comment_body::Body,
    likes: String,
}

pub(super) fn render(card: &Card, assets: &HashMap<String, Vec<u8>>) -> Result<Vec<u8>> {
    let metadata = card.metadata();
    let cover_url = metadata
        .video_cover_url
        .as_deref()
        .context("poster cover is missing")?;
    let bytes = assets
        .get(cover_url)
        .context("poster cover asset is missing")?;
    let info = image_info(bytes)?;
    let comment_media = comment_body::Media::prepare(&metadata.comments, assets);
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
    let blurred = image::imageops::blur(&tiny, 7.0);
    let background = image::imageops::resize(&blurred, WIDTH, plan.height, FilterType::Triangle);
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
        }
    }
    draw_author(&mut canvas, card, assets, &plan);
    draw_comments(&mut canvas, assets, &plan);
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

#[cfg(test)]
fn layout(card: &Card, cover: ImageInfo) -> Result<PosterLayout> {
    layout_with_media(card, cover, comment_body::Media::default())
}

fn layout_with_media(
    card: &Card,
    cover: ImageInfo,
    comment_media: comment_body::Media,
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
    if let Some(count) = count_text(poster.comment_count, &poster.stats_display.comments) {
        details.push(format!("{count}评论"));
    } else if let Some(count) = poster.danmaku_count {
        details.push(format!("{}弹幕", compact_count(count)));
    }
    let details = wrap_text(&details.join("   "), DETAIL_SIZE, TEXT_WIDTH, 2).0;
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
    let stats = statistics(poster);
    let stats_y = description_y + description.len() as u32 * DESCRIPTION_LEADING + 42;
    let author_y = stats_y + if stats.is_empty() { 8 } else { 96 };
    let author_x = MARGIN + POSTER_AVATAR + 32;
    let author_width = WIDTH - MARGIN - author_x;
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
    let comments_y = author_y + POSTER_AVATAR + 48;
    let mut comment_y = comments_y + 28;
    let mut comments = Vec::new();
    let bodies: Vec<_> = metadata
        .comments
        .iter()
        .take(3)
        .map(|comment| (comment, comment_body::Body::new(comment, &comment_media)))
        .filter(|(_, body)| !body.is_empty())
        .collect();
    // Reserve all three replies and the footer first. Images share only the
    // remaining height, rather than silently pushing lower-ranked replies out.
    let minimum_comment_height: u32 = bodies
        .iter()
        .map(|(_, body)| 36 + body.minimum_height() + COMMENT_GAP)
        .sum();
    let mut extra_picture_height =
        MAX_HEIGHT.saturating_sub(comment_y + minimum_comment_height + 106 - COMMENT_GAP);
    let mut picture_comments = bodies
        .iter()
        .filter(|(_, body)| body.has_pictures())
        .count() as u32;
    for (comment, mut body) in bodies {
        if body.has_pictures() {
            let allowance = extra_picture_height / picture_comments;
            let minimum_height = body.minimum_height();
            let minimum_picture_area =
                minimum_height - body.lines.iter().map(|line| line.height).sum::<u32>();
            body.fit_pictures(minimum_picture_area + allowance);
            extra_picture_height =
                extra_picture_height.saturating_sub(body.height().saturating_sub(minimum_height));
            picture_comments -= 1;
        }
        let likes = ellipsize(
            &comment
                .like_count_display
                .clone()
                .unwrap_or_else(|| compact_count(comment.like_count)),
            17.0,
            140,
        );
        let likes_width = text_width(&likes, 17.0).ceil() as u32 + COMMENT_LIKE_SIZE + 8;
        let author = ellipsize(
            if comment.author.trim().is_empty() {
                "评论者"
            } else {
                &comment.author
            },
            19.0,
            COMMENT_TEXT_WIDTH.saturating_sub(likes_width + 24),
        );
        let row_height = 36 + body.height();
        // Comment content never pushes the footer beyond the existing canvas
        // limit, even if another part of the poster later grows taller.
        if comment_y + row_height + 106 > MAX_HEIGHT {
            break;
        }
        comments.push(CommentLayout {
            y: comment_y,
            author,
            avatar_url: comment.avatar_url.clone(),
            body,
            likes,
        });
        comment_y += row_height + COMMENT_GAP;
    }
    let height = if comments.is_empty() {
        author_y + POSTER_AVATAR + 130
    } else {
        comment_y - COMMENT_GAP + 106
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
        author_y,
        author,
        author_id,
        comments_y,
        comments,
        comment_media,
        footer_y: height - 60,
        height,
    })
}

fn statistics(poster: &CardPoster) -> Vec<(StatKind, String)> {
    [
        (
            StatKind::Like,
            count_text(poster.like_count, &poster.stats_display.likes),
        ),
        (StatKind::Coin, poster.coin_count.map(compact_count)),
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

fn count_text(exact: Option<u64>, display: &Option<String>) -> Option<String> {
    exact.map(compact_count).or_else(|| display.clone())
}

fn compact_count(count: u64) -> String {
    if count >= 100_000_000 {
        decimal_unit(count, 100_000_000, "亿")
    } else if count >= 10_000 {
        decimal_unit(count, 10_000, "万")
    } else {
        let raw = count.to_string();
        let mut out = String::new();
        for (index, digit) in raw.chars().enumerate() {
            if index > 0 && (raw.len() - index).is_multiple_of(3) {
                out.push(',');
            }
            out.push(digit);
        }
        out
    }
}

fn decimal_unit(count: u64, divisor: u64, unit: &str) -> String {
    let scale = divisor / 10;
    let rounded = count / scale + u64::from(count % scale >= scale / 2);
    let whole = rounded / 10;
    let tenth = rounded % 10;
    if tenth == 0 {
        format!("{whole}{unit}")
    } else {
        format!("{whole}.{tenth}{unit}")
    }
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
        let initial = plan
            .author
            .chars()
            .find(|ch| !ch.is_whitespace())
            .unwrap_or('分')
            .to_string();
        let x = MARGIN + (POSTER_AVATAR - text_width(&initial, 38.0).ceil() as u32) / 2;
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

fn draw_comments(canvas: &mut RgbImage, assets: &HashMap<String, Vec<u8>>, plan: &PosterLayout) {
    if plan.comments.is_empty() {
        return;
    }
    for x in MARGIN..WIDTH - MARGIN {
        blend(canvas, x, plan.comments_y, SECONDARY, 0.22);
    }
    for (index, comment) in plan.comments.iter().enumerate() {
        // A quiet divider and breathing room separate replies without a
        // heading or pill. Keep the line aligned with the comment body.
        if index > 0 {
            for x in COMMENT_TEXT_X..WIDTH - MARGIN {
                blend(canvas, x, comment.y - COMMENT_GAP / 2, SECONDARY, 0.12);
            }
        }
        draw_comment_avatar(canvas, assets, comment);
        draw_text(
            canvas,
            &comment.author,
            COMMENT_TEXT_X,
            comment.y + 3,
            19.0,
            SECONDARY,
        );
        let icon_x =
            COMMENT_TEXT_X + text_width(&comment.author, 19.0).ceil() as u32 + COMMENT_LIKES_GAP;
        let count_x = icon_x + COMMENT_LIKE_SIZE + 8;
        for (x, y, pixel) in COMMENT_LIKE_ICON.enumerate_pixels() {
            blend(
                canvas,
                icon_x + x,
                comment.y + 6 + y,
                ICON,
                f32::from(pixel[3]) / 255.0,
            );
        }
        draw_text(
            canvas,
            &comment.likes,
            count_x,
            comment.y + 3,
            17.0,
            SECONDARY,
        );
        comment
            .body
            .draw(canvas, &plan.comment_media, COMMENT_TEXT_X, comment.y + 36);
    }
}

fn draw_comment_avatar(
    canvas: &mut RgbImage,
    assets: &HashMap<String, Vec<u8>>,
    comment: &CommentLayout,
) {
    let radius = COMMENT_AVATAR as f32 / 2.0;
    circle(
        canvas,
        MARGIN + COMMENT_AVATAR / 2,
        comment.y + COMMENT_AVATAR / 2,
        radius,
        [65, 70, 80],
        1.0,
    );
    let avatar = comment
        .avatar_url
        .as_deref()
        .and_then(|url| assets.get(url))
        .and_then(|bytes| {
            let info = image_info(bytes).ok()?;
            if u64::from(info.width) * u64::from(info.height) > MAX_AVATAR_PIXELS {
                return None;
            }
            let mut small = decode(bytes).ok()?.resize_to_fill(
                COMMENT_AVATAR,
                COMMENT_AVATAR,
                FilterType::Triangle,
            );
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
                comment.y + y,
                [pixel[0], pixel[1], pixel[2]],
                (radius - distance).clamp(0.0, 1.0) * f32::from(pixel[3]) / 255.0,
            );
        }
    } else {
        let initial = comment
            .author
            .chars()
            .find(|ch| !ch.is_whitespace())
            .unwrap_or('评')
            .to_string();
        let text_width = text_width(&initial, 18.0).ceil() as u32;
        draw_text(
            canvas,
            &initial,
            MARGIN + COMMENT_AVATAR.saturating_sub(text_width) / 2,
            comment.y + 7,
            18.0,
            PALE,
        );
    }
}

fn scaled(font: &FontArc, size: f32) -> PxScale {
    PxScale::from(size * font.height_unscaled() / font.units_per_em().unwrap_or(1000.0))
}

fn width(font: &FontArc, text: &str, size: f32) -> f32 {
    let font = font.as_scaled(scaled(font, size));
    let mut previous = None;
    let mut result = 0.0;
    for ch in text.chars().filter_map(visible_character) {
        let id = font.glyph_id(ch);
        if let Some(previous) = previous {
            result += font.kern(previous, id);
        }
        result += font.h_advance(id);
        previous = Some(id);
    }
    result
}

fn draw_font(
    canvas: &mut RgbImage,
    font: &FontArc,
    text: &str,
    x: u32,
    y: u32,
    size: f32,
    color: [u8; 3],
) {
    let scale = scaled(font, size);
    let scaled_font = font.as_scaled(scale);
    let mut pen = x as f32;
    let mut previous = None;
    for ch in text.chars().filter_map(visible_character) {
        let id = scaled_font.glyph_id(ch);
        if let Some(previous) = previous {
            pen += scaled_font.kern(previous, id);
        }
        if let Some(outline) =
            font.outline_glyph(id.with_scale_and_position(scale, point(pen, y as f32 + size)))
        {
            let bounds = outline.px_bounds();
            outline.draw(|glyph_x, glyph_y, coverage| {
                let target_x = bounds.min.x as i64 + i64::from(glyph_x);
                let target_y = bounds.min.y as i64 + i64::from(glyph_y);
                if target_x >= 0 && target_y >= 0 {
                    blend(canvas, target_x as u32, target_y as u32, color, coverage);
                }
            });
        }
        pen += scaled_font.h_advance(id);
        previous = Some(id);
    }
}

fn clip_font(font: &FontArc, text: &str, size: f32, max_width: u32) -> String {
    let mut chars = text.chars();
    let mut result = chars
        .by_ref()
        .take(256)
        .filter_map(visible_character)
        .map(|ch| if ch == '\n' { ' ' } else { ch })
        .collect::<String>();
    let truncated = chars.next().is_some();
    if !truncated && width(font, &result, size) <= max_width as f32 {
        return result;
    }
    while !result.is_empty() && width(font, &(result.clone() + "…"), size) > max_width as f32 {
        result.pop();
    }
    if width(font, "…", size) <= max_width as f32 {
        result.push('…');
    }
    result
}

fn wrap_font(
    font: &FontArc,
    text: &str,
    size: f32,
    max_width: u32,
    max_lines: usize,
) -> Vec<String> {
    let mut source = text.chars();
    let bounded = source
        .by_ref()
        .take(MAX_TEXT_CHARS)
        .filter_map(visible_character)
        .collect::<Vec<_>>();
    let mut truncated = source.next().is_some();
    let mut lines = Vec::new();
    let mut line = String::new();
    for (index, ch) in bounded.iter().copied().enumerate() {
        if ch == '\n'
            || (!line.is_empty()
                && width(font, &(line.clone() + &ch.to_string()), size) > max_width as f32)
        {
            if !line.trim().is_empty() {
                lines.push(std::mem::take(&mut line));
            }
            if lines.len() >= max_lines {
                truncated |= index + 1 < bounded.len() || ch != '\n';
                break;
            }
        }
        if ch != '\n' && !(line.is_empty() && ch.is_whitespace()) {
            line.push(ch);
        }
    }
    if lines.len() < max_lines && !line.trim().is_empty() {
        lines.push(line);
    }
    if truncated && let Some(last) = lines.last_mut() {
        *last = clip_font(font, &(last.clone() + "…"), size, max_width);
    }
    lines
}

// These trusted, bundled SVGs are parsed once. Keep their curves and even-odd
// cutouts intact; only use alpha so the poster controls the monochrome tint.
static STAT_ICONS: LazyLock<[resvg::tiny_skia::Pixmap; 4]> = LazyLock::new(|| {
    [
        include_bytes!("../../assets/icons/bilibili/like.svg").as_slice(),
        include_bytes!("../../assets/icons/bilibili/coin.svg").as_slice(),
        include_bytes!("../../assets/icons/bilibili/star.svg").as_slice(),
        include_bytes!("../../assets/icons/bilibili/share.svg").as_slice(),
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

static COMMENT_LIKE_ICON: LazyLock<image::RgbaImage> = LazyLock::new(|| {
    let original = image::RgbaImage::from_raw(ICON_SIZE, ICON_SIZE, STAT_ICONS[0].data().to_vec())
        .expect("bundled statistic icon has fixed RGBA dimensions");
    image::imageops::resize(
        &original,
        COMMENT_LIKE_SIZE,
        COMMENT_LIKE_SIZE,
        FilterType::Lanczos3,
    )
});

fn draw_stat_icon(canvas: &mut RgbImage, kind: StatKind, x: u32, y: u32) {
    let index = match kind {
        StatKind::Like => 0,
        StatKind::Coin => 1,
        StatKind::Favorite => 2,
        StatKind::Share => 3,
    };
    for (offset, pixel) in STAT_ICONS[index].pixels().iter().enumerate() {
        blend(
            canvas,
            x + offset as u32 % ICON_SIZE,
            y + offset as u32 / ICON_SIZE,
            ICON,
            f32::from(pixel.alpha()) / 255.0,
        );
    }
}

fn rounded_png(canvas: RgbImage) -> Result<Vec<u8>> {
    let mut canvas = DynamicImage::ImageRgb8(canvas).into_rgba8();
    let radius = 42.0f32;
    let (width, height) = canvas.dimensions();
    for (x, y, pixel) in canvas.enumerate_pixels_mut() {
        let center_x = (x as f32 + 0.5).clamp(radius, width as f32 - radius);
        let center_y = (y as f32 + 0.5).clamp(radius, height as f32 - radius);
        let distance =
            ((x as f32 + 0.5 - center_x).powi(2) + (y as f32 + 0.5 - center_y).powi(2)).sqrt();
        pixel[3] = ((radius - distance).clamp(0.0, 1.0) * 255.0).round() as u8;
    }
    let mut output = Vec::new();
    PngEncoder::new_with_quality(&mut output, CompressionType::Fast, PngFilterType::Adaptive)
        .write_image(
            canvas.as_raw(),
            width,
            height,
            image::ExtendedColorType::Rgba8,
        )?;
    ensure!(
        output.len() <= MAX_PNG_BYTES,
        "rendered poster exceeds output byte limit"
    );
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolver::card::{CardComment, CardMetadata};

    fn comment(id: u64, text: &str) -> CardComment {
        CardComment {
            id: id.to_string(),
            author: format!("评论者{id}"),
            author_id: Some(id.to_string()),
            avatar_url: Some(format!("comment-avatar-{id}")),
            text: text.into(),
            like_count: 100 / id.max(1),
            published_at: None,
            ..Default::default()
        }
    }

    fn fixture(poster: CardPoster) -> (Card, HashMap<String, Vec<u8>>) {
        let card = Card::new()
            .with_metadata(CardMetadata {
                platform: Some(Platform::Bilibili),
                author: Some("作者名字".into()),
                avatar_url: Some("avatar".into()),
                video_cover_url: Some("cover".into()),
                poster: Some(poster),
                ..Default::default()
            })
            .image("cover");
        let assets = HashMap::from([
            (
                "cover".into(),
                encode_png(&RgbImage::from_pixel(160, 90, Rgb([20, 170, 240]))).unwrap(),
            ),
            (
                "avatar".into(),
                encode_png(&RgbImage::from_pixel(20, 20, Rgb([250, 20, 20]))).unwrap(),
            ),
        ]);
        (card, assets)
    }

    #[test]
    fn poster_uses_video_cover_not_avatar_and_has_transparent_corners() {
        let (card, assets) = fixture(CardPoster {
            title: "视频标题".into(),
            ..Default::default()
        });
        let png = super::super::render(&card, &assets).unwrap();
        let output = image::load_from_memory(&png).unwrap().into_rgba8();
        assert_eq!(output.width(), 800);
        assert!(output.height() <= MAX_HEIGHT);
        assert_eq!(output.get_pixel(0, 0)[3], 0);
        assert_eq!(output.get_pixel(799, output.height() - 1)[3], 0);
        let cover = output.get_pixel(400, 100);
        assert!(cover[2] > cover[0] * 3, "top must use the blue video cover");
        let plan = layout(&card, image_info(&assets["cover"]).unwrap()).unwrap();
        let avatar = output.get_pixel(MARGIN + 48, plan.author_y + 48);
        assert!(avatar[0] > avatar[2] * 3, "author must use the red avatar");
    }

    #[test]
    fn missing_counts_are_omitted_but_real_zero_is_retained() {
        assert!(statistics(&CardPoster::default()).is_empty());
        let stats = statistics(&CardPoster {
            coin_count: Some(0),
            like_count: Some(42_321),
            ..Default::default()
        });
        assert_eq!(
            stats,
            [
                (StatKind::Like, "4.2万".into()),
                (StatKind::Coin, "0".into())
            ]
        );
        assert_eq!(compact_count(2_003), "2,003");
        assert_eq!(compact_count(662_100), "66.2万");
        assert_eq!(compact_count(41_804), "4.2万");
        assert_eq!(compact_count(99_999), "10万");
        assert_eq!(compact_count(250_000_000), "2.5亿");
    }

    #[test]
    fn abbreviated_web_counts_render_without_inventing_exact_numbers() {
        use crate::resolver::card::CardStatsDisplay;
        let (card, assets) = fixture(CardPoster {
            title: "抖音作品".into(),
            stats_display: CardStatsDisplay {
                likes: Some("20.9万".into()),
                comments: Some("1.2万".into()),
                favorites: Some("1.4万".into()),
                shares: Some("5.2万".into()),
            },
            ..Default::default()
        });
        let plan = layout(&card, image_info(&assets["cover"]).unwrap()).unwrap();
        assert_eq!(
            plan.stats,
            [
                (StatKind::Like, "20.9万".into()),
                (StatKind::Favorite, "1.4万".into()),
                (StatKind::Share, "5.2万".into())
            ]
        );
        assert!(plan.details.join(" ").contains("1.2万评论"));
        assert_eq!(
            count_text(Some(1234), &Some("1.2万".into())).as_deref(),
            Some("1,234")
        );
    }

    #[test]
    fn portrait_cover_is_contained_and_exif_rotation_changes_its_layout() {
        let (card, _) = fixture(CardPoster::default());
        let portrait = ImageInfo {
            width: 90,
            height: 160,
            orientation: Orientation::NoTransforms,
        };
        let plan = layout(&card, portrait).unwrap();
        assert_eq!(plan.cover_height, 1400);
        assert!(plan.cover_width <= WIDTH);
        assert!((plan.cover_width as f32 / plan.cover_height as f32 - 90.0 / 160.0).abs() < 0.001);
        assert!(plan.title_y > plan.cover_height);
        let landscape = layout(
            &card,
            ImageInfo {
                orientation: Orientation::Rotate90,
                ..portrait
            },
        )
        .unwrap();
        assert_eq!((landscape.cover_width, landscape.cover_height), (800, 450));
    }

    #[test]
    fn long_title_author_ids_and_description_stay_inside_layout() {
        let (card, assets) = fixture(CardPoster {
            title: "很长的标题 ABCDEFGH ".repeat(1000),
            description: Some("正文说明".repeat(4000)),
            content_id: Some("BV0123456789".repeat(100)),
            author_id: Some("0123456789".repeat(100)),
            ..Default::default()
        });
        let mut metadata = card.metadata().clone();
        metadata.author = Some("非常长的作者名称".repeat(100));
        let card = card.with_metadata(metadata);
        let plan = layout(&card, image_info(&assets["cover"]).unwrap()).unwrap();
        assert_eq!(plan.title.len(), 4);
        assert!(
            plan.title
                .iter()
                .all(|line| width(&BOLD, line, TITLE_SIZE) <= TEXT_WIDTH as f32)
        );
        assert!(plan.title.last().unwrap().ends_with('…'));
        assert!(plan.description.len() <= 6);
        assert!(
            plan.description
                .iter()
                .all(|line| text_width(line, DESCRIPTION_SIZE) <= TEXT_WIDTH as f32)
        );
        let author_width = WIDTH - MARGIN * 2 - POSTER_AVATAR - 32;
        assert!(width(&BOLD, &plan.author, 38.0) <= author_width as f32);
        assert!(text_width(plan.author_id.as_deref().unwrap(), 26.0) <= author_width as f32);
        assert!(text_width(plan.content_id.as_deref().unwrap(), DETAIL_SIZE) <= TEXT_WIDTH as f32);
        assert!(!plan.content_id_inline);
        assert!(plan.author_y + POSTER_AVATAR < plan.footer_y);
        assert!(plan.height <= MAX_HEIGHT);
    }

    #[test]
    fn bad_cover_fails_and_bad_avatar_uses_placeholder() {
        let (card, mut assets) = fixture(CardPoster::default());
        assets.insert("avatar".into(), vec![1, 2, 3]);
        assert!(render(&card, &assets).is_ok());
        assets.insert("cover".into(), vec![1, 2, 3]);
        assert!(render(&card, &assets).is_err());
        assets.remove("cover");
        assert!(render(&card, &assets).is_err());
    }

    #[test]
    fn comments_append_below_author_and_optional_avatars_do_not_break_poster() {
        let (card, mut assets) = fixture(CardPoster::default());
        let cover = image_info(&assets["cover"]).unwrap();
        let original_plan = layout(&card, cover).unwrap();
        let mut metadata = card.metadata().clone();
        metadata.comments = vec![
            comment(1, "第一条评论"),
            comment(2, "头像损坏仍然保留评论"),
            comment(3, "没有头像也可以展示"),
            comment(4, "不展示第四条"),
        ];
        metadata.comments[2].like_count = 0;
        let card = card.with_metadata(metadata);
        assets.insert(
            "comment-avatar-1".into(),
            encode_png(&RgbImage::from_pixel(20, 20, Rgb([10, 240, 20]))).unwrap(),
        );
        assets.insert("comment-avatar-2".into(), vec![1, 2, 3]);
        let plan = layout(&card, image_info(&assets["cover"]).unwrap()).unwrap();
        assert_eq!(plan.comments.len(), 3);
        assert_eq!(plan.comments[2].likes, "0");
        assert_eq!(original_plan.author_y, plan.author_y);
        assert_eq!(original_plan.title_y, plan.title_y);
        assert!(plan.comments_y > plan.author_y + POSTER_AVATAR);
        assert!(plan.comments[0].y > plan.comments_y);
        assert!(plan.height > original_plan.height);
        let png = super::super::render(&card, &assets).unwrap();
        let output = image::load_from_memory(&png).unwrap().into_rgba8();
        let avatar = output.get_pixel(
            MARGIN + COMMENT_AVATAR / 2,
            plan.comments[0].y + COMMENT_AVATAR / 2,
        );
        assert!(
            avatar[1] > 200 && avatar[0] < 30,
            "comment uses its own avatar"
        );
        assert_eq!(output.height(), plan.height);

        let mut metadata = card.metadata().clone();
        metadata.comments = vec![comment(1, " \n ")];
        let empty = card.with_metadata(metadata);
        let empty_plan = layout(&empty, image_info(&assets["cover"]).unwrap()).unwrap();
        assert!(empty_plan.comments.is_empty());
        assert_eq!(empty_plan.height, original_plan.height);
    }

    #[test]
    fn long_comments_fit_three_lines_and_keep_footer_inside_tall_poster() {
        let (card, _) = fixture(CardPoster {
            title: "很长的标题".repeat(1000),
            description: Some("正文说明".repeat(1000)),
            ..Default::default()
        });
        let mut metadata = card.metadata().clone();
        metadata.comments = (1..=3)
            .map(|id| {
                let mut value = comment(id, &"评论长文 English words ".repeat(1000));
                value.author = "很长的昵称".repeat(1000);
                value.like_count = u64::MAX;
                value
            })
            .collect();
        let card = card.with_metadata(metadata);
        let plan = layout(
            &card,
            ImageInfo {
                width: 90,
                height: 160,
                orientation: Orientation::NoTransforms,
            },
        )
        .unwrap();
        assert_eq!(plan.comments.len(), 3);
        for comment in &plan.comments {
            assert_eq!(comment.body.lines.len(), 3);
            assert!(comment.body.lines.last().unwrap().text().ends_with('…'));
            assert!(
                comment
                    .body
                    .lines
                    .iter()
                    .all(|line| { line.width <= COMMENT_TEXT_WIDTH as f32 })
            );
            let author_and_likes = text_width(&comment.author, 19.0)
                + text_width(&comment.likes, 17.0)
                + COMMENT_LIKE_SIZE as f32
                + 32.0;
            assert!(author_and_likes <= COMMENT_TEXT_WIDTH as f32);
            assert!(comment.y + 36 + 3 * COMMENT_LEADING < plan.footer_y);
        }
        assert!(plan.height <= MAX_HEIGHT);
    }

    #[test]
    fn inline_emotes_and_pictures_keep_all_three_comments_within_canvas() {
        use crate::resolver::card::CardCommentEmote;

        let (card, mut assets) = fixture(CardPoster {
            title: "长标题".repeat(200),
            description: Some("详细说明".repeat(200)),
            like_count: Some(10),
            content_id: Some("稿件编号".repeat(100)),
            ..Default::default()
        });
        assets.insert(
            "emote".into(),
            encode_png(&RgbImage::from_pixel(100, 100, Rgb([20, 240, 30]))).unwrap(),
        );
        assets.insert(
            "picture".into(),
            encode_png(&RgbImage::from_pixel(400, 300, Rgb([20, 30, 240]))).unwrap(),
        );
        let mut metadata = card.metadata().clone();
        metadata.comments = (1..=3)
            .map(|id| CardComment {
                emotes: vec![CardCommentEmote {
                    text: "[MyGO]".into(),
                    url: "emote".into(),
                    large: true,
                }],
                pictures: vec!["picture".into(); 3],
                ..comment(id, &"评论[MyGO]".repeat(200))
            })
            .collect();
        let card = card.with_metadata(metadata);
        let media = comment_body::Media::prepare(&card.metadata().comments, &assets);
        let plan = layout_with_media(
            &card,
            ImageInfo {
                width: 90,
                height: 160,
                orientation: Orientation::NoTransforms,
            },
            media,
        )
        .unwrap();
        assert_eq!(plan.comments.len(), 3);
        assert!(plan.height <= MAX_HEIGHT);
        for comment in &plan.comments {
            assert_eq!(comment.body.lines.len(), 3);
            assert!(
                comment
                    .body
                    .lines
                    .iter()
                    .all(|line| line.height > COMMENT_LEADING)
            );
            assert!(comment.y + 36 + comment.body.height() < plan.footer_y);
        }
        for comments in plan.comments.windows(2) {
            assert!(comments[0].y + 36 + comments[0].body.height() < comments[1].y);
        }
    }

    #[test]
    fn picture_only_comment_and_failed_picture_are_visible() {
        let (card, mut assets) = fixture(CardPoster::default());
        let mut metadata = card.metadata().clone();
        metadata.comments = vec![CardComment {
            pictures: vec!["comment-picture".into()],
            ..comment(1, "")
        }];
        let card = card.with_metadata(metadata);
        assets.insert(
            "comment-picture".into(),
            encode_png(&RgbImage::from_pixel(100, 80, Rgb([20, 240, 30]))).unwrap(),
        );
        for valid in [true, false] {
            if !valid {
                assets.insert("comment-picture".into(), vec![1, 2, 3]);
            }
            let media = comment_body::Media::prepare(&card.metadata().comments, &assets);
            let plan =
                layout_with_media(&card, image_info(&assets["cover"]).unwrap(), media).unwrap();
            assert_eq!(plan.comments.len(), 1);
            assert!(plan.comments[0].body.lines.is_empty());
            assert!(plan.comments[0].body.height() >= 26);
            assert!(render(&card, &assets).is_ok());
        }
    }

    #[test]
    #[ignore = "requires a local public metadata/cover fixture; performs no network requests"]
    fn real_video_poster_preview() {
        let directory =
            std::env::var_os("NUONO_POSTER_FIXTURE_DIR").expect("set NUONO_POSTER_FIXTURE_DIR");
        let directory = std::path::Path::new(&directory);
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(directory.join("view.json")).unwrap()).unwrap();
        let value = value.get("data").unwrap_or(&value);
        let stat = &value["stat"];
        let owner = &value["owner"];
        let comments_path = directory.join("comments.json");
        let comments = if comments_path.exists() {
            let value = serde_json::from_slice(&std::fs::read(comments_path).unwrap()).unwrap();
            crate::resolver::sites::bilibili::comments::map_comments(&value)
        } else {
            Vec::new()
        };
        let card = Card::new()
            .with_metadata(CardMetadata {
                platform: Some(Platform::Bilibili),
                author: owner["name"].as_str().map(str::to_owned),
                avatar_url: Some("avatar".into()),
                video_cover_url: Some("cover".into()),
                published_at: value["pubdate"].as_i64(),
                comments,
                poster: Some(CardPoster {
                    title: value["title"].as_str().unwrap_or("").into(),
                    description: value["desc"].as_str().map(str::to_owned),
                    content_id: value["bvid"].as_str().map(str::to_owned),
                    author_id: owner["mid"].as_u64().map(|id| id.to_string()),
                    view_count: stat["view"].as_u64(),
                    danmaku_count: stat["danmaku"].as_u64(),
                    comment_count: stat["reply"].as_u64(),
                    like_count: stat["like"].as_u64(),
                    coin_count: stat["coin"].as_u64(),
                    favorite_count: stat["favorite"].as_u64(),
                    share_count: stat["share"].as_u64(),
                    stats_display: Default::default(),
                }),
                ..Default::default()
            })
            .image("cover");
        let mut assets = HashMap::from([
            (
                "cover".into(),
                std::fs::read(directory.join("cover.jpg")).unwrap(),
            ),
            (
                "avatar".into(),
                std::fs::read(directory.join("avatar.jpg")).unwrap(),
            ),
        ]);
        for comment in &card.metadata().comments {
            if let Some(url) = &comment.avatar_url
                && let Ok(bytes) =
                    std::fs::read(directory.join(format!("comments-avatar-{}.jpg", comment.id)))
            {
                assets.insert(url.clone(), bytes);
            }
            for (index, emote) in comment.emotes.iter().enumerate() {
                if let Ok(bytes) = std::fs::read(
                    directory.join(format!("comment-emote-{}-{index}.bin", comment.id)),
                ) {
                    assets.insert(emote.url.clone(), bytes);
                }
            }
            for (index, url) in comment.pictures.iter().enumerate() {
                if let Ok(bytes) = std::fs::read(
                    directory.join(format!("comment-picture-{}-{index}.bin", comment.id)),
                ) {
                    assets.insert(url.clone(), bytes);
                }
            }
        }
        let output =
            std::env::var_os("NUONO_CARD_PREVIEW_DIR").expect("set NUONO_CARD_PREVIEW_DIR");
        std::fs::create_dir_all(&output).unwrap();
        std::fs::write(
            std::path::Path::new(&output).join("linkshownn-poster-preview.png"),
            super::super::render(&card, &assets).unwrap(),
        )
        .unwrap();
    }
}
