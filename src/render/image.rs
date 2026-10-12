//! Bounded image inspection, EXIF-aware thumbnails and PNG output.

use super::limits::{MAX_DIMENSION, MAX_IMAGE_BYTES, MAX_IMAGE_PIXELS, MAX_PNG_BYTES};
use ::image::{
    DynamicImage, ImageDecoder, ImageEncoder, ImageFormat, ImageReader, Limits, RgbImage,
    codecs::png::{CompressionType, FilterType as PngFilterType, PngEncoder},
    imageops::FilterType,
    metadata::Orientation,
};
use anyhow::{Context, Result, bail, ensure};
use std::io::Cursor;

#[derive(Clone, Copy)]
pub(super) struct ImageInfo {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) orientation: Orientation,
}

impl ImageInfo {
    pub(super) fn display_dimensions(self) -> (u32, u32) {
        if swaps_axes(self.orientation) {
            (self.height, self.width)
        } else {
            (self.width, self.height)
        }
    }
}

pub(super) fn fit(width: u32, height: u32, max_width: u32, max_height: u32) -> (u32, u32) {
    let scale =
        (f64::from(max_width) / f64::from(width)).min(f64::from(max_height) / f64::from(height));
    (
        (f64::from(width) * scale).floor().max(1.0) as u32,
        (f64::from(height) * scale).floor().max(1.0) as u32,
    )
}

pub(super) fn reader(bytes: &[u8]) -> Result<ImageReader<Cursor<&[u8]>>> {
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

pub(super) fn dimensions(bytes: &[u8]) -> Result<(u32, u32)> {
    Ok(image_info(bytes)?.display_dimensions())
}

pub(super) fn image_info(bytes: &[u8]) -> Result<ImageInfo> {
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

pub(super) fn decode(bytes: &[u8]) -> Result<DynamicImage> {
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

pub(super) fn oriented_thumbnail(
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

pub(super) fn encode_png(canvas: &RgbImage) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    PngEncoder::new_with_quality(&mut output, CompressionType::Fast, PngFilterType::Adaptive)
        .write_image(
            canvas.as_raw(),
            canvas.width(),
            canvas.height(),
            ::image::ExtendedColorType::Rgb8,
        )?;
    if output.len() > MAX_PNG_BYTES {
        bail!("rendered card exceeds output byte limit");
    }
    Ok(output)
}

pub(super) fn rounded_png(canvas: RgbImage) -> Result<Vec<u8>> {
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
            ::image::ExtendedColorType::Rgba8,
        )?;
    ensure!(
        output.len() <= MAX_PNG_BYTES,
        "rendered poster exceeds output byte limit"
    );
    Ok(output)
}
