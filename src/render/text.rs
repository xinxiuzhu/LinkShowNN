//! Embedded fonts, color Emoji, matching text metrics and bounded wrapping.

use super::{drawing::blend, limits::MAX_TEXT_CHARS};
use ::image::{ImageFormat, ImageReader, Limits, RgbImage, imageops::FilterType};
use ab_glyph::{Font, FontArc, GlyphId, PxScale, ScaleFont, point};
use std::{
    collections::HashMap,
    hash::Hash,
    io::Cursor,
    sync::{Arc, LazyLock, Mutex},
};
use unicode_segmentation::UnicodeSegmentation;

pub(super) static FONT: LazyLock<FontArc> = LazyLock::new(|| {
    FontArc::try_from_slice(include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/fonts/NotoSansSC-Regular.otf"
    )))
    .expect("bundled Noto Sans SC must be a valid font")
});

pub(super) static BOLD: LazyLock<FontArc> = LazyLock::new(|| {
    FontArc::try_from_slice(include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/fonts/NotoSansSC-Bold.otf"
    )))
    .expect("bundled Noto Sans SC Bold must be a valid font")
});

const EMOJI_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/fonts/NotoColorEmoji.ttf"
));
const MAX_EMOJI_SCALARS: usize = 32;
const MAX_SHAPES: usize = 512;
const MAX_BITMAPS: usize = 64;
const MAX_BITMAP_SIDE: u32 = 256;
const MAX_DRAW_SIZE: u32 = 256;

static EMOJI_FACE: LazyLock<rustybuzz::Face<'static>> = LazyLock::new(|| {
    rustybuzz::Face::from_slice(EMOJI_BYTES, 0)
        .expect("bundled Noto Color Emoji must be a valid OpenType font")
});
static SHAPES: LazyLock<Mutex<BoundedCache<String, Option<u16>>>> =
    LazyLock::new(|| Mutex::new(BoundedCache::new(MAX_SHAPES)));
static BITMAPS: LazyLock<Mutex<BoundedCache<u16, Arc<::image::RgbaImage>>>> =
    LazyLock::new(|| Mutex::new(BoundedCache::new(MAX_BITMAPS)));

/// A full cache starts a fresh generation. Keys and decoded images have separate
/// fixed limits; arbitrary strings or requested font sizes cannot grow it forever.
struct BoundedCache<K, V> {
    values: HashMap<K, V>,
    capacity: usize,
}

impl<K: Eq + Hash, V> BoundedCache<K, V> {
    fn new(capacity: usize) -> Self {
        Self {
            values: HashMap::new(),
            capacity,
        }
    }

    fn insert(&mut self, key: K, value: V) {
        if self.values.len() >= self.capacity && !self.values.contains_key(&key) {
            self.values.clear();
        }
        self.values.insert(key, value);
    }
}

fn emoji_glyph(unit: &str) -> Option<u16> {
    let first = unit.chars().next()?;
    // Noto also maps ASCII digits/symbols. Only their explicit keycap sequences
    // belong here; ordinary numbers must retain the text font and its metrics.
    if unit.chars().take(MAX_EMOJI_SCALARS + 1).count() > MAX_EMOJI_SCALARS
        || unit.contains('\u{fe0e}')
        || (first.is_ascii() && !unit.contains('\u{20e3}'))
        || EMOJI_FACE.glyph_index(first).is_none()
    {
        return None;
    }
    if let Some(cached) = SHAPES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .values
        .get(unit)
        .copied()
    {
        return cached;
    }

    let mut buffer = rustybuzz::UnicodeBuffer::new();
    buffer.push_str(unit);
    buffer.set_direction(rustybuzz::Direction::LeftToRight);
    buffer.set_flags(rustybuzz::BufferFlags::REMOVE_DEFAULT_IGNORABLES);
    buffer.guess_segment_properties();
    let shaped = rustybuzz::shape(&EMOJI_FACE, &[], buffer);
    let glyph = match shaped.glyph_infos() {
        [info] if info.glyph_id != 0 => u16::try_from(info.glyph_id).ok().filter(|id| {
            EMOJI_FACE
                .glyph_raster_image(rustybuzz::ttf_parser::GlyphId(*id), u16::MAX)
                .is_some_and(|image| image.format == rustybuzz::ttf_parser::RasterImageFormat::PNG)
        }),
        _ => None,
    };
    SHAPES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(unit.to_owned(), glyph);
    glyph
}

