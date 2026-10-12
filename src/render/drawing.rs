//! Pixel compositing and geometric primitives shared by card layouts.

use ::image::RgbImage;

pub(super) const WHITE: [u8; 3] = [255, 255, 255];

pub(super) fn circle(
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

pub(super) fn blend(canvas: &mut RgbImage, x: u32, y: u32, color: [u8; 3], alpha: f32) {
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
