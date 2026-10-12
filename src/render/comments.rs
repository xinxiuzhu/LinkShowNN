//! Shared comment rows: rich text, platform emotes, pictures and author rows.

use crate::render::icons::COMMENT_LIKE_ICON;

pub(super) const MARGIN: u32 = 36;
pub(super) const COMMENT_AVATAR: u32 = 42;
const COMMENT_TEXT_X: u32 = MARGIN + COMMENT_AVATAR + 16;
pub(super) const COMMENT_TEXT_WIDTH: u32 = WIDTH - MARGIN - COMMENT_TEXT_X;
const COMMENT_SIZE: f32 = 22.0;
pub(super) const COMMENT_LEADING: u32 = 32;
pub(super) const COMMENT_LIKE_SIZE: u32 = 16;
pub(super) const COMMENT_GAP: u32 = 48;
const COMMENT_LIKES_GAP: u32 = 20;

#[derive(Clone, Copy)]
pub(super) struct Palette {
    pub(super) text: [u8; 3],
    pub(super) secondary: [u8; 3],
    pub(super) icon: [u8; 3],
    pub(super) avatar: [u8; 3],
}

impl Palette {
    pub(super) const POSTER: Self = Self {
        text: [238, 239, 241],
        secondary: [175, 178, 184],
        icon: [180, 183, 190],
        avatar: [65, 70, 80],
    };
    pub(super) const GALLERY: Self = Self {
        text: [51, 51, 51],
        secondary: [115, 120, 128],
        icon: [139, 144, 151],
        avatar: [240, 242, 245],
    };
}

use crate::render::{
    drawing::{blend, circle},
    image::{decode, fit, image_info, oriented_thumbnail},
    limits::{MAX_AVATAR_PIXELS, MAX_TEXT_CHARS, WIDTH},
    text::{self, avatar_initial, draw_text, text_width},
};
use crate::resolver::CardComment;
use ::image::{DynamicImage, RgbImage, imageops::FilterType};
use std::collections::HashMap;
use unicode_segmentation::UnicodeSegmentation;

pub(super) struct CommentLayout {
    pub(super) y: u32,
    pub(super) author: String,
    pub(super) avatar_url: Option<String>,
    pub(super) body: Body,
    pub(super) likes: String,
}

const SMALL_EMOTE: u32 = 26;
const LARGE_EMOTE: u32 = 44;
const EMOTE_GAP: u32 = 4;
const PICTURE_GAP: u32 = 12;
const PICTURE_NOTE_HEIGHT: u32 = 26;
const MIN_PICTURE_AREA: u32 = PICTURE_GAP + PICTURE_NOTE_HEIGHT;

#[derive(Default)]
pub(super) struct Media {
    emotes: HashMap<String, EmoteImage>,
    pictures: HashMap<String, ::image::RgbaImage>,
}

struct EmoteImage {
    small: ::image::RgbaImage,
    large: ::image::RgbaImage,
}

impl Media {
    pub(super) fn prepare(comments: &[CardComment], assets: &HashMap<String, Vec<u8>>) -> Self {
        let mut result = Self::default();
        let mut emote_pixels = 0;
        let mut picture_pixels = 0;
        let mut emote_seen = std::collections::HashSet::new();
        let mut picture_seen = std::collections::HashSet::new();
        for comment in comments.iter().take(3) {
            for emote in comment.emotes.iter().take(8) {
                if emote.text.is_empty() || emote_seen.len() >= 16 || !emote_seen.insert(&emote.url)
                {
                    continue;
                }
                if let Some(large) = thumbnail(
                    assets.get(&emote.url),
                    1_000_000,
                    4_000_000,
                    &mut emote_pixels,
                    LARGE_EMOTE,
                    LARGE_EMOTE,
                ) {
                    let small = DynamicImage::ImageRgba8(large.clone())
                        .resize(SMALL_EMOTE, SMALL_EMOTE, FilterType::Lanczos3)
                        .into_rgba8();
                    result
                        .emotes
                        .insert(emote.url.clone(), EmoteImage { small, large });
                }
            }
            for url in comment.pictures.iter().take(3) {
                if picture_seen.len() >= 9 || !picture_seen.insert(url) {
                    continue;
                }
                if let Some(image) = thumbnail(
                    assets.get(url),
                    4_000_000,
                    12_000_000,
                    &mut picture_pixels,
                    260,
                    220,
                ) {
                    result.pictures.insert(url.clone(), image);
                }
            }
        }
        result
    }
}

