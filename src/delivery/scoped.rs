//! Lifecycle-bound delivery used by both ordinary replies and original galleries.

use anyhow::{Result, ensure};
use nuono_plugin::{PluginTaskScope, SendableMessage};
use std::sync::Arc;

use super::{GalleryDelivery, ReplyTarget};

pub(crate) enum PreparedReply {
    Message(SendableMessage),
    Gallery(GalleryDelivery),
}

pub(crate) async fn send_scoped(
    target: ReplyTarget,
    tasks: Arc<PluginTaskScope>,
    reply: PreparedReply,
) -> Result<()> {
    let (result_tx, result_rx) = tokio::sync::oneshot::channel();
    ensure!(
        tasks.try_spawn("resolver-content-send", async move {
            let result = match reply {
                PreparedReply::Message(message) => target.send_confirmed(message).await,
                PreparedReply::Gallery(gallery) => target.send_gallery_confirmed(gallery).await,
            };
            let _ = result_tx.send(result);
        }),
        "插件已停用，解析内容未发送"
    );
    result_rx
        .await
        .map_err(|_| anyhow::anyhow!("解析内容发送已取消"))?
}
