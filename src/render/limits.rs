//! Shared canvas, source-work, decoding and output limits.

pub(super) const WIDTH: u32 = 800;
pub(super) const MAX_HEIGHT: u32 = 4096;
pub(super) const MAX_TEXT_CHARS: usize = 8000;
pub(super) const MAX_IMAGE_BYTES: usize = 12 * 1024 * 1024;
pub(super) const MAX_ASSET_BYTES: usize = 48 * 1024 * 1024;
pub(super) const MAX_DIMENSION: u32 = 12_000;
pub(super) const MAX_IMAGE_PIXELS: u64 = 24_000_000;
pub(super) const MAX_TOTAL_PIXELS: u64 = 60_000_000;
pub(super) const MAX_AVATAR_PIXELS: u64 = 4_000_000;
pub(super) const MAX_PNG_BYTES: usize = nuono_plugin::media::MAX_IMAGE_BYTES;
