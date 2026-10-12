//! Prepare bounded original images for gallery delivery, without rendering them.

use std::sync::Arc;

use anyhow::{Result, ensure};
use nuono_plugin::PluginTaskScope;

use crate::{
    delivery::{GalleryDelivery, MAX_GALLERY_IMAGES, gallery_text},
    resolver::Card,
};

#[cfg(test)]
pub(crate) mod live_test;

pub(crate) async fn prepare(
    card: Card,
    proxy: &str,
    tasks: Arc<PluginTaskScope>,
) -> Result<GalleryDelivery> {
    ensure!(!tasks.is_closed(), "插件已停用");
    let fetcher = super::images::ImageFetcher::new(proxy)?;
    let (mut sender, receiver) = tokio::sync::oneshot::channel();
    let result_tasks = tasks.clone();
    ensure!(
        tasks.try_spawn("gallery-images", async move {
            tokio::select! {
                _ = sender.closed() => {}
                result = async {
                    let total = card.image_urls().count();
                    let mut assets = fetcher.prepare_gallery(&card).await?;
                    let mut images = Vec::new();
                let sources = card.image_urls().take(MAX_GALLERY_IMAGES).collect::<Vec<_>>();
                for (index, source) in sources.iter().enumerate() {
                    let image = if sources[index + 1..].contains(source) {
                        assets.get(*source).cloned()
                    } else {
                        assets.remove(*source)
                    };
                    if let Some(image) = image {
                            images.push(image);
                        }
                    }
                    GalleryDelivery::new(
                        gallery_text(&card), images, total,
                        card.metadata().source_url.as_deref().unwrap_or_default(),
                    )
                } => { let _ = sender.send(result); }
            }
        }),
        "插件已停用，图集获取已取消"
    );
    let result = receiver
        .await
        .map_err(|_| anyhow::anyhow!("图集获取已取消"))?;
    ensure!(!result_tasks.is_closed(), "插件已停用");
    result
}
