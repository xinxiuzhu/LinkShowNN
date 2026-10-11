//! 直播订阅的持久化记录。
//!
//! 保留既有文件字段，投递时再转换为 SDK 消息目标，不让磁盘结构决定发送协议。

use anyhow::{Result, bail};
use nuono_plugin::{RuntimeBotRoute, RuntimeMessageTarget, RuntimeTargetKind};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SubscriptionRoute {
    pub platform: String,
    pub instance_id: String,
    pub bot_self_id: String,
    pub target_type: String,
    pub target_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_parent: Option<String>,
}

impl SubscriptionRoute {
    pub fn from_target(target: &RuntimeMessageTarget) -> Self {
        let (target_type, target_id, target_parent) = match &target.kind {
            RuntimeTargetKind::Private { user_id } => ("private", user_id, None),
            RuntimeTargetKind::Group { group_id } => ("group", group_id, None),
            RuntimeTargetKind::Channel { channel_id, parent } => {
                ("channel", channel_id, parent.clone())
            }
        };
        Self {
            platform: target.route.platform.clone(),
            instance_id: target.route.instance_id.clone(),
            bot_self_id: target.route.bot_self_id.clone(),
            target_type: target_type.into(),
            target_id: target_id.clone(),
            target_parent,
        }
    }

    pub fn message_target(&self) -> Result<RuntimeMessageTarget> {
        let route = RuntimeBotRoute::new(&self.platform, &self.instance_id, &self.bot_self_id);
        let target = match self.target_type.as_str() {
            "private" => RuntimeMessageTarget::private(route, &self.target_id),
            "group" => RuntimeMessageTarget::group(route, &self.target_id),
            "channel" => {
                RuntimeMessageTarget::channel(route, &self.target_id, self.target_parent.clone())
            }
            other => bail!("unknown live subscription target type: {other}"),
        };
        target.validate()?;
        Ok(target)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LiveSubscription {
    pub room_id: u64,
    pub route: SubscriptionRoute,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn legacy_route_roundtrips_without_new_fields() {
        let legacy = json!({
            "platform": "onebot",
            "instance_id": "main",
            "bot_self_id": "10001",
            "target_type": "group",
            "target_id": "12345"
        });
        let route: SubscriptionRoute = serde_json::from_value(legacy.clone()).unwrap();
        assert_eq!(serde_json::to_value(&route).unwrap(), legacy);
        assert_eq!(
            route.message_target().unwrap(),
            RuntimeMessageTarget::group(RuntimeBotRoute::new("onebot", "main", "10001"), "12345")
        );
    }

    #[test]
    fn channel_route_preserves_exact_ids_and_parent() {
        let target = RuntimeMessageTarget::channel(
            RuntimeBotRoute::new("discord", "main", "18446744073709551615"),
            "18446744073709551614",
            Some("18446744073709551613"),
        );
        let route = SubscriptionRoute::from_target(&target);
        let encoded = serde_json::to_vec(&route).unwrap();
        let restored: SubscriptionRoute = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(restored.message_target().unwrap(), target);
    }
}
