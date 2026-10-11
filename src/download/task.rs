//! 媒体下载调度：候选筛选、并发预约、后台任务与用户可见的失败通知。
//!
//! 下载和合并本身在 [`crate::download`]；这里只决定要不要下、能不能下，以及
//! 把结果送回聊天。消息编码在 [`crate::delivery`]，目标和路由由 SDK 处理。

use std::sync::{Arc, Mutex};

use nuono_plugin::prelude::*;
use tracing::{debug, info, warn};

use crate::delivery;
use crate::runtime::state::RuntimeState;
use crate::{download::DownloadReservation, resolver::Resolved};

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
    info!(filename = %candidate.filename, has_audio = candidate.audio_url.is_some(),
        "media download candidate created");
    let reservation = reserve(state)?;
    let filename = candidate.filename.clone();
    let stats = state.clone();
    info!(filename = %filename, "media download task queued");
    tasks.try_spawn("media-download", async move {
        let download = async {
            match reservation.download(&candidate).await {
                Ok(media) => {
                    if let Ok(mut state) = stats.lock() {
                        state.metrics.complete();
                    }
                    info!(filename = %filename, path = %media.path.display(), "media download completed");
                    delivery::media_message(&candidate, &media.path, media.thumb.as_deref())
                }
                Err(error) => {
                    if let Ok(mut state) = stats.lock() {
                        state.metrics.fail(&error);
                    }
                    warn!(filename = %filename, error = %error, "background media download failed");
                    failure_notice(&error).into()
                }
            }
        };
        if let Some(message) = after_card(wait_for_card, download).await
            && let Err(error) = target.notify(message).await
        {
            warn!(filename = %filename, error = %error, "media download result send failed");
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
        let Self {
            target,
            confirmed,
            tasks,
        } = self;
        let (result_tx, result_rx) = tokio::sync::oneshot::channel();
        anyhow::ensure!(
            tasks.try_spawn("resolver-card-send", async move {
                let result = target.send_confirmed(message).await;
                let _ = result_tx.send(result);
            }),
            "插件已停用，解析卡片未发送"
        );
        result_rx
            .await
            .map_err(|_| anyhow::anyhow!("解析卡片发送已取消"))??;
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
        "视频文件体积超过上限，已放弃发送视频。"
    } else {
        "视频下载失败，已放弃发送视频。"
    }
}

#[cfg(test)]
#[path = "order_tests.rs"]
mod order_tests;

#[cfg(test)]
mod tests {
    use super::failure_notice;

    #[test]
    fn size_limit_failure_gets_its_own_notice() {
        let oversize = anyhow::anyhow!("file exceeds configured size limit of 100 MiB");
        assert!(failure_notice(&oversize).contains("体积超过上限"));
        let other = anyhow::anyhow!("connection reset by peer");
        assert!(failure_notice(&other).contains("下载失败"));
    }
}