fn thumbnail(
    bytes: Option<&Vec<u8>>,
    max_pixels: u64,
    total_limit: u64,
    used_pixels: &mut u64,
    width: u32,
    height: u32,
) -> Option<::image::RgbaImage> {
    let bytes = bytes?;
    let info = image_info(bytes).ok()?;
    let pixels = u64::from(info.width) * u64::from(info.height);
    if pixels > max_pixels || pixels > total_limit.saturating_sub(*used_pixels) {
        return None;
    }
    // Count attempted decodes too, so invalid bodies cannot evade the work cap.
    *used_pixels += pixels;
    let original = decode(bytes).ok()?;
    let (source_width, source_height) = info.display_dimensions();
    let (width, height) = fit(source_width, source_height, width, height);
    Some(oriented_thumbnail(&original, info.orientation, width, height).into_rgba8())
}

#[derive(Clone, Debug)]
enum Atom {
    Text(String),
    Emote { url: String, size: u32 },
}

#[derive(Default)]
pub(super) struct Line {
    atoms: Vec<Atom>,
    pub(super) width: f32,
    pub(super) height: u32,
}

impl Line {
    fn measure(&mut self) {
        let mut run = String::new();
        self.width = 0.0;
        self.height = COMMENT_LEADING;
        for atom in &self.atoms {
            match atom {
                Atom::Text(unit) => run.push_str(unit),
                Atom::Emote { size, .. } => {
                    self.width += text_width(&run, COMMENT_SIZE);
                    run.clear();
                    self.width += (size + EMOTE_GAP) as f32;
                    self.height = self.height.max(size + 6);
                }
            }
        }
        self.width += text_width(&run, COMMENT_SIZE);
    }

    fn is_empty(&self) -> bool {
        self.atoms
            .iter()
            .all(|atom| matches!(atom, Atom::Text(unit) if unit.chars().all(char::is_whitespace)))
    }

    #[cfg(test)]
    pub(super) fn text(&self) -> String {
        self.atoms
            .iter()
            .filter_map(|atom| match atom {
                Atom::Text(unit) => Some(unit.as_str()),
                Atom::Emote { .. } => None,
            })
            .collect()
    }
}

struct Picture {
    image: ::image::RgbaImage,
    x: u32,
}

pub(super) struct Body {
    pub(super) lines: Vec<Line>,
    pictures: Vec<Picture>,
    picture_count: usize,
    missing_pictures: usize,
    picture_note: Option<String>,
    picture_area: u32,
}

impl Body {
    pub(super) fn new(comment: &CardComment, media: &Media) -> Self {
        let lines = wrap(comment, media, COMMENT_TEXT_WIDTH, 3);
        let picture_count = comment.pictures.len().min(3);
        let pictures: Vec<_> = comment
            .pictures
            .iter()
            .take(3)
            .filter_map(|url| media.pictures.get(url))
            .map(|image| Picture {
                image: image.clone(),
                x: 0,
            })
            .collect();
        Self {
            lines,
            missing_pictures: picture_count - pictures.len(),
            pictures,
            picture_count,
            picture_note: None,
            picture_area: 0,
        }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.lines.is_empty() && self.picture_count == 0
    }

    pub(super) fn has_pictures(&self) -> bool {
        self.picture_count > 0
    }

    pub(super) fn minimum_height(&self) -> u32 {
        self.text_height()
            + if self.has_pictures() {
                MIN_PICTURE_AREA
            } else {
                0
            }
    }

    pub(super) fn height(&self) -> u32 {
        self.text_height() + self.picture_area
    }

    fn text_height(&self) -> u32 {
        self.lines.iter().map(|line| line.height).sum()
    }

