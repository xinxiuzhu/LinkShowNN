//! Offline QR codes for the original public share link.

use image::RgbImage;
use qrcode::{Color, EcLevel, QrCode};
use url::{Host, Url};

use super::drawing::blend;
use crate::resolver::url_policy::validate_public_url;

const MAX_URL_BYTES: usize = 2048;
const MAX_SIZE: u32 = 220;
const MIN_MODULE_PIXELS: u32 = 2;
const QUIET_ZONE_MODULES: u32 = 4;

pub(super) const LABEL: &str = "扫码查看原作品";

#[derive(Clone, Copy)]
pub(super) enum Theme {
    Poster,
    Plain,
}

impl Theme {
    fn foreground(self) -> [u8; 3] {
        match self {
            Self::Poster => [224, 226, 230],
            Self::Plain => [55, 59, 68],
        }
    }
}

pub(super) struct LinkQr {
    code: QrCode,
    modules: u32,
    module_pixels: u32,
    size: u32,
}

impl LinkQr {
    /// Keep the exact share URL bytes, including signed query parameters.
    /// Dense codes grow within the card's size budget rather than shrinking
    /// modules below a readable integer size; invalid inputs simply omit QR.
    pub(super) fn new(source: Option<&str>, preferred_size: u32) -> Option<Self> {
        let source = source?;
        if source.is_empty() || source.len() > MAX_URL_BYTES {
            return None;
        }
        let url = Url::parse(source).ok()?;
        validate_public_url(&url).ok()?;
        if !public_host(&url) {
            return None;
        }

        // Prefer high correction; dense links retain the medium-correction
        // size budget without truncating their original bytes.
        for level in [EcLevel::H, EcLevel::M] {
            let code = QrCode::with_error_correction_level(source.as_bytes(), level).ok()?;
            let modules = u32::try_from(code.width()).ok()?;
            let total_modules = modules.checked_add(QUIET_ZONE_MODULES * 2)?;
            let module_pixels =
                (preferred_size.min(MAX_SIZE) / total_modules).max(MIN_MODULE_PIXELS);
            let size = total_modules.checked_mul(module_pixels)?;
            if size <= MAX_SIZE {
                return Some(Self {
                    code,
                    modules,
                    module_pixels,
                    size,
                });
            }
        }
        None
    }

    pub(super) fn size(&self) -> u32 {
        self.size
    }

    /// Keep the quiet zone free of ink and the reserved card background visible.
    /// Soft module corners never change the module grid or resize its pixels.
    pub(super) fn draw(&self, canvas: &mut RgbImage, x: u32, y: u32, theme: Theme) {
        let Some(right) = x.checked_add(self.size) else {
            return;
        };
        let Some(bottom) = y.checked_add(self.size) else {
            return;
        };
        if right > canvas.width() || bottom > canvas.height() {
            return;
        }
        let foreground = theme.foreground();
        let margin = QUIET_ZONE_MODULES * self.module_pixels;
        for module_y in 0..self.modules {
            for module_x in 0..self.modules {
                if self.code[(module_x as usize, module_y as usize)] != Color::Dark {
                    continue;
                }
                let left = x + margin + module_x * self.module_pixels;
                let top = y + margin + module_y * self.module_pixels;
                for row in top..top + self.module_pixels {
                    for column in left..left + self.module_pixels {
                        let size = self.module_pixels as f32;
                        let finder_or_timing = (module_y < 8
                            && (module_x < 8 || module_x >= self.modules - 8))
                            || (module_x < 8 && module_y >= self.modules - 8)
                            || module_x == 6
                            || module_y == 6;
                        let radius = if self.module_pixels >= 3 && !finder_or_timing {
                            size * 0.28
                        } else {
                            0.0
                        };
                        let dx = column as f32 + 0.5 - left as f32;
                        let dy = row as f32 + 0.5 - top as f32;
                        let cx = dx.clamp(radius, size - radius);
                        let cy = dy.clamp(radius, size - radius);
                        let coverage = if radius == 0.0 {
                            1.0
                        } else {
                            (radius + 0.5 - ((dx - cx).powi(2) + (dy - cy).powi(2)).sqrt())
                                .clamp(0.0, 1.0)
                        };
                        blend(canvas, column, row, foreground, coverage);
                    }
                }
            }
        }
    }
}

fn public_host(url: &Url) -> bool {
    match url.host() {
        Some(Host::Domain(host)) => {
            let host = host.trim_end_matches('.');
            host.contains('.')
                && host != "localhost"
                && ![".localhost", ".local", ".internal", ".lan"]
                    .iter()
                    .any(|suffix| host.ends_with(suffix))
        }
        // The common policy handles regular IPs; also reject IPv4 loopback
        // and private addresses written as mapped IPv6 addresses.
        Some(Host::Ipv6(ip)) => ip.to_ipv4_mapped().is_none_or(|mapped| {
            Url::parse(&format!("http://{mapped}/"))
                .ok()
                .is_some_and(|mapped| validate_public_url(&mapped).is_ok())
        }),
        Some(Host::Ipv4(_)) => true,
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgb;

    #[test]
    fn qr_has_integer_modules_and_keeps_the_quiet_zone_clear() {
        let qr = LinkQr::new(Some("https://xhslink.cn/o/6Ji5ENKJVty"), 160).unwrap();
        assert!(qr.size() <= 160);
        assert!(qr.module_pixels >= MIN_MODULE_PIXELS);
        assert_eq!(qr.size(), (qr.modules + 8) * qr.module_pixels);
        let offset = 7;
        let background = Rgb([30, 40, 50]);
        let mut canvas =
            RgbImage::from_pixel(qr.size() + offset * 2, qr.size() + offset * 2, background);
        qr.draw(&mut canvas, offset, offset, Theme::Poster);
        let margin = QUIET_ZONE_MODULES * qr.module_pixels;
        for (x, y, pixel) in canvas.enumerate_pixels() {
            if x < offset || y < offset || x >= offset + qr.size() || y >= offset + qr.size() {
                assert_eq!(*pixel, background);
            } else if x < offset + margin
                || y < offset + margin
                || x >= offset + qr.size() - margin
                || y >= offset + qr.size() - margin
            {
                assert_eq!(*pixel, background);
            }
        }
    }

    #[test]
    fn unsafe_or_overdense_links_are_omitted_without_truncation() {
        for source in [
            "javascript:alert(1)",
            "https://user:password@www.bilibili.com/video/BV1jJhU6kETw",
            "http://127.0.0.1/",
            "http://[::ffff:192.168.1.1]/",
            "http://localhost/",
            "http://example.local/",
            "http://internal/",
        ] {
            assert!(LinkQr::new(Some(source), 160).is_none(), "{source}");
        }
        assert!(LinkQr::new(None, 160).is_none());
        let dense = format!(
            "https://www.xiaohongshu.com/explore/6ac6146a000000001b021019?xsec_token={}",
            "x".repeat(1700)
        );
        assert!(dense.len() < MAX_URL_BYTES);
        assert!(LinkQr::new(Some(&dense), 160).is_none());
        let signed = "https://www.xiaohongshu.com/explore/6ac6146a000000001b021019?xsec_token=AbC%2Bdef%2Fghi%3D&xsec_source=pc_share&literal=a+b&empty=";
        assert!(LinkQr::new(Some(signed), 160).is_some());
    }
}