fn emoji_image(unit: &str) -> Option<Arc<::image::RgbaImage>> {
    let id = emoji_glyph(unit)?;
    if let Some(image) = BITMAPS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .values
        .get(&id)
        .cloned()
    {
        return Some(image);
    }
    let glyph = EMOJI_FACE.glyph_raster_image(rustybuzz::ttf_parser::GlyphId(id), u16::MAX)?;
    // Decode only PNG bytes from our fixed embedded font, with an additional
    // allocation bound. Runtime media and remote SVG never enter this cache.
    let mut reader = ImageReader::with_format(Cursor::new(glyph.data), ImageFormat::Png);
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_BITMAP_SIDE);
    limits.max_image_height = Some(MAX_BITMAP_SIDE);
    limits.max_alloc = Some(u64::from(MAX_BITMAP_SIDE).pow(2) * 16);
    reader.limits(limits);
    let image = Arc::new(reader.decode().ok()?.into_rgba8());
    BITMAPS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(id, image.clone());
    Some(image)
}

/// Return indivisible display units under an input Unicode-scalar budget.
/// A grapheme that cannot fit is excluded as a whole, never cut through a ZWJ,
/// variation-selector, skin-tone, flag or keycap sequence.
pub(super) fn units(text: &str, max_chars: usize) -> (Vec<String>, bool) {
    // Bound source scanning before asking the grapheme iterator to find a whole
    // cluster: otherwise one base followed by millions of marks defeats the
    // display budget. One lookahead scalar detects a cluster crossing the limit.
    let end = text
        .char_indices()
        .take(max_chars.saturating_add(1))
        .last()
        .map_or(0, |(offset, ch)| offset + ch.len_utf8());
    let bounded = &text[..end];
    let mut result = Vec::new();
    let mut used = 0usize;
    for unit in bounded.graphemes(true) {
        let count = unit
            .chars()
            .take(max_chars.saturating_sub(used).saturating_add(1))
            .count();
        if count > max_chars.saturating_sub(used) {
            return (result, true);
        }
        used += count;
        let visible = if emoji_image(unit).is_some() {
            unit.to_owned()
        } else {
            unit.chars().filter_map(visible_character).collect()
        };
        if !visible.is_empty() {
            result.push(visible);
        }
    }
    (result, end < text.len())
}

fn scale(font: &FontArc, size: f32) -> PxScale {
    PxScale::from(size * font.height_unscaled() / font.units_per_em().unwrap_or(1000.0))
}

enum Paint {
    Glyph(GlyphId),
    Emoji(Arc<::image::RgbaImage>),
}

/// Measuring and painting consume the identical runs and advances. Emoji use a
/// single em and break kerning; adjacent ordinary glyphs retain font kerning.
fn walk(font: &FontArc, text: &str, size: f32, mut paint: impl FnMut(Paint, f32)) -> f32 {
    if !size.is_finite() || size <= 0.0 {
        return 0.0;
    }
    let scaled = font.as_scaled(scale(font, size));
    let mut pen = 0.0;
    let mut previous = None;
    for unit in units(text, MAX_TEXT_CHARS).0 {
        if let Some(image) = emoji_image(&unit) {
            paint(Paint::Emoji(image), pen);
            pen += size;
            previous = None;
            continue;
        }
        for ch in unit.chars().filter_map(visible_character) {
            // Callers wrap lines separately; line separators have no ink/advance.
            if ch == '\n' {
                previous = None;
                continue;
            }
            let id = scaled.glyph_id(ch);
            if let Some(previous) = previous {
                pen += scaled.kern(previous, id);
            }
            paint(Paint::Glyph(id), pen);
            pen += scaled.h_advance(id);
            previous = Some(id);
        }
    }
    pen
}

pub(super) fn width(font: &FontArc, text: &str, size: f32) -> f32 {
    walk(font, text, size, |_, _| {})
}