    /// The caller reserves every comment's text and a small media status line
    /// before assigning extra image space, so one tall image never drops a reply.
    pub(super) fn fit_pictures(&mut self, allowed_area: u32) {
        if !self.has_pictures() {
            return;
        }
        let missing_note = if self.missing_pictures > 0 {
            Some(format!("{} 张图片未加载", self.missing_pictures))
        } else {
            None
        };
        let note_height = if missing_note.is_some() {
            PICTURE_NOTE_HEIGHT
        } else {
            0
        };
        let max_height = allowed_area.saturating_sub(PICTURE_GAP + note_height);
        if !self.pictures.is_empty() && max_height < 48 {
            self.pictures.clear();
            self.picture_note = Some(format!("{} 张图片（篇幅限制）", self.picture_count));
            self.picture_area = MIN_PICTURE_AREA;
            return;
        }
        let max_width = if self.picture_count == 1 { 260 } else { 200 };
        let max_height = max_height.min(if self.picture_count == 1 { 220 } else { 180 });
        let mut x = 0;
        let mut height = 0;
        for picture in &mut self.pictures {
            let small = DynamicImage::ImageRgba8(std::mem::take(&mut picture.image))
                .resize(max_width, max_height.max(1), FilterType::Lanczos3)
                .into_rgba8();
            picture.x = x;
            x += small.width() + PICTURE_GAP;
            height = height.max(small.height());
            picture.image = small;
        }
        self.picture_note = missing_note;
        self.picture_area = PICTURE_GAP + height + note_height;
    }

    pub(super) fn draw(
        &self,
        canvas: &mut RgbImage,
        media: &Media,
        x: u32,
        y: u32,
        palette: Palette,
    ) {
        let mut line_y = y;
        for line in &self.lines {
            let mut pen: f32 = 0.0;
            let mut text = String::new();
            for atom in &line.atoms {
                match atom {
                    Atom::Text(unit) => text.push_str(unit),
                    Atom::Emote { url, size } => {
                        if !text.is_empty() {
                            draw_text(
                                canvas,
                                &text,
                                x + pen.round() as u32,
                                line_y + (line.height - COMMENT_LEADING) / 2,
                                COMMENT_SIZE,
                                palette.text,
                            );
                            pen += text_width(&text, COMMENT_SIZE);
                            text.clear();
                        }
                        if let Some(emote) = media.emotes.get(url) {
                            let image = if *size == LARGE_EMOTE {
                                &emote.large
                            } else {
                                &emote.small
                            };
                            draw_image(
                                canvas,
                                image,
                                x + pen.round() as u32 + (size - image.width()) / 2,
                                line_y + (line.height - image.height()) / 2,
                            );
                        }
                        pen += (size + EMOTE_GAP) as f32;
                    }
                }
            }
            if !text.is_empty() {
                draw_text(
                    canvas,
                    &text,
                    x + pen.round() as u32,
                    line_y + (line.height - COMMENT_LEADING) / 2,
                    COMMENT_SIZE,
                    palette.text,
                );
            }
            line_y += line.height;
        }
        let picture_y = line_y + PICTURE_GAP;
        let mut picture_height = 0;
        for picture in &self.pictures {
            draw_image(canvas, &picture.image, x + picture.x, picture_y);
            picture_height = picture_height.max(picture.image.height());
        }
        if let Some(note) = &self.picture_note {
            draw_text(
                canvas,
                note,
                x,
                picture_y + picture_height + 3,
                17.0,
                palette.secondary,
            );
        }
    }
}

fn draw_image(canvas: &mut RgbImage, image: &::image::RgbaImage, x: u32, y: u32) {
    for (image_x, image_y, pixel) in image.enumerate_pixels() {
        blend(
            canvas,
            x + image_x,
            y + image_y,
            [pixel[0], pixel[1], pixel[2]],
            f32::from(pixel[3]) / 255.0,
        );
    }
}

fn scalar_window(value: &str, max_chars: usize) -> &str {
    let end = value
        .char_indices()
        .take(max_chars)
        .last()
        .map_or(0, |(offset, ch)| offset + ch.len_utf8());
    &value[..end]
}

