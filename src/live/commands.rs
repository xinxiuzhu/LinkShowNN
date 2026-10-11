//! `url直播间` 命令：订阅、取消、列表和主动查询。

use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use nuono_plugin::prelude::*;

use super::{
    LiveStore,
    subscription::{LiveSubscription, SubscriptionRoute},
};
use crate::{resolver::ParserManager, runtime::state::RuntimeState};

/// 订阅存储中的当前会话身份，由 SDK 消息目标转换而来。
struct Session {
    route: SubscriptionRoute,
}

impl Session {
    fn from_event(event: &Event<'_>) -> Result<Self> {
        Ok(Self {
            route: SubscriptionRoute::from_target(&event.message_target()?),
        })
    }

    /// 保持既有「平台:实例:会话类型:会话ID」键，不加入 bot_self_id。
    fn key_prefix(&self) -> String {
        format!(
            "{}:{}:{}:{}",
            self.route.platform,
            self.route.instance_id,
            self.route.target_type,
            self.route.target_id
        )
    }

    fn owns(&self, route: &SubscriptionRoute) -> bool {
        route.platform == self.route.platform
            && route.instance_id == self.route.instance_id
            && route.target_type == self.route.target_type
            && route.target_id == self.route.target_id
    }
}

/// 处理 `url直播间` 命令；非该命令返回 `Ok(None)` 交回事件链。
pub async fn handle(
    state: &Arc<Mutex<RuntimeState>>,
    event: &Event<'_>,
    text: &str,
) -> Result<Option<PluginResponse>> {
    let Some(args) = text.strip_prefix("url直播间") else {
        return Ok(None);
    };
    if !matches!(event.sender_role(), "owner" | "admin") {
        return Ok(Some(event.reply_text("直播订阅命令需要群主或管理员权限。")));
    }
    let (config, live, parser) = {
        let state = state
            .lock()
            .map_err(|_| anyhow::anyhow!("resolver state lock poisoned"))?;
        (
            state.config.clone(),
            state.live.clone(),
            state.parser.clone(),
        )
    };
    if !config.live_enabled {
        return Ok(Some(event.reply_text("直播订阅功能已禁用。")));
    }
    let Some(live) = live else {
        return Ok(Some(event.reply_text("直播订阅尚未初始化。")));
    };
    let session = Session::from_event(event)?;
    let parts = args.split_whitespace().collect::<Vec<_>>();

    let reply = match parts.first().copied().unwrap_or("sublist") {
        "sub" => subscribe(&live, &session, &parts, config.live_max_subscriptions).await?,
        "unsub" => unsubscribe(&live, &session, &parts).await?,
        "sublist" => list(&live, &session).await,
        "push" => push(&live, &session, parser).await,
        _ => "用法: url直播间 sub|unsub|sublist|push [房间号]".to_string(),
    };
    Ok(Some(event.reply_text(reply)))
}

fn room_id(parts: &[&str], usage: &'static str) -> Result<u64> {
    parts
        .get(1)
        .context(usage)?
        .parse::<u64>()
        .context("直播间号无效")
}

async fn subscribe(
    live: &LiveStore,
    session: &Session,
    parts: &[&str],
    maximum: usize,
) -> Result<String> {
    let room = room_id(parts, "用法: url直播间 sub <房间号>")?;
    let inserted = live
        .subscribe(
            format!("{}:{room}", session.key_prefix()),
            LiveSubscription {
                room_id: room,
                route: session.route.clone(),
            },
            maximum,
        )
        .await?;
    Ok(if inserted {
        format!("已订阅 B站直播间 {room}")
    } else {
        "直播订阅已达到上限。".to_string()
    })
}

async fn unsubscribe(live: &LiveStore, session: &Session, parts: &[&str]) -> Result<String> {
    let room = room_id(parts, "用法: url直播间 unsub <房间号>")?;
    let removed = live
        .unsubscribe(&format!("{}:{room}", session.key_prefix()))
        .await?;
    Ok(if removed {
        format!("已取消订阅 B站直播间 {room}")
    } else {
        "未找到该直播间订阅。".to_string()
    })
}

/// 本会话的订阅，sublist 与 push 共用。
async fn owned(live: &LiveStore, session: &Session) -> Vec<LiveSubscription> {
    live.list()
        .await
        .into_iter()
        .filter(|item| session.owns(&item.route))
        .collect()
}

async fn list(live: &LiveStore, session: &Session) -> String {
    let rooms = owned(live, session).await;
    if rooms.is_empty() {
        return "当前会话没有直播订阅。".to_string();
    }
    format!(
        "当前直播订阅:\n{}",
        rooms
            .into_iter()
            .map(|value| value.room_id.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    )
}

async fn push(live: &LiveStore, session: &Session, parser: Option<ParserManager>) -> String {
    let Some(parser) = parser else {
        return "解析器尚未初始化。".to_string();
    };
    let mut lines = Vec::new();
    for value in owned(live, session).await {
        if let Ok(info) = parser.live_room(value.room_id).await {
            lines.push(format!(
                "{}: {} ({})",
                value.room_id,
                info.title,
                if info.live_status == 1 {
                    "直播中"
                } else {
                    "未开播"
                }
            ));
        }
    }
    if lines.is_empty() {
        "暂无可检查的直播订阅。".to_string()
    } else {
        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn group_and_private_keep_existing_subscription_keys() {
        for (message_type, id_key) in [("group", "group_id"), ("private", "user_id")] {
            let mut event = json!({
                "route": {
                    "platform": "onebot",
                    "instance_id": "main",
                    "bot_self_id": "10001"
                },
                "message_type": message_type
            });
            event[id_key] = json!("12345");
            let session = Session::from_event(&Event::new(&event)).unwrap();
            assert_eq!(
                session.key_prefix(),
                format!("onebot:main:{message_type}:12345")
            );
            let mut other_bot = session.route.clone();
            other_bot.bot_self_id = "10002".into();
            assert!(session.owns(&other_bot));
        }
    }

    #[test]
    fn channel_session_retains_parent_for_later_delivery() {
        let event = json!({
            "route": {"platform": "discord", "instance_id": "main", "bot_self_id": "bot"},
            "message_type": "channel",
            "channel_id": "18446744073709551615",
            "guild_id": "guild"
        });
        let session = Session::from_event(&Event::new(&event)).unwrap();
        assert_eq!(
            session.key_prefix(),
            "discord:main:channel:18446744073709551615"
        );
        assert_eq!(session.route.target_parent.as_deref(), Some("guild"));
    }
}
