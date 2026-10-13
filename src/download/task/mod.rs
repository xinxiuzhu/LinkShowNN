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
    download::{DownloadCandidate, DownloadReservation, Downloaded, MediaKind, MediaSizeLimit},
    resolver::Resolved,
};

/// Start downloading immediately, but keep every follow-up behind this card's
/// confirmed send. Dropping the returned handle closes the gate.
pub fn schedule(
    state: &Arc<Mutex<RuntimeState>>,
    event: &Event<'_>,
    resolved: &Resolved,
) -> Option<CardFirst> {
    let (maximum_duration, transport, delivery_slots, bot, tasks) = {
        let state = state.lock().ok()?;
        (
            state.config.video_duration_maximum,
            state.config.media_transport.clone(),
            state.media_delivery_slots.clone()?,
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
        return schedule_notice(&tasks, target, wait_for_card, notice).then_some(first);
    };
    let media_kind = match candidate.kind {
        MediaKind::Video => "video",
        MediaKind::Audio => "audio",
    };
    info!(filename = %candidate.filename, media_kind,
        has_separate_audio = candidate.audio_url.is_some(),
        "media download candidate created");
    let reservation = match reserve(state) {
        Ok(reservation) => reservation,
        Err(notice) => {
            return schedule_notice(&tasks, target, wait_for_card, notice.into()).then_some(first);
        }
    };
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
                    Ok(media)
                }
                Err(error) => {
                    if let Ok(mut state) = stats.lock() {
                        state.metrics.fail(&error);
                    }
                    warn!(filename = %filename, media_kind, error = %error, "background media download failed");
                    Err(error)
                }
            }
        };
        if let Some(result) = after_card(wait_for_card, download).await {
            info!(filename = %filename, media_kind, media_transport = %transport,
                "card send confirmed, dispatching media download result");
            // Encode only after the card is confirmed: completed downloads wait
            // on disk, without retaining large Base64 strings behind the gate.
            let sent = match result {
                Ok(media) => send_media(&target, &candidate, &media, &transport, &delivery_slots).await,
                Err(error) => target.send_confirmed(failure_notice(&error, candidate.kind).into()).await,
            };
            match sent {
                Ok(()) => info!(filename = %filename, media_kind, "media download result send confirmed"),
                Err(error) => {
                    warn!(filename = %filename, media_kind, error = %error, "media download result send failed");
                    if error.to_string().contains("delivery budget") {
                        let notice = "媒体与缩略图超过 64 MiB 内容传输上限，已放弃发送。大文件可选择本地路径传输，并确保 OneBot 能读取共享目录。";
                        if let Err(error) = target.send_confirmed(notice.into()).await {
                            warn!(error = %error, "media delivery limit notice send failed");
                        }
                    }
                }
            }
        } else {
            debug!(filename = %filename, media_kind,
                "media delivery cancelled: card send was not confirmed");
        }
    }).then_some(first)
}

fn schedule_notice(
    tasks: &Arc<PluginTaskScope>,
    target: delivery::ReplyTarget,
    wait_for_card: tokio::sync::oneshot::Receiver<()>,
    notice: String,
) -> bool {
    tasks.try_spawn("media-skip-notice", async move {
        if let Some(notice) = after_card(wait_for_card, std::future::ready(notice)).await {
            info!(reason = %notice, "card send confirmed, sending media skip reason");
            if let Err(error) = target.send_confirmed(notice.into()).await {
                warn!(error = %error, "media download skip notice send failed");
            }
        }
    })
}

/// The same receipt-bearing path handles video and MP3 voice. Unknown outcomes
/// are errors and never trigger a second media send or a local-path fallback.
async fn send_media(
    target: &delivery::ReplyTarget,
    candidate: &DownloadCandidate,
    media: &Downloaded,
    transport: &str,
    delivery_slots: &tokio::sync::Semaphore,
) -> anyhow::Result<()> {
    use anyhow::Context;
    let _permit = delivery_slots
        .acquire()
        .await
        .context("media delivery scope closed")?;
    let message =
        delivery::media_message(candidate, &media.path, media.thumb.as_deref(), transport).await?;
    target.send_confirmed(message).await
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

fn reserve(
    state: &Arc<Mutex<RuntimeState>>,
) -> std::result::Result<DownloadReservation, &'static str> {
    let mut guard = state
        .lock()
        .map_err(|_| "媒体下载状态不可用，已跳过下载，仅发送图文。")?;
    guard.metrics.candidate();
    let Some(manager) = guard.downloads.clone() else {
        guard.metrics.skip(None);
        warn!("media download skipped: download manager is not loaded");
        return Err("媒体下载器未就绪，已跳过下载，仅发送图文。");
    };
    let Some(reservation) = manager.try_reserve() else {
        guard.metrics.skip(None);
        warn!("media download skipped: download queue is full");
        return Err("媒体下载队列已满，已跳过下载，仅发送图文。");
    };
    guard.metrics.queue();
    Ok(reservation)
}

/// 体积超限是配置可调的，单独提示；其余归为通用失败。
fn failure_notice(error: &anyhow::Error, kind: MediaKind) -> String {
    let name = match kind {
        MediaKind::Video => "视频",
        MediaKind::Audio => "音频",
    };
    if let Some(limit) = error.downcast_ref::<MediaSizeLimit>() {
        let maximum = if limit.maximum_bytes % (1024 * 1024) == 0 {
            format!("{} MB", limit.maximum_bytes / (1024 * 1024))
        } else {
            format!("{} 字节", limit.maximum_bytes)
        };
        format!("{name}文件大小超过配置上限（{maximum}），已停止下载，仅发送图文。")
    } else {
        format!("{name}下载失败，已放弃发送{name}。")
    }
}

#[cfg(test)]
mod tests;
