//! 媒体下载调度：候选筛选、并发预约、后台任务与用户可见的失败通知。
//!
//! 下载和合并本身在 [`crate::download`]；这里只决定要不要下、能不能下，以及
//! 把结果送回聊天。消息编码在 [`crate::delivery`]，目标和路由由 SDK 处理。

use std::sync::{Arc, Mutex};

use nuono_plugin::prelude::*;
use tracing::{debug, info, warn};

use crate::delivery;
use crate::runtime::state::RuntimeState;
use crate::{
    download::{DownloadReservation, MediaKind},
    resolver::Resolved,
};

/// Start downloading immediately, but keep every follow-up behind this card's
/// confirmed send. Dropping the returned handle closes the gate.
pub fn schedule(
    state: &Arc<Mutex<RuntimeState>>,
    event: &Event<'_>,
    resolved: &Resolved,
) -> Option<CardFirst> {
    let (maximum_duration, bot, tasks) = {
        let state = state.lock().ok()?;
        (
            state.config.video_duration_maximum,
            state.host_bot.clone()?,
            state.tasks.clone()?,
        )
    };
    let target = delivery::ReplyTarget::new(bot, event).ok()?;
    let (confirmed, wait_for_card) = tokio::sync::oneshot::channel();
    let first = CardFirst {
        target: target.clone(),
        confirmed,
        tasks: tasks.clone(),
    };
    let Some(candidate) = resolved.download_candidate(maximum_duration) else {
        let reason = resolved.skip_reason(maximum_duration);
        if let Ok(mut state) = state.lock() {
            state.metrics.skip(reason.as_ref());
        }
        debug!(
            maximum_duration,
            "media download skipped: no compatible download candidate"
        );
        let notice = reason?.message();
        return tasks
            .try_spawn("media-skip-notice", async move {
                if let Some(message) =
                    after_card(wait_for_card, std::future::ready(notice.into())).await
                    && let Err(error) = target.notify(message).await
                {
                    warn!(error = %error, "media download skip notice send failed");
                }
            })
            .then_some(first);
    };
    let media_kind = match candidate.kind {
        MediaKind::Video => "video",
        MediaKind::Audio => "audio",
    };
    info!(filename = %candidate.filename, media_kind,
        has_separate_audio = candidate.audio_url.is_some(),
        "media download candidate created");
    let reservation = reserve(state)?;
    let filename = candidate.filename.clone();
    let stats = state.clone();
    info!(filename = %filename, media_kind, "media download task queued");
    tasks.try_spawn("media-download", async move {
        let download = async {
            match reservation.download(&candidate).await {
                Ok(media) => {
                    if let Ok(mut state) = stats.lock() {
                        state.metrics.complete();
                    }
                    info!(filename = %filename, media_kind, path = %media.path.display(), "media download completed");
                    delivery::media_message(&candidate, &media.path, media.thumb.as_deref())
                }
                Err(error) => {
                    if let Ok(mut state) = stats.lock() {
                        state.metrics.fail(&error);
                    }
                    warn!(filename = %filename, media_kind, error = %error, "background media download failed");
                    failure_notice(&error).into()
                }
            }
        };
        if let Some(message) = after_card(wait_for_card, download).await {
            info!(filename = %filename, media_kind,
                "card send confirmed, dispatching media download result");
            if let Err(error) = target.notify(message).await {
                warn!(filename = %filename, media_kind, error = %error, "media download result send failed");
            }
        } else {
            debug!(filename = %filename, media_kind,
                "media delivery cancelled: card send was not confirmed");
        }
    }).then_some(first)
}

pub struct CardFirst {
    target: delivery::ReplyTarget,
    confirmed: tokio::sync::oneshot::Sender<()>,
    tasks: Arc<PluginTaskScope>,
}

impl CardFirst {
    /// The scoped send is cancelled on plugin shutdown. Never retry an unknown
    /// receipt: the platform might already have accepted the image.
    pub async fn send(self, message: SendableMessage) -> anyhow::Result<()> {
        self.send_prepared(delivery::PreparedReply::Message(message))
            .await
    }

    pub(crate) async fn send_gallery(
        self,
        gallery: delivery::GalleryDelivery,
    ) -> anyhow::Result<()> {
        self.send_prepared(delivery::PreparedReply::Gallery(gallery))
            .await
    }

    async fn send_prepared(self, reply: delivery::PreparedReply) -> anyhow::Result<()> {
        let Self {
            target,
            confirmed,
            tasks,
        } = self;
        delivery::send_scoped(target, tasks, reply).await?;
        let _ = confirmed.send(());
        Ok(())
    }
}

/// Poll both futures concurrently, releasing the result only after the card
/// receipt. A closed gate cancels pending work instead of sending it alone.
async fn after_card<T>(
    confirmed: tokio::sync::oneshot::Receiver<()>,
    work: impl std::future::Future<Output = T>,
) -> Option<T> {
    tokio::try_join!(async { confirmed.await.map_err(|_| ()) }, async {
        Ok::<_, ()>(work.await)
    },)
    .ok()
    .map(|(_, result)| result)
}

fn reserve(state: &Arc<Mutex<RuntimeState>>) -> Option<DownloadReservation> {
    let mut guard = state.lock().ok()?;
    guard.metrics.candidate();
    let Some(manager) = guard.downloads.clone() else {
        guard.metrics.skip(None);
        warn!("media download skipped: download manager is not loaded");
        return None;
    };
    let Some(reservation) = manager.try_reserve() else {
        guard.metrics.skip(None);
        warn!("media download skipped: download queue is full");
        return None;
    };
    guard.metrics.queue();
    Some(reservation)
}

/// 体积超限是配置可调的，单独提示；其余归为通用失败。
fn failure_notice(error: &anyhow::Error) -> &'static str {
    if error.to_string().contains("exceeds configured size limit") {
        "媒体文件体积超过上限，已放弃发送媒体。"
    } else {
        "媒体下载失败，已放弃发送媒体。"
    }
}

#[cfg(test)]
mod tests;