fn wrap(comment: &CardComment, media: &Media, max_width: u32, max_lines: usize) -> Vec<Line> {
    let mut remaining_chars = MAX_TEXT_CHARS;
    // One extra scalar detects a grapheme crossing the budget without ever
    // asking Unicode segmentation to scan the unbounded original input.
    let bounded = scalar_window(&comment.text, remaining_chars + 1);
    let mut truncated = bounded.len() < comment.text.len();
    let mut emotes: Vec<_> = comment
        .emotes
        .iter()
        .take(8)
        .filter(|emote| !emote.text.is_empty())
        .collect();
    emotes.sort_by_key(|emote| std::cmp::Reverse(emote.text.len()));
    let mut remaining = bounded;
    let mut line = Line::default();
    let mut lines = Vec::new();
    let mut literal_remaining: usize = 0;
    while !remaining.is_empty() {
        let matched = if literal_remaining == 0 {
            emotes.iter().find_map(|emote| {
                let token_chars = emote.text.chars().take(remaining_chars + 1).count();
                // Oversize known tokens are compared only through the budget
                // window; a matching prefix is rejected whole below.
                let token = scalar_window(&emote.text, remaining_chars + 1);
                remaining
                    .starts_with(token)
                    .then_some((*emote, token_chars))
            })
        } else {
            None
        };
        // Bound source work without splitting a complete platform token or a
        // Unicode grapheme (including skin tone, variation selector and ZWJ).
        if let Some((_, token_chars)) = matched
            && token_chars > remaining_chars
        {
            truncated = true;
            break;
        }
        let (atom, consumed) = if let Some((emote, token_chars)) =
            matched.filter(|(emote, _)| media.emotes.contains_key(&emote.url))
        {
            remaining_chars -= token_chars;
            (
                Atom::Emote {
                    url: emote.url.clone(),
                    size: if emote.large {
                        LARGE_EMOTE
                    } else {
                        SMALL_EMOTE
                    },
                },
                emote.text.len(),
            )
        } else {
            // Both `remaining` and its scalar budget shrink together, so
            // this slice always contains at most remaining_chars + 1.
            let grapheme = remaining
                .graphemes(true)
                .next()
                .expect("nonempty UTF-8 text");
            let chars = grapheme.chars().count();
            if chars > remaining_chars {
                truncated = true;
                break;
            }
            remaining_chars -= chars;
            // Missing pixels keep the entire longest token literal, including
            // any prefix that could otherwise match a shorter available token.
            if let Some((emote, _)) = matched {
                literal_remaining = emote.text.len();
            }
            literal_remaining = literal_remaining.saturating_sub(grapheme.len());
            let (units, _) = text::units(grapheme, chars);
            (Atom::Text(units.concat()), grapheme.len())
        };
        remaining = &remaining[consumed..];
        if matches!(&atom, Atom::Text(unit) if unit.is_empty()) {
            continue;
        }
        if matches!(&atom, Atom::Text(unit) if unit == "\n") {
            if !line.is_empty() {
                lines.push(std::mem::take(&mut line));
            }
            if lines.len() == max_lines {
                truncated |= !remaining.trim().is_empty();
                break;
            }
            continue;
        }
        if line.is_empty()
            && matches!(&atom, Atom::Text(unit) if unit.chars().all(char::is_whitespace))
        {
            continue;
        }
        line.atoms.push(atom);
        line.measure();
        if line.width > max_width as f32 && line.atoms.len() > 1 {
            let overflow = line.atoms.pop().expect("one overflow atom");
            line.measure();
            lines.push(std::mem::take(&mut line));
            if lines.len() == max_lines {
                truncated = true;
                break;
            }
            if !matches!(&overflow, Atom::Text(unit) if unit.chars().all(char::is_whitespace)) {
                line.atoms.push(overflow);
                line.measure();
            }
        }
    }
    if lines.len() < max_lines && !line.is_empty() {
        lines.push(line);
    }
    if truncated && let Some(last) = lines.last_mut() {
        loop {
            last.atoms.push(Atom::Text("…".into()));
            last.measure();
            if last.width <= max_width as f32 || last.atoms.len() == 1 {
                break;
            }
            last.atoms.pop();
            last.atoms.pop();
        }
    }
    lines
}

