//! Share-card rendering: asset preparation, text/images and distinct card layouts.

mod comments;
mod drawing;
mod format;
mod icons;
mod image;
mod limits;
mod link_qr;
mod plain;
mod poster;
mod renderer;
mod text;

pub(crate) use renderer::CardRenderer;

use crate::resolver::Card;
use anyhow::{Context, Result, ensure};
use std::collections::HashMap;

/// The single offline entry point validates the full asset budget before choosing a layout.
fn compose(card: &Card, assets: &HashMap<String, Vec<u8>>) -> Result<Vec<u8>> {
    let asset_bytes = assets
        .values()
        .try_fold(0usize, |total, bytes| total.checked_add(bytes.len()))
        .context("card asset byte count overflow")?;
    ensure!(
        asset_bytes <= limits::MAX_ASSET_BYTES,
        "card assets exceed byte limit"
    );
    // A gallery may also include downloadable media. Only a card whose main
    // image is the poster cover uses this layout; all gallery pictures keep order.
    let metadata = card.metadata();
    let is_cover_only = card
        .poster_cover_url()
        .is_some_and(|cover| card.image_urls().all(|image| image == cover));
    if metadata.poster.is_some() && is_cover_only {
        poster::render(card, assets)
    } else {
        plain::render(card, assets)
    }
}
