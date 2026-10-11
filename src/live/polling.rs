//! 宿主任务生命周期和 B站开播轮询。
//!
//! 仅在「确认未开播 → 开播」时推送；首次检查只记基线，发送失败保留旧状态重试。

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::Result;
use nuono_plugin::prelude::*;
use tokio::sync::oneshot;

use super::{LiveStore, subscription::LiveSubscription};
use crate::runtime::state::RuntimeState;

struct PollTask {
    cancel: oneshot::Sender<()>,
    finished: oneshot::Receiver<()>,
}

#[derive(Clone, Default)]
pub struct LivePoller {
    task: Arc<Mutex<Option<PollTask>>>,
}

impl LivePoller {
    pub async fn start_polling<F, Fut>(
        &self,
        tasks: &PluginTaskScope,
        store: LiveStore,
        interval: Duration,
        mut poll: F,
    ) where
        F: FnMut(Vec<LiveSubscription>) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        self.stop().await;
        let (cancel, cancelled) = oneshot::channel();
        let (done, finished) = oneshot::channel();
        tasks.spawn("bilibili-live-poll", async move {
            let polling = async move {
                let mut ticker = tokio::time::interval(interval.max(Duration::from_secs(60)));
                loop {
                    ticker.tick().await;
                    poll(store.list().await).await;
                }
            };
            tokio::select! { _ = cancelled => {}, _ = polling => {} }
            let _ = done.send(());
        });
        if let Ok(mut slot) = self.task.lock() {
            *slot = Some(PollTask { cancel, finished });
        }
    }

    pub async fn stop(&self) {
        let handle = self.task.lock().ok().and_then(|mut slot| slot.take());
        if let Some(handle) = handle {
            let _ = handle.cancel.send(());
            let _ = handle.finished.await;
        }
    }
}

/// 启动轮询；功能关闭或依赖未就绪时静默返回，由插件生命周期决定是否重试。
pub async fn start_polling(state: &Arc<Mutex<RuntimeState>>) -> Result<()> {
    let (config, live, poller, parser, bot, tasks) = {
        let guard = state
            .lock()
            .map_err(|_| anyhow::anyhow!("resolver state lock poisoned"))?;
        (
            guard.config.clone(),
            guard.live.clone(),
            guard.live_poller.clone(),
            guard.parser.clone(),
            guard.host_bot.clone(),
            guard.tasks.clone(),
        )
    };
    if !config.live_enabled {
        return Ok(());
    }
    let (Some(live), Some(parser), Some(bot), Some(tasks)) = (live, parser, bot, tasks) else {
        return Ok(());
    };
    let previous = Arc::new(Mutex::new(HashMap::<String, bool>::new()));
    poller
        .start_polling(
            &tasks,
            live,
            Duration::from_secs(config.live_poll_interval.max(60)),
            move |subscriptions| {
                let parser = parser.clone();
                let bot = bot.clone();
                let previous = previous.clone();
                async move {
                    for subscription in subscriptions {
                        let Ok(info) = parser.live_room(subscription.room_id).await else {
                            continue;
                        };
                        let key = poll_key(&subscription);
                        let live_now = info.live_status == 1;
                        let seen = previous
                            .lock()
                            .ok()
                            .and_then(|values| values.get(&key).copied());
                        if !should_notify(seen, live_now) {
                            if let Ok(mut values) = previous.lock() {
                                values.insert(key, live_now);
                            }
                            continue;
                        }
                        let Ok(target) = subscription.route.message_target() else {
                            continue;
                        };
                        let notice = format!(
                            "B站直播提醒\n{}\n房间: {}",
                            info.title, subscription.room_id
                        );
                        // 不等待平台 receipt，与既有通知语义一致。
                        if bot.send_routed_notify(&target, notice).await.is_ok()
                            && let Ok(mut values) = previous.lock()
                        {
                            values.insert(key, true);
                        }
                    }
                }
            },
        )
        .await;
    Ok(())
}

fn should_notify(seen: Option<bool>, live_now: bool) -> bool {
    seen == Some(false) && live_now
}

/// 基线按机器人身份隔离，保留旧键格式。
fn poll_key(subscription: &LiveSubscription) -> String {
    let route = &subscription.route;
    format!(
        "{}:{}:{}:{}:{}:{}",
        route.platform,
        route.instance_id,
        route.bot_self_id,
        route.target_type,
        route.target_id,
        subscription.room_id
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::live::subscription::SubscriptionRoute;

    #[tokio::test]
    async fn host_scope_cancels_polling_and_allows_restart() {
        let store = LiveStore::load(
            std::env::temp_dir().join(format!("nuono-live-{}.json", uuid::Uuid::new_v4())),
        )
        .await
        .unwrap();
        let poller = LivePoller::default();
        let tasks = PluginTaskScope::new("linkshownn-test");
        let (started, ready) = oneshot::channel();
        let mut started = Some(started);
        poller
            .start_polling(&tasks, store.clone(), Duration::from_secs(60), move |_| {
                let started = started.take();
                async move {
                    if let Some(started) = started {
                        let _ = started.send(());
                    }
                    std::future::pending::<()>().await;
                }
            })
            .await;
        tokio::time::timeout(Duration::from_secs(2), ready)
            .await
            .unwrap()
            .unwrap();
        tasks.shutdown().await;
        tokio::time::timeout(Duration::from_secs(2), poller.stop())
            .await
            .unwrap();
        poller
            .start_polling(&tasks, store, Duration::from_secs(60), |_| async {})
            .await;
        tokio::time::timeout(Duration::from_secs(2), poller.stop())
            .await
            .unwrap();
        tasks.shutdown().await;
    }

    #[test]
    fn notifies_only_on_offline_to_live_transition() {
        assert!(should_notify(Some(false), true));
        assert!(!should_notify(None, true));
        assert!(!should_notify(Some(true), true));
        assert!(!should_notify(Some(true), false));
        assert!(!should_notify(Some(false), false));
    }

    #[test]
    fn poll_key_separates_same_room_across_sessions() {
        let subscription = |target_id: &str| LiveSubscription {
            room_id: 21452505,
            route: SubscriptionRoute {
                platform: "onebot".into(),
                instance_id: "main".into(),
                bot_self_id: "10001".into(),
                target_type: "group".into(),
                target_id: target_id.into(),
                target_parent: None,
            },
        };
        assert_ne!(
            poll_key(&subscription("100")),
            poll_key(&subscription("200"))
        );
    }
}