/// Reserve all visible comment texts before allocating optional picture height.
pub(super) fn minimum_height(comments: &[CardComment], media: &Media) -> u32 {
    let bodies: Vec<_> = comments
        .iter()
        .take(3)
        .map(|comment| Body::new(comment, media))
        .filter(|body| !body.is_empty())
        .collect();
    if bodies.is_empty() {
        0
    } else {
        28 + bodies
            .iter()
            .map(|body| 36 + body.minimum_height() + COMMENT_GAP)
            .sum::<u32>()
            - COMMENT_GAP
    }
}

pub(super) fn layout(
    comments: &[CardComment],
    media: &Media,
    separator_y: u32,
    maximum_end: u32,
) -> Vec<CommentLayout> {
    use crate::render::{format::compact_count, text::ellipsize};
    let bodies: Vec<_> = comments
        .iter()
        .take(3)
        .map(|comment| (comment, Body::new(comment, media)))
        .filter(|(_, body)| !body.is_empty())
        .collect();
    let mut y = separator_y + 28;
    let minimum: u32 = bodies
        .iter()
        .map(|(_, body)| 36 + body.minimum_height() + COMMENT_GAP)
        .sum();
    let mut extra = maximum_end.saturating_sub(y + minimum.saturating_sub(COMMENT_GAP));
    let mut picture_comments = bodies
        .iter()
        .filter(|(_, body)| body.has_pictures())
        .count() as u32;
    let mut rows = Vec::new();
    for (comment, mut body) in bodies {
        if body.has_pictures() {
            let minimum_height = body.minimum_height();
            let picture_area = minimum_height - body.text_height();
            body.fit_pictures(picture_area + extra / picture_comments);
            extra = extra.saturating_sub(body.height().saturating_sub(minimum_height));
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
        if y + row_height > maximum_end {
            break;
        }
        rows.push(CommentLayout {
            y,
            author,
            avatar_url: comment.avatar_url.clone(),
            body,
            likes,
        });
        y += row_height + COMMENT_GAP;
    }
    rows
}

pub(super) fn draw(
    canvas: &mut RgbImage,
    assets: &HashMap<String, Vec<u8>>,
    comments: &[CommentLayout],
    media: &Media,
    y: u32,
    palette: Palette,
) {
    if comments.is_empty() {
        return;
    }
    for x in MARGIN..WIDTH - MARGIN {
        blend(canvas, x, y, palette.secondary, 0.22);
    }
    for (index, comment) in comments.iter().enumerate() {
        // A quiet divider and breathing room separate replies without a
        // heading or pill. Keep the line aligned with the comment body.
        if index > 0 {
            for x in COMMENT_TEXT_X..WIDTH - MARGIN {
                blend(
                    canvas,
                    x,
                    comment.y - COMMENT_GAP / 2,
                    palette.secondary,
                    0.12,
                );
            }
        }
        draw_comment_avatar(canvas, assets, comment, palette);
        draw_text(
            canvas,
            &comment.author,
            COMMENT_TEXT_X,
            comment.y + 3,
            19.0,
            palette.secondary,
        );
        let icon_x =
            COMMENT_TEXT_X + text_width(&comment.author, 19.0).ceil() as u32 + COMMENT_LIKES_GAP;
        let count_x = icon_x + COMMENT_LIKE_SIZE + 8;
        for (x, y, pixel) in COMMENT_LIKE_ICON.enumerate_pixels() {
            blend(
                canvas,
                icon_x + x,
                comment.y + 6 + y,
                palette.icon,
                f32::from(pixel[3]) / 255.0,
            );
        }
        draw_text(
            canvas,
            &comment.likes,
            count_x,
            comment.y + 3,
            17.0,
            palette.secondary,
        );
        comment
            .body
            .draw(canvas, media, COMMENT_TEXT_X, comment.y + 36, palette);
    }
}

fn draw_comment_avatar(
    canvas: &mut RgbImage,
    assets: &HashMap<String, Vec<u8>>,
    comment: &CommentLayout,
    palette: Palette,
) {
    let radius = COMMENT_AVATAR as f32 / 2.0;
    circle(
        canvas,
        MARGIN + COMMENT_AVATAR / 2,
        comment.y + COMMENT_AVATAR / 2,
        radius,
        palette.avatar,
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
        let initial = avatar_initial(&comment.author, "评");
        let text_width = text_width(&initial, 18.0).ceil() as u32;
        draw_text(
            canvas,
            &initial,
            MARGIN + COMMENT_AVATAR.saturating_sub(text_width) / 2,
            comment.y + 7,
            18.0,
            palette.text,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::image::encode_png;
    use crate::resolver::card::CardCommentEmote;
    use ::image::Rgb;

    fn emote(text: &str, large: bool) -> CardCommentEmote {
        CardCommentEmote {
            text: text.into(),
            url: text.into(),
            large,
        }
    }

    fn png(width: u32, height: u32) -> Vec<u8> {
        encode_png(&RgbImage::from_pixel(width, height, Rgb([10, 200, 80]))).unwrap()
    }

    #[test]
    fn valid_emotes_use_longest_match_atomically_and_dynamic_line_height() {
        let comment = CardComment {
            text: "文字[MyGO_猫猫][笑哭]尾巴".into(),
            emotes: vec![
                emote("[MyGO_", false),
                emote("[MyGO_猫猫]", true),
                emote("[笑哭]", false),
            ],
            ..Default::default()
        };
        let assets = comment
            .emotes
            .iter()
            .map(|emote| (emote.url.clone(), png(64, 64)))
            .collect();
        let media = Media::prepare(std::slice::from_ref(&comment), &assets);
        let lines = wrap(&comment, &media, 112, 3);
        assert!(lines.iter().any(|line| line.height == LARGE_EMOTE + 6));
        assert!(lines.iter().all(|line| line.width <= 112.0));
        let urls: Vec<_> = lines
            .iter()
            .flat_map(|line| &line.atoms)
            .filter_map(|atom| match atom {
                Atom::Emote { url, .. } => Some(url.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(urls, ["[MyGO_猫猫]", "[笑哭]"]);
        assert_eq!(lines.iter().map(Line::text).collect::<String>(), "文字尾巴");
    }

    #[test]
    fn emote_matching_precedes_font_glyph_and_control_character_cleanup() {
        let token = "[🧑\u{200d}🚀]";
        let comment = CardComment {
            text: format!("前{token}后"),
            emotes: vec![emote(token, true)],
            ..Default::default()
        };
        let assets = HashMap::from([(token.into(), png(64, 64))]);
        let media = Media::prepare(std::slice::from_ref(&comment), &assets);
        let body = Body::new(&comment, &media);
        assert_eq!(body.lines.len(), 1);
        assert_eq!(body.lines[0].text(), "前后");
        assert!(
            body.lines[0]
                .atoms
                .iter()
                .any(|atom| matches!(atom, Atom::Emote { .. }))
        );
    }

    #[test]
    fn unicode_emoji_and_platform_images_share_complete_graphemes_and_text_metrics() {
        let prefix = "AV🌙👩🏽‍🚀❤️";
        let suffix = "👍🏽后";
        let token = "[MyGO_猫猫]";
        let comment = CardComment {
            text: format!("{prefix}{token}{suffix}"),
            emotes: vec![emote(token, true)],
            ..Default::default()
        };
        let assets = HashMap::from([(token.into(), png(64, 64))]);
        let media = Media::prepare(std::slice::from_ref(&comment), &assets);
        let lines = wrap(&comment, &media, 1000, 3);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text(), format!("{prefix}{suffix}"));
        assert_eq!(lines[0].height, LARGE_EMOTE + 6);
        assert!(
            (lines[0].width
                - (text_width(prefix, COMMENT_SIZE)
                    + (LARGE_EMOTE + EMOTE_GAP) as f32
                    + text_width(suffix, COMMENT_SIZE)))
            .abs()
                < 0.01
        );
        for emoji in ["🌙", "👩🏽‍🚀", "❤️", "👍🏽"] {
            assert!(
                lines[0]
                    .atoms
                    .iter()
                    .any(|atom| matches!(atom, Atom::Text(unit) if unit == emoji))
            );
        }
        assert_eq!(
            lines[0]
                .atoms
                .iter()
                .filter(|atom| matches!(atom, Atom::Emote { .. }))
                .count(),
            1
        );
    }

    #[test]
    fn unicode_emoji_wrap_and_truncate_without_splitting_clusters_or_image_tokens() {
        let token = "[笑哭]";
        let comment = CardComment {
            text: format!("👩🏽‍🚀{token}👍🏽❤️🌙👩🏽‍🚀"),
            emotes: vec![emote(token, false)],
            ..Default::default()
        };
        let assets = HashMap::from([(token.into(), png(32, 32))]);
        let media = Media::prepare(std::slice::from_ref(&comment), &assets);
        let width = ["👩🏽‍🚀", "👍🏽", "❤️", "🌙", "…"]
            .into_iter()
            .map(|unit| text_width(unit, COMMENT_SIZE).ceil() as u32)
            .chain([SMALL_EMOTE + EMOTE_GAP])
            .max()
            .unwrap()
            + 1;
        let full = wrap(&comment, &media, width, 20);
        assert!(full.len() > 2);
        assert_eq!(
            full.iter().map(Line::text).collect::<String>(),
            "👩🏽‍🚀👍🏽❤️🌙👩🏽‍🚀"
        );
        assert!(full.iter().all(|line| line.width <= width as f32));
        assert_eq!(
            full.iter()
                .flat_map(|line| &line.atoms)
                .filter(|atom| matches!(atom, Atom::Emote { .. }))
                .count(),
            1
        );
        let truncated = wrap(&comment, &media, width, 2);
        assert_eq!(truncated.len(), 2);
        assert!(truncated.last().unwrap().text().ends_with('…'));
        assert!(truncated.iter().all(|line| line.width <= width as f32));
        for atom in full.iter().chain(&truncated).flat_map(|line| &line.atoms) {
            match atom {
                Atom::Text(unit) => {
                    assert!(["👩🏽‍🚀", "👍🏽", "❤️", "🌙", "…"].contains(&unit.as_str()));
                }
                Atom::Emote { url, .. } => assert_eq!(url, token),
            }
        }
    }

    #[test]
    fn source_budget_never_keeps_a_partial_emoji_or_known_platform_token() {
        let token = "[笑哭]";
        let media = Media::prepare(
            &[CardComment {
                emotes: vec![emote(token, false)],
                ..Default::default()
            }],
            &HashMap::from([(token.into(), png(32, 32))]),
        );
        for suffix in ["👩🏽‍🚀", token] {
            let comment = CardComment {
                text: format!("{}A{suffix}", "\u{200b}".repeat(MAX_TEXT_CHARS - 4)),
                emotes: vec![emote(token, false)],
                ..Default::default()
            };
            let lines = wrap(&comment, &media, 300, 3);
            assert_eq!(lines.len(), 1);
            assert_eq!(lines[0].text(), "A…");
            assert!(
                lines[0]
                    .atoms
                    .iter()
                    .all(|atom| matches!(atom, Atom::Text(_)))
            );
        }
    }

    #[test]
    fn giant_combining_graphemes_are_rejected_within_the_raw_scalar_window() {
        let combining = format!("A{}", "\u{0301}".repeat(1_000_000));
        let window = scalar_window(&combining, MAX_TEXT_CHARS + 1);
        assert_eq!(window.chars().count(), MAX_TEXT_CHARS + 1);
        assert!(window.len() < combining.len());
        let media = Media::default();
        let comment = CardComment {
            text: combining.clone(),
            ..Default::default()
        };
        assert!(wrap(&comment, &media, 300, 3).is_empty());
        let comment = CardComment {
            text: format!("前{combining}"),
            ..Default::default()
        };
        let lines = wrap(&comment, &media, 300, 3);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text(), "前…");
    }

    #[test]
    fn giant_known_tokens_are_rejected_without_matching_their_unbounded_suffix() {
        let token = format!("[{}]", "x".repeat(1_000_000));
        let comment = CardComment {
            text: format!("前{token}"),
            emotes: vec![emote(&token, true), emote("[x", false)],
            ..Default::default()
        };
        let media = Media::prepare(
            std::slice::from_ref(&comment),
            &HashMap::from([("[x".into(), png(32, 32))]),
        );
        let lines = wrap(&comment, &media, 300, 3);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text(), "前…");
        assert!(
            lines[0]
                .atoms
                .iter()
                .all(|atom| matches!(atom, Atom::Text(_)))
        );
    }

    #[test]
    fn missing_bad_and_oversize_emotes_stay_as_tokens_and_text_is_truncated() {
        let comment = CardComment {
            text: "[失效][缺失][太大]".into(),
            emotes: vec![
                emote("[失", false),
                emote("[失效]", true),
                emote("[缺失]", false),
                emote("[太大]", false),
            ],
            ..Default::default()
        };
        let assets = HashMap::from([
            ("[失".into(), png(32, 32)),
            ("[失效]".into(), vec![1, 2, 3]),
            ("[太大]".into(), png(1001, 1000)),
        ]);
        let media = Media::prepare(std::slice::from_ref(&comment), &assets);
        let body = Body::new(&comment, &media);
        assert_eq!(
            body.lines.iter().map(Line::text).collect::<String>(),
            comment.text
        );
        assert!(body.lines.iter().all(|line| line.height == COMMENT_LEADING));
        let long = CardComment {
            text: "很长评论[失效]".repeat(200),
            ..comment
        };
        let body = Body::new(&long, &media);
        assert_eq!(body.lines.len(), 3);
        assert!(body.lines.last().unwrap().text().ends_with('…'));
    }

    #[test]
    fn pictures_preserve_aspect_ratio_and_missing_assets_have_status() {
        let comment = CardComment {
            text: "正文".into(),
            pictures: vec!["wide".into(), "tall".into(), "missing".into()],
            ..Default::default()
        };
        let assets = HashMap::from([
            ("wide".into(), png(400, 100)),
            ("tall".into(), png(100, 400)),
        ]);
        let media = Media::prepare(std::slice::from_ref(&comment), &assets);
        let mut body = Body::new(&comment, &media);
        body.fit_pictures(250);
        assert_eq!(body.pictures[0].image.dimensions(), (200, 50));
        assert_eq!(body.pictures[1].image.dimensions(), (45, 180));
        assert_eq!(body.picture_note.as_deref(), Some("1 张图片未加载"));
        assert!(body.height() <= body.text_height() + 250);
        let mut bounded = Body::new(&comment, &media);
        bounded.fit_pictures(MIN_PICTURE_AREA);
        assert!(bounded.pictures.is_empty());
        assert!(
            bounded
                .picture_note
                .as_deref()
                .unwrap()
                .contains("篇幅限制")
        );
    }

    #[test]
    fn aggregate_decode_budgets_bound_optional_media() {
        let comments: Vec<_> = (0..3)
            .map(|index| CardComment {
                emotes: (0..3)
                    .map(|inner| emote(&format!("[{index}-{inner}]"), false))
                    .collect(),
                pictures: (0..3)
                    .map(|inner| format!("picture-{index}-{inner}"))
                    .collect(),
                ..Default::default()
            })
            .collect();
        let mut assets = HashMap::new();
        let emote = png(1000, 1000);
        let picture = png(2000, 2000);
        for comment in &comments {
            for value in &comment.emotes {
                assets.insert(value.url.clone(), emote.clone());
            }
            for url in &comment.pictures {
                assets.insert(url.clone(), picture.clone());
            }
        }
        let media = Media::prepare(&comments, &assets);
        assert_eq!(media.emotes.len(), 4);
        assert_eq!(media.pictures.len(), 3);
    }
}
