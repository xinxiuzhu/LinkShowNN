//! Offline rich comment bodies. Remote metadata only names tokens and URLs;
//! decoded, bounded assets determine whether an inline token can be replaced.

use super::*;
use crate::resolver::card::CardComment;

const SMALL_EMOTE: u32 = 26;
const LARGE_EMOTE: u32 = 44;
const EMOTE_GAP: u32 = 4;
const PICTURE_GAP: u32 = 12;
const PICTURE_NOTE_HEIGHT: u32 = 26;
const MIN_PICTURE_AREA: u32 = PICTURE_GAP + PICTURE_NOTE_HEIGHT;

#[derive(Default)]
pub(super) struct Media {
    emotes: HashMap<String, EmoteImage>,
    pictures: HashMap<String, image::RgbaImage>,
}

struct EmoteImage {
    small: image::RgbaImage,
    large: image::RgbaImage,
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
) -> Option<image::RgbaImage> {
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
    Text(char),
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
        let font = FONT.as_scaled(scaled(&FONT, COMMENT_SIZE));
        let mut previous = None;
        self.width = 0.0;
        self.height = COMMENT_LEADING;
        for atom in &self.atoms {
            match atom {
                Atom::Text(ch) => {
                    let id = font.glyph_id(*ch);
                    if let Some(previous) = previous {
                        self.width += font.kern(previous, id);
                    }
                    self.width += font.h_advance(id);
                    previous = Some(id);
                }
                Atom::Emote { size, .. } => {
                    self.width += (size + EMOTE_GAP) as f32;
                    self.height = self.height.max(size + 6);
                    previous = None;
                }
            }
        }
    }

    fn is_empty(&self) -> bool {
        self.atoms
            .iter()
            .all(|atom| matches!(atom, Atom::Text(ch) if ch.is_whitespace()))
    }

    #[cfg(test)]
    pub(super) fn text(&self) -> String {
        self.atoms
            .iter()
            .filter_map(|atom| match atom {
                Atom::Text(ch) => Some(*ch),
                Atom::Emote { .. } => None,
            })
            .collect()
    }
}

struct Picture {
    image: image::RgbaImage,
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

    pub(super) fn draw(&self, canvas: &mut RgbImage, media: &Media, x: u32, y: u32) {
        let mut line_y = y;
        for line in &self.lines {
            let mut pen: f32 = 0.0;
            let mut text = String::new();
            for atom in &line.atoms {
                match atom {
                    Atom::Text(ch) => text.push(*ch),
                    Atom::Emote { url, size } => {
                        if !text.is_empty() {
                            draw_text(
                                canvas,
                                &text,
                                x + pen.round() as u32,
                                line_y + (line.height - COMMENT_LEADING) / 2,
                                COMMENT_SIZE,
                                PALE,
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
                    PALE,
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
                SECONDARY,
            );
        }
    }
}

fn draw_image(canvas: &mut RgbImage, image: &image::RgbaImage, x: u32, y: u32) {
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

fn wrap(comment: &CardComment, media: &Media, max_width: u32, max_lines: usize) -> Vec<Line> {
    let mut source = comment.text.chars();
    let bounded: String = source.by_ref().take(MAX_TEXT_CHARS).collect();
    let mut truncated = source.next().is_some();
    let mut emotes: Vec<_> = comment
        .emotes
        .iter()
        .take(8)
        .filter(|emote| !emote.text.is_empty())
        .collect();
    emotes.sort_by_key(|emote| std::cmp::Reverse(emote.text.len()));
    let mut remaining = bounded.as_str();
    let mut line = Line::default();
    let mut lines = Vec::new();
    let mut literal_remaining: usize = 0;
    while !remaining.is_empty() {
        let matched = if literal_remaining == 0 {
            emotes
                .iter()
                .find(|emote| remaining.starts_with(&emote.text))
        } else {
            None
        };
        let (atom, consumed) =
            if let Some(emote) = matched.filter(|emote| media.emotes.contains_key(&emote.url)) {
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
                let ch = remaining.chars().next().expect("nonempty UTF-8 text");
                // Missing pixels keep the entire longest token literal, including
                // any prefix that could otherwise match a shorter available token.
                if let Some(emote) = matched {
                    literal_remaining = emote.text.len();
                }
                literal_remaining = literal_remaining.saturating_sub(ch.len_utf8());
                (Atom::Text(ch), ch.len_utf8())
            };
        remaining = &remaining[consumed..];
        let atom = match atom {
            Atom::Text(ch) => {
                let Some(ch) = visible_character(ch) else {
                    continue;
                };
                Atom::Text(ch)
            }
            emote => emote,
        };
        if matches!(atom, Atom::Text('\n')) {
            if !line.is_empty() {
                lines.push(std::mem::take(&mut line));
            }
            if lines.len() == max_lines {
                truncated |= !remaining.trim().is_empty();
                break;
            }
            continue;
        }
        if line.is_empty() && matches!(atom, Atom::Text(ch) if ch.is_whitespace()) {
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
            if !matches!(overflow, Atom::Text(ch) if ch.is_whitespace()) {
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
            last.atoms.push(Atom::Text('…'));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolver::card::CardCommentEmote;

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
