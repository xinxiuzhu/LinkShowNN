//! Plain, bounded QR images for already validated login URLs.

use anyhow::{Result, anyhow, ensure};
use image::{ExtendedColorType, GrayImage, ImageEncoder, Luma, codecs::png::PngEncoder};
use qrcode::{Color, QrCode};

const MAX_URL_BYTES: usize = 2048;
const MAX_IMAGE_WIDTH: u32 = 1024;
const MAX_PNG_BYTES: usize = 256 * 1024;
const MODULE_PIXELS: u32 = 8;
const QUIET_ZONE_MODULES: u32 = 4;

/// Encode only the QR matrix, without exposing the login URL in image text or errors.
pub(crate) fn render(url: &str) -> Result<Vec<u8>> {
    ensure!(
        !url.is_empty() && url.len() <= MAX_URL_BYTES,
        "login QR input exceeds limits"
    );
    let code = QrCode::new(url.as_bytes()).map_err(|_| anyhow!("cannot encode login QR"))?;
    let modules = u32::try_from(code.width()).map_err(|_| anyhow!("login QR exceeds limits"))?;
    let width = (modules + QUIET_ZONE_MODULES * 2) * MODULE_PIXELS;
    ensure!(width <= MAX_IMAGE_WIDTH, "login QR exceeds limits");

    let mut canvas = GrayImage::from_pixel(width, width, Luma([255]));
    let margin = QUIET_ZONE_MODULES * MODULE_PIXELS;
    for module_y in 0..modules {
        for module_x in 0..modules {
            if code[(module_x as usize, module_y as usize)] != Color::Dark {
                continue;
            }
            let left = margin + module_x * MODULE_PIXELS;
            let top = margin + module_y * MODULE_PIXELS;
            for y in top..top + MODULE_PIXELS {
                for x in left..left + MODULE_PIXELS {
                    canvas.put_pixel(x, y, Luma([0]));
                }
            }
        }
    }

    let mut png = Vec::new();
    PngEncoder::new(&mut png)
        .write_image(canvas.as_raw(), width, width, ExtendedColorType::L8)
        .map_err(|_| anyhow!("cannot encode login QR image"))?;
    ensure!(png.len() <= MAX_PNG_BYTES, "login QR image exceeds limits");
    Ok(png)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FAKE_URL: &str = "https://example.invalid/login?fake_key=not-a-login-challenge";

    #[test]
    fn png_has_an_exact_white_quiet_zone_and_integer_sized_modules() {
        let png = render(FAKE_URL).unwrap();
        assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
        assert!(png.len() <= MAX_PNG_BYTES);
        let canvas = image::load_from_memory(&png).unwrap().to_luma8();
        let code = QrCode::new(FAKE_URL.as_bytes()).unwrap();
        let margin = QUIET_ZONE_MODULES * MODULE_PIXELS;
        let expected_width = (code.width() as u32 + QUIET_ZONE_MODULES * 2) * MODULE_PIXELS;
        assert_eq!(canvas.dimensions(), (expected_width, expected_width));
        assert!(expected_width <= MAX_IMAGE_WIDTH);

        // Check every pixel, including all four complete quiet-zone borders.
        for (x, y, pixel) in canvas.enumerate_pixels() {
            let in_matrix = x >= margin
                && y >= margin
                && x < expected_width - margin
                && y < expected_width - margin;
            let expected = if in_matrix
                && code[(
                    ((x - margin) / MODULE_PIXELS) as usize,
                    ((y - margin) / MODULE_PIXELS) as usize,
                )] == Color::Dark
            {
                0
            } else {
                255
            };
            assert_eq!(pixel[0], expected, "unexpected QR pixel at ({x}, {y})");
        }
    }

    #[test]
    fn input_limit_is_in_bytes_and_errors_do_not_expose_input() {
        for input in [
            String::new(),
            format!("private-fake-key-{}", "x".repeat(MAX_URL_BYTES)),
            "中".repeat(MAX_URL_BYTES / 2),
        ] {
            let error = render(&input).unwrap_err();
            assert_eq!(error.to_string(), "login QR input exceeds limits");
            assert!(!format!("{error:?}").contains("private-fake-key"));
        }
    }

    #[test]
    fn dense_qr_is_rejected_instead_of_reducing_module_pixels() {
        let input = format!(
            "https://example.invalid/private-fake-key?{}",
            "x".repeat(1800)
        );
        assert!(input.len() <= MAX_URL_BYTES);
        let code = QrCode::new(input.as_bytes()).unwrap();
        assert!((code.width() as u32 + QUIET_ZONE_MODULES * 2) * MODULE_PIXELS > MAX_IMAGE_WIDTH);
        let error = render(&input).unwrap_err();
        assert_eq!(error.to_string(), "login QR exceeds limits");
        assert!(!format!("{error:?}").contains("private-fake-key"));
    }
}
