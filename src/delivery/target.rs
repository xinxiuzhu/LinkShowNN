//! 一次解析请求的回复目标、上下文和成功发送回执。

use std::{sync::Arc, time::Duration};

use anyhow::Context;
use nuono_plugin::{Bot, Event, RuntimeMessageTarget, RuntimeTargetKind, SendableMessage};
use serde_json::{Value, json};

use super::GalleryDelivery;

/// A reply destination owned by this request, including passive-reply context.
/// The SDK still encodes the platform target and preserves native string IDs.
#[derive(Clone)]
pub(crate) struct ReplyTarget {
    bot: Arc<Bot>,
    target: RuntimeMessageTarget,
    context: serde_json::Map<String, Value>,
}

impl ReplyTarget {
    pub(crate) fn new(bot: Arc<Bot>, event: &Event<'_>) -> anyhow::Result<Self> {
        let target = event.message_target()?;
        target.validate()?;
        let context = [
            "message_type",
            "user_id",
            "group_id",
            "channel_id",
            "guild_id",
            "chat_id",
            "openid",
            "group_openid",
            "event_id",
            "qq_target_type",
            "message_id",
            "reply_context",
        ]
        .into_iter()
        .filter_map(|key| {
            event
                .raw()
                .get(key)
                .or_else(|| {
                    event
                        .raw()
                        .get("reply_context")
                        .and_then(|context| context.get(key))
                })
                .cloned()
                .map(|value| (key.into(), value))
        })
        .collect();
        Ok(Self {
            bot,
            target,
            context,
        })
    }

    fn action(&self, message: SendableMessage) -> anyhow::Result<Value> {
        let mut action = nuono_plugin::api::routed::build_send_msg_action(
            &self.target.route.platform,
            &self.target,
            &message,
        )?;
        let params = action["params"]
            .as_object_mut()
            .context("reply params missing")?;
        for (key, value) in &self.context {
            params.entry(key.clone()).or_insert_with(|| value.clone());
        }
        action["reply_current"] = json!(true);
        Ok(action)
    }

    pub(crate) async fn send_confirmed(&self, message: SendableMessage) -> anyhow::Result<()> {
        self.send_action_confirmed(self.action(message)?).await
    }

    /// 图集直接发原图或合并转发，只有成功回执才完成。
    /// 调用方应在插件任务作用域内执行整个批次；失败和未知回执都不能重发。
    pub(crate) async fn send_gallery_confirmed(
        &self,
        gallery: GalleryDelivery,
    ) -> anyhow::Result<()> {
        if !gallery.uses_forward() {
            return self.send_confirmed(gallery.into_direct(false)).await;
        }
        let onebot = self.is_onebot();
        let (action, target_key, target_id) = match (&self.target.kind, onebot) {
            (RuntimeTargetKind::Group { group_id }, true) => {
                ("send_group_forward_msg", "group_id", group_id)
            }
            (RuntimeTargetKind::Private { user_id }, true) => {
                ("send_private_forward_msg", "user_id", user_id)
            }
            _ => return self.send_confirmed(gallery.into_direct(true)).await,
        };
        let name = self.forward_display_name().await;
        let nodes = gallery.into_nodes(&name, &self.target.route.bot_self_id);
        let mut params = serde_json::Map::new();
        params.insert(target_key.to_owned(), json!(target_id));
        params.insert("messages".into(), json!(nodes));
        // A failed/unknown send could already be accepted. No direct-image retry.
        self.send_action_confirmed(json!({"action":action, "params":params}))
            .await
    }

    async fn forward_display_name(&self) -> String {
        // Query exactly the bot receiving this event, never the SDK's legacy
        // process-wide numeric bot. Failure is harmless: use its own real ID.
        let fallback = self.target.route.bot_self_id.clone();
        let Ok(Ok(info)) = tokio::time::timeout(
            Duration::from_secs(5),
            self.bot.call_routed(
                &self.target.route,
                json!({"action":"get_login_info", "params":{}}),
            ),
        )
        .await
        else {
            return fallback;
        };
        let data = info.get("data").unwrap_or(&info);
        let user_id = data.get("user_id").and_then(|id| match id {
            Value::String(id) => Some(id.clone()),
            Value::Number(id) => Some(id.to_string()),
            _ => None,
        });
        if user_id.as_deref() != Some(fallback.as_str()) {
            return fallback;
        }
        data.get("nickname")
            .and_then(Value::as_str)
            .filter(|name| !name.trim().is_empty())
            .map(|name| name.chars().take(128).collect())
            .unwrap_or(fallback)
    }

    async fn send_action_confirmed(&self, action: Value) -> anyhow::Result<()> {
        let receipt = tokio::time::timeout(
            Duration::from_secs(45),
            self.bot.call_routed_receipt(&self.target.route, action),
        )
        .await
        .context("消息发送回执超时")??;
        // Native adapters validate their own transport results in the SDK; their
        // legitimate response data can contain keys such as `data` or `status`.
        // Protocol-envelope interpretation below applies only to OneBot.
        if !self.is_onebot() {
            return Ok(());
        }
        // Known successful OneBot calls have their envelope removed by the SDK,
        // so native data or null remain valid. A raw protocol envelope, however,
        // must explicitly confirm success; enqueued/unknown actions cannot
        // release the next message, even when they carry retcode=0.
        let has_envelope = ["status", "retcode", "echo", "data"]
            .into_iter()
            .any(|key| receipt.raw.get(key).is_some());
        anyhow::ensure!(
            !has_envelope
                || (receipt.raw.get("status").and_then(Value::as_str) == Some("ok")
                    && receipt
                        .raw
                        .get("retcode")
                        .is_none_or(|code| code.as_i64() == Some(0))),
            "消息未返回成功发送回执"
        );
        Ok(())
    }

    fn is_onebot(&self) -> bool {
        matches!(
            self.target.route.platform.to_ascii_lowercase().as_str(),
            "onebot" | "onebot v11" | "onebot_v11"
        )
    }

    pub(crate) async fn notify(&self, message: SendableMessage) -> anyhow::Result<()> {
        self.bot
            .notify_routed(&self.target.route, self.action(message)?)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