pub(super) fn draw(
    canvas: &mut RgbImage,
    font: &FontArc,
    text: &str,
    x: u32,
    y: u32,
    size: f32,
    color: [u8; 3],
) {
    let glyph_scale = scale(font, size);
    walk(font, text, size, |paint, pen| match paint {
        Paint::Glyph(id) => {
            if let Some(outline) = font.outline_glyph(
                id.with_scale_and_position(glyph_scale, point(x as f32 + pen, y as f32 + size)),
            ) {
                let bounds = outline.px_bounds();
                outline.draw(|glyph_x, glyph_y, coverage| {
                    let target_x = bounds.min.x as i64 + i64::from(glyph_x);
                    let target_y = bounds.min.y as i64 + i64::from(glyph_y);
                    if target_x >= 0 && target_y >= 0 {
                        blend(canvas, target_x as u32, target_y as u32, color, coverage);
                    }
                });
            }
        }
        Paint::Emoji(image) => {
            let side = size.round().clamp(1.0, MAX_DRAW_SIZE as f32) as u32;
            let ratio = side as f32 / image.width().max(image.height()) as f32;
            let width = (image.width() as f32 * ratio).round().max(1.0) as u32;
            let height = (image.height() as f32 * ratio).round().max(1.0) as u32;
            let small = ::image::imageops::resize(&*image, width, height, FilterType::Lanczos3);
            let left = (x as f32 + pen + (size - width as f32) / 2.0)
                .round()
                .max(0.0) as u32;
            let top = (y as f32 + (size - height as f32) / 2.0).round().max(0.0) as u32;
            for (dx, dy, pixel) in small.enumerate_pixels() {
                blend(
                    canvas,
                    left.saturating_add(dx),
                    top.saturating_add(dy),
                    [pixel[0], pixel[1], pixel[2]],
                    f32::from(pixel[3]) / 255.0,
                );
            }
        }
    });
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

pub(super) fn text_width(text: &str, size: f32) -> f32 {
    self::width(&FONT, text, size)
}

pub(super) fn draw_text(
    canvas: &mut RgbImage,
    text: &str,
    x: u32,
    y: u32,
    size: f32,
    color: [u8; 3],
) {
    self::draw(canvas, &FONT, text, x, y, size, color);
}

pub(super) fn ellipsize(text: &str, size: f32, max_width: u32) -> String {
    let (mut units, truncated) = self::units(text, 256);
    for unit in &mut units {
        if unit == "\n" {
            *unit = " ".into();
        }
    }
    let mut result = units.concat();
    if !truncated && text_width(&result, size) <= max_width as f32 {
        return result;
    }
    let suffix = "…";
    while !units.is_empty() && text_width(&(result.clone() + suffix), size) > max_width as f32 {
        units.pop();
        result = units.concat();
    }
    if text_width(suffix, size) <= max_width as f32 {
        result.push_str(suffix);
    }
    result
}

pub(super) fn wrap_text(
    text: &str,
    size: f32,
    max_width: u32,
    max_lines: usize,
) -> (Vec<String>, bool) {
    let (units, mut truncated) = self::units(text, MAX_TEXT_CHARS);
    let bounded = units.concat();
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
            for unit in token.graphemes(true) {
                if current.is_empty() && unit.chars().all(char::is_whitespace) {
                    continue;
                }
                let previous_len = current.len();
                current.push_str(unit);
                if text_width(&current, size) > max_width as f32 {
                    current.truncate(previous_len);
                    if !current.is_empty() {
                        lines.push(std::mem::take(&mut current));
                    }
                    if lines.len() >= max_lines {
                        truncated = true;
                        break;
                    }
                    current.push_str(unit);
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
    for unit in text.graphemes(true) {
        if unit.is_ascii() && !unit.chars().any(char::is_whitespace) {
            word.push_str(unit);
        } else {
            if !word.is_empty() {
                tokens.push(std::mem::take(&mut word));
            }
            tokens.push(unit.to_string());
        }
    }
    if !word.is_empty() {
        tokens.push(word);
    }
    tokens
}

pub(super) fn avatar_initial(author: &str, fallback: &str) -> String {
    self::units(author, 256)
        .0
        .into_iter()
        .find(|unit| !unit.chars().all(char::is_whitespace))
        .unwrap_or_else(|| fallback.to_string())
}

pub(super) fn clip_font(font: &FontArc, text: &str, size: f32, max_width: u32) -> String {
    let (mut units, truncated) = self::units(text, 256);
    for unit in &mut units {
        if unit == "\n" {
            *unit = " ".into();
        }
    }
    let mut result = units.concat();
    if !truncated && width(font, &result, size) <= max_width as f32 {
        return result;
    }
    while !units.is_empty() && width(font, &(result.clone() + "…"), size) > max_width as f32 {
        units.pop();
        result = units.concat();
    }
    if width(font, "…", size) <= max_width as f32 {
        result.push('…');
    }
    result
}

pub(super) fn wrap_font(
    font: &FontArc,
    text: &str,
    size: f32,
    max_width: u32,
    max_lines: usize,
) -> Vec<String> {
    if max_lines == 0 {
        return Vec::new();
    }
    let (bounded, mut truncated) = self::units(text, MAX_TEXT_CHARS);
    let mut lines = Vec::new();
    let mut line = String::new();
    for (index, unit) in bounded.iter().enumerate() {
        if unit == "\n"
            || (!line.is_empty() && width(font, &(line.clone() + unit), size) > max_width as f32)
        {
            if !line.trim().is_empty() {
                lines.push(std::mem::take(&mut line));
            }
            if lines.len() >= max_lines {
                truncated |= index + 1 < bounded.len() || unit != "\n";
                break;
            }
        }
        if unit != "\n" && !(line.is_empty() && unit.chars().all(char::is_whitespace)) {
            line.push_str(unit);
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

#[cfg(test)]
mod tests {
    use super::*;
    use ::image::Rgb;

    #[test]
    fn common_emoji_sequences_keep_one_unit_and_one_em() {
        for emoji in ["🌙", "❤️", "👍🏽", "🇨🇳", "👩‍💻", "👨‍👩‍👧‍👦", "1️⃣"]
        {
            assert!(
                emoji_image(emoji).is_some(),
                "no embedded color glyph for {emoji}"
            );
            assert_eq!(units(emoji, 32), (vec![emoji.to_string()], false));
            assert_eq!(width(&FONT, emoji, 26.0), 26.0);
        }
    }

    #[test]
    fn grapheme_budget_and_control_cleanup_never_split_sequences() {
        assert_eq!(units("A👍🏽B", 2), (vec!["A".into()], true));
        assert_eq!(units("A👍🏽B", 3), (vec!["A".into(), "👍🏽".into()], true));
        assert_eq!(
            units("A👍🏽B", 4),
            (vec!["A".into(), "👍🏽".into(), "B".into()], false)
        );
        assert_eq!(
            units("a\t\r\nb\0", 20),
            (vec!["a".into(), " ".into(), "\n".into(), "b".into()], false)
        );
        assert_eq!(units("\u{10ffff}", 1), (vec!["□".into()], false));
        let long = format!("🌙{}", "\u{0301}".repeat(80));
        assert!(emoji_glyph(&long).is_none());
        assert!(units(&long, 32).0.is_empty());
        let huge_cluster = format!("A{}", "\u{0301}".repeat(1_000_000));
        assert_eq!(units(&huge_cluster, 32), (Vec::<String>::new(), true));
        assert_eq!(width(&FONT, &huge_cluster, 26.0), 0.0);
    }

    #[test]
    fn ordinary_ascii_and_text_variation_selectors_keep_text_metrics() {
        for text in ["1", "9", "#", "*", "A", "\n", "❤\u{fe0e}"] {
            assert!(
                emoji_image(text).is_none(),
                "unexpected color glyph for {text:?}"
            );
        }
        let size = 26.0;
        let scaled = FONT.as_scaled(scale(&FONT, size));
        let mut expected = 0.0;
        let mut previous = None;
        for ch in "AV0123".chars() {
            let id = scaled.glyph_id(ch);
            if let Some(previous) = previous {
                expected += scaled.kern(previous, id);
            }
            expected += scaled.h_advance(id);
            previous = Some(id);
        }
        assert_eq!(width(&FONT, "AV0123", size), expected);
        assert_eq!(
            width(&FONT, "A🌙V", size),
            scaled.h_advance(scaled.glyph_id('A')) + size + scaled.h_advance(scaled.glyph_id('V'))
        );
    }

    #[test]
    fn drawing_uses_measured_runs_and_retains_color_without_system_fonts() {
        let mut canvas = RgbImage::from_pixel(200, 60, Rgb([0, 0, 0]));
        let measured = width(&FONT, "A🌙V", 32.0);
        let mut positions = Vec::new();
        let advance = walk(&FONT, "A🌙V", 32.0, |paint, pen| {
            positions.push((matches!(paint, Paint::Emoji(_)), pen));
        });
        assert_eq!(advance, measured);
        assert_eq!(positions.len(), 3);
        assert!(positions[1].0);
        assert_eq!(positions[2].1 - positions[1].1, 32.0);
        draw(&mut canvas, &FONT, "A🌙V", 4, 4, 32.0, [255, 255, 255]);
        let colored: Vec<_> = canvas
            .enumerate_pixels()
            .filter(|(_, _, p)| p[0].max(p[1]).max(p[2]) - p[0].min(p[1]).min(p[2]) > 30)
            .collect();
        assert!(
            colored.len() > 40,
            "emoji must paint colored pixels, not a text box"
        );
        assert!(
            colored
                .iter()
                .all(|(x, _, _)| *x >= (4.0 + positions[1].1).floor() as u32
                    && *x < (4.0 + positions[2].1).ceil() as u32)
        );
        assert!(
            canvas
                .enumerate_pixels()
                .all(|(x, _, p)| x <= (4.0 + measured + 1.0).ceil() as u32 || p.0 == [0, 0, 0])
        );
    }

    #[test]
    fn caches_have_fixed_capacity_independent_of_requested_font_sizes() {
        let mut cache = BoundedCache::new(3);
        for id in 0..100 {
            cache.insert(id, id);
            assert!(cache.values.len() <= 3);
        }
        // Only glyph IDs enter the bitmap cache; different sizes share a bitmap.
        let first = emoji_glyph("🌙").unwrap();
        for size in [17.0, 26.0, 44.0, 64.0] {
            assert_eq!(width(&FONT, "🌙", size), size);
            assert_eq!(emoji_glyph("🌙"), Some(first));
        }
    }
}
